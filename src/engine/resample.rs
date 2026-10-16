//! 精简重采样器：文件采样率 != 设备采样率时使用。
//!
//! 参考 hummingbird `src/devices/resample.rs`，大幅精简：
//! - rubato `Fft<f64>` 内核，`FixedSync::Input` 模式（凑满固定输入块才出数据）
//! - 输入用 VecDeque 攒块，输出直接返回 `&[Vec<f64>]` 视图
//! - flush 只是泵入静音把尾巴挤出来（可能多几十毫秒尾音），
//!   TODO: 以后从 hummingbird 搬精确截断的 flush_into

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler as _};

/// 输入块大小（帧）。凑满才处理，块越大吞吐越高、延迟也越高。
const CHUNK_FRAMES: usize = 2048;

pub struct Resampler {
    inner: Fft<f64>,
    src_rate: u32,
    dst_rate: u32,
    in_channels: usize,
    input: Vec<std::collections::VecDeque<f64>>,
    in_chunk: Vec<Vec<f64>>,
    out_chunk: Vec<Vec<f64>>,
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> anyhow::Result<Self> {
        let inner = Fft::<f64>::new(
            src_rate as usize,
            dst_rate as usize,
            CHUNK_FRAMES,
            channels,
            FixedSync::Input,
        )?;
        let out_max = inner.output_frames_max();
        Ok(Self {
            inner,
            src_rate,
            dst_rate,
            in_channels: channels,
            input: (0..channels)
                .map(|_| std::collections::VecDeque::with_capacity(CHUNK_FRAMES * 2))
                .collect(),
            in_chunk: (0..channels).map(|_| vec![0.0; CHUNK_FRAMES]).collect(),
            out_chunk: (0..channels).map(|_| vec![0.0; out_max]).collect(),
        })
    }

    pub fn needs_resampling(&self) -> bool {
        self.src_rate != self.dst_rate
    }

    /// 参数是否与当前实例一致（源/目标采样率 + 通道数）
    pub fn matches(&self, src_rate: u32, dst_rate: u32, channels: usize) -> bool {
        self.src_rate == src_rate && self.dst_rate == dst_rate && self.in_channels == channels
    }

    fn input_available(&self) -> usize {
        self.input.iter().map(|b| b.len()).min().unwrap_or(0)
    }

    /// 追加一段 planar 输入，处理产生的输出追加到 `out`（每声道一个 Vec）。
    pub fn push(&mut self, planar: &[Vec<f64>], out: &mut [Vec<f64>]) -> anyhow::Result<()> {
        let frames = planar.first().map(|v| v.len()).unwrap_or(0);
        for (buf, chan) in self.input.iter_mut().zip(planar.iter()) {
            let take = chan.len().min(frames);
            buf.extend(chan.iter().take(take));
        }

        while self.input_available() >= CHUNK_FRAMES {
            for (chunk, buf) in self.in_chunk.iter_mut().zip(self.input.iter_mut()) {
                for (dst, src) in chunk.iter_mut().zip(buf.drain(..CHUNK_FRAMES)) {
                    *dst = src;
                }
            }
            self.process_once(out)?;
        }
        Ok(())
    }

    /// 泵入静音，把内部缓冲的尾巴挤出来。EOF 时调用。
    pub fn flush(&mut self, out: &mut [Vec<f64>]) -> anyhow::Result<()> {
        if !self.needs_resampling() {
            return Ok(());
        }
        // 两个块通常足够排空；限制循环次数防止理论上的不收敛
        for _ in 0..4 {
            if self.input_available() == 0 {
                for chunk in &mut self.in_chunk {
                    chunk.fill(0.0);
                }
            } else {
                let take = self.input_available().min(CHUNK_FRAMES);
                for (chunk, buf) in self.in_chunk.iter_mut().zip(self.input.iter_mut()) {
                    let mut n = 0;
                    while n < take {
                        chunk[n] = buf.pop_front().unwrap_or(0.0);
                        n += 1;
                    }
                    // 块的剩余部分补零
                    for slot in chunk.iter_mut().skip(take) {
                        *slot = 0.0;
                    }
                }
            }
            let before = out[0].len();
            self.process_once(out)?;
            if out[0].len() == before && self.input_available() == 0 {
                break;
            }
        }
        Ok(())
    }

    fn process_once(&mut self, out: &mut [Vec<f64>]) -> anyhow::Result<()> {
        let out_max = self.inner.output_frames_max();
        let input_adapter =
            SequentialSliceOfVecs::new(&self.in_chunk, self.in_channels, CHUNK_FRAMES)?;
        let mut output_adapter =
            SequentialSliceOfVecs::new_mut(&mut self.out_chunk, self.in_channels, out_max)?;
        let (_, frames_written) = self
            .inner
            .process_into_buffer(&input_adapter, &mut output_adapter, None)?;
        for (out_ch, temp) in out
            .iter_mut()
            .zip(self.out_chunk.iter())
            .take(self.in_channels)
        {
            out_ch.extend_from_slice(&temp[..frames_written.min(out_max)]);
        }
        Ok(())
    }
}
