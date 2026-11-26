//! dj 的持久状态：口味画像（profile.md）+ 提名墓碑（tombstones.json）+
//! 每轮历史（history.json）。全在 `<data_dir>/dj/` 下。
//!
//! - 画像：LLM 每轮顺手重写的 markdown，下轮原样进 prompt；
//! - 墓碑：验证/下载失败的提名记下来，下轮 prompt 里带上"别再提名"，
//!   反幻觉的第二道闸（第一道是搜索源验证）；
//! - 历史：每轮的完整结果，播放闭环（阶段 D）的数据源。

use anyhow::Result;
use rmus_sources::split_artists;
use serde::{Deserialize, Serialize};

use crate::config::{dj_dir, read_text, write_text};

const PROFILE: &str = "profile.md";
const TOMBSTONES: &str = "tombstones.json";
const HISTORY: &str = "history.json";

/// 墓碑/去重用的规范键：标题+歌手去空白、统一小写。歌手串先按分隔符
/// 拆开再排序拼接——"周杰伦&费玉清" 与 "费玉清、周杰伦" 是同一组合。
pub fn dedup_key(title: &str, artist: &str) -> String {
    let mut names: Vec<String> = split_artists(artist)
        .map(|n| n.to_lowercase().replace(' ', ""))
        .collect();
    names.sort();
    format!(
        "{}|{}",
        title.to_lowercase().replace(' ', ""),
        names.join("&")
    )
}

// ---- 画像 ----

pub fn profile_read() -> String {
    let dir = dj_dir();
    read_text(&format!("{dir}/{PROFILE}")).unwrap_or_default()
}

pub fn profile_write(text: &str) -> Result<()> {
    let dir = dj_dir();
    write_text(&format!("{dir}/{PROFILE}"), text)
}

// ---- 墓碑 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub title: String,
    pub artist: String,
    /// 失败原因（搜索源验证失败 / 下载失败的错误头）
    pub reason: String,
    pub at: u64,
}

impl Tombstone {
    pub fn key(&self) -> String {
        dedup_key(&self.title, &self.artist)
    }
}

/// 墓碑条目上限：多了 prompt 会爆；足够老的失败也值得再试一次。
const TOMBSTONE_CAP: usize = 200;

pub fn tombstones_load() -> Vec<Tombstone> {
    let dir = dj_dir();
    read_text(&format!("{dir}/{TOMBSTONES}"))
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn tombstones_save(list: &[Tombstone]) -> Result<()> {
    let dir = dj_dir();
    write_text(
        &format!("{dir}/{TOMBSTONES}"),
        &serde_json::to_string_pretty(list)?,
    )
}

/// 追加新墓碑（去重后旧的挪到末尾），裁到上限。
pub fn tombstone_push(list: &mut Vec<Tombstone>, item: Tombstone) {
    let key = item.key();
    list.retain(|t| t.key() != key);
    list.push(item);
    if list.len() > TOMBSTONE_CAP {
        let drop = list.len() - TOMBSTONE_CAP;
        list.drain(0..drop);
    }
}

// ---- 历史 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongOutcome {
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    pub at: u64,
    pub added: Vec<SongOutcome>,
    pub skipped: Vec<SongOutcome>,
    pub failed: Vec<SongOutcome>,
    #[serde(default)]
    pub cancelled: bool,
}

const HISTORY_CAP: usize = 100;

/// worker 调度状态（state.json）：只有 next_due_ms，UI/CLI 读它显示
/// "下次自动轮"，worker 写它。读不到就当没排程。
pub fn worker_state_read() -> Option<u64> {
    let dir = dj_dir();
    let v: serde_json::Value =
        serde_json::from_str(&read_text(&format!("{dir}/state.json"))?).ok()?;
    v.get("next_due_ms").and_then(|x| x.as_u64())
}

pub fn worker_state_write(next_due_ms: u64) -> Result<()> {
    let dir = dj_dir();
    write_text(
        &format!("{dir}/state.json"),
        &serde_json::to_string(&serde_json::json!({ "next_due_ms": next_due_ms }))?,
    )
}

pub fn history_load() -> Vec<RoundRecord> {
    let dir = dj_dir();
    read_text(&format!("{dir}/{HISTORY}"))
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn history_push(record: RoundRecord) -> Result<()> {
    let mut list = history_load();
    list.push(record);
    if list.len() > HISTORY_CAP {
        let drop = list.len() - HISTORY_CAP;
        list.drain(0..drop);
    }
    let dir = dj_dir();
    write_text(
        &format!("{dir}/{HISTORY}"),
        &serde_json::to_string_pretty(&list)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_key_normalizes_artist_order_and_case() {
        assert_eq!(
            dedup_key("晴天", "周杰伦&费玉清"),
            dedup_key(" 晴天 ", "费玉清、周杰伦")
        );
        assert_ne!(dedup_key("晴天", "周杰伦"), dedup_key("晴天", "费玉清"));
        // 空歌手 vs 有歌手：不同键（同名曲不同版本是不同歌）
        assert!(!dedup_key("晴天", "").is_empty());
    }

    #[test]
    fn tombstone_push_dedup_and_cap() {
        let mut list: Vec<Tombstone> = (0..200)
            .map(|i| Tombstone {
                title: format!("t{i}"),
                artist: format!("a{i}"),
                reason: "x".into(),
                at: i as u64,
            })
            .collect();
        tombstone_push(
            &mut list,
            Tombstone {
                title: "t199".into(),
                artist: "a199".into(),
                reason: "again".into(),
                at: 999,
            },
        );
        assert_eq!(list.len(), 200);
        assert_eq!(list.last().unwrap().at, 999);
        assert_eq!(list.iter().filter(|t| t.title == "t199").count(), 1);

        tombstone_push(
            &mut list,
            Tombstone {
                title: "new".into(),
                artist: "a".into(),
                reason: "x".into(),
                at: 1000,
            },
        );
        assert_eq!(list.len(), 200); // 触发裁剪，最老的 t0 被挤掉
        assert_eq!(list.first().unwrap().title, "t1");
    }
}
