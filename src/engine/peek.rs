//! 试听态：进入时存档当前播放状态，结束/播完时原样恢复。

use super::Engine;
use super::queue::{Queue, QueueItem};
use crate::proto::{Event, PlayState, RepeatMode};

pub(super) struct PeekSnapshot {
    queue: Queue,
    queue_origin: Option<String>,
    /// 存档时的播放进度（毫秒）。必须在换队列（输出流 reset 清零帧计数）前取
    position_ms: u64,
    /// 恢复后回到的状态：Playing 继续播，Paused 停在原进度
    state: PlayState,
    repeat: RepeatMode,
    random: bool,
}

impl Engine {
    /// 播放自然结束：试听态下恢复原队列，否则停止
    pub(super) fn finish_or_restore(&mut self) {
        if self.peek.is_some() {
            self.restore();
        } else {
            self.stop();
        }
    }

    /// 进入试听：首次调用时存档当前状态，然后换成临时队列从头播放。
    /// 已在试听态时只换临时队列（原存档保持不变）。
    pub(super) fn enter_peek(&mut self, items: Vec<QueueItem>) {
        if self.peek.is_none() {
            // position_ms 必须在换队列前取：start_track 的 reset 会清零帧计数
            let position_ms = self.position_ms();
            let snapshot = PeekSnapshot {
                queue: std::mem::take(&mut self.queue),
                queue_origin: self.queue_origin.take(),
                position_ms,
                state: self.state,
                repeat: self.repeat,
                random: self.random,
            };
            self.peek = Some(snapshot);
        }
        self.kill_decoder();
        self.queue.items = items;
        self.queue.current = None;
        self.queue_origin = None;
        // 临时队列固定顺序播放，循环/随机语义留给恢复后的原队列
        self.repeat = RepeatMode::Off;
        self.random = false;
        self.dirty = true;
        self.emit(Event::QueueChanged);
        self.emit(Event::ModeChanged {
            repeat: self.repeat,
            random: self.random,
        });
        self.start_track(0, 0, true);
    }

    /// 结束试听：恢复存档的队列、模式与进度。
    pub(super) fn restore(&mut self) {
        let Some(snap) = self.peek.take() else {
            return;
        };
        let current = snap.queue.current;
        self.kill_decoder();
        self.queue = snap.queue;
        self.queue_origin = snap.queue_origin;
        self.repeat = snap.repeat;
        self.random = snap.random;
        self.fatal_streak = 0;
        self.dirty = true;
        self.emit(Event::QueueChanged);
        self.emit(Event::ModeChanged {
            repeat: self.repeat,
            random: self.random,
        });
        match current {
            Some(i) if i < self.queue.items.len() => {
                // 播放态继续播；暂停态解码预填但不播放，进度停在原处
                self.start_track(i, snap.position_ms, snap.state == PlayState::Playing);
            }
            _ => {
                self.state = PlayState::Stopped;
                self.seek_base_ms = 0;
                self.emit(Event::StateChanged {
                    state: PlayState::Stopped,
                });
                self.emit(Event::SongChanged { index: None });
            }
        }
    }
}
