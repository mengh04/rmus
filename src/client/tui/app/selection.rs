//! 列表选中项：移动、播放选中、删除、队列内调序、首次定位。

use super::state::parse_tracks;
use super::{App, Tab};
use crate::proto::Request;
use std::path::Path;

impl App {
    /// 当前活动列表的长度（仅 Normal 模式；搜索面板的选择自己管）。
    fn active_len(&self) -> usize {
        match self.tab {
            Tab::Queue => self.queue.len(),
            Tab::Library => self.library.len(),
            Tab::Playlists => self.playlists.len(),
            Tab::Online => self.online.as_ref().map_or(0, |o| o.songs.len()),
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        let base = match self.tab {
            Tab::Queue => self.queue_selected,
            Tab::Library => self.library_selected,
            Tab::Playlists => self.playlists_selected,
            Tab::Online => self.online.as_ref().map_or(0, |o| o.selected),
        };
        let next = (base as i32 + delta).clamp(0, len as i32 - 1) as usize;
        self.set_selection(next);
    }

    pub(super) fn set_selection(&mut self, index: usize) {
        let len = self.active_len();
        let index = index.min(len.saturating_sub(1));
        match self.tab {
            Tab::Queue => self.queue_selected = index,
            Tab::Library => self.library_selected = index,
            Tab::Playlists => self.playlists_selected = index,
            Tab::Online => {
                if let Some(online) = &mut self.online {
                    online.selected = index;
                }
            }
        }
    }

    /// Enter：播放当前选中的东西。
    pub(super) fn play_selected(&mut self) {
        match self.tab {
            Tab::Queue => {
                let index = self.queue_selected;
                if index < self.queue.len() {
                    self.simple(Request::Play { index: Some(index) });
                }
            }
            Tab::Library => {
                if let Some(track) = self.library.get(self.library_selected) {
                    let path = track.path.clone();
                    self.play_library_track(&path);
                }
            }
            Tab::Playlists => {
                let index = self.playlists_selected;
                if let Some(name) = self.playlists.get(index).cloned() {
                    self.simple(Request::PlaylistPlay { name });
                    self.need_queue = true;
                }
            }
            // 在线视图：下载选中曲目，完成后自动试听
            Tab::Online => self.download_selected(true),
        }
    }

    /// d：按视图删除选中的东西——队列删曲，播放列表删整张。
    pub(super) fn remove_selected(&mut self) {
        match self.tab {
            Tab::Queue => {
                let index = self.queue_selected;
                if index >= self.queue.len() {
                    return;
                }
                self.simple(Request::Remove { index: Some(index) });
                self.need_queue = true;
            }
            Tab::Playlists => {
                let Some(name) = self.playlists.get(self.playlists_selected).cloned() else {
                    return;
                };
                self.simple(Request::PlaylistRemove { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("已删除播放列表 “{name}”"), false);
            }
            Tab::Library => {}
            Tab::Online => {}
        }
    }

    /// J / K：队列内把选中曲上移 / 下移（音乐库与播放列表无序可调）。
    /// 光标跟着曲子走，连续按不丢位置。
    pub(super) fn move_selected(&mut self, delta: i32) {
        if self.tab != Tab::Queue {
            return;
        }
        let len = self.queue.len();
        if len == 0 {
            return;
        }
        let from = self.queue_selected;
        let to = (from as i32 + delta).clamp(0, len as i32 - 1) as usize;
        if to == from {
            return;
        }
        self.simple(Request::Move { from, to });
        self.queue_selected = to;
        self.need_queue = true;
    }

    /// a（音乐库视图）：把选中曲目加到队列尾部，不打断当前播放。
    pub(super) fn add_selected_to_queue(&mut self) {
        if self.tab != Tab::Library {
            return;
        }
        let Some(track) = self.library.get(self.library_selected) else {
            return;
        };
        let path = track.path.clone();
        let title = track.title.clone();
        if self.enqueue_path(&path, &title) {
            // Add 成功后引擎会推 QueueChanged，这里只补状态里的队列长度
            self.need_status = true;
        }
    }

    /// `gd`（队列视图）：把选中的曲子在音乐库里定位（vim goto definition）。
    /// 只在队列视图生效；曲子不在索引里（外部路径加进来的）则提示。
    pub(super) fn locate_selected_in_library(&mut self) {
        if self.tab != Tab::Queue {
            return;
        }
        let Some(track) = self.queue.get(self.queue_selected) else {
            return;
        };
        let path = track.path.clone();
        if !self.reveal_in_library(&path) {
            self.notify("这首曲子不在音乐库里", true);
        }
    }

    /// 把音乐库视图切到 path 这首曲子上：现拉一次库列表拿到最新下标，
    /// 光标停上去。找到返回 true（找不到不动视图）。
    /// 搜索面板的 gf 与队列的 gd 共用。
    pub(super) fn reveal_in_library(&mut self, path: &Path) -> bool {
        if let Ok(value) = self.daemon.request(Request::LibList) {
            self.library = parse_tracks(&value);
            self.clamp_selections();
        }
        let Some(index) = self.library.iter().position(|t| t.path == path) else {
            return false;
        };
        self.library_selected = index;
        self.tab = Tab::Library;
        true
    }

    /// 队列范围删除（dj / dk / dG / dgg），删 [start, end)。
    /// 光标停在删除点；引擎侧整段记一次撤销快照，一次 u 就能全回滚。
    pub(super) fn delete_queue_range(&mut self, start: usize, end: usize) {
        if self.tab != Tab::Queue || self.queue.is_empty() {
            return;
        }
        let len = self.queue.len();
        let start = start.min(len - 1);
        let end = end.min(len).max(start + 1);
        self.simple(Request::RemoveRange { start, end });
        self.need_queue = true;
        // 光标停在删除点（列表刷新后 clamp_selections 兜底）
        self.queue_selected = start;
    }

    /// yy：把选中曲复制进未命名寄存器（队列 / 音乐库视图可用）。
    pub(super) fn yank_selected(&mut self) {
        let yanked = match self.tab {
            Tab::Queue => self
                .queue
                .get(self.queue_selected)
                .map(|t| (t.path.clone(), t.title.clone())),
            Tab::Library => self
                .library
                .get(self.library_selected)
                .map(|t| (t.path.clone(), t.title.clone())),
            Tab::Playlists | Tab::Online => None,
        };
        match yanked {
            Some((path, title)) => {
                self.notify(format!("已复制 {title}（p 粘贴到队列）"), false);
                self.register = Some((path, title));
            }
            None => self.notify("这里没有可复制的曲子", true),
        }
    }

    /// p / P：把寄存器里的曲子插到队列光标后 / 前（队列空则直接加入）。
    pub(super) fn put_from_register(&mut self, after: bool) {
        let Some((path, title)) = self.register.clone() else {
            self.notify("寄存器是空的，先用 yy 复制一首", true);
            return;
        };
        // Add 落队尾（下标 = 当前长度），再 Move 到插入点；
        // vim 语义：p 在光标后、P 在光标前
        let len = self.queue.len();
        let target = match (after, len) {
            (_, 0) => len,
            (true, _) => (self.queue_selected + 1).min(len),
            (false, _) => self.queue_selected.min(len),
        };
        if !self.enqueue_path(&path, &title) {
            return;
        }
        if target != len {
            self.simple(Request::Move {
                from: len,
                to: target,
            });
        }
        self.need_queue = true;
        self.queue_selected = target;
    }

    pub(super) fn clamp_selections(&mut self) {
        let clamp = |sel: &mut usize, len: usize| {
            if len == 0 {
                *sel = 0;
            } else if *sel >= len {
                *sel = len - 1;
            }
        };
        clamp(&mut self.queue_selected, self.queue.len());
        clamp(&mut self.library_selected, self.library.len());
        clamp(&mut self.playlists_selected, self.playlists.len());
        if let Some(online) = &mut self.online {
            clamp(&mut online.selected, online.songs.len());
        }
    }

    /// 队列第一次到手时，把光标停在正在播放的那首上。
    pub(super) fn seed_queue_selection(&mut self) {
        if self.seeded_queue || self.queue.is_empty() || self.status.is_none() {
            return;
        }
        self.seeded_queue = true;
        if let Some(current) = self.status.as_ref().and_then(|s| s.current)
            && current < self.queue.len()
        {
            self.queue_selected = current;
        }
    }

    /// 播放列表第一次到手时，把光标停在当前在用的那张上。
    pub(super) fn seed_playlist_selection(&mut self) {
        if self.seeded_playlists || self.playlists.is_empty() {
            return;
        }
        self.seeded_playlists = true;
        if let Some(name) = self.current_playlist.as_deref()
            && let Some(index) = self.playlists.iter().position(|n| n == name)
        {
            self.playlists_selected = index;
        }
    }
}
