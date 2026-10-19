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
                if !self.queue.is_empty() {
                    self.push_history();
                }
                self.kill_decoder();
                self.queue.clear();
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
                self.push_history();
                let removed_current = self.queue.remove(index);
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
            // dd / dj / dk / dG / dgg 的整段删除：一次请求记一次撤销
            Request::RemoveRange { start, end } => {
                let len = self.queue.len();
                if start >= end || end > len {
                    return Err(format!("范围 {start}..{end} 超出队列（0..{len}）"));
                }
                self.push_history();
                let removed_current =
                    self.queue.current.is_some_and(|c| c >= start && c < end);
                // 从高下标往低删，前面的下标不失效
                for i in (start..end).rev() {
                    self.queue.remove(i);
                }
                self.dirty = true;
                self.emit(Event::QueueChanged);
                if removed_current {
                    if self.queue.items.is_empty() {
                        self.finish_or_restore();
                    } else if self.state == PlayState::Stopped {
                        self.queue.current = Some(start.min(self.queue.items.len() - 1));
                    } else {
                        let at = start.min(self.queue.items.len() - 1);
                        self.start_track(at, 0, true);
                    }
                }
                Ok(Reply::Ok)
            }
            Request::Shuffle => {
                if self.queue.len() > 1 {
                    self.push_history();
                }
                self.queue.shuffle();
                self.dirty = true;
                self.emit(Event::QueueChanged);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::Move { from, to } => {
                let len = self.queue.len();
                if from >= len || to >= len {
                    return Err(format!("下标超出队列范围（0..{len}）"));
                }
                self.push_history();
                self.queue.move_item(from, to)?;
                self.dirty = true;
                self.emit(Event::QueueChanged);
                Ok(Reply::Ok)
            }
            Request::PlaylistsList => {
                let names = self.playlists.keys().cloned().collect();
                Ok(Reply::Playlists {
                    names,
                    current: self.queue_name.clone(),
                })
            }
            Request::PlaylistNew { name } => {
                if self.playlists.contains_key(&name) {
                    return Err(format!("播放列表 {name} 已存在"));
                }
                self.playlists.insert(name, Vec::new());
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistSave { name } => {
                self.playlists.insert(name.clone(), self.queue.items.clone());
                self.queue_name = Some(name);
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistPlay { name } => {
                let Some(items) = self.playlists.get(&name) else {
                    return Err(format!("播放列表 {name} 不存在"));
                };
                if items.is_empty() {
                    return Err(format!("播放列表 {name} 是空的"));
                }
                let items = items.clone();
                // 显式换队列 = 放弃试听恢复点
                self.peek = None;
                self.kill_decoder();
                self.queue.items = items;
                self.queue.current = None;
                // 复制进队列，之后各播各的：queue_name 只是来源标签
                self.queue_name = Some(name);
                self.dirty = true;
                self.emit(Event::QueueChanged);
                self.start_track(0, 0, true);
                Ok(Reply::Status(Box::new(self.status())))
            }
            Request::PlaylistRemove { name } => {
                if self.playlists.remove(&name).is_none() {
                    return Err(format!("播放列表 {name} 不存在"));
                }
                self.dirty = true;
                Ok(Reply::Ok)
            }
            Request::PlaylistRename { from, to } => {
                if from == to {
                    return Ok(Reply::Ok);
                }
                if !self.playlists.contains_key(&from) {
                    return Err(format!("播放列表 {from} 不存在"));
                }
                if self.playlists.contains_key(&to) {
                    return Err(format!("播放列表 {to} 已存在"));
                }
                let items = self.playlists.remove(&from).unwrap_or_default();
                self.playlists.insert(to.clone(), items);
                // 正在播的队列来自这张：来源标签跟随
                if self.queue_name.as_deref() == Some(from.as_str()) {
                    self.queue_name = Some(to);
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
            Request::Undo => match self.undo() {
                Ok(()) => Ok(Reply::Status(Box::new(self.status()))),
                Err(e) => Err(e),
            },
            Request::Redo => match self.redo() {
                Ok(()) => Ok(Reply::Status(Box::new(self.status()))),
                Err(e) => Err(e),
            },
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
            // 插件请求都在 daemon 层（plugin_host）处理，进不来这里
            Request::PluginList | Request::PluginCall { .. } | Request::PluginCancel { .. }
            | Request::PluginKey { .. } | Request::PluginViewReq { .. }
            | Request::PluginData { .. } => {
                Err("内部错误：插件请求未在 daemon 层处理".into())
            }
            // Find/PlayMatch/LibList/LibRescan 由 daemon 层处理（查库/扫描），不应到达这里
            Request::Find { .. } | Request::PlayMatch { .. } | Request::LibList
            | Request::LibRescan => {
                Err("内部错误：库请求未在 daemon 层处理".into())
            }
        }
    }
}
