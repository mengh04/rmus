//! rmus 协议消息定义，`rmusd` 与客户端（TUI/GUI/CLI）共享。
//!
//! 线格式为 NDJSON（每行一个 JSON 对象）：
//! - 请求：`{"id":1,"method":"play","params":{...}}`
//! - 成功响应：`{"id":1,"result":{...}}`
//! - 错误响应：`{"id":1,"error":"..."}`
//! - 事件（服务端主动推送）：`{"event":"state_changed","data":{...}}`

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlayState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    /// 列表循环：播完队尾回到队首
    All,
    /// 单曲循环
    One,
}

/// 队列中一曲的对外展示信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub index: usize,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusInfo {
    pub state: PlayState,
    /// 当前曲在队列中的下标（stop 后仍保留最后一曲）
    pub current: Option<usize>,
    /// 当前播放位置（毫秒），由声卡实际消费的帧数换算，暂停时不再前进
    pub position_ms: u64,
    pub duration_ms: u64,
    /// 0.0..=1.0
    pub volume: f32,
    pub queue_len: usize,
    pub track: Option<TrackInfo>,
    pub repeat: RepeatMode,
    pub random: bool,
    /// 当前队列的名字；None = 匿名临时队列
    pub queue_name: Option<String>,
}

/// 客户端请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// 查询状态快照
    Status,
    /// 添加文件/目录到队列尾部（目录递归扫描）。由 daemon 层完成扫描后转发给引擎。
    Add { paths: Vec<PathBuf> },
    /// 替换整个队列为这些文件/目录并从头播放（"只播放这些"）
    Open { paths: Vec<PathBuf> },
    /// 播放：Some(i) 从第 i 曲开始；None = 恢复暂停，或在停止状态下从头/继续
    Play { index: Option<usize> },
    Pause,
    /// Playing <-> Paused/Stopped 切换
    TogglePlay,
    /// 停止并清空输出（保留队列与 current 下标）
    Stop,
    Next,
    Prev,
    /// 绝对位置（毫秒）
    Seek { position_ms: u64 },
    /// 相对位置（毫秒，可为负）
    SeekRelative { delta_ms: i64 },
    SetVolume { value: f32 },
    AdjustVolume { delta: f32 },
    Clear,
    Remove { index: usize },
    /// 打乱队列（当前曲移到队首）
    Shuffle,
    QueueList,
    /// 列出命名队列
    PlaylistsList,
    /// 创建空命名队列
    PlaylistNew { name: String },
    /// 当前队列保存为命名队列（覆盖同名）
    PlaylistSave { name: String },
    /// 切换到命名队列并从头播放
    PlaylistPlay { name: String },
    /// 添加文件/目录到命名队列（不存在则创建；即当前队列时同步生效）
    PlaylistAdd { name: String, paths: Vec<PathBuf> },
    /// 删除命名队列（正在播放的队列不可删）
    PlaylistRemove { name: String },
    /// 循环模式：不设则循环切换 off -> all -> one -> off
    SetRepeat { mode: Option<RepeatMode> },
    /// 随机播放开关
    SetRandom { on: Option<bool> },
    /// 请求 daemon 退出（daemon 层处理，不进引擎）
    Quit,
}

/// 请求的成功结果。各方法对应的变体见 `Request` 注释。
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Reply {
    Status(Box<StatusInfo>),
    Queue { tracks: Vec<TrackInfo> },
    Added { added: usize },
    Playlists {
        names: Vec<String>,
        current: Option<String>,
    },
    Ok,
}

/// 服务端主动推送的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    StateChanged { state: PlayState },
    SongChanged { index: Option<usize> },
    /// 播放中约每 250ms 推送一次
    Position { position_ms: u64 },
    QueueChanged,
    VolumeChanged { value: f32 },
    ModeChanged { repeat: RepeatMode, random: bool },
}

/// 客户端 -> 服务端的单行请求。
#[derive(Debug, Serialize, Deserialize)]
pub struct RequestMsg {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// 服务端 -> 客户端的单行消息（响应或事件）。
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ServerMsg {
    Reply { id: u64, result: Reply },
    Error { id: u64, error: String },
    Event(Event),
}
