//! `App` 持有的界面状态类型：视图、模式、浮层、消息。

use crate::proto::TrackInfo;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 主界面视图。拓展：加变体 + 改 `Tab::ALL` / `title` 即可。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Queue,
    Library,
    Playlists,
    /// 插件提供的在线搜索 / 下载（rmus-dl）
    Online,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Queue, Tab::Library, Tab::Playlists, Tab::Online];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Queue => "队列",
            Tab::Library => "音乐库",
            Tab::Playlists => "播放列表",
            Tab::Online => "在线",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[(i + n - 1) % n]
    }
}

/// 输入模式的用途（vim 命令行：`:w` 存播放列表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPurpose {
    SavePlaylist,
    /// 重命名播放列表（携带旧名，确认时发 PlaylistRename）
    RenamePlaylist(String),
    /// 在线搜索关键词（确认时走插件的 search）
    OnlineSearch,
}

impl InputPurpose {
    /// 底部命令行显示的前缀。
    pub fn prefix(&self) -> &'static str {
        match self {
            InputPurpose::SavePlaylist => ":w ",
            InputPurpose::RenamePlaylist(_) => ":rename ",
            InputPurpose::OnlineSearch => "/ ",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Telescope 式搜索面板：输入与实时结果一体（`search::handle_key` 处理）
    Search,
    /// Telescope 式命令面板（`palette::handle_key` 处理）
    Command,
    Input(InputPurpose),
}

/// 搜索面板内的子模式：字母键要么进关键词，要么当动作（vim normal）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// 正在输入关键词（默认）
    #[default]
    Input,
    /// vim normal：j/k 移动，o/a/gf… 直接触发动作，`i` 回输入
    Normal,
}

/// 搜索面板状态；`None` = 未打开。与 [`Mode::Search`] 同生共死。
pub struct SearchState {
    pub query: String,
    pub results: Vec<TrackInfo>,
    pub selected: usize,
    /// 面板内子模式；Esc 在输入态进 normal，normal 态再 Esc 才关面板
    pub mode: SearchMode,
    /// normal 态按过 `g`：等 gg（跳顶）/ gf（音乐库定位）
    pub pending_g: bool,
}

/// 在线 tab 状态：最近一次搜索的关键词与结果。
#[derive(Debug, Default)]
pub struct OnlineState {
    pub query: String,
    pub songs: Vec<OnlineSong>,
    pub selected: usize,
}

/// 一条在线搜索结果；字段形状即插件协议的 `song` 对象（下载时原样回传）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSong {
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    /// 来源标识（如 kuwo / kugou），插件回源用
    pub source: String,
    /// 源内唯一标识
    pub song_id: String,
    #[serde(default)]
    pub duration_ms: u64,
}

/// 已发现插件的信息（`plugin list`）。methods 形状即协议的
/// `[{name,kind}]`，直接复用契约类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub methods: Vec<crate::plugin::PluginMethod>,
}

impl PluginInfo {
    /// 在线 tab 约定：插件要同时提供 search 与 download。
    pub fn supports_online(&self) -> bool {
        let names: Vec<&str> = self.methods.iter().map(|m| m.name.as_str()).collect();
        names.contains(&"search") && names.contains(&"download")
    }
}

/// 一次下载在界面上的踪迹（按 daemon 分配的任务号 call 索引）。
#[derive(Debug)]
pub struct DownloadState {
    pub song: OnlineSong,
    pub received: u64,
    pub total: Option<u64>,
    pub status: DownloadStatus,
    /// 完成后自动试听（Peek）
    pub peek: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStatus {
    Running,
    Done,
    Failed(String),
}

/// 后台线程回来的异步回复（同步 Client 不能在主线程久等，搜索走这边）。
pub enum AsyncReply {
    OnlineSearch {
        generation: u64,
        query: String,
        result: anyhow::Result<serde_json::Value>,
    },
}

/// `:` 命令面板状态；`None` = 未打开。与 [`Mode::Command`] 同生共死。
/// 命令列表由 `cmd::filter(&input)` 现算，不存这里。
pub struct CmdPalette {
    pub input: String,
    pub selected: usize,
    /// 上次回车的解析错误（面板内展示；输入变化即清除）
    pub error: Option<String>,
}

/// 底部状态行的临时消息。
pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

impl Message {
    const TTL: Duration = Duration::from_secs(5);

    pub fn alive(&self) -> bool {
        self.at.elapsed() < Self::TTL
    }
}

/// 协议回复里取 `tracks` 数组。
pub(super) fn parse_tracks(value: &serde_json::Value) -> Vec<TrackInfo> {
    serde_json::from_value(value.get("tracks").cloned().unwrap_or_default()).unwrap_or_default()
}
