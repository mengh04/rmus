//! 播放引擎：单线程编排（std::thread），持有队列/设备/解码句柄。
//!
//! 与 daemon 层的边界：
//! - 入：`ToEngine`（std mpsc，socket 层和解码线程都往里发）
//! - 出：`tokio::broadcast` 事件（send 是同步方法，可在本线程直接调用）
//! - 响应：每请求携带 `tokio::sync::oneshot`，处理完立刻回
//!
//! 子模块（都是 `impl Engine` 的片段）：
//! - [`run`]：事件循环（命令、解码线程回报、位置推送）
//! - [`handlers`]：请求分发
//! - [`playback`]：播放/暂停/切歌/跳转/音量
//! - [`track`]：起一条解码线程（含插播）
//! - [`peek`]：试听态存档与恢复
//!
//! 参考 hummingbird `src/playback/thread.rs`，第一版精简掉：
//! gapless 双缓冲 / EQ / ReplayGain / AudioBlock 池 / 解码卡死恢复。

mod decoder;
mod handlers;
mod output;
mod peek;
mod playback;
pub mod queue;
mod resample;
mod run;
mod store;
mod track;
mod undo;

use crate::proto::{Event, PlayState, RepeatMode, Reply, Request, StatusInfo};
use anyhow::Result;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot};

use output::Output;
use peek::PeekSnapshot;
use queue::{Queue, QueueItem};
use undo::Snapshot;

/// position 事件推送节流间隔
pub(super) const POSITION_EVENT_INTERVAL: Duration = Duration::from_millis(250);

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
    /// daemon 层完成扫描后追加到播放列表
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

pub(super) struct DecoderHandle {
    pub(super) generation: u64,
    pub(super) handle: JoinHandle<()>,
}

pub struct Engine {
    queue: Queue,
    /// 队列来源标签（来源播放列表的名字）；None = 匿名临时队列（open/add 出来的）。
    /// 仅用于顶栏显示和 `w` 的默认保存目标——队列怎么改都不回写播放列表
    queue_name: Option<String>,
    /// 播放列表：持久命名集合。播放 = 复制进队列，保存 = PlaylistSave 显式写回
    playlists: BTreeMap<String, Vec<QueueItem>>,
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
    /// 队列改动的撤销 / 重做栈（`undo::push_history` 在每次改动前压入）
    undo: VecDeque<Snapshot>,
    redo: VecDeque<Snapshot>,
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
            playlists: persisted.playlists,
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
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            last_position_event: Instant::now() - POSITION_EVENT_INTERVAL, // 首个事件立即发
        })
    }

    pub(super) fn persist(&self) {
        store::save(&store::PersistedState {
            volume: Some(self.output.volume()),
            repeat: Some(self.repeat),
            random: Some(self.random),
            queue_name: self.queue_name.clone(),
            current_index: self.queue.current,
            current_queue: self.queue.items.clone(),
            playlists: self.playlists.clone(),
        });
    }

    pub(super) fn emit(&self, event: Event) {
        // 没有订阅者时 send 返回 Err，忽略即可
        let _ = self.events.send(event);
    }

    pub(super) fn position_ms(&self) -> u64 {
        if self.state == PlayState::Stopped {
            return 0;
        }
        let played = self.output.played_frames().load(Ordering::Relaxed);
        self.seek_base_ms + played * 1000 / u64::from(self.output.sample_rate().max(1))
    }

    pub(super) fn status(&self) -> StatusInfo {
        StatusInfo {
            state: self.state,
            current: self.queue.current,
            position_ms: self.position_ms(),
            duration_ms: self
                .queue
                .current_item()
                .map(|i| i.duration_ms)
                .unwrap_or(0),
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
