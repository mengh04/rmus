//! cpal 音频输出：无锁 ring buffer + 回调内增益斜坡。
//!
//! 参考 hummingbird `src/devices/builtin/cpal.rs` 的精简版：
//! - 回调只做"读 ring -> 补零 -> 音量斜坡"，无锁、无分配、无系统调用
//! - 重活（解码/重采样/声道适配/交错）都在解码线程预完成
//! - pause 先淡出 50ms 再真正停流，避免爆音
//! - seek/切歌用 `reset()` 销毁重建流（顺带获得一个全新的空 ring）

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 设备回调目标缓冲时长
const DEVICE_BUFFER_TARGET: Duration = Duration::from_millis(20);
/// 内部 ring buffer 时长，给解码器吸收调度抖动
const RING_BUFFER_TARGET: Duration = Duration::from_millis(100);
/// pause 淡出完成后真正暂停流的等待时长
const PAUSE_FADE_WAIT: Duration = Duration::from_millis(50);
/// 增益斜坡时长（毫秒），满幅 0 -> 1 或 1 -> 0 的耗时
const GAIN_RAMP_MS: f64 = 15.0;

/// f64 的原子类型（音量在回调里读、引擎线程写）。
pub struct AtomicF64 {
    inner: AtomicU64,
}

impl AtomicF64 {
    pub fn new(value: f64) -> Self {
        Self {
            inner: AtomicU64::new(value.to_bits()),
        }
    }

    pub fn store(&self, value: f64, ordering: Ordering) {
        self.inner.store(value.to_bits(), ordering)
    }

    pub fn load(&self, ordering: Ordering) -> f64 {
        f64::from_bits(self.inner.load(ordering))
    }
}

/// 线性增益斜坡：每个回调内逐帧向 target 步进，消除音量突变的爆音。
struct GainRamp {
    current: f64,
    step: f64,
    frame_pos: usize,
}

impl GainRamp {
    fn new(sample_rate: u32) -> Self {
        let ramp_frames = (f64::from(sample_rate) * GAIN_RAMP_MS / 1000.0).max(1.0);
        Self {
            current: 0.0,
            step: 1.0 / ramp_frames,
            frame_pos: 0,
        }
    }

    /// `data` 是交错缓冲，按帧步进增益
    fn apply(&mut self, data: &mut [f32], channels: usize, target: f64) {
        if channels == 0 || data.is_empty() {
            return;
        }
        // 稳态 unity 直通零开销
        if self.current == 1.0 && target == 1.0 {
            self.frame_pos = (self.frame_pos + data.len()) % channels;
            return;
        }
        for sample in data.iter_mut() {
            if self.frame_pos == 0 {
                if self.current < target {
                    self.current = (self.current + self.step).min(target);
                } else {
                    self.current = (self.current - self.step).max(target);
                }
            }
            *sample = (*sample as f64 * self.current) as f32;
            self.frame_pos = (self.frame_pos + 1) % channels;
        }
    }
}

/// ring 里还剩多少样本没被回调读走（用于估算已播放位置之外的积压）。
fn read_available<T: Copy>(consumer: &mut Consumer<T>, data: &mut [T]) -> usize {
    let readable = consumer.slots().min(data.len());
    if readable == 0 {
        return 0;
    }
    let Ok(chunk) = consumer.read_chunk(readable) else {
        return 0;
    };
    let (first, second) = chunk.as_slices();
    data[..first.len()].copy_from_slice(first);
    data[first.len()..first.len() + second.len()].copy_from_slice(second);
    let read = first.len() + second.len();
    chunk.commit_all();
    read
}

/// f32 交错输出流。`reset()` 会整体重建（新 ring、新回调、新播放计数）。
pub struct Output {
    device: cpal::Device,
    config: cpal::StreamConfig,
    stream: Option<cpal::Stream>,
    producer: Option<Producer<f32>>,
    target_gain: Arc<AtomicF64>,
    /// 声卡回调累计消费的帧数；reset 后换成新的
    played_frames: Arc<AtomicU64>,
    last_user_volume: f64,
    pause_at: Option<Instant>,
    channels: usize,
    sample_rate: u32,
}

impl Output {
    /// 打开默认输出设备，固定 f32 交错格式（采样率/通道数用设备默认）。
    /// 文件侧的重采样/声道适配由解码线程完成。
    pub fn open() -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("找不到默认音频输出设备")?;
        let default = device
            .default_output_config()
            .context("无法获取默认输出格式")?;

        let config = if default.sample_format() == cpal::SampleFormat::F32 {
            stream_config_from(&default, default.buffer_size())
        } else {
            // 默认格式不是 f32：找一条采样率/通道一致的 f32 配置
            let mut found = None;
            for range in device
                .supported_output_configs()
                .context("无法枚举设备支持的输出格式")?
            {
                if range.sample_format() != cpal::SampleFormat::F32
                    || range.channels() != default.channels()
                {
                    continue;
                }
                let rate = default.sample_rate();
                let rate = rate.clamp(range.min_sample_rate(), range.max_sample_rate());
                if let Some(cfg) = range.try_with_sample_rate(rate) {
                    found = Some(stream_config_from(&cfg, &range.buffer_size().clone()));
                    break;
                }
            }
            found.context("设备不支持 f32 输出")?
        };

        let channels = config.channels as usize;
        let sample_rate = config.sample_rate;
        Ok(Self {
            device,
            config,
            stream: None,
            producer: None,
            target_gain: Arc::new(AtomicF64::new(1.0)),
            played_frames: Arc::new(AtomicU64::new(0)),
            last_user_volume: 1.0,
            pause_at: None,
            channels,
            sample_rate,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// 已被声卡消费的帧数（引擎线程读取）
    pub fn played_frames(&self) -> &Arc<AtomicU64> {
        &self.played_frames
    }

    /// （重新）构建流。构建后流处于 paused 状态，需要调用 `play()` 才会发声。
    fn build(&mut self) -> Result<()> {
        self.stream = None; // 先销毁旧流（同时释放旧回调对旧 Arc 的引用不影响）
        self.producer = None;

        let ring_samples =
            frames_for_duration(self.sample_rate, RING_BUFFER_TARGET) as usize * self.channels;
        let (producer, mut consumer) = RingBuffer::<f32>::new(ring_samples);
        let target_gain = self.target_gain.clone();
        let played = Arc::new(AtomicU64::new(0));
        let played_cb = played.clone();
        let channels = self.channels;
        let mut ramp = GainRamp::new(self.sample_rate);

        let stream = self.device.build_output_stream(
            self.config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let read = read_available(&mut consumer, data);
                if read < data.len() {
                    data[read..].fill(0.0); // underrun：补静音
                }
                played_cb.fetch_add((read / channels.max(1)) as u64, Ordering::Relaxed);
                let target = target_gain.load(Ordering::Relaxed);
                ramp.apply(data, channels, target);
            },
            |err| eprintln!("[rmusd] 音频流错误: {err}"),
            None,
        );
        let stream = stream.context("构建音频输出流失败")?;
        self.stream = Some(stream);
        self.producer = Some(producer);
        self.played_frames = played;
        Ok(())
    }

    /// 销毁重建流：清空 ring 中的陈旧数据（seek/切歌时用）。重建后处于 paused。
    pub fn reset(&mut self) -> Result<()> {
        self.pause_at = None;
        self.build()
    }

    /// 取走写端（build 后只能取一次）。解码线程独占持有； Producer 不可 Clone。
    pub fn take_producer(&mut self) -> Option<Producer<f32>> {
        if self.producer.is_none() {
            self.build().ok();
        }
        self.producer.take()
    }

    /// 恢复播放（音量从当前值淡入到用户音量）。
    pub fn play(&mut self) {
        self.pause_at = None;
        self.target_gain
            .store(self.last_user_volume, Ordering::Relaxed);
        if let Some(stream) = &self.stream {
            let _ = stream.play();
        }
    }

    /// 开始暂停流程：先把目标增益置 0 淡出，50ms 后由 `poll_pause` 真正停流。
    pub fn begin_pause(&mut self) {
        if self.pause_at.is_some() {
            return;
        }
        self.target_gain.store(0.0, Ordering::Relaxed);
        self.pause_at = Some(Instant::now() + PAUSE_FADE_WAIT);
    }

    /// 引擎主循环定期调用，完成延迟的真正暂停。
    pub fn poll_pause(&mut self) {
        if let Some(at) = self.pause_at && Instant::now() >= at {
            if let Some(stream) = &self.stream {
                let _ = stream.pause();
            }
            self.pause_at = None;
        }
    }

    /// 设置音量（0.0..=1.0，超出会 clamp）。播放中通过回调斜坡平滑过渡。
    pub fn set_volume(&mut self, value: f64) {
        self.last_user_volume = value.clamp(0.0, 1.0);
        if self.pause_at.is_none() {
            self.target_gain
                .store(self.last_user_volume, Ordering::Relaxed);
        }
    }

    pub fn volume(&self) -> f64 {
        self.last_user_volume
    }
}

fn stream_config_from(
    config: &cpal::SupportedStreamConfig,
    buffer_size: &cpal::SupportedBufferSize,
) -> cpal::StreamConfig {
    let target = frames_for_duration(config.sample_rate(), DEVICE_BUFFER_TARGET);
    let buffer_size = match buffer_size {
        cpal::SupportedBufferSize::Range { min, max } => {
            cpal::BufferSize::Fixed(target.clamp(*min, *max))
        }
        cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
    };
    cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: config.sample_rate(),
        buffer_size,
    }
}

fn frames_for_duration(sample_rate: u32, duration: Duration) -> u32 {
    ((u64::from(sample_rate) * duration.as_micros() as u64) / 1_000_000).max(1) as u32
}
