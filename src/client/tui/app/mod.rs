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
mod online;
mod palette;
mod search;
mod selection;
mod state;

pub use state::{
    AsyncReply, CmdPalette, DownloadState, DownloadStatus, InputPurpose, Message, Mode,
    OnlineSong, OnlineState, PluginInfo, SearchMode, SearchState, Tab,
};

use super::conn::{Daemon, Feed};
use super::keymap;
use crate::proto::{Event, RepeatMode, Request, StatusInfo, TrackInfo};
use crossterm::event::{KeyCode, KeyModifiers};
use state::parse_tracks;
use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};

pub struct App {
    pub daemon: Daemon,

    pub mode: Mode,
    input_buffer: String,
    pub tab: Tab,

    // 缓存的 daemon 状态（refresh_if_needed 按需更新）
    pub status: Option<StatusInfo>,
    pub queue: Vec<TrackInfo>,
    pub library: Vec<TrackInfo>,
    pub playlists: Vec<String>,
    pub current_playlist: Option<String>,

    // 在线（插件）状态
    pub plugins: Vec<PluginInfo>,
    /// 提供在线搜索/下载的插件（第一个带 search + download 方法的）
    pub online_plugin: Option<PluginInfo>,
    pub online: Option<OnlineState>,
    /// 在途/已完成的下载，按 daemon 分配的任务号索引
    pub downloads: HashMap<u64, DownloadState>,
    /// 后台请求（在线搜索）的回执；gen 防止过期结果覆盖新查询
    replies_tx: Sender<AsyncReply>,
    replies: std::sync::mpsc::Receiver<AsyncReply>,
    search_generation: u64,

    // 每个列表的选中下标（列表空时为 0，渲染时注意 len==0）
    pub queue_selected: usize,
    pub library_selected: usize,
    pub playlists_selected: usize,
    pub search: Option<SearchState>,
    pub palette: Option<CmdPalette>,

    // 纯展示态（只由 ui 层读写）：列表滚动位置、帮助滚动
    pub scroll_queue: usize,
    pub scroll_library: usize,
    pub scroll_playlists: usize,
    pub scroll_online: usize,
    pub help_scroll: usize,
    // 首次载入后把选中项定位到正在播放的曲目/歌单，之后不再抢焦点
    seeded_queue: bool,
    seeded_playlists: bool,

    // 命令历史（内存；history_cursor = Some(i) 表示正在翻第 i 条）
    cmd_history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,

    /// Normal 模式的 operator-pending 状态：按过 `g` / `d` / `y` 之后等
    /// 第二个键。`g` 前缀：gg / gd / gt / gT；`d` 前缀：dd / dj / dk /
    /// dG / dgg；`y` 前缀：yy。`d` 只在队列 / 播放列表视图开前缀
    /// （在线视图 d 是下载）。
    pending: Pending,
    /// visual 模式：v 进入，选段 = anchor..=当前光标，d/y/x 收割
    pub visual: Option<VisualSel>,
    /// 未命名寄存器：yy 复制、dd / dj 等剪切进来的曲子（路径 + 标题），
    /// p / P 粘贴进队列。vim 的删除即剪切。
    register: Option<Vec<(std::path::PathBuf, String)>>,

    pub help_open: bool,
    pub message: Option<Message>,
    pub should_quit: bool,
    fatal: Option<String>,

    // 按需刷新标记
    need_status: bool,
    need_queue: bool,
    need_library: bool,
    need_playlists: bool,
    need_plugins: bool,
}

/// Normal 模式等待第二个键的状态（vim operator-pending）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pending {
    #[default]
    None,
    /// 按过 `g`：gg 跳顶 / gd 定位 / gt gT 切视图
    G,
    /// 按过 `d`：dd 删本行 / dj dk dG dgg 范围删
    Delete,
    /// `dg`：等第二个 g（dgg = 从顶部删到选中行）
    DeleteG,
    /// 按过 `y`：yy 复制选中曲到寄存器
    Yank,
}

/// visual 选段状态；anchor 是按下 v 时的光标位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualSel {
    pub anchor: usize,
}
impl App {
    pub fn new(daemon: Daemon) -> Self {
        let (replies_tx, replies) = channel();
        Self {
            daemon,
            mode: Mode::Normal,
            input_buffer: String::new(),
            tab: Tab::Queue,
            status: None,
            queue: Vec::new(),
            library: Vec::new(),
            playlists: Vec::new(),
            current_playlist: None,
            plugins: Vec::new(),
            online_plugin: None,
            online: None,
            downloads: HashMap::new(),
            replies_tx,
            replies,
            search_generation: 0,
            queue_selected: 0,
            library_selected: 0,
            playlists_selected: 0,
            search: None,
            palette: None,
            scroll_queue: 0,
            scroll_library: 0,
            scroll_playlists: 0,
            scroll_online: 0,
            help_scroll: 0,
            seeded_queue: false,
            seeded_playlists: false,
            cmd_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            pending: Pending::None,
            visual: None,
            register: None,
            help_open: false,
            message: None,
            should_quit: false,
            fatal: None,
            need_status: false,
            need_queue: false,
            need_library: false,
            need_playlists: false,
            need_plugins: false,
        }
    }

    /// 启动时全量拉一遍。
    pub fn startup(&mut self) {
        self.need_status = true;
        self.need_queue = true;
        self.need_library = true;
        self.need_playlists = true;
        self.need_plugins = true;
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
                // visual 选段：操作键立即生效，移动键照常（选段跟着光标走）
                if self.visual.is_some() {
                    use KeyModifiers as KM;
                    let used = match (key.modifiers, key.code) {
                        (KM::NONE, KeyCode::Char('d' | 'x')) => {
                            self.visual_cut();
                            true
                        }
                        (KM::NONE, KeyCode::Char('y')) => {
                            self.visual_yank();
                            true
                        }
                        (KM::NONE, KeyCode::Char('o')) => {
                            self.visual_swap_ends();
                            true
                        }
                        (KM::NONE, KeyCode::Char('v')) | (KM::SHIFT, KeyCode::Char('V')) => {
                            self.exit_visual();
                            true
                        }
                        (_, KeyCode::Esc) => {
                            self.exit_visual();
                            true
                        }
                        _ => false,
                    };
                    if used {
                        return;
                    }
                    // 其余键（j/k/gg/G/C-d/Enter…）走下面的普通路径
                }
                // operator-pending：gg gd gt gT / dd dj dk dG dgg / yy 的第二个键
                if self.pending != Pending::None {
                    self.apply_pending(key);
                    return;
                }
                // `g` / `d` / `y` 是前缀键的起点。
                // `d` 只在队列 / 播放列表视图开前缀：在线视图的 d 是下载。
                if matches!((key.modifiers, key.code), (KeyModifiers::NONE, KeyCode::Char('g'))) {
                    self.pending = Pending::G;
                } else if matches!((key.modifiers, key.code), (KeyModifiers::NONE, KeyCode::Char('d')))
                    && matches!(self.tab, Tab::Queue | Tab::Playlists)
                {
                    self.pending = Pending::Delete;
                } else if matches!((key.modifiers, key.code), (KeyModifiers::NONE, KeyCode::Char('y'))) {
                    self.pending = Pending::Yank;
                } else if matches!(
                    (key.modifiers, key.code),
                    (KeyModifiers::NONE, KeyCode::Char('v')) | (KeyModifiers::SHIFT, KeyCode::Char('V'))
                ) && matches!(self.tab, Tab::Queue | Tab::Library | Tab::Playlists)
                {
                    // v / V：进入 visual，anchor 定在当前光标
                    let anchor = match self.tab {
                        Tab::Queue => self.queue_selected,
                        Tab::Library => self.library_selected,
                        _ => self.playlists_selected,
                    };
                    self.visual = Some(VisualSel { anchor });
                    self.notify("-- VISUAL -- d 剪切 · y 复制 · o 对调 · Esc 退出", false);
                } else if let Some(action) = keymap::resolve(key) {
                    self.execute(action);
                }
            }
        }
    }

    /// 消费 operator-pending 的第二个键。无效组合照 vim 一样吞掉
    /// （取消前缀，不执行别的动作）。
    fn apply_pending(&mut self, key: crossterm::event::KeyEvent) {
        use keymap::Action;
        use KeyModifiers as KM;
        let pending = std::mem::take(&mut self.pending);
        match (pending, key.modifiers, key.code) {
            // gg 跳顶 / gd 去音乐库定位
            (Pending::G, KM::NONE, KeyCode::Char('g')) => self.execute(Action::Top),
            (Pending::G, KM::NONE, KeyCode::Char('d')) => self.execute(Action::LocateInLibrary),
            // gt / gT 切视图
            (Pending::G, KM::NONE, KeyCode::Char('t')) => self.execute(Action::NextTab),
            (Pending::G, KM::SHIFT, KeyCode::Char('T')) => self.execute(Action::PrevTab),
            // dd 删选中行（剪切进寄存器）
            (Pending::Delete, KM::NONE, KeyCode::Char('d')) => self.cut_selected_line(),
            // dj / dk / dG / dgg：队列范围剪切
            (Pending::Delete, KM::NONE, KeyCode::Char('j')) => {
                let sel = self.queue_selected;
                self.delete_queue_range(sel, sel + 2);
            }
            (Pending::Delete, KM::NONE, KeyCode::Char('k')) => {
                let sel = self.queue_selected;
                self.delete_queue_range(sel.saturating_sub(1), sel + 1);
            }
            (Pending::Delete, KM::SHIFT, KeyCode::Char('G')) => {
                let sel = self.queue_selected;
                self.delete_queue_range(sel, usize::MAX);
            }
            (Pending::Delete, KM::NONE, KeyCode::Char('g')) => self.pending = Pending::DeleteG,
            (Pending::DeleteG, KM::NONE, KeyCode::Char('g')) => {
                let sel = self.queue_selected;
                self.delete_queue_range(0, sel + 1);
            }
            // yy 复制选中曲
            (Pending::Yank, KM::NONE, KeyCode::Char('y')) => self.yank_selected(),
            _ => {}
        }
    }

    /// 取走 daemon 推送并转化成刷新标记；顺带收取后台请求的回执。
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
                Feed::Daemon(Event::Plugin { plugin, call, data }) => {
                    self.apply_plugin_event(&plugin, call, &data);
                }
                Feed::Disconnected => {
                    self.fatal = Some("与 rmusd 失去连接".into());
                }
            }
        }
        // 后台线程的搜索回执（过期的直接丢）
        while let Ok(reply) = self.replies.try_recv() {
            match reply {
                AsyncReply::OnlineSearch { generation, query, result } if generation == self.search_generation => {
                    self.apply_online_search(&query, result);
                }
                AsyncReply::OnlineSearch { .. } => {}
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
        if self.need_library {
            self.need_library = false;
            if let Ok(value) = self.daemon.request(Request::LibList) {
                self.library = parse_tracks(&value);
                self.clamp_selections();
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
        if self.need_plugins {
            self.need_plugins = false;
            if let Ok(value) = self.daemon.request(Request::PluginList) {
                self.plugins =
                    serde_json::from_value(value.get("plugins").cloned().unwrap_or_default())
                        .unwrap_or_default();
                self.online_plugin = self.plugins.iter().find(|p| p.supports_online()).cloned();
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
