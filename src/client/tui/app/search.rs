//! 搜索面板（telescope 式）：边输入边向 daemon 查，回车插播。
//!
//! 键位贴 vim / telescope 的肌肉记忆——普通字母留给关键词输入，动作都在
//! Ctrl / Enter / Tab 上：
//! - 移动：`C-j/C-n` `C-k/C-p`（或方向键）；`Home/End` 跳到首/尾
//! - `Enter` 立刻插播；`C-o` 插到当前曲之后（vim `o`：在下面开一行）
//! - `Tab` 加到队列末尾；`C-g` 切到音乐库定位这首曲子（vim `gf` 打开文件）
//! - `C-w` 删上一个词、`C-u` 清空关键词（vim 插入模式同款）

use super::state::parse_tracks;
use super::{App, Mode, SearchState, Tab};
use crate::proto::{Request, StatusInfo, TrackInfo};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

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

    /// 当前选中的结果（克隆出来，免得动作执行时占用借用）。
    fn selected_track(&self) -> Option<TrackInfo> {
        let search = self.search.as_ref()?;
        search.results.get(search.selected).cloned()
    }

    pub(super) fn handle_search_key(&mut self, key: KeyEvent) {
        use KeyModifiers as KM;
        // 先把按键翻译成意图再执行，避开借用冲突
        enum Intent {
            Keep,
            Close,
            /// 移动结果选中项
            Select(i32),
            /// 跳到第 index 个结果（usize::MAX = 最后一个）
            SelectAbs(usize),
            /// query 变了，重新过滤
            Edit,
            /// C-u：清空关键词
            ClearQuery,
            /// C-w：删掉最后一个词
            DeleteWord,
            Play(PathBuf),
            /// 插到当前曲之后（不播）
            PlayNext(TrackInfo),
            /// 把选中结果加到队尾（不播）
            Enqueue(TrackInfo),
            /// 去音乐库视图定位这首曲子
            Locate(PathBuf),
        }
        let intent = match (key.modifiers, key.code) {
            (KM::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => Intent::Close,
            (_, KeyCode::Enter) => self
                .selected_track()
                .map_or(Intent::Keep, |t| Intent::Play(t.path)),
            // Tab：入队不播——"还想再听几首再轮到它"的加法
            (_, KeyCode::Tab) => self
                .selected_track()
                .map_or(Intent::Keep, Intent::Enqueue),
            // C-o：插到当前曲之后，不打断正在播的（vim `o` 开新行的位置感）
            (KM::CONTROL, KeyCode::Char('o')) => self
                .selected_track()
                .map_or(Intent::Keep, Intent::PlayNext),
            // C-g：切到音乐库把光标停到这首曲子上（vim `gf` 打开文件的联想）
            (KM::CONTROL, KeyCode::Char('g')) => self
                .selected_track()
                .map_or(Intent::Keep, |t| Intent::Locate(t.path)),
            // vim/telescope 习惯：C-j/C-k/C-n/C-p 或方向键移动
            (KM::CONTROL, KeyCode::Char('j' | 'n')) | (KeyModifiers::NONE, KeyCode::Down) => {
                Intent::Select(1)
            }
            (KM::CONTROL, KeyCode::Char('k' | 'p')) | (KeyModifiers::NONE, KeyCode::Up) => {
                Intent::Select(-1)
            }
            (KeyModifiers::NONE, KeyCode::Home) => Intent::SelectAbs(0),
            (KeyModifiers::NONE, KeyCode::End) => Intent::SelectAbs(usize::MAX),
            // vim 插入模式的词编辑：C-w 删上一个词，C-u 清空整行
            (KM::CONTROL, KeyCode::Char('w')) => Intent::DeleteWord,
            (KM::CONTROL, KeyCode::Char('u')) => Intent::ClearQuery,
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
            Intent::SelectAbs(index) => {
                let len = self.search.as_ref().map_or(0, |s| s.results.len());
                if len > 0 {
                    let search = self.search.as_mut().expect("Search 模式必有面板");
                    search.selected = index.min(len - 1);
                }
            }
            Intent::Edit => self.run_search(),
            Intent::ClearQuery => {
                if let Some(search) = &mut self.search {
                    search.query.clear();
                }
                self.run_search();
            }
            Intent::DeleteWord => {
                if let Some(search) = &mut self.search {
                    delete_word(&mut search.query);
                }
                self.run_search();
            }
            Intent::Play(path) => self.play_library_track(&path),
            Intent::PlayNext(track) => {
                let (path, title) = (track.path, track.title);
                self.play_next_in_queue(&path, &title);
            }
            Intent::Enqueue(track) => {
                let (path, title) = (track.path, track.title);
                self.enqueue_path(&path, &title);
            }
            Intent::Locate(path) => self.locate_in_library(&path),
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
    pub(super) fn play_library_track(&mut self, path: &Path) {
        let Ok(value) = self.daemon.request(Request::Status) else {
            return;
        };
        let Ok(status) = serde_json::from_value::<StatusInfo>(value) else {
            return;
        };
        let index = status.queue_len; // Add 后新曲目落在队尾
        let title = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !self.enqueue_path(path, &title) {
            return;
        }
        self.simple(Request::Play { index: Some(index) });
        self.need_queue = true;
        self.notify(format!("插播 {title}"), false);
    }

    /// C-o：插到当前曲之后（不播）。协议没有"按路径插队"，
    /// 用 Add（落到队尾）+ Move（挪到当前曲后一位）组合实现。
    pub(super) fn play_next_in_queue(&mut self, path: &Path, title: &str) {
        let Ok(value) = self.daemon.request(Request::Status) else {
            return;
        };
        let Ok(status) = serde_json::from_value::<StatusInfo>(value) else {
            return;
        };
        if !self.enqueue_path(path, title) {
            return;
        }
        // Add 后新曲在队尾（下标 = 加之前的队列长度）；没有当前曲或当前曲
        // 就是最后一首时，队尾本来就是"下一首"，免掉这步 Move
        let tail = status.queue_len;
        let target = status.current.map_or(tail, |cur| (cur + 1).min(tail));
        if target != tail {
            self.simple(Request::Move { from: tail, to: target });
        }
        self.need_queue = true;
        self.notify(format!("下一首播放：{title}"), false);
    }

    /// C-g：切到音乐库视图，光标停到这首曲子上。
    pub(super) fn locate_in_library(&mut self, path: &Path) {
        // 现拉一次库列表（切视图本来也会刷新），拿到最新下标才能定位
        if let Ok(value) = self.daemon.request(Request::LibList) {
            self.library = parse_tracks(&value);
            self.clamp_selections();
        }
        let index = self.library.iter().position(|t| t.path == path);
        self.tab = Tab::Library;
        match index {
            Some(i) => {
                self.library_selected = i;
                self.close_search();
            }
            None => self.notify("这首曲子不在音乐库里", true),
        }
    }

    /// 把一首曲加到队列尾部（不播）。成功返回 true。
    /// `selection::add_selected_to_queue` 与搜索面板的 Tab 入队共用。
    pub(super) fn enqueue_path(&mut self, path: &Path, title: &str) -> bool {
        match self.daemon.request(Request::Add {
            paths: vec![path.to_path_buf()],
        }) {
            Ok(_) => {
                self.notify(format!("已加入队列：{title}"), false);
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), true);
                false
            }
        }
    }
}

/// vim `C-w`：删掉光标前的词。光标固定在末尾，即删掉最后一个词
/// （连同它前面的空白）；query 已空则不动。
fn delete_word(query: &mut String) {
    while query.ends_with(char::is_whitespace) {
        query.pop();
    }
    while query.chars().next_back().is_some_and(|c| !c.is_whitespace()) {
        query.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::delete_word;

    #[test]
    fn 删词_从末尾删一个词() {
        let mut q = String::from("周杰伦 晴天");
        delete_word(&mut q);
        assert_eq!(q, "周杰伦 ");

        delete_word(&mut q);
        assert_eq!(q, "");
    }

    #[test]
    fn 删词_先吃掉词尾空白_空串不动() {
        let mut q = String::from("abc def   ");
        delete_word(&mut q);
        assert_eq!(q, "abc ");

        let mut empty = String::new();
        delete_word(&mut empty);
        assert_eq!(empty, "");
    }
}
