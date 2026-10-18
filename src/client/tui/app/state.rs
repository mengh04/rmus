//! `App` 持有的界面状态类型：视图、模式、浮层、消息。

use crate::proto::TrackInfo;
use std::time::{Duration, Instant};

/// 主界面视图。拓展：加变体 + 改 `Tab::ALL` / `title` 即可。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Queue,
    Library,
    Playlists,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Queue, Tab::Library, Tab::Playlists];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Queue => "队列",
            Tab::Library => "音乐库",
            Tab::Playlists => "播放列表",
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
}

impl InputPurpose {
    /// 底部命令行显示的前缀。
    pub fn prefix(&self) -> &'static str {
        match self {
            InputPurpose::SavePlaylist => ":w ",
            InputPurpose::RenamePlaylist(_) => ":rename ",
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

/// 搜索面板状态；`None` = 未打开。与 [`Mode::Search`] 同生共死。
pub struct SearchState {
    pub query: String,
    pub results: Vec<TrackInfo>,
    pub selected: usize,
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
