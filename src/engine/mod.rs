//! 播放引擎：单线程编排（std::thread），持有队列/设备/解码句柄。
//!
//! 与 daemon 层的边界：
//! - 入：`ToEngine`（std mpsc，socket 层和解码线程都往里发）
//! - 出：`tokio::broadcast` 事件（send 是同步方法，可在本线程直接调用）
//! - 响应：每请求携带 `tokio::sync::oneshot`，处理完立刻回
//!
//! 参考 hummingbird `src/playback/thread.rs`，第一版精简掉：
//! gapless 双缓冲 / EQ / ReplayGain / AudioBlock 池 / 解码卡死恢复。

mod decoder;
mod output;
pub mod queue;
mod resample;

use crate::proto::{Event, PlayState, Reply, Request, StatusInfo};
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot};

use output::Output;
use queue::{Queue, QueueItem};

/// position 事件推送节流间隔
const POSITION_EVENT_INTERVAL: Duration = Duration::from_millis(250);

/// 引擎收到的所有消息（命令 + 解码线程回报）。
pub enum ToEngine {
    Request {
        id: u64,
        request: Request,
        reply: oneshot::Sender<Result<Reply, String>>,
    },
    /// daemon 层完成目录扫描后追加队列
    AddScanned {
        id: u64,
        items: Vec<QueueItem>,
        reply: oneshot::Sender<Result<Reply, String>>,
    },
    TrackEof {
        generation: u64,
    },
    TrackFatal {
        generation: u64,
        error: String,
    },
}

struct DecoderHandle {
    generation: u64,
    handle: JoinHandle<()>,
}

pub struct Engine {
    queue: Queue,
    state: PlayState,
    output: Output,
    decoder: Option<DecoderHandle>,
    /// 递增计数；切歌/seek 时 +1，旧解码线程发现不一致即退出
    generation: Arc<AtomicU64>,
    /// 当前曲从多少毫秒处开始解码（seek 用）
    seek_base_ms: u64,
    events: broadcast::Sender<Event>,
    /// 解码线程回报 EOF/错误用（与外部命令同一个接收端）
    to_engine_tx: mpsc::Sender<ToEngine>,
    last_position_event: Instant,
}

impl Engine {
    /// 启动引擎线程。`cmd_tx` 是命令通道的发送端克隆，供解码线程回报消息。
    pub fn spawn(
        cmd_tx: mpsc::Sender<ToEngine>,
        cmd_rx: mpsc::Receiver<ToEngine>,
        events: broadcast::Sender<Event>,
    ) -> Result<JoinHandle<()>> {
        let engine = Engine::new(events, cmd_tx)?;
        Ok(std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || engine.run(cmd_rx))?)
    }

    fn new(events: broadcast::Sender<Event>, to_engine_tx: mpsc::Sender<ToEngine>) -> Result<Self> {
        let output = Output::open()?;
        Ok(Self {
            queue: Queue::default(),
            state: PlayState::Stopped,
            output,
            decoder: None,
            generation: Arc::new(AtomicU64::new(0)),
            seek_base_ms: 0,
            events,
            to_engine_tx,
            last_position_event: Instant::now()
                - POSITION_EVENT_INTERVAL, // 首个事件立即发
        })
    }

    fn run(mut self, cmd_rx: mpsc::Receiver<ToEngine>) {
        loop {
            self.output.poll_pause();
            if self.state == PlayState::Playing
                && self.last_position_event.elapsed() >= POSITION_EVENT_INTERVAL
            {
                self.last_position_event = Instant::now();
                self.emit(Event::Position {
                    position_ms: self.position_ms(),
                });
            }
            match cmd_rx.recv_timeout(POSITION_EVENT_INTERVAL) {
                Ok(ToEngine::Request { request, reply, .. }) => {
                    let result = self.handle(request);
                    let _ = reply.send(result);
                }
                Ok(ToEngine::AddScanned { items, reply, .. }) => {
                    let added = items.len();
                    self.queue.items.extend(items);
                    self.emit(Event::QueueChanged);
                    let _ = reply.send(Ok(Reply::Added { added }));
                }
                Ok(ToEngine::TrackEof { generation }) => {
                    if self.decoder.as_ref().is_some_and(|d| d.generation == generation) {
                        self.decoder = None;
                        self.advance();
                    }
                }
                Ok(ToEngine::TrackFatal { generation, error }) => {
                    if self.decoder.as_ref().is_some_and(|d| d.generation == generation) {
                        self.decoder = None;
                        eprintln!("[rmusd] 跳过无法播放的曲子: {error}");
                        self.advance();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn emit(&self, event: Event) {
        // 没有订阅者时 send 返回 Err，忽略即可
        let _ = self.events.send(event);
    }

    fn handle(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Status => Ok(Reply::Status(Box::new(self.status()))),
            Request::QueueList => {
                let tracks = (0..self.queue.len())
                    .filter_map(|i| self.queue.track_info(i))
                    .collect();
                Ok(Reply::Queue { tracks })
            }
            Request::Play { index } => {
                self.play(index);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Pause => {
                self.pause();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::TogglePlay => {
                match self.state {
                    PlayState::Playing => self.pause(),
                    _ => self.play(None),
                }
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Stop => {
                self.stop();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Next => {
                self.advance();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Prev => {
                self.rewind();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Seek { position_ms } => {
                self.seek(position_ms);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::SeekRelative { delta_ms } => {
                let current = self.position_ms() as i64;
                let target = (current + delta_ms).max(0) as u64;
                self.seek(target);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::SetVolume { value } => {
                self.set_volume(f64::from(value));
                Ok(Reply::Ok)
            }
            Request::AdjustVolume { delta } => {
                let v = (self.output.volume() + f64::from(delta)).clamp(0.0, 1.0);
                self.set_volume(v);
                Ok(Reply::Ok)
            }
            Request::Clear => {
                self.kill_decoder();
                self.queue.clear();
                self.state = PlayState::Stopped;
                self.seek_base_ms = 0;
                self.emit(Event::QueueChanged);
                self.emit(Event::StateChanged {
                    state: PlayState::Stopped,
                });
                self.emit(Event::SongChanged { index: None });
                Ok(Reply::Ok)
            }
            Request::Remove { index } => {
                let removed_current = self.queue.remove(index);
                if removed_current {
                    self.stop();
                }
                self.emit(Event::QueueChanged);
                Ok(Reply::Ok)
            }
            Request::Shuffle => {
                self.queue.shuffle();
                self.emit(Event::QueueChanged);
                Ok(Reply::Status(Box::new(self.status())))
            }
            // Add 由 daemon 层拦截扫描，不应到达这里
            Request::Add { .. } => Err("内部错误：Add 未在 daemon 层处理".into()),
            // Quit 由 daemon 层处理，不应到达这里
            Request::Quit => Err("内部错误：Quit 未在 daemon 层处理".into()),
        }
    }

    // ---- 播放控制 ----

    /// 播放。Some(i) 从第 i 曲开始；None = 恢复暂停或从头（Stopped）。
    fn play(&mut self, index: Option<usize>) {
        match self.state {
            PlayState::Paused if index.is_none() => {
                self.output.play();
                self.state = PlayState::Playing;
                self.emit(Event::StateChanged {
                    state: PlayState::Playing,
                });
            }
            _ => {
                let index = index
                    .or(self.queue.current)
                    .unwrap_or(0);
                if index < self.queue.len() {
                    self.start_track(index, 0, true);
                }
            }
        }
    }

    fn pause(&mut self) {
        if self.state == PlayState::Playing {
            self.output.begin_pause();
            self.state = PlayState::Paused;
            self.emit(Event::StateChanged {
                state: PlayState::Paused,
            });
        }
    }

    fn stop(&mut self) {
        self.kill_decoder();
        self.output.reset().ok();
        self.seek_base_ms = 0;
        self.state = PlayState::Stopped;
        self.emit(Event::StateChanged {
            state: PlayState::Stopped,
        });
    }

    /// 下一曲（EOF 或用户）。队尾则停在 Stopped。
    fn advance(&mut self) {
        match self.queue.advance() {
            Some(next) => self.start_track(next, 0, true),
            None => self.stop(),
        }
    }

    /// 上一曲。
    fn rewind(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let prev = self.queue.rewind().expect("rewind 在非空队列必有值");
        self.start_track(prev, 0, true);
    }

    fn seek(&mut self, position_ms: u64) {
        let Some(index) = self.queue.current else {
            return;
        };
        if self.state == PlayState::Stopped {
            return;
        }
        let duration = self.queue.current_item().map(|i| i.duration_ms).unwrap_or(0);
        let position_ms = if duration > 0 {
            position_ms.min(duration.saturating_sub(1))
        } else {
            position_ms
        };
        let autoplay = self.state == PlayState::Playing;
        self.start_track(index, position_ms, autoplay);
    }

    /// 核心：从 index 曲的 position_ms 处开始播放。
    /// 杀旧解码线程 -> 重建输出流 -> 起新解码线程。
    fn start_track(&mut self, index: usize, position_ms: u64, autoplay: bool) {
        self.kill_decoder();

        let Ok(()) = self.output.reset() else {
            eprintln!("[rmusd] 重置输出流失败");
            return;
        };

        self.queue.current = Some(index);
        self.seek_base_ms = position_ms;

        // kill_decoder 已把计数 +1 作废旧线程；这里再 +1 作为新线程的代号
        let new_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        // 写端交给解码线程独占（回调消费、解码生产）
        let Some(producer) = self.output.take_producer() else {
            eprintln!("[rmusd] 获取输出写端失败");
            return;
        };

        let path = self.queue.items[index].path.clone();
        let device_rate = self.output.sample_rate();
        let device_channels = self.output.channels();
        let handle = decoder::spawn(
            path,
            position_ms,
            Arc::clone(&self.generation),
            new_gen,
            producer,
            device_rate,
            device_channels,
            self.to_engine_tx.clone(),
        );
        self.decoder = Some(DecoderHandle {
            generation: new_gen,
            handle,
        });

        if autoplay {
            self.state = PlayState::Playing;
            self.output.play();
        } else {
            // 保持 Paused：解码线程仍会预填 ring，但流不 play
            self.state = PlayState::Paused;
        }
        self.emit(Event::SongChanged { index: Some(index) });
        self.emit(Event::StateChanged { state: self.state });
    }

    fn kill_decoder(&mut self) {
        if let Some(d) = self.decoder.take() {
            self.generation.fetch_add(1, Ordering::SeqCst);
            let _ = d.handle.join();
        }
    }

    fn set_volume(&mut self, value: f64) {
        let value = value.clamp(0.0, 1.0);
        self.output.set_volume(value);
        self.emit(Event::VolumeChanged {
            value: value as f32,
        });
    }

    // ---- 状态 ----

    fn position_ms(&self) -> u64 {
        if self.state == PlayState::Stopped {
            return 0;
        }
        let played = self.output.played_frames().load(Ordering::Relaxed);
        self.seek_base_ms + played * 1000 / u64::from(self.output.sample_rate().max(1))
    }

    fn status(&self) -> StatusInfo {
        StatusInfo {
            state: self.state,
            current: self.queue.current,
            position_ms: self.position_ms(),
            duration_ms: self.queue.current_item().map(|i| i.duration_ms).unwrap_or(0),
            volume: self.output.volume() as f32,
            queue_len: self.queue.len(),
            track: self.queue.current.and_then(|i| self.queue.track_info(i)),
        }
    }
}
