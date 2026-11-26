//! 引擎事件循环：命令、解码线程回报、位置推送、按需落盘。

use super::{Engine, POSITION_EVENT_INTERVAL, ToEngine};
use crate::proto::{Event, PlayState, Reply};
use std::sync::mpsc;
use std::time::Instant;

impl Engine {
    pub(super) fn run(mut self, cmd_rx: mpsc::Receiver<ToEngine>) {
        loop {
            self.output.poll_pause();
            if self.state == PlayState::Playing
                && self.last_position_event.elapsed() >= POSITION_EVENT_INTERVAL
            {
                self.last_position_event = Instant::now();
                self.emit(Event::Position {
                    position_ms: self.position_ms(),
                });
            }
            match cmd_rx.recv_timeout(POSITION_EVENT_INTERVAL) {
                Ok(ToEngine::Request { request, reply, .. }) => {
                    let result = self.handle(request);
                    let _ = reply.send(result);
                }
                Ok(ToEngine::AddScanned { items, reply, .. }) => {
                    if !items.is_empty() {
                        self.push_history();
                    }
                    let added = items.len();
                    self.queue.items.extend(items);
                    self.dirty = true;
                    self.emit(Event::QueueChanged);
                    let _ = reply.send(Ok(Reply::Added { added, skipped: 0 }));
                }
                Ok(ToEngine::InsertNextScanned { items, reply, .. }) => {
                    let added = items.len();
                    if added > 0 {
                        self.push_history();
                        let at = self
                            .queue
                            .current
                            .map_or(0, |c| (c + 1).min(self.queue.items.len()));
                        self.queue.items.splice(at..at, items);
                        self.dirty = true;
                        self.emit(Event::QueueChanged);
                    }
                    let _ = reply.send(Ok(Reply::Added { added, skipped: 0 }));
                }
                Ok(ToEngine::InsertAuto {
                    items,
                    after_current,
                }) => {
                    // 试听态下不插歌；按路径去重，不压撤销栈以免打乱用户的 u 操作
                    if self.peek.is_some() || items.is_empty() {
                        continue;
                    }
                    let fresh: Vec<_> = items
                        .into_iter()
                        .filter(|it| !self.queue.items.iter().any(|q| q.path == it.path))
                        .map(|mut it| {
                            it.recommended = true;
                            it
                        })
                        .collect();
                    if fresh.is_empty() {
                        continue;
                    }
                    if after_current {
                        let at = self
                            .queue
                            .current
                            .map_or(0, |c| (c + 1).min(self.queue.items.len()));
                        self.queue.items.splice(at..at, fresh);
                    } else {
                        self.queue.items.extend(fresh);
                    }
                    self.dirty = true;
                    self.emit(Event::QueueChanged);
                }
                Ok(ToEngine::RecommendSnapshot { reply }) => {
                    let _ = reply.send(super::RecommendContext {
                        state: self.state,
                        current: self.queue.current,
                        queue: self.queue.items.clone(),
                        playlists: self.playlists.clone(),
                        repeat: self.repeat,
                        random: self.random,
                        peeking: self.peek.is_some(),
                        auto_recommend: self.auto_recommend,
                    });
                }
                Ok(ToEngine::PurgePath { path, reply }) => {
                    let outcome = self.purge_path(path);
                    let _ = reply.send(outcome);
                }
                Ok(ToEngine::OpenScanned { items, reply, .. }) => {
                    if items.is_empty() {
                        let _ = reply.send(Err("没有找到可播放的音频文件".into()));
                        continue;
                    }
                    // 显式换队列 = 放弃试听恢复点
                    self.peek = None;
                    self.kill_decoder();
                    self.queue.items = items;
                    self.queue.current = None;
                    self.queue_origin = None;
                    self.dirty = true;
                    self.emit(Event::QueueChanged);
                    self.start_track(0, 0, true);
                    let _ = reply.send(Ok(Reply::Status(Box::new(self.status()))));
                }
                Ok(ToEngine::PeekScanned { items, reply, .. }) => {
                    if items.is_empty() {
                        let _ = reply.send(Err("没有找到可播放的音频文件".into()));
                        continue;
                    }
                    self.enter_peek(items);
                    let _ = reply.send(Ok(Reply::Status(Box::new(self.status()))));
                }
                Ok(ToEngine::PlayNow { item, reply, .. }) => {
                    self.play_now(item);
                    let _ = reply.send(Ok(Reply::Status(Box::new(self.status()))));
                }
                Ok(ToEngine::PlaylistAddScanned {
                    name, items, reply, ..
                }) => {
                    // 纯播放列表编辑：动的是"文件"，不碰正在播放的队列
                    let (added, skipped) =
                        super::playlist::append(&mut self.playlists, &name, items);
                    self.dirty = true;
                    let _ = reply.send(Ok(Reply::Added { added, skipped }));
                }
                Ok(ToEngine::TrackEof { generation }) => {
                    if self
                        .decoder
                        .as_ref()
                        .is_some_and(|d| d.generation == generation)
                    {
                        self.decoder = None;
                        self.fatal_streak = 0; // 正常播完一首，重置连续失败计数
                        self.on_eof();
                    }
                }
                Ok(ToEngine::TrackFatal { generation, error }) => {
                    if self
                        .decoder
                        .as_ref()
                        .is_some_and(|d| d.generation == generation)
                    {
                        self.decoder = None;
                        eprintln!("[rmusd] 跳过无法播放的曲子: {error}");
                        self.fatal_streak += 1;
                        let limit = self.queue.len().max(1);
                        if self.fatal_streak >= limit {
                            // 整个队列挨个试过都失败：停止（试听态则恢复），避免循环重试烧 CPU
                            eprintln!("[rmusd] 队列连续 {limit} 首无法播放，停止播放");
                            self.emit(Event::Error {
                                message: format!("无法播放，已停止：{error}"),
                            });
                            self.finish_or_restore();
                        } else {
                            self.emit(Event::Error {
                                message: format!("无法播放，已跳过：{error}"),
                            });
                            self.on_eof();
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if self.dirty {
                self.dirty = false;
                self.persist();
            }
        }
    }

    /// 永久删盘前的引擎清理：解析目标路径并校验在 `~/Music` 内，停播/切歌，
    /// 从当前队列、全部播放列表与撤销/重做栈里摘除。
    fn purge_path(
        &mut self,
        path: Option<std::path::PathBuf>,
    ) -> Result<Option<super::queue::QueueItem>, String> {
        let target = match path {
            Some(p) => p,
            None => match self.queue.current_item() {
                Some(item) => item.path.clone(),
                None => return Err("当前没有正在播放的曲子".into()),
            },
        };
        let lib_root = std::fs::canonicalize(crate::library::root())
            .map_err(|e| format!("无法访问音乐库目录：{e}"))?;
        let canon = std::fs::canonicalize(&target)
            .map_err(|_| format!("文件不存在：{}", target.display()))?;
        if !canon.starts_with(&lib_root) || !canon.is_file() {
            return Err("只能删除音乐库目录（~/Music）内的音频文件".into());
        }

        // 从当前队列移除所有命中项；若删到正在播的曲子，记下恢复位置
        let mut matched: Option<super::queue::QueueItem> = None;
        let mut removed_current = false;
        let mut resume_at: Option<usize> = None;
        let mut idx = 0;
        while idx < self.queue.items.len() {
            let same = self.queue.items[idx].path == target
                || std::fs::canonicalize(&self.queue.items[idx].path)
                    .is_ok_and(|c| c == canon);
            if same {
                if matched.is_none() {
                    matched = Some(self.queue.items[idx].clone());
                }
                if self.queue.remove(idx) {
                    removed_current = true;
                    resume_at = Some(idx);
                }
            } else {
                idx += 1;
            }
        }

        // 从所有保存的播放列表摘除
        let mut playlists_changed = false;
        for items in self.playlists.values_mut() {
            let before = items.len();
            items.retain(|it| {
                if it.path == target {
                    if matched.is_none() {
                        matched = Some(it.clone());
                    }
                    false
                } else {
                    true
                }
            });
            if items.len() != before {
                playlists_changed = true;
            }
        }

        self.purge_history_path(&target);

        if matched.is_some() || playlists_changed {
            self.dirty = true;
            self.emit(Event::QueueChanged);
        }

        if removed_current {
            if self.queue.items.is_empty() {
                self.finish_or_restore();
            } else if self.state == PlayState::Stopped {
                let at = resume_at
                    .unwrap_or(0)
                    .min(self.queue.items.len().saturating_sub(1));
                self.queue.current = Some(at);
            } else {
                // 先杀旧解码器并无缝切到删除点的下一首（确保文件句柄彻底释放）
                let at = resume_at
                    .unwrap_or(0)
                    .min(self.queue.items.len().saturating_sub(1));
                self.start_track(at, 0, self.state == PlayState::Playing);
            }
        }

        Ok(matched)
    }
}
