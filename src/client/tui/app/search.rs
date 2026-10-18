//! 搜索面板（telescope 式）：边输入边向 daemon 查，回车插播。
//!
//! 面板内分两个子模式，Esc 切换（再按 Esc 才关面板）：
//! - 输入态（默认）：字母进关键词，动作在 Ctrl / Enter / Tab 上，
//!   贴 telescope 的肌肉记忆——`C-j/C-k` `C-n/C-p` 移动、`C-o` 下一首、
//!   `C-g` 音乐库定位、`C-w` 删词、`C-u` 清空。
//! - normal 态：字母键让给动作，vim 直感——`j/k` 移动、`gg`/`G` 首/尾、
//!   `Enter` 立刻插播、`o` 插到当前曲之后、`a` 加到队尾、`gf` 去音乐库
//!   定位、`i` 或 `/` 回到输入。

use super::state::parse_tracks;
use super::{App, Mode, SearchMode, SearchState};
use crate::proto::{Request, StatusInfo, TrackInfo};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

/// 按键翻译出的意图。两个子模式各自负责翻译，执行统一走
/// [`App::apply_search_intent`]（避开借用冲突，也避免动作写两遍）。
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
    /// normal -> 输入态（i / /）
    ToInput,
    Play(PathBuf),
    /// 插到当前曲之后（不播）
    PlayNext(TrackInfo),
    /// 把选中结果加到队尾（不播）
    Enqueue(TrackInfo),
    /// 去音乐库视图定位这首曲子
    Locate(PathBuf),
}

impl App {
    /// `/` 打开：空 query 起步，边输入边过滤。
    pub(super) fn open_search(&mut self) {
        self.search = Some(SearchState {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            mode: SearchMode::Input,
            pending_g: false,
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
        let intent = match self.search.as_ref().map_or(SearchMode::Input, |s| s.mode) {
            SearchMode::Input => self.translate_search_input_key(key),
            SearchMode::Normal => self.translate_search_normal_key(key),
        };
        self.apply_search_intent(intent);
    }

    /// 输入态：普通字母进 query，动作键与 telescope 一致，Esc 进 normal。
    fn translate_search_input_key(&mut self, key: KeyEvent) -> Intent {
        use KeyModifiers as KM;
        match (key.modifiers, key.code) {
            (KM::CONTROL, KeyCode::Char('c')) => Intent::Close,
            (_, KeyCode::Esc) => {
                if let Some(search) = &mut self.search {
                    search.mode = SearchMode::Normal;
                    search.pending_g = false;
                }
                Intent::Keep
            }
            (_, KeyCode::Enter) => self
                .selected_track()
                .map_or(Intent::Keep, |t| Intent::Play(t.path)),
            // Tab：入队不播——"还想再听几首再轮到它"的加法
            (_, KeyCode::Tab) => self.selected_track().map_or(Intent::Keep, Intent::Enqueue),
            // C-o：插到当前曲之后，不打断正在播的（vim `o` 开新行的位置感）
            (KM::CONTROL, KeyCode::Char('o')) => self
                .selected_track()
                .map_or(Intent::Keep, Intent::PlayNext),
            // C-g：去音乐库定位（vim `gf` 打开文件的联想）
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
        }
    }

    /// normal 态：字母键就是动作。`i` / `/` 回到输入。
    fn translate_search_normal_key(&mut self, key: KeyEvent) -> Intent {
        use KeyModifiers as KM;
        // `g` 前缀：gg 跳顶 / gf 音乐库定位；其余键取消前缀照常处理
        if self.search.as_ref().is_some_and(|s| s.pending_g) {
            if let Some(search) = &mut self.search {
                search.pending_g = false;
            }
            match (key.modifiers, key.code) {
                (KM::NONE, KeyCode::Char('g')) => return Intent::SelectAbs(0),
                (KM::NONE, KeyCode::Char('f')) => {
                    return self
                        .selected_track()
                        .map_or(Intent::Keep, |t| Intent::Locate(t.path));
                }
                _ => {}
            }
        }
        match (key.modifiers, key.code) {
            (KM::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => Intent::Close,
            (KM::NONE, KeyCode::Char('i' | '/')) => Intent::ToInput,
            // vim 上下移动
            (KM::NONE, KeyCode::Char('j'))
            | (KM::CONTROL, KeyCode::Char('j' | 'n'))
            | (KM::NONE, KeyCode::Down) => Intent::Select(1),
            (KM::NONE, KeyCode::Char('k'))
            | (KM::CONTROL, KeyCode::Char('k' | 'p'))
            | (KM::NONE, KeyCode::Up) => Intent::Select(-1),
            (KM::NONE, KeyCode::Char('g')) => {
                if let Some(search) = &mut self.search {
                    search.pending_g = true;
                }
                Intent::Keep
            }
            (KM::SHIFT, KeyCode::Char('G')) | (KM::NONE, KeyCode::End) => {
                Intent::SelectAbs(usize::MAX)
            }
            (KM::NONE, KeyCode::Home) => Intent::SelectAbs(0),
            // Enter 立刻插播；o 插到当前曲之后；a / Tab 加到队尾
            (_, KeyCode::Enter) => self
                .selected_track()
                .map_or(Intent::Keep, |t| Intent::Play(t.path)),
            (KM::NONE, KeyCode::Char('o')) | (KM::CONTROL, KeyCode::Char('o')) => self
                .selected_track()
                .map_or(Intent::Keep, Intent::PlayNext),
            (KM::NONE, KeyCode::Char('a')) | (_, KeyCode::Tab) => {
                self.selected_track().map_or(Intent::Keep, Intent::Enqueue)
            }
            // C-g：去音乐库定位
            (KM::CONTROL, KeyCode::Char('g')) => self
                .selected_track()
                .map_or(Intent::Keep, |t| Intent::Locate(t.path)),
            // 编辑键顺手保留：清词 / 删词后回到输入态
            (KM::CONTROL, KeyCode::Char('u')) => {
                if let Some(search) = &mut self.search {
                    search.query.clear();
                }
                Intent::ToInput
            }
            (KM::CONTROL, KeyCode::Char('w')) => {
                if let Some(search) = &mut self.search {
                    delete_word(&mut search.query);
                }
                Intent::ToInput
            }
            _ => Intent::Keep,
        }
    }

    fn apply_search_intent(&mut self, intent: Intent) {
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
            Intent::ToInput => {
                if let Some(search) = &mut self.search {
                    search.mode = SearchMode::Input;
                    search.pending_g = false;
                }
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

    /// 插到当前曲之后（不播）。协议没有"按路径插队"，
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

    /// 切到音乐库视图，光标停到这首曲子上。
    pub(super) fn locate_in_library(&mut self, path: &Path) {
        if self.reveal_in_library(path) {
            self.close_search();
        } else {
            self.notify("这首曲子不在音乐库里", true);
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
    use super::{SearchMode, delete_word};

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

    #[test]
    fn 子模式_默认输入态() {
        assert_eq!(SearchMode::default(), SearchMode::Input);
    }
}
