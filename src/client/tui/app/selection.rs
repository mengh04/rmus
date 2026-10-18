//! 列表选中项：移动、播放选中、删除、队列内调序、首次定位。

use super::{App, Tab};
use crate::proto::Request;

impl App {
    /// 当前活动列表的长度（仅 Normal 模式；搜索面板的选择自己管）。
    fn active_len(&self) -> usize {
        match self.tab {
            Tab::Queue => self.queue.len(),
            Tab::Library => self.library.len(),
            Tab::Playlists => self.playlists.len(),
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
