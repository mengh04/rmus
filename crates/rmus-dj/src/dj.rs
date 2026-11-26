//! 一轮推荐的主流程：信号采集 → prompt → LLM 提名 → 逐首验证下载 →
//! 入库 → 落档（画像/墓碑/历史）。
//!
//! 生成器-过滤器：LLM 只提名（title/artist/reason），下载对象永远是
//! 搜索源验证过的真实结果（[`rmus_sources::download::title_artist_matches`]
//! 双向匹配），幻觉提名到不了磁盘。

use anyhow::{Result, bail};
use rmus_plugin_sdk::{daemon, library_dir, notify, now_ms, cancelled};
use rmus_sources::{self, SearchResult, download::title_artist_matches, split_artists};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::config::Config;
use crate::llm;
use crate::store::{self, RoundRecord, SongOutcome, Tombstone};

/// 进 prompt 的墓碑数量上限（多了 token 爆，够最近的就行）。
const TOMB_IN_PROMPT: usize = 40;
/// 轮次互斥锁的陈旧判定：超过此时长的残留锁视为死锁（实例被杀没能清理）。
const LOCK_STALE_MS: u64 = 2 * 3_600_000;

/// 轮次互斥锁：手动轮（CLI/UI）和 worker 自动轮可能同时触发，
/// 共享档案文件（墓碑/历史/画像）是读改写，重叠会丢更新。
/// 锁是 `dj/lock` 文件，内容为起始毫秒；结束删除。返回 Err 表示
/// 另一轮正在进行（错误文案固定，worker 靠它识别静默退避）。
struct RoundLock;

pub const LOCK_BUSY_TEXT: &str = "另一轮推荐正在进行";

impl RoundLock {
    fn acquire() -> Result<Self> {
        use rmus_plugin_sdk::fs;
        let dir = crate::config::dj_dir();
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
        let dir = crate::config::dj_dir();
        rmus_plugin_sdk::fs::remove(&format!("{dir}/lock"));
    }
}
/// 曲库概况的字符预算（画像信号够用即可，留 token 给提名）。
const SUMMARY_CAP: usize = 1800;
/// 曲库概况收录的歌手上限。
const ARTIST_TOP: usize = 40;

const SYSTEM: &str = "你是资深音乐 DJ，擅长从用户的曲库习惯里找规律、发现相邻口味。\
你的输出会被程序直接解析，必须只含一个 JSON 对象，不要围栏、不要多余文字。";

/// 跑一轮。返回 `{"added":[…],"skipped":[…],"failed":[…],…}`。
/// 拿不到轮次锁（另一轮在进行）时返回错误，文案含 [`LOCK_BUSY_TEXT`]。
pub fn run(cfg: &Config) -> Result<serde_json::Value> {
    let _lock = RoundLock::acquire()?;

    // ---- 信号采集：曲库（已有歌用于去重，概况用于画像）----
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
    let lib_keys: HashSet<String> = tracks
        .iter()
        .map(|(t, a)| store::dedup_key(t, a))
        .collect();
    let summary = library_summary(&tracks);

    // ---- prompt：概况 + 画像 + 墓碑 + 拉黑 + 任务 ----
    let mut tombs = store::tombstones_load();
    let mut tomb_keys: HashSet<String> = tombs.iter().map(Tombstone::key).collect();
    let data_root = PathBuf::from(rmus_plugin_sdk::data_dir());
    let blocked = rmus_plugin_sdk::blocklist::load_entries(&data_root);
    for b in &blocked {
        tomb_keys.insert(rmus_plugin_sdk::blocklist::dedup_key(&b.title, &b.artist));
    }
    let mut avoid: Vec<Tombstone> = tombs.clone();
    for b in blocked {
        avoid.push(Tombstone {
            title: b.title,
            artist: b.artist,
            reason: "用户删除拉黑".into(),
            at: b.at,
        });
    }
    let profile = store::profile_read();
    let user = build_prompt(&summary, &profile, &avoid, cfg.count);

    // ---- LLM 提名 ----
    notify(&json!({ "phase": "llm", "model": cfg.model }));
    let content = llm::chat(cfg, SYSTEM, &user, 0.8)?;
    let value = llm::extract_json(&content)
        .ok_or_else(|| anyhow::anyhow!("LLM 输出里抠不出 JSON：{}", content.chars().take(200).collect::<String>()))?;
    let songs = value
        .get("songs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if songs.is_empty() {
        bail!("LLM 没有给出提名（检查模型是否理解了输出格式）");
    }

    // ---- 逐首：验证 → 下载 ----
    let dest_dir = PathBuf::from(library_dir());
    if dest_dir.as_os_str().is_empty() {
        bail!("缺少库目录（rmusd 未注入 library_dir）");
    }
    let mut added: Vec<SongOutcome> = Vec::new();
    let mut skipped: Vec<SongOutcome> = Vec::new();
    let mut failed: Vec<SongOutcome> = Vec::new();
    let mut was_cancelled = false;

    for song in songs.iter().take(cfg.count) {
        if cancelled() {
            was_cancelled = true;
            break;
        }
        let title = song["title"].as_str().unwrap_or("").trim().to_string();
        let artist = song["artist"].as_str().unwrap_or("").trim().to_string();
        if title.is_empty() {
            continue;
        }
        let key = store::dedup_key(&title, &artist);

        if lib_keys.contains(&key) {
            skipped.push(SongOutcome {
                title,
                artist,
                path: None,
                reason: "曲库已有".into(),
            });
            continue;
        }
        if tomb_keys.contains(&key) {
            skipped.push(SongOutcome {
                title,
                artist,
                path: None,
                reason: "此前失败（墓碑）".into(),
            });
            continue;
        }

        // 反幻觉闸门：提名必须在搜索源里找到双向匹配的实体
        notify(&json!({ "phase": "search", "title": title, "artist": artist }));
        let (hits, _notes) = rmus_sources::search_all(&format!("{title} {artist}"), cfg.flac_only);
        let verified: Vec<&SearchResult> = hits
            .iter()
            .filter(|h| {
                title_artist_matches(&h.title, h.artist.as_deref().unwrap_or(""), &title, &artist)
            })
            .collect();
        let pick: Option<&SearchResult> = if cfg.flac_only {
            verified
                .iter()
                .find(|h| flac_ok(h))
                .or_else(|| verified.first())
                .copied()
        } else {
            verified.first().copied()
        };
        let Some(pick) = pick else {
            let reason = "搜索源无双向匹配（提名可能不存在）".to_string();
            failed.push(SongOutcome {
                title: title.clone(),
                artist: artist.clone(),
                path: None,
                reason: reason.clone(),
            });
            store::tombstone_push(
                &mut tombs,
                Tombstone {
                    title: title.clone(),
                    artist: artist.clone(),
                    reason,
                    at: now_ms(),
                },
            );
            continue;
        };

        // 下载（共享管线自带品质/探针/元数据门禁）
        notify(&json!({ "phase": "download", "title": title, "artist": artist }));
        match rmus_sources::download(pick, &dest_dir, cfg.flac_only) {
            Ok(v) => {
                let path = v["path"].as_str().map(str::to_string);
                added.push(SongOutcome {
                    title: pick.title.clone(),
                    artist: pick.artist.clone().unwrap_or_default(),
                    path,
                    reason: song["reason"].as_str().unwrap_or("").to_string(),
                });
            }
            Err(e) => {
                let reason = format!("{e:#}");
                failed.push(SongOutcome {
                    title,
                    artist,
                    path: None,
                    reason: head(&reason, 120),
                });
                store::tombstone_push(
                    &mut tombs,
                    Tombstone {
                        title: pick.title.clone(),
                        artist: pick.artist.clone().unwrap_or_default(),
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
        let paths: Vec<String> = added
            .iter()
            .filter_map(|s| s.path.clone())
            .collect();
        daemon::playlist_add(&cfg.playlist, paths)
            .map_err(|e| anyhow::anyhow!("文件已下载但加入列表「{}」失败：{e}", cfg.playlist))?;
    }

    // ---- 落档：画像重写 + 墓碑 + 历史 ----
    let mut profile_updated = false;
    if let Some(new_profile) = value.get("profile").and_then(|v| v.as_str()) {
        let text = new_profile.trim();
        if !text.is_empty() && text != profile.trim() {
            store::profile_write(text)?;
            profile_updated = true;
        }
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
        "profile_updated": profile_updated,
        "summary": format!(
            "入库 {} · 跳过 {} · 失败 {}{} → 列表「{}」",
            added.len(),
            skipped.len(),
            failed.len(),
            if was_cancelled { "（本轮已取消，后续提名未处理）" } else { "" },
            cfg.playlist,
        ),
    }))
}

/// 酷我源必须标注 flac（或没标注——老行为里 None 放行给探针把关）；
/// 酷狗源内部用无损专属接口，天生合格。
fn flac_ok(h: &SearchResult) -> bool {
    h.source != "kuwo" || h.format.as_deref().is_none_or(|f| f.eq_ignore_ascii_case("flac"))
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 曲库概况：歌手频次 top + 每人代表作 ≤3。这是画像的主信号。
fn library_summary(tracks: &[(String, String)]) -> String {
    let mut artist_count: HashMap<&str, usize> = HashMap::new();
    let mut by_artist: HashMap<&str, Vec<&str>> = HashMap::new();
    for (title, artist) in tracks {
        if title.is_empty() {
            continue;
        }
        for name in split_artists(artist) {
            *artist_count.entry(name).or_insert(0) += 1;
            let titles = by_artist.entry(name).or_default();
            if titles.len() < 3 && !titles.contains(&title.as_str()) {
                titles.push(title.as_str());
            }
        }
    }
    let mut top: Vec<_> = artist_count.into_iter().collect();
    top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let mut out = String::from("（曲库还是空的——凭常识提名大众口碑曲）\n");
    for (name, n) in top.into_iter().take(ARTIST_TOP) {
        let samples = by_artist.get(name).map(|v| v.join(" / ")).unwrap_or_default();
        let line = format!("- {name}（{n} 首）：{samples}\n");
        if out.len() + line.len() > SUMMARY_CAP {
            out.push_str("…（更多从略）\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

fn build_prompt(summary: &str, profile: &str, tombs: &[Tombstone], count: usize) -> String {
    let mut s = String::from("# 我的曲库概况（按歌手频次）\n");
    s.push_str(summary);
    if profile.trim().is_empty() {
        s.push_str("\n# 口味画像\n（还没有——请在本轮输出里首次总结）\n");
    } else {
        s.push_str("\n# 我的口味画像（你之前总结的）\n");
        s.push_str(profile.trim());
        s.push('\n');
    }
    let recent: Vec<_> = tombs.iter().rev().take(TOMB_IN_PROMPT).collect();
    if !recent.is_empty() {
        s.push_str("\n# 别再提名（此前验证或下载失败）\n");
        for t in recent {
            s.push_str(&format!("- {} - {}\n", t.title, t.artist));
        }
    }
    s.push_str(&format!(
        "\n# 任务\n基于我的口味提名 {count} 首新歌。要求：\n\
1. 不要提名曲库已有的歌，也不要提名失败名单里的\n\
2. 翻唱要写原唱者；组合歌手写主要艺人\n\
3. 只输出 JSON：\n\
{{\"profile\":\"更新后的口味画像，中文 200 字内\",\"songs\":[{{\"title\":\"\",\"artist\":\"\",\"reason\":\"一句话\"}}]}}\n"
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_ranks_by_frequency_with_samples() {
        let tracks = vec![
            ("晴天".to_string(), "周杰伦".to_string()),
            ("七里香".to_string(), "周杰伦".to_string()),
            ("稻香".to_string(), "周杰伦".to_string()),
            ("青花瓷".to_string(), "周杰伦".to_string()),
            ("泡沫".to_string(), "G.E.M.邓紫棋".to_string()),
            ("光年之外".to_string(), "G.E.M.邓紫棋 & 村西".to_string()),
        ];
        let s = library_summary(&tracks);
        let zhou = s.lines().find(|l| l.contains("周杰伦")).unwrap();
        assert!(zhou.contains("4 首"));
        assert!(zhou.contains("晴天"));
        // 第三首代表作封顶
        assert!(!zhou.contains("青花瓷"));
        // 合唱里村西也计了一首
        let cunxi = s.lines().find(|l| l.contains("村西")).unwrap();
        assert!(cunxi.contains("1 首"));
    }

    #[test]
    fn empty_library_gets_hint_line() {
        let s = library_summary(&[]);
        assert!(s.contains("曲库还是空的"));
    }

    #[test]
    fn prompt_carries_sections() {
        let tombs = vec![Tombstone {
            title: "不存在的歌".to_string(),
            artist: "张三".to_string(),
            reason: "x".to_string(),
            at: 1,
        }];
        let p = build_prompt("概况A", "画像B", &tombs, 5);
        assert!(p.contains("概况A"));
        assert!(p.contains("画像B"));
        assert!(p.contains("不存在的歌 - 张三"));
        assert!(p.contains("提名 5 首"));
    }
}
