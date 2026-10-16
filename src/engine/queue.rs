//! 播放队列。引擎内部状态，不含 UI 概念。

use crate::proto::TrackInfo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

#[derive(Debug, Default)]
pub struct Queue {
    pub items: Vec<QueueItem>,
    /// 当前曲下标；stop 后保留（方便"继续"），clear 后为 None
    pub current: Option<usize>,
}

impl Queue {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn current_item(&self) -> Option<&QueueItem> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn track_info(&self, index: usize) -> Option<TrackInfo> {
        self.items.get(index).map(|item| TrackInfo {
            index,
            path: item.path.clone(),
            title: item.title.clone(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            duration_ms: item.duration_ms,
        })
    }

    /// 前进一格下标（不做越界回绕——循环/随机语义由引擎决定）。
    /// 返回 None 表示越过队尾。
    pub fn next_index(&self) -> Option<usize> {
        let next = self.current.map_or(0, |c| c + 1);
        if next < self.items.len() {
            Some(next)
        } else {
            None
        }
    }

    /// 后退一格下标；队首再退停在 0。返回 None 表示队列为空。
    pub fn prev_index(&self) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        Some(self.current.map_or(0, |c| c.saturating_sub(1)))
    }

    /// 打乱队列，当前曲（若有）移到队首并成为第 0 曲。
    pub fn shuffle(&mut self) {
        if self.items.len() <= 1 {
            return;
        }
        let keep = self.current;
        if let Some(cur) = keep {
            let item = self.items.remove(cur);
            self.items.insert(0, item);
        }
        // 洗牌除当前曲以外的部分
        let n = self.items.len();
        let mut rng = simple_rng();
        for i in (1..n).rev() {
            let j = 1 + (next_rand(&mut rng) as usize) % i;
            self.items.swap(i, j);
        }
        self.current = keep.map(|_| 0);
    }

    /// 删除下标 index 的曲。返回 true 若当前曲被删（调用方负责停止播放）。
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        let removed_current = self.current == Some(index);
        self.items.remove(index);
        match self.current {
            Some(c) if c == index => self.current = None,
            Some(c) if c > index => self.current = Some(c - 1),
            _ => {}
        }
        removed_current
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
    }
}

/// 轻量 xorshift PRNG，避免仅为 shuffle 引入 rand 依赖。
pub fn simple_rng() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    seed | 1
}

pub fn next_rand(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}
