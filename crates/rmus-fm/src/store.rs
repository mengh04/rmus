//! fm 状态档案（`data_dir/fm/`）：墓碑 + 历史 + worker 排班。
//!
//! 与 rmus-dj 同构（独立目录互不干扰）：失败提名进墓碑避免反复重试，
//! 轮次历史供 UI/CLI 回看，state.json 存 worker 下次到期时间。

use anyhow::Result;
use rmus_sources::split_artists;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::config;

/// 去重键：小写化、拆歌手排序拼接，抗 “A & B”/“B & A” 漂移。
pub fn dedup_key(title: &str, artist: &str) -> String {
    let mut names: Vec<String> = split_artists(artist)
        .map(|a| a.to_lowercase())
        .collect();
    names.sort();
    names.dedup();
    format!("{}|{}", title.trim().to_lowercase(), names.join("&"))
}

/// 失败提名的墓碑：同曲不再提名（`reason` 供排查）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub title: String,
    pub artist: String,
    pub reason: String,
    pub at: u64,
}

impl Tombstone {
    pub fn key(&self) -> String {
        dedup_key(&self.title, &self.artist)
    }
}

/// 一首歌的轮内结局（成功带 path，跳过/失败带 reason）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongOutcome {
    pub title: String,
    pub artist: String,
    pub path: Option<String>,
    pub reason: String,
}

/// 一轮档案（history.json 里最多留 100 轮）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    pub at: u64,
    pub added: Vec<SongOutcome>,
    pub skipped: Vec<SongOutcome>,
    pub failed: Vec<SongOutcome>,
    pub cancelled: bool,
}

const TOMB_CAP: usize = 200;
const HISTORY_CAP: usize = 100;

pub fn tombstones_path() -> String {
    format!("{}/tombstones.json", config::fm_dir())
}

pub fn tombstones_load() -> Vec<Tombstone> {
    config::read_text(&tombstones_path())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 追加并落盘（去重 + 容量裁剪）。
pub fn tombstone_push(tombs: &mut Vec<Tombstone>, t: Tombstone) {
    let key = t.key();
    tombs.retain(|x| x.key() != key);
    tombs.push(t);
    let overflow = tombs.len().saturating_sub(TOMB_CAP);
    if overflow > 0 {
        tombs.drain(..overflow);
    }
}

pub fn tombstones_save(tombs: &[Tombstone]) -> Result<()> {
    config::write_text(&tombstones_path(), &serde_json::to_string_pretty(tombs)?)
}

pub fn history_path() -> String {
    format!("{}/history.json", config::fm_dir())
}

pub fn history_load() -> Vec<RoundRecord> {
    config::read_text(&history_path())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn history_push(rec: RoundRecord) -> Result<()> {
    let mut h = history_load();
    h.push(rec);
    let overflow = h.len().saturating_sub(HISTORY_CAP);
    if overflow > 0 {
        h.drain(..overflow);
    }
    config::write_text(&history_path(), &serde_json::to_string_pretty(&h)?)
}

/// worker 下次到期毫秒（UI 'a'/外部拉早排班也写这）。
pub fn worker_state_path() -> String {
    format!("{}/state.json", config::fm_dir())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerState {
    pub next_due_ms: u64,
}

pub fn worker_state_read() -> Option<u64> {
    config::read_text(&worker_state_path())
        .and_then(|t| serde_json::from_str::<WorkerState>(&t).ok())
        .map(|s| s.next_due_ms)
}

pub fn worker_state_write(next_due_ms: u64) -> Result<()> {
    config::write_text(
        &worker_state_path(),
        &serde_json::to_string(&WorkerState { next_due_ms })?,
    )
}

/// 榜单候选里属于库内歌手的加分用集合（按 dedup 粒度太细，这里按
/// 歌手名小写集合）。
pub fn artist_set(artist: &str) -> HashSet<String> {
    split_artists(artist)
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_key_is_order_insensitive() {
        assert_eq!(dedup_key("大鱼", "周深"), dedup_key("大鱼", "周深"));
        assert_eq!(dedup_key("Lemon", "米津玄師"), dedup_key("lemon ", "米津玄師"));
        // “A & B” 与 “B & A” 同键
        assert_eq!(dedup_key("t", "A & B"), dedup_key("t", "B & A"));
        // 标题不同则不同
        assert_ne!(dedup_key("消愁", "毛不易"), dedup_key("大鱼", "毛不易"));
    }
}
