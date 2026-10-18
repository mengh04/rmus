//! 列表选中项：移动、播放选中、删除、队列内调序、首次定位。

use super::state::parse_tracks;
use super::{App, Tab, VisualSel};
use crate::proto::Request;
use std::path::{Path, PathBuf};

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

    /// 当前 visual 选段（含两端，按列表长度夹紧）。
    /// 没开 visual 或当前视图不可选段（在线）则 None。
    pub fn visual_range(&self) -> Option<(usize, usize)> {
        let anchor = self.visual?.anchor;
        let (cursor, len) = match self.tab {
            Tab::Queue => (self.queue_selected, self.queue.len()),
            Tab::Library => (self.library_selected, self.library.len()),
            Tab::Playlists => (self.playlists_selected, self.playlists.len()),
            Tab::Online => return None,
        };
        if len == 0 {
            return None;
        }
        let (lo, hi) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((lo.min(len - 1), hi.min(len - 1)))
    }

    pub(super) fn exit_visual(&mut self) {
        self.visual = None;
    }

    /// o：光标跳到选段另一端，往反方向延展。
    pub(super) fn visual_swap_ends(&mut self) {
        let Some(v) = self.visual else {
            return;
        };
        let cursor = match self.tab {
            Tab::Queue => self.queue_selected,
            Tab::Library => self.library_selected,
            Tab::Playlists => self.playlists_selected,
            Tab::Online => return,
        };
        self.visual = Some(VisualSel { anchor: cursor });
        self.set_selection(v.anchor);
    }

    /// visual d / x：把选段剪切进寄存器。
    /// 队列走范围剪切（一次撤销）；播放列表逐个删；音乐库不可删。
    pub(super) fn visual_cut(&mut self) {
        let Some((start, end)) = self.visual_range() else {
            self.exit_visual();
            return;
        };
        match self.tab {
            Tab::Queue => self.delete_queue_range(start, end + 1),
            Tab::Playlists => {
                let names: Vec<String> = self.playlists[start..=end].to_vec();
                for name in names {
                    self.simple(Request::PlaylistRemove { name: name.clone() });
                    self.notify(format!("已删除播放列表 “{name}”"), false);
                }
                self.need_playlists = true;
            }
            Tab::Library => self.notify("音乐库不能删", true),
            Tab::Online => {}
        }
        self.exit_visual();
    }

    /// visual y：把选段整段复制进寄存器（队列 / 音乐库）。
    pub(super) fn visual_yank(&mut self) {
        let Some((start, end)) = self.visual_range() else {
            self.exit_visual();
            return;
        };
        let yank = |items: &[crate::proto::TrackInfo]| {
            Some(
                items[start..=end]
                    .iter()
                    .map(|t| (t.path.clone(), t.title.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let (registered, n) = match self.tab {
            Tab::Queue => (yank(&self.queue), end - start + 1),
            Tab::Library => (yank(&self.library), end - start + 1),
            Tab::Playlists | Tab::Online => (None, 0),
        };
        if let Some(items) = registered {
            self.register = Some(items);
            self.notify(format!("已复制 {n} 首（p 粘贴）"), false);
        }
        self.exit_visual();
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
        self.exit_visual();
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
        // vim 剪切语义：删掉的整段进寄存器
        self.register = Some(
            self.queue[start..end]
                .iter()
                .map(|t| (t.path.clone(), t.title.clone()))
                .collect(),
        );
        let n = end - start;
        self.simple(Request::RemoveRange { start, end });
        self.need_queue = true;
        // 光标停在删除点（列表刷新后 clamp_selections 兜底）
        self.queue_selected = start;
        self.notify(format!("已剪切 {n} 首（p 粘贴）"), false);
    }

    /// dd：删除选中行，删掉的曲子进寄存器（vim 剪切语义，dd + p = 移动）。
    /// 队列视图剪切曲目；播放列表视图只删不留（寄存器只装曲子）。
    pub(super) fn cut_selected_line(&mut self) {
        match self.tab {
            Tab::Queue => {
                let Some(track) = self.queue.get(self.queue_selected) else {
                    return;
                };
                let (path, title) = (track.path.clone(), track.title.clone());
                self.register = Some(vec![(path, title.clone())]);
                self.simple(Request::Remove {
                    index: Some(self.queue_selected),
                });
                self.need_queue = true;
                self.notify(format!("已剪切 {title}（p 粘贴）"), false);
            }
            Tab::Playlists => self.remove_selected(),
            Tab::Library | Tab::Online => {}
        }
    }

    /// m：标记 / 取消标记当前行（间隔选段，按路径记，挪队列不失效）。
    pub(super) fn toggle_mark(&mut self) {
        if self.visual.is_some() {
            return;
        }
        match self.tab {
            Tab::Queue => {
                if let Some(t) = self.queue.get(self.queue_selected) {
                    let path = t.path.clone();
                    if !self.queue_marks.remove(&path) {
                        self.queue_marks.insert(path);
                    }
                    self.notify_marked(self.queue_marks.len());
                }
            }
            Tab::Library => {
                if let Some(t) = self.library.get(self.library_selected) {
                    let path = t.path.clone();
                    if !self.library_marks.remove(&path) {
                        self.library_marks.insert(path);
                    }
                    self.notify_marked(self.library_marks.len());
                }
            }
            Tab::Playlists => {
                if let Some(name) = self.playlists.get(self.playlists_selected) {
                    let name = name.clone();
                    if !self.playlist_marks.remove(&name) {
                        self.playlist_marks.insert(name);
                    }
                    self.notify_marked(self.playlist_marks.len());
                }
            }
            Tab::Online => {}
        }
    }

    fn notify_marked(&mut self, n: usize) {
        self.notify(format!("已标记 {n} 行（d 剪切 / y 复制 / M 清除）"), false);
    }

    /// M：清空当前视图的所有标记。
    pub(super) fn clear_marks(&mut self) {
        let had = self.marks_active();
        match self.tab {
            Tab::Queue => self.queue_marks.clear(),
            Tab::Library => self.library_marks.clear(),
            Tab::Playlists => self.playlist_marks.clear(),
            Tab::Online => {}
        }
        if had {
            self.notify("标记已清空", false);
        }
    }

    /// 当前视图有没有标记（有则 d / y 收割标记而不是单行操作）。
    pub(super) fn marks_active(&self) -> bool {
        match self.tab {
            Tab::Queue => !self.queue_marks.is_empty(),
            Tab::Library => !self.library_marks.is_empty(),
            Tab::Playlists => !self.playlist_marks.is_empty(),
            Tab::Online => false,
        }
    }

    /// d（有标记时）：剪切所有标记行。队列按连续段分组删，
    /// 从高往低删避免下标失效；播放列表逐个删；音乐库不可删。
    pub(super) fn cut_marked(&mut self) {
        match self.tab {
            Tab::Queue => {
                let marks = self.queue_marks.clone();
                let idxs: Vec<usize> = self
                    .queue
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| marks.contains(&t.path))
                    .map(|(i, _)| i)
                    .collect();
                if idxs.is_empty() {
                    self.notify("还没有标记（按 m 标记）", true);
                    return;
                }
                self.register = Some(
                    idxs.iter()
                        .filter_map(|&i| self.queue.get(i))
                        .map(|t| (t.path.clone(), t.title.clone()))
                        .collect(),
                );
                // 连续下标合成一段，一段一次 RemoveRange（一段记一次撤销）
                let mut runs: Vec<(usize, usize)> = Vec::new();
                for &i in &idxs {
                    match runs.last_mut() {
                        Some((_, end)) if *end + 1 == i => *end = i,
                        _ => runs.push((i, i)),
                    }
                }
                let n = idxs.len();
                for (start, end) in runs.into_iter().rev() {
                    self.simple(Request::RemoveRange { start, end: end + 1 });
                }
                self.need_queue = true;
                self.queue_marks.clear();
                self.notify(format!("已剪切 {n} 首（p 粘贴）"), false);
            }
            Tab::Playlists => {
                let names: Vec<String> = self.playlist_marks.iter().cloned().collect();
                for name in &names {
                    self.simple(Request::PlaylistRemove { name: name.clone() });
                    self.notify(format!("已删除播放列表 “{name}”"), false);
                }
                self.need_playlists = true;
                self.playlist_marks.clear();
            }
            Tab::Library => self.notify("音乐库不能删", true),
            Tab::Online => {}
        }
    }

    /// y（有标记时）：把标记行按列表顺序整段复制进寄存器。
    /// 标记保留，可以继续补标再 y。
    pub(super) fn yank_marked(&mut self) {
        fn yank(
            items: &[crate::proto::TrackInfo],
            marks: &std::collections::BTreeSet<PathBuf>,
        ) -> (usize, Vec<(PathBuf, String)>) {
            let picked: Vec<_> = items
                .iter()
                .filter(|t| marks.contains(&t.path))
                .map(|t| (t.path.clone(), t.title.clone()))
                .collect();
            (picked.len(), picked)
        }
        let (n, picked) = match self.tab {
            Tab::Queue => yank(&self.queue, &self.queue_marks),
            Tab::Library => yank(&self.library, &self.library_marks),
            Tab::Playlists | Tab::Online => return,
        };
        if n == 0 {
            self.notify("还没有标记（按 m 标记）", true);
            return;
        }
        self.register = Some(picked);
        self.notify(format!("已复制 {n} 首（p 粘贴）"), false);
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
                self.register = Some(vec![(path, title)]);
            }
            None => self.notify("这里没有可复制的曲子", true),
        }
    }

    /// p / P：把寄存器里的曲子插到队列光标后 / 前（队列空则直接加入）。
    pub(super) fn put_from_register(&mut self, after: bool) {
        let Some(items) = self.register.clone() else {
            self.notify("寄存器是空的，先用 yy / dd 剪切", true);
            return;
        };
        // Add 一次落队尾（下标从 len 起），再逐个挪到插入点；
        // vim 语义：p 在光标后、P 在光标前
        let len = self.queue.len();
        let target = match (after, len) {
            (_, 0) => len,
            (true, _) => (self.queue_selected + 1).min(len),
            (false, _) => self.queue_selected.min(len),
        };
        let paths: Vec<PathBuf> = items.iter().map(|(p, _)| p.clone()).collect();
        if let Err(e) = self.daemon.request(Request::Add { paths }) {
            self.notify(format!("{e}"), true);
            return;
        }
        // 第 i 首加完在 len+i；挪到 target+i 后，剩下的仍在 len+i
        for i in 0..items.len() {
            if target + i != len + i {
                self.simple(Request::Move {
                    from: len + i,
                    to: target + i,
                });
            }
        }
        self.need_queue = true;
        // 光标停在贴进来的第一首上
        self.queue_selected = target;
        self.notify(format!("已粘贴 {} 首", items.len()), false);
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
