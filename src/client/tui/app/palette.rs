//! `:` 命令面板：过滤、代入（Tab）、命令历史、回车执行。

use super::super::cmd;
use super::{App, CmdPalette, Mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    /// `:` 打开：空输入起步，列出全部命令。
    pub(super) fn open_palette(&mut self) {
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

    pub(super) fn handle_palette_key(&mut self, key: KeyEvent) {
        use KeyModifiers as KM;
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
                    .map_or(0, |p| cmd::filter(&p.input).len());
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
                let len = cmd::filter(&palette.input).len();
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
        match cmd::parse(&input) {
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
        let adopt = cmd::filter(&input).get(selected).and_then(|spec| {
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
}
