//! 每曲一个解码线程：文件 -> symphonia 解码 -> planar f64 -> 声道适配
//! -> (按需)重采样 -> 交错 f32 -> rtrb ring。
//!
//! 参考 hummingbird `src/media/builtin/symphonia.rs` 的精简版：
//! - 坏包跳过不跳曲（DecodeError/瞬态 IoError -> continue）
//! - 切歌/seek 由引擎递增 generation 计数，本线程发现不一致即安静退出，
//!   避免旧线程把数据写进新曲的 ring
//! - ring 写满时睡 3ms 再试（背压）
//! - EOF 后等约一个 ring 时长再上报，让尾巴播完（简陋版 drain；
//!   TODO: 换成 hummingbird 那种精确的 DrainState 排空）

use anyhow::{Context, Result};
use rtrb::Producer;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use symphonia::core::{
    audio::{Audio, GenericAudioBufferRef},
    codecs::{
        audio::AudioDecoderOptions,
        registry::CodecRegistry,
    },
    errors::Error,
    formats::{FormatOptions, SeekMode, SeekTo, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
    units::Time,
};
use symphonia::default::codecs::{
    AdpcmDecoder, AlacDecoder, FlacDecoder, MpaDecoder, PcmDecoder, VorbisDecoder,
};

use super::resample::Resampler;
use super::ToEngine;

const RING_WRITE_WAIT: Duration = Duration::from_millis(3);
/// EOF 后等待 ring 排空的时长（ring 容量 100ms + 余量）
const EOF_DRAIN_WAIT: Duration = Duration::from_millis(150);

enum Outcome {
    Eof,
    Cancelled,
}

/// 启动一个解码线程。返回 JoinHandle；线程结束时若正常播完会向引擎发
/// `ToEngine::TrackEof`，若打不开/解不了会发 `ToEngine::TrackFatal`。
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    path: PathBuf,
    start_ms: u64,
    generation: Arc<AtomicU64>,
    my_generation: u64,
    mut producer: Producer<f32>,
    device_rate: u32,
    device_channels: usize,
    to_engine: std::sync::mpsc::Sender<ToEngine>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("decode-{}", path.display()))
        .spawn(move || {
            let result = decode_loop(
                &path,
                start_ms,
                &generation,
                my_generation,
                &mut producer,
                device_rate,
                device_channels,
            );
            let cancelled = generation.load(Ordering::Acquire) != my_generation;
            if cancelled {
                return;
            }
            match result {
                Ok(Outcome::Eof) => {
                    // 等尾巴播完；期间被切歌就不再上报
                    for _ in 0..(EOF_DRAIN_WAIT.as_millis() / 10) {
                        thread::sleep(Duration::from_millis(10));
                        if generation.load(Ordering::Acquire) != my_generation {
                            return;
                        }
                    }
                    let _ = to_engine.send(ToEngine::TrackEof {
                        generation: my_generation,
                    });
                }
                Ok(Outcome::Cancelled) => {}
                Err(error) => {
                    let _ = to_engine.send(ToEngine::TrackFatal {
                        generation: my_generation,
                        error: format!("{}: {error:#}", path.display()),
                    });
                }
            }
        })
        .expect("spawn decoder thread")
}

fn cancelled(generation: &AtomicU64, mine: u64) -> bool {
    generation.load(Ordering::Acquire) != mine
}

fn decode_loop(
    path: &PathBuf,
    start_ms: u64,
    generation: &AtomicU64,
    my_generation: u64,
    producer: &mut Producer<f32>,
    device_rate: u32,
    device_channels: usize,
) -> Result<Outcome> {
    let file = File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .with_context(|| format!("无法识别音频格式: {}", path.display()))?;

    let track = format
        .first_track_known_codec(TrackType::Audio)
        .context("没有可播放的音频轨")?;
    let codec_params = track.codec_params.as_ref().context("轨道缺少 codec 参数")?;
    let audio_params = codec_params.audio().context("轨道不含音频参数")?;
    let src_channels = audio_params
        .channels
        .as_ref()
        .map(|c| c.count())
        .unwrap_or(2)
        .max(1);
    let track_id = track.id;

    let mut codecs = CodecRegistry::new();
    codecs.register_audio_decoder::<MpaDecoder>();
    codecs.register_audio_decoder::<PcmDecoder>();
    codecs.register_audio_decoder::<AlacDecoder>();
    codecs.register_audio_decoder::<FlacDecoder>();
    codecs.register_audio_decoder::<VorbisDecoder>();
    codecs.register_audio_decoder::<AdpcmDecoder>();
    let mut decoder = codecs
        .make_audio_decoder(audio_params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow::anyhow!("不支持的编码: {e}"))?;

    if start_ms > 0 {
        let time = Time::try_from_secs_f64(start_ms as f64 / 1000.0).unwrap_or(Time::ZERO);
        format.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: None })?;
    }

    // planar f64 源缓冲 / 声道适配后缓冲 / 重采样输出缓冲 / 交错缓冲
    let mut planar: Vec<Vec<f64>> = vec![Vec::new(); src_channels];
    let mut adapted: Vec<Vec<f64>> = vec![Vec::new(); device_channels];
    let mut resampled: Vec<Vec<f64>> = vec![Vec::new(); device_channels];
    let mut interleaved: Vec<f32> = Vec::new();
    let mut resampler: Option<Resampler> = None;

    loop {
        if cancelled(generation, my_generation) {
            return Ok(Outcome::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break, // EOF
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        // 多轨容器里只解选定轨
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // 坏包跳过，不跳曲
            Err(Error::DecodeError(_)) => continue,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };

        let rate = decoded.spec().rate();
        let channels = decoded.spec().channels().count();
        if channels != src_channels {
            return Err(anyhow::anyhow!(
                "播放中通道数变化（{src_channels} -> {channels}），不支持"
            ));
        }

        // 解码包转 planar f64，适配到设备通道数
        convert_planar(&decoded, &mut planar);
        adapt_channels(&planar, &mut adapted);

        // 按需重采样，输出统一为设备采样率
        let out: &[Vec<f64>] = if rate != device_rate {
            if resampler
                .as_ref()
                .is_none_or(|r| !r.matches(rate, device_rate, device_channels))
            {
                resampler = Some(Resampler::new(rate, device_rate, device_channels)?);
            }
            for ch in &mut resampled {
                ch.clear();
            }
            resampler
                .as_mut()
                .expect("刚创建的 resampler")
                .push(&adapted, &mut resampled)?;
            &resampled
        } else {
            &adapted
        };

        interleave(out, &mut interleaved);
        if !write_ring(producer, &interleaved, generation, my_generation)? {
            return Ok(Outcome::Cancelled);
        }
    }

    // 排空重采样器的尾巴
    if let Some(r) = resampler.as_mut() {
        for ch in &mut resampled {
            ch.clear();
        }
        r.flush(&mut resampled)?;
        if !resampled.is_empty() && !resampled[0].is_empty() {
            interleave(&resampled, &mut interleaved);
            if !write_ring(producer, &interleaved, generation, my_generation)? {
                return Ok(Outcome::Cancelled);
            }
        }
    }

    Ok(Outcome::Eof)
}

fn convert_planar(decoded: &GenericAudioBufferRef<'_>, planar: &mut [Vec<f64>]) {
    for v in planar.iter_mut() {
        v.clear();
    }
    let frames = decoded.frames();
    let channels = decoded.spec().channels().count();

    macro_rules! conv {
        ($buf:expr, $f:expr) => {{
            for ch in 0..planar.len().min(channels) {
                if let Some(plane) = $buf.plane(ch) {
                    planar[ch].extend(plane.iter().take(frames).map($f));
                }
            }
        }};
    }

    match decoded {
        GenericAudioBufferRef::U8(v) => conv!(v, |&s| (f64::from(s) - 128.0) / 128.0),
        GenericAudioBufferRef::U16(v) => conv!(v, |&s| (f64::from(s) - 32768.0) / 32768.0),
        GenericAudioBufferRef::U24(v) => conv!(v, |s| {
            (f64::from(s.0) - 8_388_608.0) / 8_388_608.0
        }),
        GenericAudioBufferRef::U32(v) => {
            conv!(v, |&s| (f64::from(s) - 2_147_483_648.0) / 2_147_483_648.0)
        }
        GenericAudioBufferRef::S8(v) => conv!(v, |&s| f64::from(s) / 128.0),
        GenericAudioBufferRef::S16(v) => conv!(v, |&s| f64::from(s) / 32768.0),
        GenericAudioBufferRef::S24(v) => conv!(v, |s| f64::from(s.0) / 8_388_608.0),
        GenericAudioBufferRef::S32(v) => conv!(v, |&s| f64::from(s) / 2_147_483_648.0),
        GenericAudioBufferRef::F32(v) => conv!(v, |&s| f64::from(s)),
        GenericAudioBufferRef::F64(v) => conv!(v, |&s| s),
    }
}

/// planar 源声道 -> 设备声道：
/// 相同直通；mono 复制到所有目标；源少于目标时前几路直通其余静音；源多于目标时平均下混。
fn adapt_channels(src: &[Vec<f64>], dst: &mut [Vec<f64>]) {
    let src_ch = src.len();
    let frames = src.first().map(|v| v.len()).unwrap_or(0);
    for d in dst.iter_mut() {
        d.clear();
    }
    if src_ch == dst.len() {
        for (d, s) in dst.iter_mut().zip(src.iter()) {
            d.extend_from_slice(s);
        }
    } else if src_ch == 1 {
        for d in dst.iter_mut() {
            d.extend_from_slice(&src[0]);
        }
    } else if src_ch < dst.len() {
        for (i, d) in dst.iter_mut().enumerate() {
            if let Some(s) = src.get(i) {
                d.extend_from_slice(s);
            } else {
                d.resize(frames, 0.0);
            }
        }
    } else {
        for fi in 0..frames {
            let sum: f64 = src.iter().map(|s| s[fi]).sum();
            let avg = sum / src.len() as f64;
            for d in dst.iter_mut() {
                d.push(avg);
            }
        }
    }
}

fn interleave(planar: &[Vec<f64>], out: &mut Vec<f32>) {
    let frames = planar.first().map(|v| v.len()).unwrap_or(0);
    out.clear();
    out.reserve(frames * planar.len());
    for f in 0..frames {
        for ch in planar {
            out.push(ch[f] as f32);
        }
    }
}

/// 把交错数据写入 ring，满则等。返回 false 表示期间被切歌（generation 变了）。
fn write_ring(
    producer: &mut Producer<f32>,
    data: &[f32],
    generation: &AtomicU64,
    my_generation: u64,
) -> Result<bool> {
    let mut offset = 0;
    while offset < data.len() {
        if cancelled(generation, my_generation) {
            return Ok(false);
        }
        // 必须用 slots() 而非 cached_slots()：后者不刷新 consumer 进度缓存，
        // 写满一次后就永远返回 0，解码线程会卡死在这里
        let avail = producer.slots();
        if avail == 0 {
            thread::sleep(RING_WRITE_WAIT);
            continue;
        }
        let n = avail.min(data.len() - offset);
        let mut chunk = producer.write_chunk(n).context("输出流已关闭")?;
        let written = {
            let (first, second) = chunk.as_mut_slices();
            first.copy_from_slice(&data[offset..offset + first.len()]);
            let first_len = first.len();
            if !second.is_empty() {
                second
                    .copy_from_slice(&data[offset + first_len..offset + first_len + second.len()]);
            }
            first_len + second.len()
        };
        chunk.commit_all();
        offset += written;
    }
    Ok(true)
}

