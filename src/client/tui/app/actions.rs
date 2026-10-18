//! Normal 模式的按键动作：把 [`Action`] 翻译成 daemon 请求或本地状态改动。

use super::super::keymap::Action;
use super::{App, InputPurpose};
use crate::proto::Request;
use crossterm::event::{KeyCode, KeyEvent};

impl App {
    pub(super) fn execute(&mut self, action: Action) {
        use Action::*;
        match action {
            Quit => self.should_quit = true,
            Help => {
                self.help_open = !self.help_open;
                self.help_scroll = 0;
            }
            Close => {
                // Normal 模式下只剩帮助浮层（搜索面板由 Mode::Search 自己管）
                if self.help_open {
                    self.help_open = false;
                    self.help_scroll = 0;
                }
            }
            NextTab => self.tab = self.tab.next(),
            PrevTab => self.tab = self.tab.prev(),
            GotoTab(n) => self.tab = super::Tab::ALL[n],
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

    /// 帮助浮层的滚动键（j/k/g/G）。真正越界的部分由渲染层夹紧。
    pub(super) fn scroll_help(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.help_scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1)
            }
            KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::Char('G') => self.help_scroll = usize::MAX,
            _ => return false,
        }
        true
    }

    /// 只发请求、失败弹提示的小工具（不关心返回值）。
    pub(super) fn simple(&mut self, request: Request) {
        if let Err(e) = self.daemon.request(request) {
            self.notify(format!("{e}"), true);
        }
        self.need_status = true;
    }

    /// 全量重建库索引，完成后提示曲目数。
    pub(super) fn rescan_library(&mut self) {
        match self.daemon.request(Request::LibRescan) {
            Ok(value) => {
                let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
                self.notify(format!("库索引已重建：{added} 首"), false);
            }
            Err(e) => self.notify(format!("{e}"), true),
        }
    }
}
