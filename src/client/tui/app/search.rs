//! 搜索面板（telescope 式）：边输入边向 daemon 查，Enter 插播。

use super::state::parse_tracks;
use super::{App, Mode, SearchState};
use crate::proto::{Request, StatusInfo};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::Path;

impl App {
    /// `/` 打开：空 query 起步，边输入边过滤。
    pub(super) fn open_search(&mut self) {
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

    pub(super) fn handle_search_key(&mut self, key: KeyEvent) {
        use KeyModifiers as KM;
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
                self.search
                    .as_mut()
                    .expect("Search 模式必有面板")
                    .query
                    .push(c);
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

    /// 播放库里的一首：加到队尾并跳过去播。
    /// （协议没有"按路径直接插播"，用 Add + Play(index) 组合实现）
    fn play_library_track(&mut self, path: &Path) {
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
}
