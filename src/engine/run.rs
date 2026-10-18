//! 引擎事件循环：命令、解码线程回报、位置推送、按需落盘。

use super::queue::QueueItem;
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
                    // 当前队列是命名队列：存档同步追加，与 Remove/Move/Undo
                    // 的同步行为保持一致（失步会让 RemoveRange 越界）
                    if let Some(name) = self.queue_name.clone()
                        && let Some(archived) = self.named.get_mut(&name)
                    {
                        archived.extend(items.iter().cloned());
                    }
                    self.queue.items.extend(items);
                    self.dirty = true;
                    self.emit(Event::QueueChanged);
                    let _ = reply.send(Ok(Reply::Added { added }));
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
                    self.queue_name = None;
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
                    let added = items.len();
                    // 加的就是当前队列才可撤销（改别的命名队列不在撤销范围）
                    if self.queue_name.as_deref() == Some(name.as_str()) && !items.is_empty() {
                        self.push_history();
                    }
                    self.named.entry(name.clone()).or_default().extend(items);
                    if self.queue_name.as_deref() == Some(name.as_str()) {
                        // 命名队列就是当前队列：同步追加，播放体验连续
                        let extra: Vec<QueueItem> = self
                            .named
                            .get(&name)
                            .map(|items| items[items.len() - added..].to_vec())
                            .unwrap_or_default();
                        self.queue.items.extend(extra);
                        self.emit(Event::QueueChanged);
                    }
                    self.dirty = true;
                    let _ = reply.send(Ok(Reply::Added { added }));
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
                            self.finish_or_restore();
                        } else {
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
}
