//! 队列改动的撤销 / 重做：改动前拍快照，`u` 回滚、`C-r` 前滚。
//!
//! 范围是队列内容本身（增删、排序、清空、打乱、插播）——vim 里 `u`
//! 撤销的是文本改动；切换队列（Open / PlaylistPlay）更像换 buffer，
//! 不进撤销栈。快照只存内存（daemon 会话内有效），上限 100 条。
//!
//! 恢复语义：正在播的曲子若还在恢复后的队列里就不打断（只修正下标）；
//!
//! 不在了（比如撤销的是"删掉正在播的这首"）则从恢复点的曲目重头播，
//! 保持播放 / 暂停态。

use super::queue::QueueItem;
use super::Engine;
use crate::proto::{Event, PlayState};

/// 撤销栈深度上限。快照只是曲目元数据，100 条绰绰有余。
const LIMIT: usize = 100;

/// 一次改动前的队列快照。
///
/// 只存队列内容与当前下标：队列的"出处标签"（`queue_origin`）不是队列内容，
/// 撤销删曲不能把"我从哪张列表来的"一起回滚——那是列表身份，不是曲目
#[derive(Clone)]
pub(super) struct Snapshot {
    items: Vec<QueueItem>,
    current: Option<usize>,
}

impl Engine {
    /// 改动前调用：当前队列压入撤销栈，清空重做栈（vim 语义：
    /// 有了新改动，旧的 redo 分支作废）。
    pub(super) fn push_history(&mut self) {
        self.push_undo();
        self.redo.clear();
    }

    /// 只压撤销栈（不清重做栈）。redo 回滚时用它。
    fn push_undo(&mut self) {
        let snap = self.take_snapshot();
        self.undo.push_back(snap);
        if self.undo.len() > LIMIT {
            self.undo.pop_front();
        }
    }

    /// u：回滚到上一次改动前。
    pub(super) fn undo(&mut self) -> Result<(), String> {
        if self.peek.is_some() {
            return Err("试听中不可撤销（B 结束试听）".into());
        }
        let Some(snap) = self.undo.pop_back() else {
            return Err("没有可撤销的队列改动".into());
        };
        self.redo.push_back(self.take_snapshot());
        self.apply_snapshot(snap);
        Ok(())
    }

    /// C-r：重做被撤销的改动。
    pub(super) fn redo(&mut self) -> Result<(), String> {
        if self.peek.is_some() {
            return Err("试听中不可重做（B 结束试听）".into());
        }
        let Some(snap) = self.redo.pop_back() else {
            return Err("没有可重做的队列改动".into());
        };
        self.push_undo();
        self.apply_snapshot(snap);
        Ok(())
    }

    fn take_snapshot(&self) -> Snapshot {
        Snapshot {
            items: self.queue.items.clone(),
            current: self.queue.current,
        }
    }

    /// 恢复快照并广播。正在播的曲子不在恢复后的
    /// 队列里时，从恢复点曲目重头播（不打断时保持原进度）。
    fn apply_snapshot(&mut self, snap: Snapshot) {
        let playing_path = self.queue.current_item().map(|i| i.path.clone());
        self.queue.items = snap.items;
        self.queue.current = snap.current;
        self.dirty = true;
        self.emit(Event::QueueChanged);
        let same_track = self
            .queue
            .current
            .zip(playing_path.as_ref())
            .is_some_and(|(i, p)| self.queue.items.get(i).is_some_and(|it| &it.path == p));
        if let Some(i) = self.queue.current
            && self.state != PlayState::Stopped
            && !same_track
        {
            self.start_track(i, 0, self.state == PlayState::Playing);
        }
    }
}
