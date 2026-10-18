//! 播放控制：播放/暂停/停止/切歌/跳转/音量。
//!
//! 切歌本身（起解码线程）在 [`super::track`]，这里只决定"接下来放哪首"。

use super::Engine;
use super::queue;
use crate::proto::{Event, PlayState, RepeatMode};

impl Engine {
    /// 播放。Some(i) 从第 i 曲开始；None = 恢复暂停或从头（Stopped）。
    pub(super) fn play(&mut self, index: Option<usize>) {
        match self.state {
            PlayState::Paused if index.is_none() => {
                self.output.play();
                self.state = PlayState::Playing;
                self.emit(Event::StateChanged {
                    state: PlayState::Playing,
                });
            }
            _ => {
                let index = index.or(self.queue.current).unwrap_or(0);
                if index < self.queue.len() {
                    self.start_track(index, 0, true);
                }
            }
        }
    }

    pub(super) fn pause(&mut self) {
        if self.state == PlayState::Playing {
            self.output.begin_pause();
            self.state = PlayState::Paused;
            self.emit(Event::StateChanged {
                state: PlayState::Paused,
            });
        }
    }

    pub(super) fn stop(&mut self) {
        self.kill_decoder();
        self.output.reset().ok();
        self.seek_base_ms = 0;
        self.state = PlayState::Stopped;
        self.emit(Event::StateChanged {
            state: PlayState::Stopped,
        });
    }

    /// 下一曲（EOF 自动推进）。单曲循环时重播当前曲；否则按模式选曲。
    pub(super) fn on_eof(&mut self) {
        if self.repeat == RepeatMode::One && self.queue.current.is_some() {
            let cur = self.queue.current.expect("已检查 Some");
            self.start_track(cur, 0, true);
            return;
        }
        // EOF 推进：只有 repeat all 才在队尾回绕
        let wrap = self.repeat == RepeatMode::All;
        match self.pick_next(wrap) {
            Some(i) => self.start_track(i, 0, true),
            None => self.finish_or_restore(),
        }
    }

    /// 用户手动 next：单曲循环不拦（主动切歌），队尾总是回绕。
    pub(super) fn on_next(&mut self) {
        match self.pick_next(true) {
            Some(i) => self.start_track(i, 0, true),
            None => self.stop(),
        }
    }

    /// 用户手动 prev：顺序后退（随机模式下也是），队首停在第 0 曲。
    pub(super) fn on_prev(&mut self) {
        match self.queue.prev_index() {
            Some(i) => self.start_track(i, 0, true),
            None => self.stop(),
        }
    }

    /// 选下一曲下标。随机开启时随机挑（避开当前曲）；否则顺序，
    /// 越过队尾时按 `wrap` 决定回绕到 0 还是 None（停止）。
    pub(super) fn pick_next(&self, wrap: bool) -> Option<usize> {
        let len = self.queue.items.len();
        if len == 0 {
            return None;
        }
        if self.random && len > 1 {
            let cur = self.queue.current;
            let mut rng = queue::simple_rng();
            loop {
                let i = (queue::next_rand(&mut rng) as usize) % len;
                if Some(i) != cur {
                    return Some(i);
                }
            }
        }
        match self.queue.next_index() {
            Some(i) => Some(i),
            None if wrap => Some(0),
            None => None,
        }
    }

    pub(super) fn seek(&mut self, position_ms: u64) {
        let Some(index) = self.queue.current else {
            return;
        };
        if self.state == PlayState::Stopped {
            return;
        }
        let duration = self
            .queue
            .current_item()
            .map(|i| i.duration_ms)
            .unwrap_or(0);
        let position_ms = if duration > 0 {
            position_ms.min(duration.saturating_sub(1))
        } else {
            position_ms
        };
        let autoplay = self.state == PlayState::Playing;
        self.start_track(index, position_ms, autoplay);
    }

    pub(super) fn set_volume(&mut self, value: f64) {
        let value = value.clamp(0.0, 1.0);
        self.output.set_volume(value);
        self.emit(Event::VolumeChanged {
            value: value as f32,
        });
    }
}
