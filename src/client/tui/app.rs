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

/// 输入模式的用途（vim 命令行：`:w` 存播放列表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPurpose {
    SavePlaylist,
}

impl InputPurpose {
    /// 底部命令行显示的前缀。
    pub fn prefix(self) -> &'static str {
        match self {
            InputPurpose::SavePlaylist => ":w ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Telescope 式搜索面板：输入与实时结果一体（`handle_search_key` 处理）
    Search,
    /// Telescope 式命令面板（`handle_palette_key` 处理）
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
                // Normal 模式下只剩帮助浮层（搜索面板由 Mode::Search 自己管）
                if self.help_open {
                    self.help_open = false;
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
            Search => self.open_search(),
            Command => self.open_palette(),
            SavePlaylist => {
                let name = self.current_playlist.clone().unwrap_or_default();
                self.enter_input(InputPurpose::SavePlaylist, name);
            }
            Rescan => self.rescan_library(),
        }
    }

    /// 只发请求、失败弹提示的小工具（不关心返回值）。
    fn simple(&mut self, request: Request) {
        if let Err(e) = self.daemon.request(request) {
            self.notify(format!("{e}"), true);
        }
        self.need_status = true;
    }

    /// 全量重建库索引，完成后提示曲目数。
    fn rescan_library(&mut self) {
        match self.daemon.request(Request::LibRescan) {
            Ok(value) => {
                let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
                self.notify(format!("库索引已重建：{added} 首"), false);
            }
            Err(e) => self.notify(format!("{e}"), true),
        }
    }

    // ---- 列表选择与播放 ----------------------------------------------------

    /// 当前活动列表的长度（仅 Normal 模式；搜索面板的选择自己管）。
    fn active_len(&self) -> usize {
        match self.tab {
            Tab::Queue => self.queue.len(),
            Tab::Playlists => self.playlists.len(),
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let base = match self.tab {
            Tab::Queue => self.queue_selected,
            Tab::Playlists => self.playlists_selected,
        };
        let next = (base as i32 + delta).clamp(0, len as i32 - 1) as usize;
        self.set_selection(next);
    }

    fn set_selection(&mut self, index: usize) {
        let len = self.active_len();
        let index = index.min(len.saturating_sub(1));
        match self.tab {
            Tab::Queue => self.queue_selected = index,
            Tab::Playlists => self.playlists_selected = index,
        }
    }

    /// Enter：播放当前选中的东西。
    fn play_selected(&mut self) {
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
    }

    // ---- 搜索面板（telescope 式）-------------------------------------------

    /// `/` 打开：空 query 起步，边输入边过滤。
    fn open_search(&mut self) {
        self.search = Some(SearchState {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
        });
        self.mode = Mode::Search;
    }

    fn close_search(&mut self) {
        self.search = None;
        self.mode = Mode::Normal;
    }

    fn handle_search_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyModifiers as KM;
        // 先把按键翻译成意图再执行，避开借用冲突
        enum Intent {
            Keep,
            Close,
            /// 移动结果选中项
            Select(i32),
            /// query 变了，重新过滤
            Edit,
            Play(std::path::PathBuf),
        }
        let intent = match (key.modifiers, key.code) {
            (KM::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => Intent::Close,
            (_, KeyCode::Enter) => self
                .search
                .as_ref()
                .and_then(|s| s.results.get(s.selected))
                .map_or(Intent::Keep, |t| Intent::Play(t.path.clone())),
            // vim/telescope 习惯：C-j/C-k/C-n/C-p 或方向键移动
            (KM::CONTROL, KeyCode::Char('j' | 'n')) | (KeyModifiers::NONE, KeyCode::Down) => {
                Intent::Select(1)
            }
            (KM::CONTROL, KeyCode::Char('k' | 'p')) | (KeyModifiers::NONE, KeyCode::Up) => {
                Intent::Select(-1)
            }
            (_, KeyCode::Backspace) => {
                let search = self.search.as_mut().expect("Search 模式必有面板");
                // 空 query 再退格 = 关闭面板
                if search.query.pop().is_none() {
                    Intent::Close
                } else {
                    Intent::Edit
                }
            }
            (m, KeyCode::Char(c)) if !m.contains(KM::CONTROL) => {
                self.search.as_mut().expect("Search 模式必有面板").query.push(c);
                Intent::Edit
            }
            _ => Intent::Keep,
        };
        match intent {
            Intent::Keep => {}
            Intent::Close => self.close_search(),
            Intent::Select(delta) => {
                let len = self.search.as_ref().map_or(0, |s| s.results.len());
                if len > 0 {
                    let search = self.search.as_mut().expect("Search 模式必有面板");
                    search.selected =
                        (search.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
                }
            }
            Intent::Edit => self.run_search(),
            Intent::Play(path) => self.play_library_track(&path),
        }
    }

    /// 实时过滤：当前 query -> daemon `Find` -> 刷新结果。
    fn run_search(&mut self) {
        let Some(query) = self
            .search
            .as_ref()
            .map(|s| s.query.trim().to_string())
            .filter(|q| !q.is_empty())
        else {
            if let Some(search) = &mut self.search {
                search.results.clear();
                search.selected = 0;
            }
            return;
        };
        match self.daemon.request(Request::Find { query }) {
            Ok(value) => {
                let results = parse_tracks(&value);
                let search = self.search.as_mut().expect("Search 模式必有面板");
                search.results = results;
                search.selected = search.selected.min(search.results.len().saturating_sub(1));
            }
            Err(e) => self.notify(format!("{e}"), true),
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
                    // 空 buffer 再退格 = 退出输入（vim 命令行习惯）
                    if buffer.pop().is_none() {
                        InputOutcome::Cancel
                    } else {
                        InputOutcome::Keep
                    }
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

    // ---- `:` 命令面板 -------------------------------------------------------

    /// 解析完成的命令 -> daemon 请求 / 本地状态。
    fn run_command(&mut self, command: super::cmd::Cmd) {
        use super::cmd::{Cmd, SeekTarget, VolumeTarget};
        match command {
            Cmd::QuitTui => self.should_quit = true,
            Cmd::KillDaemon => {
                let _ = self.daemon.request(Request::Quit);
                self.should_quit = true;
            }
            Cmd::Play(index) => self.simple(Request::Play { index }),
            Cmd::Pause => self.simple(Request::Pause),
            Cmd::Seek(target) => match target {
                SeekTarget::Abs(position_ms) => self.simple(Request::Seek { position_ms }),
                SeekTarget::Rel(delta_ms) => self.simple(Request::SeekRelative { delta_ms }),
            },
            Cmd::Volume(target) => match target {
                VolumeTarget::Set(value) => self.simple(Request::SetVolume { value }),
                VolumeTarget::Adjust(delta) => self.simple(Request::AdjustVolume { delta }),
            },
            Cmd::Repeat(mode) => self.simple(Request::SetRepeat { mode }),
            Cmd::Random(on) => self.simple(Request::SetRandom { on }),
            Cmd::Add(path) => self.load_paths(|paths| Request::Add { paths }, path, "已加入队列"),
            Cmd::Open(path) => {
                self.load_paths(|paths| Request::Open { paths }, path, "已替换队列并播放")
            }
            Cmd::Peek(path) => {
                self.load_paths(|paths| Request::Peek { paths }, path, "试听中，B 结束恢复")
            }
            Cmd::PlayMatch(query) => {
                // 先查一次拿到最佳匹配的名字，用于插播提示（匹配逻辑同一份）
                let best = self
                    .daemon
                    .request(Request::Find {
                        query: query.clone(),
                    })
                    .ok()
                    .and_then(|value| parse_tracks(&value).into_iter().next());
                if let Err(e) = self.daemon.request(Request::PlayMatch { query }) {
                    self.notify(format!("{e}"), true);
                } else if let Some(track) = best {
                    self.notify(format!("插播 {} - {}", track.title, track.artist), false);
                }
                self.need_status = true;
                self.need_queue = true;
            }
            Cmd::PlaylistNew(name) => {
                self.simple(Request::PlaylistNew { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("已创建播放列表 “{name}”"), false);
            }
            Cmd::PlaylistPlay(name) => {
                self.simple(Request::PlaylistPlay { name });
                self.need_playlists = true;
                self.need_queue = true;
            }
            Cmd::PlaylistRemove(name) => {
                self.simple(Request::PlaylistRemove { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("已删除播放列表 “{name}”"), false);
            }
            Cmd::PlaylistAdd { name, path } => {
                self.simple(Request::PlaylistAdd {
                    name: name.clone(),
                    paths: vec![path],
                });
                self.need_playlists = true;
                self.notify(format!("已加入播放列表 “{name}”"), false);
            }
            Cmd::Rescan => self.rescan_library(),
            Cmd::Help => self.help_open = true,
        }
    }

    /// add / open / peek 共用：路径扫描在 daemon 层，回复后刷新。
    fn load_paths(
        &mut self,
        build: fn(Vec<std::path::PathBuf>) -> Request,
        path: std::path::PathBuf,
        done: &str,
    ) {
        if let Err(e) = self.daemon.request(build(vec![path])) {
            self.notify(format!("{e}"), true);
        } else {
            self.notify(done, false);
        }
        self.need_status = true;
        self.need_queue = true;
    }

    // ---- `:` 命令面板 -------------------------------------------------------

    /// `:` 打开：空输入起步，列出全部命令。
    fn open_palette(&mut self) {
        self.palette = Some(CmdPalette {
            input: String::new(),
            selected: 0,
            error: None,
        });
        self.history_cursor = None;
        self.mode = Mode::Command;
    }

    fn close_palette(&mut self) {
        self.palette = None;
        self.mode = Mode::Normal;
    }

    fn handle_palette_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyModifiers as KM;
        // 先把按键翻译成意图再执行，避开借用冲突
        enum Intent {
            Keep,
            Close,
            Confirm,
            /// 移动命令列表选中项
            Select(i32),
            HistoryPrev,
            HistoryNext,
            /// Tab：把选中的命令名代入输入
            Adopt,
            Edit,
        }
        let intent = match (key.modifiers, key.code) {
            (KM::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => Intent::Close,
            (_, KeyCode::Enter) => Intent::Confirm,
            (_, KeyCode::Backspace) => {
                let palette = self.palette.as_mut().expect("Command 模式必有面板");
                // 空 input 再退格 = 关闭面板
                if palette.input.pop().is_none() {
                    Intent::Close
                } else {
                    palette.error = None;
                    Intent::Edit
                }
            }
            (KM::CONTROL, KeyCode::Char('j' | 'n')) => Intent::Select(1),
            (KM::CONTROL, KeyCode::Char('k' | 'p')) => Intent::Select(-1),
            (KeyModifiers::NONE, KeyCode::Up) => Intent::HistoryPrev,
            (KeyModifiers::NONE, KeyCode::Down) => Intent::HistoryNext,
            (KeyModifiers::NONE, KeyCode::Tab) => Intent::Adopt,
            (m, KeyCode::Char(c)) if !m.contains(KM::CONTROL) => {
                let palette = self.palette.as_mut().expect("Command 模式必有面板");
                palette.input.push(c);
                palette.error = None;
                Intent::Edit
            }
            _ => Intent::Keep,
        };
        match intent {
            Intent::Keep => {}
            Intent::Close => self.close_palette(),
            Intent::Confirm => self.confirm_palette(),
            Intent::Select(delta) => {
                let len = self
                    .palette
                    .as_ref()
                    .map_or(0, |p| super::cmd::filter(&p.input).len());
                if len > 0 {
                    let palette = self.palette.as_mut().expect("Command 模式必有面板");
                    palette.selected =
                        (palette.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
                }
            }
            Intent::HistoryPrev => {
                let mut buffer = self
                    .palette
                    .as_mut()
                    .map(|p| std::mem::take(&mut p.input))
                    .unwrap_or_default();
                self.history_prev(&mut buffer);
                if let Some(palette) = self.palette.as_mut() {
                    palette.input = buffer;
                    palette.error = None;
                }
            }
            Intent::HistoryNext => {
                let mut buffer = self
                    .palette
                    .as_mut()
                    .map(|p| std::mem::take(&mut p.input))
                    .unwrap_or_default();
                self.history_next(&mut buffer);
                if let Some(palette) = self.palette.as_mut() {
                    palette.input = buffer;
                    palette.error = None;
                }
            }
            Intent::Adopt => {
                self.adopt_selected();
            }
            Intent::Edit => {
                let palette = self.palette.as_mut().expect("Command 模式必有面板");
                let len = super::cmd::filter(&palette.input).len();
                palette.selected = palette.selected.min(len.saturating_sub(1));
            }
        }
    }

    /// 回车：能整体解析就直接执行并关闭；否则把选中的命令名代入输入
    /// （进入参数模式）；代入不了才把解析错误显示在面板里。
    fn confirm_palette(&mut self) {
        let input = self
            .palette
            .as_ref()
            .map(|p| p.input.trim().to_string())
            .unwrap_or_default();
        if input.is_empty() {
            self.close_palette();
            return;
        }
        match super::cmd::parse(&input) {
            Ok(parsed) => {
                self.remember_command(input);
                self.close_palette();
                self.run_command(parsed);
            }
            Err(e) => {
                if !self.adopt_selected()
                    && let Some(palette) = self.palette.as_mut()
                {
                    palette.error = Some(e);
                }
            }
        }
    }

    /// 把选中的命令名代入输入（保留已输入的参数部分）。
    /// 命令名已完整（错误出在参数）或无选中命令时不动，返回是否代入。
    fn adopt_selected(&mut self) -> bool {
        let input = self
            .palette
            .as_ref()
            .map_or(String::new(), |p| p.input.clone());
        let selected = self.palette.as_ref().map_or(0, |p| p.selected);
        let first = input.split_whitespace().next().unwrap_or("");
        let adopt = super::cmd::filter(&input)
            .get(selected)
            .and_then(|spec| {
                if spec.names.contains(&first) {
                    return None; // 名字已完整，错误出在参数
                }
                spec.names.iter().find(|n| n.starts_with(first)).copied()
            });
        let Some(name) = adopt else {
            return false;
        };
        let rest = input
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest.trim())
            .unwrap_or("");
        if let Some(palette) = self.palette.as_mut() {
            palette.input = if rest.is_empty() {
                format!("{name} ")
            } else {
                format!("{name} {rest}")
            };
            palette.error = None;
        }
        true
    }

    /// 回车后记忆命令（去重连续重复），并复位翻历史的游标。
    fn remember_command(&mut self, line: String) {
        self.history_cursor = None;
        if self.cmd_history.last() != Some(&line) {
            self.cmd_history.push(line);
        }
    }

    fn history_prev(&mut self, buffer: &mut String) {
        if self.cmd_history.is_empty() {
            return;
        }
        self.history_cursor = match self.history_cursor {
            None => {
                self.history_draft = std::mem::take(buffer);
                Some(self.cmd_history.len() - 1)
            }
            Some(0) => return,
            Some(i) => Some(i - 1),
        };
        *buffer = self.cmd_history[self.history_cursor.unwrap()].clone();
    }

    fn history_next(&mut self, buffer: &mut String) {
        let Some(cursor) = self.history_cursor else {
            return;
        };
        self.history_cursor = if cursor + 1 < self.cmd_history.len() {
            *buffer = self.cmd_history[cursor + 1].clone();
            Some(cursor + 1)
        } else {
            *buffer = std::mem::take(&mut self.history_draft);
            None
        };
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
