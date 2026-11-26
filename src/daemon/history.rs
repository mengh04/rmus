//! 本地听歌行为账本（`<data_dir>/history.jsonl`）：完播 / 秒切统计 + 时间衰减画像。
//!
//! 不依赖队列，不调用大模型：
//! - 完播（EOF 或听了 ≥75% / ≥120s）→ 正反馈 `+2.0`
//! - 前 25 秒内秒切（且 <25% 时长）→ 负反馈 `-2.0`
//! - 按 30 天半衰期衰减，算出长期「心头好种子曲目」与「歌手亲和度」。

use rmus_sources::split_artists;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 历史账本读取的尾部上限（足够覆盖数月听歌记录且启动零开销）。
const MAX_LINES: usize = 1000;
/// 时间衰减半衰期（毫秒）：30 天。
const HALF_LIFE_MS: f64 = 30.0 * 24.0 * 3600.0 * 1000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenRecord {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub played_ms: u64,
    pub duration_ms: u64,
    pub completed: bool,
    pub skipped: bool,
    pub at: u64,
}

/// 从本地听歌账本提炼的长期口味画像。
#[derive(Debug, Default)]
pub struct TasteProfile {
    /// 按加权分降序的心头好种子：`(title, artist, score)`（只留正向得分）
    pub top_tracks: Vec<(String, String, f64)>,
    /// 小写歌手名 → 亲和分（正 = 爱听，负 = 经常秒切）
    pub artist_affinity: HashMap<String, f64>,
}

fn history_path(data_dir: &Path) -> PathBuf {
    data_dir.join("history.jsonl")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 判定一次播放属于完播还是秒切。
pub fn classify(played_ms: u64, duration_ms: u64, eof: bool) -> (bool, bool) {
    let completed = eof
        || (duration_ms > 0 && played_ms * 100 >= duration_ms * 75)
        || played_ms >= 120_000;
    let skipped = !completed
        && (2_000..=25_000).contains(&played_ms)
        && (duration_ms == 0 || played_ms * 100 < duration_ms * 25);
    (completed, skipped)
}

/// 追加一条听歌记录到 `<data_dir>/history.jsonl`。
pub fn record(
    data_dir: &Path,
    path: PathBuf,
    title: String,
    artist: String,
    played_ms: u64,
    duration_ms: u64,
    eof: bool,
) {
    if title.trim().is_empty() {
        return;
    }
    let (completed, skipped) = classify(played_ms, duration_ms, eof);
    if !completed && !skipped {
        return;
    }
    let entry = ListenRecord {
        path,
        title,
        artist,
        played_ms,
        duration_ms,
        completed,
        skipped,
        at: now_ms(),
    };
    let file_path = history_path(data_dir);
    if let Some(dir) = file_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(line) = serde_json::to_string(&entry)
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// 从最近听歌账本构建长期口味画像（带 30 天时间衰减）。
pub fn taste_profile(data_dir: &Path) -> TasteProfile {
    let Ok(text) = std::fs::read_to_string(history_path(data_dir)) else {
        return TasteProfile::default();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    let records: Vec<ListenRecord> = lines[start..]
        .iter()
        .filter_map(|l| serde_json::from_str::<ListenRecord>(l).ok())
        .collect();
    build_profile(&records, now_ms())
}

pub(crate) fn build_profile(records: &[ListenRecord], now: u64) -> TasteProfile {
    let mut track_scores: HashMap<String, (String, String, f64)> = HashMap::new();
    let mut artist_affinity: HashMap<String, f64> = HashMap::new();

    for rec in records {
        let age = now.saturating_sub(rec.at) as f64;
        let decay = 0.5_f64.powf(age / HALF_LIFE_MS);
        let delta = if rec.completed {
            2.0 * decay
        } else if rec.skipped {
            -2.0 * decay
        } else {
            0.0
        };
        if delta == 0.0 {
            continue;
        }
        let key = rmus_plugin_sdk::blocklist::dedup_key(&rec.title, &rec.artist);
        track_scores
            .entry(key)
            .and_modify(|e| e.2 += delta)
            .or_insert_with(|| (rec.title.clone(), rec.artist.clone(), delta));

        for a in split_artists(&rec.artist) {
            let low = a.to_lowercase();
            if !low.is_empty() {
                *artist_affinity.entry(low).or_insert(0.0) += delta;
            }
        }
    }

    let mut top_tracks: Vec<(String, String, f64)> = track_scores
        .into_values()
        .filter(|(_, _, s)| *s > 0.5)
        .collect();
    top_tracks.sort_by(|a, b| b.2.total_cmp(&a.2));

    TasteProfile {
        top_tracks,
        artist_affinity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_boosts_and_quick_skip_penalizes() {
        let now = 1_700_000_000_000;
        let records = vec![
            ListenRecord {
                path: "/m/a.flac".into(),
                title: "时间中央".into(),
                artist: "郁可唯".into(),
                played_ms: 230_000,
                duration_ms: 234_000,
                completed: true,
                skipped: false,
                at: now - 10_000,
            },
            ListenRecord {
                path: "/m/a.flac".into(),
                title: "时间中央".into(),
                artist: "郁可唯".into(),
                played_ms: 234_000,
                duration_ms: 234_000,
                completed: true,
                skipped: false,
                at: now - 5_000,
            },
            ListenRecord {
                path: "/m/b.flac".into(),
                title: "难听的歌".into(),
                artist: "路人甲".into(),
                played_ms: 8_000,
                duration_ms: 200_000,
                completed: false,
                skipped: true,
                at: now - 1_000,
            },
        ];
        let profile = build_profile(&records, now);
        assert_eq!(profile.top_tracks.len(), 1);
        assert_eq!(profile.top_tracks[0].0, "时间中央");
        assert!(profile.artist_affinity["郁可唯"] > 3.5);
        assert!(profile.artist_affinity["路人甲"] < -1.5);
    }
}
