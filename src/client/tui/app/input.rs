//! 单行输入模式（`:w` 存播放列表、`c` 重命名）。

use super::{App, InputPurpose, Mode};
use crate::proto::Request;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub(super) fn enter_input(&mut self, purpose: InputPurpose, initial: String) {
        self.input_buffer = initial;
        self.mode = Mode::Input(purpose);
    }

    pub fn input_buffer(&self) -> &str {
        &self.input_buffer
    }

    pub(super) fn handle_input_key(&mut self, key: KeyEvent) {
        enum InputOutcome {
            Keep,
            Confirm(String),
            Cancel,
        }
        // Mode 带 InputPurpose（含 String）：克隆出来用，避免借用冲突
        let Mode::Input(purpose) = self.mode.clone() else {
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
                // 当前队列已有名字且改了名：走重命名，旧名字条目不残留
                if let Some(prev) = self.current_playlist.clone()
                    && prev != name
                {
                    self.simple(Request::PlaylistRename {
                        from: prev.clone(),
                        to: name.clone(),
                    });
                    self.need_playlists = true;
                    self.notify(format!("“{prev}” 已重命名为 “{name}”"), false);
                    return;
                }
                self.simple(Request::PlaylistSave { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("当前队列已保存为 “{name}”"), false);
            }
            InputPurpose::RenamePlaylist(from) => {
                let to = text.trim().to_string();
                if to.is_empty() {
                    self.notify("播放列表名不能为空", true);
                    return;
                }
                if to == from {
                    return;
                }
                self.simple(Request::PlaylistRename {
                    from: from.clone(),
                    to: to.clone(),
                });
                self.need_playlists = true;
                self.notify(format!("“{from}” 已重命名为 “{to}”"), false);
            }
            InputPurpose::NewPlaylist => {
                let name = text.trim().to_string();
                if name.is_empty() {
                    self.notify("播放列表名不能为空", true);
                    return;
                }
                self.simple(Request::PlaylistNew { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("已创建空播放列表 “{name}”"), false);
            }
            InputPurpose::PluginSearch(plugin) => {
                // 搜索交给内容插件：worker 跑 search，结果经 PluginView
                // 回注画布（TUI 不做任何在线请求）
                self.submit_search(&plugin, text);
            }
        }
    }
}
