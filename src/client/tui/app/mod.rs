//! TUI 状态与动作执行：业务逻辑都在这里，`ui/` 只读状态渲染。
//!
//! 数据流：键盘 -> [`App::handle_key`] -> `actions::execute`（发请求/改本地状态）
//!       -> daemon 事件推送 -> [`App::pump_events`] 标记需要刷新 -> [`App::refresh_if_needed`]
//!
//! 惯例：改动队列/状态后不立刻拉取，而是把 `need_status` / `need_queue` 置位，
//! 由主循环统一刷新（合并同一轮里的多次改动）。
//!
//! 子模块按"用户在干什么"切分（都是 `impl App` 的片段）：
//! - [`state`]：状态类型（视图、模式、浮层、消息）
//! - [`actions`]：Normal 模式的按键动作
//! - [`selection`]：列表选中项、播放选中、首次定位
//! - [`search`] / [`palette`] / [`input`]：三种输入态
//! - [`command`]：`:` 命令执行

mod actions;
mod command;
mod input;
mod palette;
mod search;
mod selection;
mod state;

pub use state::{CmdPalette, InputPurpose, Message, Mode, SearchState, Tab};

use super::conn::{Daemon, Feed};
use super::keymap;
use crate::proto::{Event, RepeatMode, Request, StatusInfo, TrackInfo};
use crossterm::event::{KeyCode, KeyModifiers};
use state::parse_tracks;

pub struct App {
    pub daemon: Daemon,

    pub mode: Mode,
    input_buffer: String,
    pub tab: Tab,

    // 缓存的 daemon 状态（refresh_if_needed 按需更新）
    pub status: Option<StatusInfo>,
    pub queue: Vec<TrackInfo>,
    pub playlists: Vec<String>,
    pub current_playlist: Option<String>,

    // 每个列表的选中下标（列表空时为 0，渲染时注意 len==0）
    pub queue_selected: usize,
    pub playlists_selected: usize,
    pub search: Option<SearchState>,
    pub palette: Option<CmdPalette>,

    // 纯展示态（只由 ui 层读写）：列表滚动位置、帮助滚动
    pub scroll_queue: usize,
    pub scroll_playlists: usize,
    pub help_scroll: usize,
    // 首次载入后把选中项定位到正在播放的曲目/歌单，之后不再抢焦点
    seeded_queue: bool,
    seeded_playlists: bool,

    // 命令历史（内存；history_cursor = Some(i) 表示正在翻第 i 条）
    cmd_history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,

    pub help_open: bool,
    pub message: Option<Message>,
    pub should_quit: bool,
    fatal: Option<String>,

    // 按需刷新标记
    need_status: bool,
    need_queue: bool,
    need_playlists: bool,
}

impl App {
    pub fn new(daemon: Daemon) -> Self {
        Self {
            daemon,
            mode: Mode::Normal,
            input_buffer: String::new(),
            tab: Tab::Queue,
            status: None,
            queue: Vec::new(),
            playlists: Vec::new(),
            current_playlist: None,
            queue_selected: 0,
            playlists_selected: 0,
            search: None,
            palette: None,
            scroll_queue: 0,
            scroll_playlists: 0,
            help_scroll: 0,
            seeded_queue: false,
            seeded_playlists: false,
            cmd_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            help_open: false,
            message: None,
            should_quit: false,
            fatal: None,
            need_status: false,
            need_queue: false,
            need_playlists: false,
        }
    }

    /// 启动时全量拉一遍。
    pub fn startup(&mut self) {
        self.need_status = true;
        self.need_queue = true;
        self.need_playlists = true;
    }

    // ---- 主循环接口 -------------------------------------------------------

    /// 键盘事件入口：面板 / 输入模式自己处理，Normal 模式查键位表。
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        match self.mode {
            Mode::Input(_) => self.handle_input_key(key),
            Mode::Search => self.handle_search_key(key),
            Mode::Command => self.handle_palette_key(key),
            Mode::Normal => {
                // 帮助浮层打开时，j/k/g/G 只滚帮助，不动列表选中项
                if self.help_open
                    && matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
                    && self.scroll_help(key)
                {
                    return;
                }
                // 数字键切 tab（1 起始），其余交给键位表
                if let (KeyModifiers::NONE, KeyCode::Char(c)) = (key.modifiers, key.code)
                    && let Some(action) = keymap::resolve_digit(c, Tab::ALL.len())
                {
                    self.execute(action);
                    return;
                }
                if let Some(action) = keymap::resolve(key) {
                    self.execute(action);
                }
            }
        }
    }

    /// 取走 daemon 推送并转化成刷新标记。
    pub fn pump_events(&mut self) {
        for feed in self.daemon.try_drain() {
            match feed {
                Feed::Daemon(Event::Position { position_ms }) => {
                    // 高频事件：原地更新，不触发请求
                    if let Some(status) = &mut self.status {
                        status.position_ms = position_ms;
                    }
                }
                Feed::Daemon(Event::QueueChanged) => {
                    self.need_queue = true;
                    self.need_status = true;
                }
                Feed::Daemon(
                    Event::StateChanged { .. }
                    | Event::SongChanged { .. }
                    | Event::VolumeChanged { .. }
                    | Event::ModeChanged { .. },
                ) => self.need_status = true,
                Feed::Disconnected => {
                    self.fatal = Some("与 rmusd 失去连接".into());
                }
            }
        }
    }

    /// 主循环每轮调用：按标记拉取 daemon 状态。
    pub fn refresh_if_needed(&mut self) {
        if self.need_status {
            self.need_status = false;
            if let Ok(value) = self.daemon.request(Request::Status) {
                self.status = serde_json::from_value(value).ok();
            }
        }
        if self.need_queue {
            self.need_queue = false;
            if let Ok(value) = self.daemon.request(Request::QueueList) {
                self.queue = parse_tracks(&value);
                self.clamp_selections();
                self.seed_queue_selection();
            }
        }
        if self.need_playlists {
            self.need_playlists = false;
            if let Ok(value) = self.daemon.request(Request::PlaylistsList) {
                self.playlists =
                    serde_json::from_value(value.get("names").cloned().unwrap_or_default())
                        .unwrap_or_default();
                self.current_playlist =
                    serde_json::from_value(value.get("current").cloned().unwrap_or_default())
                        .unwrap_or(None);
                self.clamp_selections();
                self.seed_playlist_selection();
            }
        }
    }

    pub fn take_fatal(&mut self) -> Option<String> {
        self.fatal.take()
    }

    /// 渲染前调一下，让过期消息自动消失。
    pub fn tick(&mut self) {
        if let Some(msg) = &self.message
            && !msg.alive()
        {
            self.message = None;
        }
    }

    // ---- 杂项 --------------------------------------------------------------

    /// 设置底部临时消息。
    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some(Message {
            text: text.into(),
            error,
            at: std::time::Instant::now(),
        });
    }

    pub fn repeat_label(repeat: RepeatMode) -> &'static str {
        match repeat {
            RepeatMode::Off => "关",
            RepeatMode::All => "列表",
            RepeatMode::One => "单曲",
        }
    }
}
