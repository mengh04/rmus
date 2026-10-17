//! TUI 状态与动作执行：所有业务逻辑集中在这里，`ui/` 只读状态渲染。
//!
//! 数据流：键盘 -> [`App::handle_key`] -> [`App::execute`]（发请求/改本地状态）
//!       -> daemon 事件推送 -> [`App::pump_events`] 标记需要刷新 -> [`App::refresh_if_needed`]
//!
//! 惯例：改动队列/状态后不立刻拉取，而是把 `need_status` / `need_queue` 置位，
//! 由主循环统一刷新（合并同一轮里的多次改动）。

use super::conn::{Daemon, Feed};
use super::keymap::{self, Action};
use crate::proto::{Event, RepeatMode, Request, StatusInfo, TrackInfo};
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

/// 主界面 tab。拓展：加变体 + 改 `Tab::ALL` / `title` 即可。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Queue,
    Playlists,
}

impl Tab {
    pub const ALL: [Tab; 2] = [Tab::Queue, Tab::Playlists];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Queue => "队列",
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

/// 输入模式的用途（vim 命令行：`/` 搜索、`:w` 存播放列表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPurpose {
    Search,
    SavePlaylist,
}

impl InputPurpose {
    /// 底部命令行显示的前缀。
    pub fn prefix(self) -> &'static str {
        match self {
            InputPurpose::Search => "/",
            InputPurpose::SavePlaylist => ":w ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Input(InputPurpose),
}

/// 搜索结果浮层；`None` = 未打开。
pub struct SearchState {
    pub query: String,
    pub results: Vec<TrackInfo>,
    pub selected: usize,
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

    /// 键盘事件入口：输入模式自己处理，Normal 模式查键位表。
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        match self.mode {
            Mode::Input(_) => self.handle_input_key(key),
            Mode::Normal => {
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

    // ---- Normal 模式动作 ---------------------------------------------------

    fn execute(&mut self, action: Action) {
        use Action::*;
        match action {
            Quit => self.should_quit = true,
            Help => self.help_open = !self.help_open,
            Close => {
                // 只关最上层的一层浮层
                if self.help_open {
                    self.help_open = false;
                } else if self.search.is_some() {
                    self.search = None;
                }
            }
            NextTab => self.tab = self.tab.next(),
            PrevTab => self.tab = self.tab.prev(),
            GotoTab(n) => self.tab = Tab::ALL[n],
            Up => self.move_selection(-1),
            Down => self.move_selection(1),
            Top => self.set_selection(0),
            Bottom => self.set_selection(usize::MAX),
            HalfUp => self.move_selection(-7),
            HalfDown => self.move_selection(7),
            PlaySelected => self.play_selected(),
            PlayToggle => self.simple(Request::TogglePlay),
            Next => self.simple(Request::Next),
            Prev => self.simple(Request::Prev),
            Stop => self.simple(Request::Stop),
            RemoveSelected => self.remove_selected(),
            RemoveCurrent => {
                self.simple(Request::Remove { index: None });
                self.need_queue = true;
            }
            ClearQueue => {
                self.simple(Request::Clear);
                self.need_queue = true;
            }
            Shuffle => {
                self.simple(Request::Shuffle);
                self.need_queue = true;
            }
            Back => self.simple(Request::Back),
            CycleRepeat => self.simple(Request::SetRepeat { mode: None }),
            ToggleRandom => self.simple(Request::SetRandom { on: None }),
            VolumeUp => self.simple(Request::AdjustVolume { delta: 0.05 }),
            VolumeDown => self.simple(Request::AdjustVolume { delta: -0.05 }),
            Search => {
                let query = self
                    .search
                    .as_ref()
                    .map(|s| s.query.clone())
                    .unwrap_or_default();
                self.enter_input(InputPurpose::Search, query);
            }
            SavePlaylist => {
                let name = self.current_playlist.clone().unwrap_or_default();
                self.enter_input(InputPurpose::SavePlaylist, name);
            }
            Rescan => match self.daemon.request(Request::LibRescan) {
                Ok(value) => {
                    let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
                    self.notify(format!("库索引已重建：{added} 首"), false);
                }
                Err(e) => self.notify(format!("{e}"), true),
            },
        }
    }

    /// 只发请求、失败弹提示的小工具（不关心返回值）。
    fn simple(&mut self, request: Request) {
        if let Err(e) = self.daemon.request(request) {
            self.notify(format!("{e}"), true);
        }
        self.need_status = true;
    }

    // ---- 列表选择与播放 ----------------------------------------------------

    /// 当前活动列表（搜索结果浮层最优先）的长度。
    fn active_len(&self) -> usize {
        if let Some(search) = &self.search {
            search.results.len()
        } else {
            match self.tab {
                Tab::Queue => self.queue.len(),
                Tab::Playlists => self.playlists.len(),
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let base = if let Some(search) = &self.search {
            search.selected
        } else {
            match self.tab {
                Tab::Queue => self.queue_selected,
                Tab::Playlists => self.playlists_selected,
            }
        };
        let next = (base as i32 + delta).clamp(0, len as i32 - 1) as usize;
        self.set_selection(next);
    }

    fn set_selection(&mut self, index: usize) {
        let len = self.active_len();
        let index = index.min(len.saturating_sub(1));
        if let Some(search) = &mut self.search {
            search.selected = index;
        } else {
            match self.tab {
                Tab::Queue => self.queue_selected = index,
                Tab::Playlists => self.playlists_selected = index,
            }
        }
    }

    /// Enter：播放当前选中的东西。
    fn play_selected(&mut self) {
        if let Some(search) = &self.search {
            if let Some(track) = search.results.get(search.selected) {
                let path = track.path.clone();
                self.play_library_track(&path);
            }
            return;
        }
        match self.tab {
            Tab::Queue => {
                let index = self.queue_selected;
                if index < self.queue.len() {
                    self.simple(Request::Play { index: Some(index) });
                }
            }
            Tab::Playlists => {
                let index = self.playlists_selected;
                if let Some(name) = self.playlists.get(index).cloned() {
                    self.simple(Request::PlaylistPlay { name });
                    self.need_queue = true;
                }
            }
        }
    }

    /// 播放库里的一首：加到队尾并跳过去播。
    /// （协议没有"按路径直接插播"，用 Add + Play(index) 组合实现）
    fn play_library_track(&mut self, path: &std::path::Path) {
        let Ok(value) = self.daemon.request(Request::Status) else {
            return;
        };
        let Ok(status) = serde_json::from_value::<StatusInfo>(value) else {
            return;
        };
        let index = status.queue_len; // Add 后新曲目落在队尾
        if let Err(e) = self.daemon.request(Request::Add {
            paths: vec![path.to_path_buf()],
        }) {
            self.notify(format!("{e}"), true);
            return;
        }
        self.simple(Request::Play { index: Some(index) });
        self.need_queue = true;
        self.notify(
            format!(
                "插播 {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            false,
        );
    }

    fn remove_selected(&mut self) {
        if self.tab != Tab::Queue {
            return;
        }
        let index = self.queue_selected;
        if index >= self.queue.len() {
            return;
        }
        self.simple(Request::Remove { index: Some(index) });
        self.need_queue = true;
    }

    fn clamp_selections(&mut self) {
        let clamp = |sel: &mut usize, len: usize| {
            if len == 0 {
                *sel = 0;
            } else if *sel >= len {
                *sel = len - 1;
            }
        };
        clamp(&mut self.queue_selected, self.queue.len());
        clamp(&mut self.playlists_selected, self.playlists.len());
        if let Some(search) = &mut self.search {
            clamp(&mut search.selected, search.results.len());
        }
    }

    // ---- 输入模式 ----------------------------------------------------------

    fn enter_input(&mut self, purpose: InputPurpose, initial: String) {
        self.input_buffer = initial;
        self.mode = Mode::Input(purpose);
    }

    pub fn input_buffer(&self) -> &str {
        &self.input_buffer
    }

    fn handle_input_key(&mut self, key: crossterm::event::KeyEvent) {
        enum InputOutcome {
            Keep,
            Confirm(String),
            Cancel,
        }
        let Mode::Input(purpose) = self.mode else {
            return;
        };
        let outcome = {
            let buffer = &mut self.input_buffer;
            match (key.modifiers, key.code) {
                (KeyModifiers::CONTROL, KeyCode::Char('c')) => InputOutcome::Cancel,
                (_, KeyCode::Esc) => InputOutcome::Cancel,
                (_, KeyCode::Enter) => InputOutcome::Confirm(buffer.clone()),
                (_, KeyCode::Backspace) => {
                    buffer.pop();
                    InputOutcome::Keep
                }
                (m, KeyCode::Char(c)) if !m.contains(KeyModifiers::CONTROL) => {
                    buffer.push(c);
                    InputOutcome::Keep
                }
                _ => InputOutcome::Keep,
            }
        };
        match outcome {
            InputOutcome::Keep => {}
            InputOutcome::Cancel => self.mode = Mode::Normal,
            InputOutcome::Confirm(text) => {
                self.mode = Mode::Normal;
                self.confirm_input(purpose, &text);
            }
        }
    }

    fn confirm_input(&mut self, purpose: InputPurpose, text: &str) {
        match purpose {
            InputPurpose::Search => {
                let query = text.trim().to_string();
                if query.is_empty() {
                    self.search = None;
                    return;
                }
                match self.daemon.request(Request::Find {
                    query: query.clone(),
                }) {
                    Ok(value) => {
                        let results = parse_tracks(&value);
                        if results.is_empty() {
                            self.notify(format!("没有匹配 “{query}” 的曲子"), true);
                        }
                        self.search = Some(SearchState {
                            query,
                            results,
                            selected: 0,
                        });
                    }
                    Err(e) => self.notify(format!("{e}"), true),
                }
            }
            InputPurpose::SavePlaylist => {
                let name = text.trim().to_string();
                if name.is_empty() {
                    self.notify("播放列表名不能为空", true);
                    return;
                }
                self.simple(Request::PlaylistSave { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("当前队列已保存为 “{name}”"), false);
            }
        }
    }

    // ---- 杂项 --------------------------------------------------------------

    /// 设置底部临时消息。
    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some(Message {
            text: text.into(),
            error,
            at: Instant::now(),
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

fn parse_tracks(value: &serde_json::Value) -> Vec<TrackInfo> {
    serde_json::from_value(value.get("tracks").cloned().unwrap_or_default()).unwrap_or_default()
}
