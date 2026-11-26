//! fm 轮次：免费 API 候选 → 反幻觉验证 → 共享下载管线 → 入列表。
//!
//! 与 rmus-dj 同骨架（验证/下载/墓碑/历史复用同一套模式），区别在
//! 候选来源：Deezer 全球榜单 + 曲库高频歌手热门曲，零配置零密钥。
//! 排序偏好口味：歌手热门（库内歌手）优先，榜单作为探索补充。

use anyhow::{Result, bail};
use rmus_plugin_sdk::{cancelled, daemon, library_dir, notify, now_ms};
use rmus_sources::{self, SearchResult, download::title_artist_matches, split_artists};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::config::Config;
use crate::deezer::{self, Candidate};
use crate::store::{self, RoundRecord, SongOutcome, Tombstone};

/// 轮次互斥锁的陈旧判定：超过此时长的残留锁视为死锁。
const LOCK_STALE_MS: u64 = 2 * 3_600_000;
/// 每位歌手最多取的热门曲数（防止单歌手刷满一轮）。
const PER_ARTIST_TAKE: usize = 3;

pub const LOCK_BUSY_TEXT: &str = "另一轮推荐正在进行";

struct RoundLock;

impl RoundLock {
    fn acquire() -> Result<Self> {
        use rmus_plugin_sdk::fs;
        let dir = crate::config::fm_dir();
        let path = format!("{dir}/lock");
        if fs::exists(&path) {
            let started = crate::config::read_text(&path)
                .and_then(|t| t.trim().parse::<u64>().ok())
                .unwrap_or(0);
            if now_ms().saturating_sub(started) < LOCK_STALE_MS {
                bail!(LOCK_BUSY_TEXT);
            }
        }
        crate::config::write_text(&path, &now_ms().to_string())?;
        Ok(RoundLock)
    }
}

impl Drop for RoundLock {
    fn drop(&mut self) {
        let dir = crate::config::fm_dir();
        rmus_plugin_sdk::fs::remove(&format!("{dir}/lock"));
    }
}

fn flac_ok(h: &SearchResult) -> bool {
    h.source != "kuwo" || h.format.as_deref().is_none_or(|f| f == "flac")
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 纯函数：两路候选合并成至多 `count` 条（歌手热门优先，榜单补位）。
/// 去重（池内 + 曲库 + 墓碑）；榜单候选若歌手在库内也享受口味优先。
pub(crate) fn assemble(
    mut taste: Vec<Candidate>,
    chart: Vec<Candidate>,
    lib_keys: &HashSet<String>,
    tomb_keys: &HashSet<String>,
    lib_artists: &HashSet<String>,
    count: usize,
) -> Vec<Candidate> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Candidate> = Vec::new();

    let push = |c: Candidate,
                    out: &mut Vec<Candidate>,
                    seen: &mut HashSet<String>| {
        let key = store::dedup_key(&c.title, &c.artist);
        if seen.contains(&key)
            || lib_keys.contains(&key)
            || tomb_keys.contains(&key)
        {
            return;
        }
        seen.insert(key);
        out.push(c);
    };

    // 口味池：歌手热门 + 榜单里歌手在库内的曲目，按现有顺序（热度序）
    taste.extend(chart.iter().filter(|c| {
        store::artist_set(&c.artist).iter().any(|a| lib_artists.contains(a))
    }).cloned());
    for c in taste.drain(..) {
        if out.len() >= count {
            break;
        }
        push(c, &mut out, &mut seen);
    }
    // 探索补位：其余榜单曲
    for c in chart {
        if out.len() >= count {
            break;
        }
        push(c, &mut out, &mut seen);
    }
    out
}

/// 跑一轮。返回 `{"added":[…],"skipped":[…],"failed":[…],…}`。
pub fn run(cfg: &Config) -> Result<serde_json::Value> {
    let _lock = RoundLock::acquire()?;

    // ---- 曲库信号：去重键 + 歌手频次 ----
    notify(&json!({ "phase": "library" }));
    let lib = daemon::lib_list().map_err(|e| anyhow::anyhow!("读取曲库失败：{e}"))?;
    let tracks: Vec<(String, String)> = lib["tracks"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| {
                    (
                        t["title"].as_str().unwrap_or("").trim().to_string(),
                        t["artist"].as_str().unwrap_or("").trim().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let lib_keys: HashSet<String> =
        tracks.iter().map(|(t, a)| store::dedup_key(t, a)).collect();
    let mut artist_freq: HashMap<String, usize> = HashMap::new();
    for (_, artist) in &tracks {
        for a in split_artists(artist) {
            if !a.is_empty() {
                *artist_freq.entry(a.to_lowercase()).or_default() += 1;
            }
        }
    }
    let lib_artists: HashSet<String> = artist_freq.keys().cloned().collect();
    let mut top_artists: Vec<(String, usize)> = artist_freq.into_iter().collect();
    top_artists.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // ---- 候选采集 ----
    let mut tombs = store::tombstones_load();
    let mut tomb_keys: HashSet<String> = tombs.iter().map(Tombstone::key).collect();
    let data_root = PathBuf::from(rmus_plugin_sdk::data_dir());
    tomb_keys.extend(rmus_plugin_sdk::blocklist::load_keys(&data_root));

    let mut taste: Vec<Candidate> = Vec::new();
    let lfm = crate::lastfm::load_config(&data_root);
    if cfg.artist_top {
        notify(&json!({ "phase": "artist", "depth": cfg.artist_depth }));
        for (artist, freq) in top_artists.iter().take(cfg.artist_depth) {
            if cancelled() {
                break;
            }
            // 频次 1 的歌手信号太弱，跳过
            if *freq < 2 {
                break;
            }
            // Last.fm 相似歌手热门（华语/外语跨歌手协同过滤）
            if let Some(ref lfm) = lfm
                && let Ok(sim_artists) = crate::lastfm::artist_similar(&lfm.api_key, artist, 3)
            {
                for (sim_name, _) in sim_artists.into_iter().take(2) {
                    if let Ok(mut tops) =
                        crate::lastfm::artist_top_tracks(&lfm.api_key, &sim_name, 2)
                    {
                        taste.append(&mut tops);
                    }
                }
            }
            let Some(id) = deezer::search_artist_id(artist)? else {
                continue; // Deezer 没收录（中文小众歌手常见），换下一位
            };
            let mut tops = deezer::artist_top(id, PER_ARTIST_TAKE)?;
            taste.append(&mut tops);
        }
    }
    let chart = if cfg.chart {
        notify(&json!({ "phase": "chart" }));
        deezer::chart_tracks(50)?
    } else {
        Vec::new()
    };
    if !cfg.chart && !cfg.artist_top {
        bail!("chart 与 artist_top 不能同时关闭");
    }
    let candidates =
        assemble(taste, chart, &lib_keys, &tomb_keys, &lib_artists, cfg.count);
    if candidates.is_empty() {
        bail!("没有可用候选（曲库/墓碑已覆盖或 API 无返回）");
    }

    // ---- 逐首：验证 → 下载 ----
    let dest_dir = PathBuf::from(library_dir());
    if dest_dir.as_os_str().is_empty() {
        bail!("缺少库目录（rmusd 未注入 library_dir）");
    }
    let mut added: Vec<SongOutcome> = Vec::new();
    let skipped: Vec<SongOutcome> = Vec::new();
    let mut failed: Vec<SongOutcome> = Vec::new();
    let mut was_cancelled = false;

    for cand in &candidates {
        if cancelled() {
            was_cancelled = true;
            break;
        }
        let title = cand.title.clone();
        let artist = cand.artist.clone();
        let from = match cand.from {
            "chart" => "全球榜单",
            "lastfm" => "Last.fm 相似",
            _ => "歌手热门",
        };

        // 候选也须过双向匹配闸门（榜单数据不作免检特权）
        notify(&json!({ "phase": "search", "title": title, "artist": artist }));
        let (hits, _notes) =
            rmus_sources::search_all(&format!("{title} {artist}"), cfg.flac_only);
        let verified: Vec<&SearchResult> = hits
            .iter()
            .filter(|h| {
                title_artist_matches(
                    &h.title,
                    h.artist.as_deref().unwrap_or(""),
                    &title,
                    &artist,
                )
            })
            .collect();
        let pick: Option<&SearchResult> = if cfg.flac_only {
            verified.iter().find(|h| flac_ok(h)).or_else(|| verified.first()).copied()
        } else {
            verified.first().copied()
        };
        let Some(pick) = pick else {
            let reason = "搜索源无双向匹配（候选不可达）".to_string();
            failed.push(SongOutcome {
                title: title.clone(),
                artist: artist.clone(),
                path: None,
                reason: reason.clone(),
            });
            store::tombstone_push(
                &mut tombs,
                Tombstone { title, artist, reason, at: now_ms() },
            );
            continue;
        };

        notify(&json!({ "phase": "download", "title": title, "artist": artist }));
        match rmus_sources::download(pick, &dest_dir, cfg.flac_only) {
            Ok(v) => {
                let path = v["path"].as_str().map(str::to_string);
                added.push(SongOutcome {
                    title: pick.title.clone(),
                    artist: pick.artist.clone().unwrap_or_default(),
                    path,
                    reason: from.to_string(),
                });
            }
            Err(e) => {
                let reason = format!("{e:#}");
                failed.push(SongOutcome {
                    title: title.clone(),
                    artist: artist.clone(),
                    path: None,
                    reason: head(&reason, 120),
                });
                store::tombstone_push(
                    &mut tombs,
                    Tombstone {
                        title,
                        artist,
                        reason: head(&reason, 200),
                        at: now_ms(),
                    },
                );
            }
        }
    }

    // ---- 入库：一次 playlist_add（引擎不存在即建）----
    if !added.is_empty() {
        notify(&json!({ "phase": "playlist", "count": added.len(), "playlist": cfg.playlist }));
        let paths: Vec<String> = added.iter().filter_map(|s| s.path.clone()).collect();
        daemon::playlist_add(&cfg.playlist, paths).map_err(|e| {
            anyhow::anyhow!("文件已下载但加入列表「{}」失败：{e}", cfg.playlist)
        })?;
    }

    store::tombstones_save(&tombs)?;
    store::history_push(RoundRecord {
        at: now_ms(),
        added: added.clone(),
        skipped: skipped.clone(),
        failed: failed.clone(),
        cancelled: was_cancelled,
    })?;

    Ok(json!({
        "added": added,
        "skipped": skipped,
        "failed": failed,
        "cancelled": was_cancelled,
        "summary": format!(
            "入库 {} · 跳过 {} · 失败 {}{} → 列表「{}」",
            added.len(),
            skipped.len(),
            failed.len(),
            if was_cancelled { "（已取消）" } else { "" },
            cfg.playlist,
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(title: &str, artist: &str, from: &'static str) -> Candidate {
        Candidate { title: title.into(), artist: artist.into(), from }
    }

    #[test]
    fn assembles_taste_first_then_chart_explore() {
        let lib_keys: HashSet<String> =
            [store::dedup_key("消愁", "毛不易")].into_iter().collect();
        let tomb_keys: HashSet<String> =
            [store::dedup_key("模特", "李荣浩")].into_iter().collect();
        let lib_artists: HashSet<String> = ["薛之谦".to_string()].into_iter().collect();

        let taste = vec![cand("演员", "薛之谦", "artist")];
        let chart = vec![
            cand("消愁", "毛不易", "chart"),  // 库内 → 跳过
            cand("纸船", "薛之谦&郁可唯", "chart"), // 榜单但歌手在库 → 口味优先
            cand("Flowers", "Miley Cyrus", "chart"), // 探索
        ];
        let out = assemble(taste, chart, &lib_keys, &tomb_keys, &lib_artists, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "演员"); // 歌手热门先入
        assert_eq!(out[1].title, "纸船"); // 榜单库内歌手次之
    }

    #[test]
    fn dedups_within_pool() {
        let empty: HashSet<String> = HashSet::new();
        let taste = vec![cand("Lemon", "米津玄師", "artist")];
        let chart = vec![cand("Lemon", "米津玄師", "chart")];
        let out = assemble(taste, chart, &empty, &empty, &empty, 5);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn respects_count_cap() {
        let empty: HashSet<String> = HashSet::new();
        let taste: Vec<Candidate> = (0..10)
            .map(|i| cand(&format!("t{i}"), "歌手A", "artist"))
            .collect();
        let out = assemble(taste, vec![], &empty, &empty, &empty, 3);
        assert_eq!(out.len(), 3);
    }
}
