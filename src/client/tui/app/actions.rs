//! Normal 模式的按键动作：把 [`Action`] 翻译成 daemon 请求或本地状态改动。

use super::super::keymap::Action;
use super::{App, InputPurpose, Tab};
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
            NextTab => {
                let next = self.tab.next(&self.visible_tabs());
                self.switch_tab(next);
            }
            PrevTab => {
                let prev = self.tab.prev(&self.visible_tabs());
                self.switch_tab(prev);
            }
            GotoTab(n) => {
                let visible = self.visible_tabs();
                if let Some(tab) = visible.get(n) {
                    self.switch_tab(tab.clone());
                }
            }
            Up => self.move_selection(-1),
            Down => self.move_selection(1),
            // 插件画布的选中/滚动归插件：首尾与半页发语义事件，宿主
            // 不再改画布行号（旧实现会把高亮挪到"看得见的最后一行"）
            Top => {
                if !self.send_plugin_nav(crate::proto::plugin_key::KIND_TOP) {
                    self.set_selection(0);
                }
            }
            Bottom => {
                if !self.send_plugin_nav(crate::proto::plugin_key::KIND_BOTTOM) {
                    self.set_selection(usize::MAX);
                }
            }
            HalfUp => {
                if !self.send_plugin_nav(crate::proto::plugin_key::KIND_HALF_UP) {
                    self.move_selection(-7);
                }
            }
            HalfDown => {
                if !self.send_plugin_nav(crate::proto::plugin_key::KIND_HALF_DOWN) {
                    self.move_selection(7);
                }
            }
            LocateInLibrary => self.locate_selected_in_library(),
            PlaySelected => self.play_selected(),
            PlayToggle => self.simple(Request::TogglePlay),
            Next => self.simple(Request::Next),
            Prev => self.simple(Request::Prev),
            Stop => self.simple(Request::Stop),
            RemoveSelected => match self.tab {
                // 插件画布：d = 下载选中项（转发给内容插件）
                Tab::Plugin(_) => self.send_tab_key(
                    crate::proto::plugin_key::KIND_CHAR,
                    'd' as i32,
                ),
                _ => self.remove_selected(),
            },
            RemoveCurrent => match self.tab {
                // 插件画布：x = 取消最近的下载任务
                Tab::Plugin(_) => self.send_tab_key(
                    crate::proto::plugin_key::KIND_CHAR,
                    'x' as i32,
                ),
                _ => {
                    // vim 剪切语义：删掉的正在播的曲子也进寄存器
                    if let Some(track) = self.status.as_ref().and_then(|s| s.track.clone()) {
                        self.register = Some(vec![(track.path, track.title)]);
                    }
                    self.simple(Request::Remove { index: None });
                    self.need_queue = true;
                }
            },
            MoveUp => self.move_selected(-1),
            MoveDown => self.move_selected(1),
            AddSelected => match self.tab {
                // 播放列表视图：a 新建播放列表
                Tab::Playlists => self.enter_input(InputPurpose::NewPlaylist, String::new()),
                _ => self.add_selected_to_queue(),
            },
            AddToPlaylist => self.add_selection_to_playlist_picker(),
            RenameSelected => match self.tab {
                // 队列视图按 c = 把当前队列另存为播放列表（预填出处名：
                // 想覆盖它就原样回车，想另存就改个名字）
                Tab::Queue => {
                    let name = self.current_playlist.clone().unwrap_or_default();
                    self.enter_input(InputPurpose::SavePlaylist, name);
                }
                // 播放列表视图：改选中的那张
                Tab::Playlists => {
                    if let Some(name) = self.playlists.get(self.playlists_selected).cloned() {
                        self.enter_input(InputPurpose::RenamePlaylist(name.clone()), name);
                    }
                }
                Tab::Library | Tab::Plugin(_) | Tab::Plugins => {}
            },
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
            Seek(delta) => self.simple(Request::SeekRelative { delta_ms: delta }),
            Undo => {
                self.simple(Request::Undo);
                self.need_queue = true;
            }
            Redo => {
                self.simple(Request::Redo);
                self.need_queue = true;
            }
            PutAfter => self.put_from_register(true),
            PutBefore => self.put_from_register(false),
            MarkToggle => self.toggle_mark(),
            MarkClear => self.clear_marks(),
            ToggleRandom => self.simple(Request::SetRandom { on: None }),
            VolumeUp => self.simple(Request::AdjustVolume { delta: 0.05 }),
            VolumeDown => self.simple(Request::AdjustVolume { delta: -0.05 }),
            Search => match &self.tab {
                // 在线视图里 / 是在线搜索（预填上次关键词）
                // 插件画布：内容插件满足在线约定才提供 / 搜索
                Tab::Plugin(pt) => {
                    if self.supports_online(&pt.plugin) {
                        let plugin = pt.plugin.clone();
                        self.enter_input(InputPurpose::PluginSearch(plugin), String::new());
                    }
                }
                _ => self.open_search(),
            },
            Command => self.open_palette(),
            SavePlaylist => {
                let name = self.current_playlist.clone().unwrap_or_default();
                self.enter_input(InputPurpose::SavePlaylist, name);
            }
            Rescan => self.rescan_library(),
        }
    }

    /// 切视图。进音乐库时重新拉索引：daemon 会在文件变化时自动重建，
    /// TUI 不收推送，进视图拉一次保证看到的是最新库。
    /// 进在线视图时刷新插件清单（可能刚装了插件、重启了 daemon）。
    /// 在线视图 = rmus-dl 画布：进视图拉插件清单 + 请求画布。
    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.exit_visual();
        // 插件画布：拉一次插件清单（可能刚装了插件）+ 请求对应画布
        if let Some(plugin) = self.plugin_of_tab(&self.tab) {
            self.need_plugins = true;
            self.request_view(&plugin);
        } else if self.tab == Tab::Library {
            self.need_library = true;
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
