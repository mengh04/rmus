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
    /// 试听态：队列是临时的，结束（播完/back）后恢复原队列与进度
    pub peeking: bool,
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
    /// 试听：保存当前播放状态（队列/曲目/进度/模式），临时换成这些文件
    /// 播放；临时队列播完自动恢复，或用 Back 提前恢复。由 daemon 层完成扫描。
    Peek { paths: Vec<PathBuf> },
    /// 结束试听，恢复进入试听前的状态
    Back,
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
    /// 删除队列中的第 index 曲；None = 删除正在播放的曲子（自动续播下一首）
    Remove { index: Option<usize> },
    /// 打乱队列（当前曲移到队首）
    Shuffle,
    /// 调整队列顺序：把第 from 曲移到第 to 的位置（0 起，其余曲顺延）
    Move { from: usize, to: usize },
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
    /// 重命名命名队列（正在播放时跟随改名；to 已存在则报错）
    PlaylistRename { from: String, to: String },
    /// 循环模式：不设则循环切换 off -> all -> one -> off
    SetRepeat { mode: Option<RepeatMode> },
    /// 随机播放开关
    SetRandom { on: Option<bool> },
    /// 库模糊搜索：按相关度返回候选（daemon 层查索引）
    Find { query: String },
    /// 库模糊播放：最佳匹配插到当前曲后立即播（空队列则直接播）。
    /// daemon 层匹配后转为引擎的 PlayNow。
    PlayMatch { query: String },
    /// 列出整个音乐库（daemon 层查索引，不进引擎）
    LibList,
    /// 全量重建库索引（~/Music；daemon 层扫描）
    LibRescan,
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
    /// 库搜索候选（按相关度降序）
    Matches { tracks: Vec<TrackInfo> },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// daemon 与客户端可能不同版本：事件序列化必须能被对面解析。
    #[test]
    fn 事件_序列化可解析() {
        let events = [
            Event::QueueChanged,
            Event::Position { position_ms: 1234 },
            Event::SongChanged { index: Some(3) },
            Event::StateChanged { state: PlayState::Playing },
            Event::VolumeChanged { value: 0.5 },
            Event::ModeChanged { repeat: RepeatMode::All, random: true },
        ];
        for ev in events {
            let text = serde_json::to_string(&ev).expect("序列化");
            let back: Event = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("解析失败: {e} <- {text}"));
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
    }

    /// 新请求的序列化抽查（snake_case 方法名是对外契约）。
    #[test]
    fn 请求_方法名() {
        fn method(req: &Request) -> String {
            let msg = serde_json::to_value(RequestMsg { id: 1, request: req.clone() }).unwrap();
            msg.get("method").and_then(|m| m.as_str()).unwrap().into()
        }
        assert_eq!(method(&Request::Move { from: 0, to: 1 }), "move");
        assert_eq!(method(&Request::PlaylistRename { from: "a".into(), to: "b".into() }), "playlist_rename");
        assert_eq!(method(&Request::LibList), "lib_list");
    }
}
