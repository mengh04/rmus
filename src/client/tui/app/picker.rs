//! 候选列表（telescope 式）：输入即模糊过滤，Enter 确认。
//!
//! 当前用途：把音乐库选中曲（visual 选段或光标单曲）加入播放列表——
//! 候选是已有的播放列表，输入不匹配任何候选时回车 = 以输入为名新建并加入。
//! 组件本身是通用的：`open_picker` 塞什么候选都行。

use super::state::{CandidateItem, CandidatePicker};
use super::{App, Mode};
use crate::proto::Request;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

impl App {
    /// 音乐库选中集（visual 选段优先，否则光标单曲）的路径列表。
    pub(super) fn library_selected_paths(&self) -> Vec<PathBuf> {
        if let Some((start, end)) = self.visual_range() {
            return self.library[start..=end]
                .iter()
                .map(|t| t.path.clone())
                .collect();
        }
        self.library
            .get(self.library_selected)
            .map(|t| vec![t.path.clone()])
            .unwrap_or_default()
    }

    /// 音乐库选中集（visual 选段优先）的展示摘要，如 “12 首” / “晴天”。
    fn selection_summary(&self) -> String {
        if let Some((start, end)) = self.visual_range() {
            return format!("{} 首", end - start + 1);
        }
        self.library
            .get(self.library_selected)
            .map(|t| t.title.clone())
            .unwrap_or_default()
    }

    /// A（音乐库）：把选中曲加入**指定的**播放列表——弹候选列表。
    /// 往哪张列表加必须看得见：不再默认写进"当前队列的出处列表"
    pub(super) fn add_selection_to_playlist_picker(&mut self) {
        let paths = self.library_selected_paths();
        if paths.is_empty() {
            return;
        }
        let summary = self.selection_summary();
        // 出处列表排第一：最常见的操作仍是"加回我正在听的那张"
        let mut names = self.playlists.clone();
        if let Some(cur) = self.current_playlist.clone()
            && let Some(pos) = names.iter().position(|n| *n == cur)
        {
            let name = names.remove(pos);
            names.insert(0, name);
        }
        let items = names
            .iter()
            .map(|name| CandidateItem {
                label: name.clone(),
                value: name.clone(),
            })
            .collect();
        self.open_picker(format!("加入播放列表 · {summary}"), items, paths);
    }

    /// 打开候选列表（通用入口）。`paths` 是确认时要用的附加数据。
    pub(super) fn open_picker(&mut self, title: String, items: Vec<CandidateItem>, paths: Vec<PathBuf>) {
        self.picker = Some(CandidatePicker {
            title,
            input: String::new(),
            items,
            selected: 0,
        });
        self.picker_paths = paths;
        self.mode = Mode::Picker;
    }

    fn close_picker(&mut self) {
        self.picker = None;
        self.picker_paths = Vec::new();
        self.mode = Mode::Normal;
    }

    pub(super) fn handle_picker_key(&mut self, key: KeyEvent) {
        use KeyModifiers as KM;
        enum Intent {
            Keep,
            Close,
            Confirm,
            Select(i32),
            Edit,
        }
        let intent = match (key.modifiers, key.code) {
            (KM::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => Intent::Close,
            (_, KeyCode::Enter) => Intent::Confirm,
            (_, KeyCode::Backspace) => {
                let picker = self.picker.as_mut().expect("Picker 模式必有候选列表");
                // 空 input 再退格 = 关闭
                if picker.input.pop().is_none() {
                    Intent::Close
                } else {
                    Intent::Edit
                }
            }
            (KM::CONTROL, KeyCode::Char('j' | 'n')) => Intent::Select(1),
            (KM::CONTROL, KeyCode::Char('k' | 'p')) => Intent::Select(-1),
            (KeyModifiers::NONE, KeyCode::Up) => Intent::Select(-1),
            (KeyModifiers::NONE, KeyCode::Down) => Intent::Select(1),
            (m, KeyCode::Char(c)) if !m.contains(KM::CONTROL) => {
                let picker = self.picker.as_mut().expect("Picker 模式必有候选列表");
                picker.input.push(c);
                Intent::Edit
            }
            _ => Intent::Keep,
        };
        match intent {
            Intent::Keep => {}
            Intent::Close => self.close_picker(),
            Intent::Confirm => self.confirm_picker(),
            Intent::Select(delta) => {
                let len = self
                    .picker
                    .as_ref()
                    .map_or(0, |p| p.filtered().len());
                if len > 0 {
                    let picker = self.picker.as_mut().expect("Picker 模式必有候选列表");
                    picker.selected =
                        (picker.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
                }
            }
            Intent::Edit => {
                let picker = self.picker.as_mut().expect("Picker 模式必有候选列表");
                let len = picker.filtered().len();
                picker.selected = picker.selected.min(len.saturating_sub(1));
            }
        }
    }

    /// 回车：输入与某个候选完全同名 → 加进它；否则视为新名字，
    /// PlaylistAdd 本就会"不存在则创建"，一步完成新建+加入。
    fn confirm_picker(&mut self) {
        let Some(picker) = self.picker.clone() else {
            return;
        };
        let input = picker.input.trim().to_string();
        let target = match picker.picked() {
            // 有候选且输入为空：用高亮候选
            Some(item) if input.is_empty() => item.value.clone(),
            // 输入与某候选同名（不区分大小写）：加进它
            _ => {
                match picker
                    .items
                    .iter()
                    .find(|item| item.value.to_lowercase() == input.to_lowercase())
                {
                    Some(item) => item.value.clone(),
                    // 其余情况（含输入了新名字）：不存在则创建
                    _ if !input.is_empty() => input,
                    _ => {
                        self.close_picker();
                        return;
                    }
                }
            }
        };
        let paths = std::mem::take(&mut self.picker_paths);
        if paths.is_empty() {
            self.close_picker();
            return;
        }
        let n = paths.len();
        let created = !self.playlists.contains(&target);
        let skipped = self
            .daemon
            .request(Request::PlaylistAdd {
                name: target.clone(),
                paths,
            })
            .ok()
            .and_then(|v| v.get("skipped").and_then(|s| s.as_u64()))
            .unwrap_or(0) as usize;
        // 只写播放列表，队列分毫不动 —— 这正是解耦后 A 的语义
        self.need_playlists = true;
        self.close_picker();
        let dup = if skipped > 0 {
            format!("，{skipped} 首已在列表里")
        } else {
            String::new()
        };
        self.notify(
            if created {
                format!("已新建播放列表 “{target}” 并加入 {} 首{dup}（当前队列未变）", n - skipped)
            } else {
                format!("已把 {} 首加入 “{target}”{dup}（当前队列未变）", n - skipped)
            },
            false,
        );
    }
}
