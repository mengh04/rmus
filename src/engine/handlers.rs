//! 请求分发：协议请求 -> 引擎内部操作。

use super::Engine;
use crate::proto::{Event, PlayState, RepeatMode, Reply, Request};
use anyhow::Result;

impl Engine {
    pub(super) fn handle(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Status => Ok(Reply::Status(Box::new(self.status()))),
            Request::QueueList => {
                let tracks = (0..self.queue.len())
                    .filter_map(|i| self.queue.track_info(i))
                    .collect();
                Ok(Reply::Queue { tracks })
            }
            Request::Play { index } => {
                self.play(index);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Pause => {
                self.pause();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::TogglePlay => {
                match self.state {
                    PlayState::Playing => self.pause(),
                    _ => self.play(None),
                }
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Stop => {
                self.stop();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Next => {
                self.on_next();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Prev => {
                self.on_prev();
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Seek { position_ms } => {
                self.seek(position_ms);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::SeekRelative { delta_ms } => {
                let current = self.position_ms() as i64;
                let target = (current + delta_ms).max(0) as u64;
                self.seek(target);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::SetVolume { value } => {
                self.set_volume(f64::from(value));
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::AdjustVolume { delta } => {
                let v = (self.output.volume() + f64::from(delta)).clamp(0.0, 1.0);
                self.set_volume(v);
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::Clear => {
                self.kill_decoder();
                self.queue.clear();
                // 当前是命名队列时同步清空它
                if let Some(name) = self.queue_name.clone() {
                    self.named.insert(name, Vec::new());
                }
                self.state = PlayState::Stopped;
                self.seek_base_ms = 0;
                self.dirty = true;
                self.emit(Event::QueueChanged);
                self.emit(Event::StateChanged {
                    state: PlayState::Stopped,
                });
                self.emit(Event::SongChanged { index: None });
                Ok(Reply::Ok)
            }
            Request::Remove { index } => {
                let index = match index {
                    Some(i) => i,
                    None => match self.queue.current {
                        Some(c) => c,
                        None => return Err("当前没有正在播放的曲子".into()),
                    },
                };
                if index >= self.queue.len() {
                    return Err(format!("下标 {index} 超出队列范围"));
                }
                let removed_current = self.queue.remove(index);
                if let Some(name) = self.queue_name.clone()
                    && let Some(items) = self.named.get_mut(&name)
                    && index < items.len()
                {
                    items.remove(index);
                }
                self.dirty = true;
                self.emit(Event::QueueChanged);
                if removed_current {
                    if self.queue.items.is_empty() {
                        // 删空了：试听态恢复原队列，否则停止
                        self.finish_or_restore();
                    } else if self.state == PlayState::Stopped {
                        // 停止态整理队列：不自动开播，current 指向删除点
                        self.queue.current = Some(index.min(self.queue.items.len() - 1));
                    } else {
                        // 续播删除点的下一首（删的是队尾则退到新的最后一首）
                        let at = index.min(self.queue.items.len() - 1);
                        self.start_track(at, 0, true);
                    }
                }
                Ok(Reply::Ok)
            }
            Request::Shuffle => {
                self.queue.shuffle();
                if let Some(name) = self.queue_name.clone()
                    && let Some(items) = self.named.get_mut(&name)
                {
                    *items = self.queue.items.clone();
                }
                self.dirty = true;
                self.emit(Event::QueueChanged);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::PlaylistsList => {
                let names = self.named.keys().cloned().collect();
                Ok(Reply::Playlists {
                    names,
                    current: self.queue_name.clone(),
                })
            }
            Request::PlaylistNew { name } => {
                if self.named.contains_key(&name) {
                    return Err(format!("队列 {name} 已存在"));
                }
                self.named.insert(name, Vec::new());
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistSave { name } => {
                self.named.insert(name.clone(), self.queue.items.clone());
                self.queue_name = Some(name);
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistPlay { name } => {
                let Some(items) = self.named.get(&name) else {
                    return Err(format!("队列 {name} 不存在"));
                };
                if items.is_empty() {
                    return Err(format!("队列 {name} 是空的"));
                }
                let items = items.clone();
                // 显式换队列 = 放弃试听恢复点
                self.peek = None;
                self.kill_decoder();
                self.queue.items = items;
                self.queue.current = None;
                self.queue_name = Some(name);
                self.dirty = true;
                self.emit(Event::QueueChanged);
                self.start_track(0, 0, true);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::PlaylistRemove { name } => {
                if self.queue_name.as_deref() == Some(name.as_str()) {
                    return Err(format!("队列 {name} 正在播放，无法删除"));
                }
                if self.named.remove(&name).is_none() {
                    return Err(format!("队列 {name} 不存在"));
                }
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::SetRepeat { mode } => {
                self.repeat = mode.unwrap_or(match self.repeat {
                    RepeatMode::Off => RepeatMode::All,
                    RepeatMode::All => RepeatMode::One,
                    RepeatMode::One => RepeatMode::Off,
                });
                self.dirty = true;
                self.emit(Event::ModeChanged {
                    repeat: self.repeat,
                    random: self.random,
                });
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::SetRandom { on } => {
                self.random = on.unwrap_or(!self.random);
                self.dirty = true;
                self.emit(Event::ModeChanged {
                    repeat: self.repeat,
                    random: self.random,
                });
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Back => match self.peek.as_ref() {
                Some(_) => {
                    self.restore();
                    Ok(Reply::Status(Box::new(self.status())))
                }
                None => Err("当前没有试听在等待恢复".into()),
            },
            // Add/Open/PlaylistAdd/Peek 由 daemon 层拦截扫描，不应到达这里
            Request::Add { .. } => Err("内部错误：Add 未在 daemon 层处理".into()),
            Request::Open { .. } => Err("内部错误：Open 未在 daemon 层处理".into()),
            Request::Peek { .. } => Err("内部错误：Peek 未在 daemon 层处理".into()),
            Request::PlaylistAdd { .. } => Err("内部错误：PlaylistAdd 未在 daemon 层处理".into()),
            // Quit 由 daemon 层处理，不应到达这里
            Request::Quit => Err("内部错误：Quit 未在 daemon 层处理".into()),
            // Find/PlayMatch/LibRescan 由 daemon 层处理（查库/扫描），不应到达这里
            Request::Find { .. } | Request::PlayMatch { .. } | Request::LibRescan => {
                Err("内部错误：库请求未在 daemon 层处理".into())
            }
        }
    }
}
