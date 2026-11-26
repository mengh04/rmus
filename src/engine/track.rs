//! 起一条解码线程：重置输出流、生成代号、把写端交给解码线程。

use super::queue::QueueItem;
use super::{DecoderHandle, Engine, decoder};
use crate::proto::{Event, PlayState};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Engine {
    /// 插播：插到当前曲后立即播放；空队列/未开始时作为第一曲。
    /// 播完由 on_eof 正常继续队列，不打断原有队列结构。
    pub(super) fn play_now(&mut self, item: QueueItem) {
        self.push_history();
        let at = match self.queue.current {
            Some(cur) => cur + 1,
            None => 0,
        };
        let at = at.min(self.queue.items.len());
        self.queue.items.insert(at, item);
        self.dirty = true;
        self.emit(Event::QueueChanged);
        self.start_track(at, 0, true);
    }

    /// 上报当前曲目的听歌结束事件（完播 / 切歌 / 停止），每曲至多上报一次；
    /// 播放不足 2 秒且非 EOF 的忽略（如连续快速按 n 翻歌）。
    pub(super) fn emit_track_finished(&mut self, eof: bool) {
        if self.track_recorded || self.state == PlayState::Stopped {
            return;
        }
        let Some(item) = self.queue.current_item().cloned() else {
            return;
        };
        let played_ms = if eof && item.duration_ms > 0 {
            self.position_ms().max(item.duration_ms)
        } else {
            self.position_ms()
        };
        self.track_recorded = true;
        if !eof && played_ms < 2_000 {
            return;
        }
        self.emit(Event::TrackFinished {
            path: item.path,
            title: item.title,
            artist: item.artist,
            played_ms,
            duration_ms: item.duration_ms,
            eof,
        });
    }

    /// 核心：从 index 曲的 position_ms 处开始播放。
    /// 杀旧解码线程 -> 重建输出流 -> 起新解码线程。
    pub(super) fn start_track(&mut self, index: usize, position_ms: u64, autoplay: bool) {
        // 切到另一首时记录上一首的实际播放时长（同曲 seek 不上报）
        if self.queue.current != Some(index) {
            self.emit_track_finished(false);
        }
        self.kill_decoder();

        let Ok(()) = self.output.reset() else {
            eprintln!("[rmusd] 重置输出流失败");
            return;
        };

        self.queue.current = Some(index);
        self.seek_base_ms = position_ms;
        self.track_recorded = false;
        self.dirty = true;

        // kill_decoder 已把计数 +1 作废旧线程；这里再 +1 作为新线程的代号
        let new_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        // 写端交给解码线程独占（回调消费、解码生产）
        let Some(producer) = self.output.take_producer() else {
            eprintln!("[rmusd] 获取输出写端失败");
            return;
        };

        let path = self.queue.items[index].path.clone();
        let device_rate = self.output.sample_rate();
        let device_channels = self.output.channels();
        let handle = decoder::spawn(
            path,
            position_ms,
            Arc::clone(&self.generation),
            new_gen,
            producer,
            device_rate,
            device_channels,
            self.to_engine_tx.clone(),
        );
        self.decoder = Some(DecoderHandle {
            generation: new_gen,
            handle,
        });

        if autoplay {
            self.state = PlayState::Playing;
            self.output.play();
        } else {
            // 保持 Paused：解码线程仍会预填 ring，但流不 play
            self.state = PlayState::Paused;
        }
        self.emit(Event::SongChanged { index: Some(index) });
        self.emit(Event::StateChanged { state: self.state });
    }

    pub(super) fn kill_decoder(&mut self) {
        if let Some(d) = self.decoder.take() {
            self.generation.fetch_add(1, Ordering::SeqCst);
            let _ = d.handle.join();
        }
    }
}
