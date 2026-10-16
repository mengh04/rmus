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
mod store;

use crate::proto::{Event, PlayState, RepeatMode, Reply, Request, StatusInfo};
use anyhow::Result;
use std::collections::BTreeMap;
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
    /// daemon 层完成扫描后替换队列并从头播放
    OpenScanned {
        id: u64,
        items: Vec<QueueItem>,
        reply: oneshot::Sender<Result<Reply, String>>,
    },
    /// daemon 层完成扫描后进入试听态（临时队列）
    PeekScanned {
        id: u64,
        items: Vec<QueueItem>,
        reply: oneshot::Sender<Result<Reply, String>>,
    },
    /// 库模糊播放：把这首插到当前曲后立即播（空队列则直接播）
    PlayNow {
        id: u64,
        item: QueueItem,
        reply: oneshot::Sender<Result<Reply, String>>,
    },
    /// daemon 层完成扫描后追加到命名队列
    PlaylistAddScanned {
        id: u64,
        name: String,
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

/// 试听态存档：进入 peek 时的完整播放状态，restore 时原样恢复
struct PeekSnapshot {
    queue: Queue,
    queue_name: Option<String>,
    /// 存档时的播放进度（毫秒）。必须在换队列（输出流 reset 清零帧计数）前取
    position_ms: u64,
    /// 恢复后回到的状态：Playing 继续播，Paused 停在原进度
    state: PlayState,
    repeat: RepeatMode,
    random: bool,
}

pub struct Engine {
    queue: Queue,
    /// 当前队列的名字；None = 匿名临时队列（open/add 出来的）
    queue_name: Option<String>,
    /// 命名队列（播放列表）
    named: BTreeMap<String, Vec<QueueItem>>,
    repeat: RepeatMode,
    random: bool,
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
    /// 状态是否需要落盘
    dirty: bool,
    /// 连续解码失败计数；达到队列长度即认为整队列都播不了，停止
    fatal_streak: usize,
    last_position_event: Instant,
    /// 试听态存档；Some = 当前队列是临时的
    peek: Option<PeekSnapshot>,
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
        let mut output = Output::open()?;
        let persisted = store::load();
        let mut queue = Queue {
            items: persisted.current_queue,
            current: persisted.current_index,
        };
        if queue.current.is_some_and(|i| i >= queue.items.len()) {
            queue.current = queue.items.len().checked_sub(1);
        }
        if let Some(volume) = persisted.volume {
            output.set_volume(volume);
        }
        Ok(Self {
            queue,
            queue_name: persisted.queue_name,
            named: persisted.named,
            repeat: persisted.repeat.unwrap_or_default(),
            random: persisted.random.unwrap_or(false),
            state: PlayState::Stopped,
            output,
            decoder: None,
            generation: Arc::new(AtomicU64::new(0)),
            seek_base_ms: 0,
            events,
            to_engine_tx,
            dirty: false,
            fatal_streak: 0,
            peek: None,
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
                    self.dirty = true;
                    self.emit(Event::QueueChanged);
                    let _ = reply.send(Ok(Reply::Added { added }));
                }
                Ok(ToEngine::OpenScanned { items, reply, .. }) => {
                    if items.is_empty() {
                        let _ = reply.send(Err("没有找到可播放的音频文件".into()));
                        continue;
                    }
                    // 显式换队列 = 放弃试听恢复点
                    self.peek = None;
                    self.kill_decoder();
                    self.queue.items = items;
                    self.queue.current = None;
                    self.queue_name = None;
                    self.dirty = true;
                    self.emit(Event::QueueChanged);
                    self.start_track(0, 0, true);
                    let _ = reply.send(Ok(Reply::Status(Box::new(self.status()))));
                }
                Ok(ToEngine::PeekScanned { items, reply, .. }) => {
                    if items.is_empty() {
                        let _ = reply.send(Err("没有找到可播放的音频文件".into()));
                        continue;
                    }
                    self.enter_peek(items);
                    let _ = reply.send(Ok(Reply::Status(Box::new(self.status()))));
                }
                Ok(ToEngine::PlayNow { item, reply, .. }) => {
                    self.play_now(item);
                    let _ = reply.send(Ok(Reply::Status(Box::new(self.status()))));
                }
                Ok(ToEngine::PlaylistAddScanned {
                    name,
                    items,
                    reply,
                    ..
                }) => {
                    let added = items.len();
                    self.named.entry(name.clone()).or_default().extend(items);
                    if self.queue_name.as_deref() == Some(name.as_str()) {
                        // 命名队列就是当前队列：同步追加，播放体验连续
                        let extra: Vec<QueueItem> = self
                            .named
                            .get(&name)
                            .map(|items| items[items.len() - added..].to_vec())
                            .unwrap_or_default();
                        self.queue.items.extend(extra);
                        self.emit(Event::QueueChanged);
                    }
                    self.dirty = true;
                    let _ = reply.send(Ok(Reply::Added { added }));
                }
                Ok(ToEngine::TrackEof { generation }) => {
                    if self.decoder.as_ref().is_some_and(|d| d.generation == generation) {
                        self.decoder = None;
                        self.fatal_streak = 0; // 正常播完一首，重置连续失败计数
                        self.on_eof();
                    }
                }
                Ok(ToEngine::TrackFatal { generation, error }) => {
                    if self.decoder.as_ref().is_some_and(|d| d.generation == generation) {
                        self.decoder = None;
                        eprintln!("[rmusd] 跳过无法播放的曲子: {error}");
                        self.fatal_streak += 1;
                        let limit = self.queue.len().max(1);
                        if self.fatal_streak >= limit {
                            // 整个队列挨个试过都失败：停止（试听态则恢复），避免循环重试烧 CPU
                            eprintln!("[rmusd] 队列连续 {limit} 首无法播放，停止播放");
                            self.finish_or_restore();
                        } else {
                            self.on_eof();
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if self.dirty {
                self.dirty = false;
                self.persist();
            }
        }
    }

    fn persist(&self) {
        store::save(&store::PersistedState {
            volume: Some(self.output.volume()),
            repeat: Some(self.repeat),
            random: Some(self.random),
            queue_name: self.queue_name.clone(),
            current_index: self.queue.current,
            current_queue: self.queue.items.clone(),
            named: self.named.clone(),
        });
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
                self.on_next();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Prev => {
                self.on_prev();
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
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::AdjustVolume { delta } => {
                let v = (self.output.volume() + f64::from(delta)).clamp(0.0, 1.0);
                self.set_volume(v);
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::Clear => {
                self.kill_decoder();
                self.queue.clear();
                // 当前是命名队列时同步清空它
                if let Some(name) = self.queue_name.clone() {
                    self.named.insert(name, Vec::new());
                }
                self.state = PlayState::Stopped;
                self.seek_base_ms = 0;
                self.dirty = true;
                self.emit(Event::QueueChanged);
                self.emit(Event::StateChanged {
                    state: PlayState::Stopped,
                });
                self.emit(Event::SongChanged { index: None });
                Ok(Reply::Ok)
            }
            Request::Remove { index } => {
                let removed_current = self.queue.remove(index);
                if let Some(name) = self.queue_name.clone()
                    && let Some(items) = self.named.get_mut(&name) {
                        items.remove(index);
                    }
                if removed_current {
                    self.stop();
                }
                self.dirty = true;
                self.emit(Event::QueueChanged);
                Ok(Reply::Ok)
            }
            Request::Shuffle => {
                self.queue.shuffle();
                if let Some(name) = self.queue_name.clone()
                    && let Some(items) = self.named.get_mut(&name) {
                        *items = self.queue.items.clone();
                    }
                self.dirty = true;
                self.emit(Event::QueueChanged);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::PlaylistsList => {
                let names = self.named.keys().cloned().collect();
                Ok(Reply::Playlists {
                    names,
                    current: self.queue_name.clone(),
                })
            }
            Request::PlaylistNew { name } => {
                if self.named.contains_key(&name) {
                    return Err(format!("队列 {name} 已存在"));
                }
                self.named.insert(name, Vec::new());
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistSave { name } => {
                self.named.insert(name.clone(), self.queue.items.clone());
                self.queue_name = Some(name);
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistPlay { name } => {
                let Some(items) = self.named.get(&name) else {
                    return Err(format!("队列 {name} 不存在"));
                };
                if items.is_empty() {
                    return Err(format!("队列 {name} 是空的"));
                }
                let items = items.clone();
                // 显式换队列 = 放弃试听恢复点
                self.peek = None;
                self.kill_decoder();
                self.queue.items = items;
                self.queue.current = None;
                self.queue_name = Some(name);
                self.dirty = true;
                self.emit(Event::QueueChanged);
                self.start_track(0, 0, true);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::PlaylistRemove { name } => {
                if self.queue_name.as_deref() == Some(name.as_str()) {
                    return Err(format!("队列 {name} 正在播放，无法删除"));
                }
                if self.named.remove(&name).is_none() {
                    return Err(format!("队列 {name} 不存在"));
                }
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::SetRepeat { mode } => {
                self.repeat = mode.unwrap_or(match self.repeat {
                    RepeatMode::Off => RepeatMode::All,
                    RepeatMode::All => RepeatMode::One,
                    RepeatMode::One => RepeatMode::Off,
                });
                self.dirty = true;
                self.emit(Event::ModeChanged {
                    repeat: self.repeat,
                    random: self.random,
                });
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::SetRandom { on } => {
                self.random = on.unwrap_or(!self.random);
                self.dirty = true;
                self.emit(Event::ModeChanged {
                    repeat: self.repeat,
                    random: self.random,
                });
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Back => match self.peek.as_ref() {
                Some(_) => {
                    self.restore();
                    Ok(Reply::Status(Box::new(self.status())))
                }
                None => Err("当前没有试听在等待恢复".into()),
            },
            // Add/Open/PlaylistAdd/Peek 由 daemon 层拦截扫描，不应到达这里
            Request::Add { .. } => Err("内部错误：Add 未在 daemon 层处理".into()),
            Request::Open { .. } => Err("内部错误：Open 未在 daemon 层处理".into()),
            Request::Peek { .. } => Err("内部错误：Peek 未在 daemon 层处理".into()),
            Request::PlaylistAdd { .. } => {
                Err("内部错误：PlaylistAdd 未在 daemon 层处理".into())
            }
            // Quit 由 daemon 层处理，不应到达这里
            Request::Quit => Err("内部错误：Quit 未在 daemon 层处理".into()),
            // Find/PlayMatch/LibRescan 由 daemon 层处理（查库/扫描），不应到达这里
            Request::Find { .. } | Request::PlayMatch { .. } | Request::LibRescan => {
                Err("内部错误：库请求未在 daemon 层处理".into())
            }
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

    /// 下一曲（EOF 自动推进）。单曲循环时重播当前曲；否则按模式选曲。
    fn on_eof(&mut self) {
        if self.repeat == RepeatMode::One && self.queue.current.is_some() {
            let cur = self.queue.current.expect("已检查 Some");
            self.start_track(cur, 0, true);
            return;
        }
        // EOF 推进：只有 repeat all 才在队尾回绕
        let wrap = self.repeat == RepeatMode::All;
        match self.pick_next(wrap) {
            Some(i) => self.start_track(i, 0, true),
            None => self.finish_or_restore(),
        }
    }

    /// 播放自然结束：试听态下恢复原队列，否则停止
    fn finish_or_restore(&mut self) {
        if self.peek.is_some() {
            self.restore();
        } else {
            self.stop();
        }
    }

    /// 插播：插到当前曲后立即播放；空队列/未开始时作为第一曲。
    /// 播完由 on_eof 正常继续队列，不打断原有队列结构。
    fn play_now(&mut self, item: QueueItem) {
        let at = match self.queue.current {
            Some(cur) => cur + 1,
            None => 0,
        };
        let at = at.min(self.queue.items.len());
        self.queue.items.insert(at, item);
        self.dirty = true;
        self.emit(Event::QueueChanged);
        self.start_track(at, 0, true);
    }

    /// 进入试听：首次调用时存档当前状态，然后换成临时队列从头播放。
    /// 已在试听态时只换临时队列（原存档保持不变）。
    fn enter_peek(&mut self, items: Vec<QueueItem>) {
        if self.peek.is_none() {
            // position_ms 必须在换队列前取：start_track 的 reset 会清零帧计数
            let position_ms = self.position_ms();
            let snapshot = PeekSnapshot {
                queue: std::mem::take(&mut self.queue),
                queue_name: self.queue_name.take(),
                position_ms,
                state: self.state,
                repeat: self.repeat,
                random: self.random,
            };
            self.peek = Some(snapshot);
        }
        self.kill_decoder();
        self.queue.items = items;
        self.queue.current = None;
        self.queue_name = None;
        // 临时队列固定顺序播放，循环/随机语义留给恢复后的原队列
        self.repeat = RepeatMode::Off;
        self.random = false;
        self.dirty = true;
        self.emit(Event::QueueChanged);
        self.emit(Event::ModeChanged {
            repeat: self.repeat,
            random: self.random,
        });
        self.start_track(0, 0, true);
    }

    /// 结束试听：恢复存档的队列、模式与进度。
    fn restore(&mut self) {
        let Some(snap) = self.peek.take() else {
            return;
        };
        let current = snap.queue.current;
        self.kill_decoder();
        self.queue = snap.queue;
        self.queue_name = snap.queue_name;
        self.repeat = snap.repeat;
        self.random = snap.random;
        self.fatal_streak = 0;
        self.dirty = true;
        self.emit(Event::QueueChanged);
        self.emit(Event::ModeChanged {
            repeat: self.repeat,
            random: self.random,
        });
        match current {
            Some(i) if i < self.queue.items.len() => {
                // 播放态继续播；暂停态解码预填但不播放，进度停在原处
                self.start_track(i, snap.position_ms, snap.state == PlayState::Playing);
            }
            _ => {
                self.state = PlayState::Stopped;
                self.seek_base_ms = 0;
                self.emit(Event::StateChanged {
                    state: PlayState::Stopped,
                });
                self.emit(Event::SongChanged { index: None });
            }
        }
    }

    /// 用户手动 next：单曲循环不拦（主动切歌），队尾总是回绕。
    fn on_next(&mut self) {
        match self.pick_next(true) {
            Some(i) => self.start_track(i, 0, true),
            None => self.stop(),
        }
    }

    /// 用户手动 prev：顺序后退（随机模式下也是），队首停在第 0 曲。
    fn on_prev(&mut self) {
        match self.queue.prev_index() {
            Some(i) => self.start_track(i, 0, true),
            None => self.stop(),
        }
    }

    /// 选下一曲下标。随机开启时随机挑（避开当前曲）；否则顺序，
    /// 越过队尾时按 `wrap` 决定回绕到 0 还是 None（停止）。
    fn pick_next(&self, wrap: bool) -> Option<usize> {
        let len = self.queue.items.len();
        if len == 0 {
            return None;
        }
        if self.random && len > 1 {
            let cur = self.queue.current;
            let mut rng = queue::simple_rng();
            loop {
                let i = (queue::next_rand(&mut rng) as usize) % len;
                if Some(i) != cur {
                    return Some(i);
                }
            }
        }
        match self.queue.next_index() {
            Some(i) => Some(i),
            None if wrap => Some(0),
            None => None,
        }
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
        self.dirty = true;

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
            repeat: self.repeat,
            random: self.random,
            queue_name: self.queue_name.clone(),
            peeking: self.peek.is_some(),
        }
    }
}
