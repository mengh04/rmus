//! 播中自动推荐（零 LLM、多源协同过滤 + 长期听歌行为画像）。
//!
//! 监听播放事件（`SongChanged` / `StateChanged` / `ModeChanged` / `TrackFinished`）：
//! - 每次曲目结束时将完播/秒切写入 `<data_dir>/history.jsonl`，持续累积真实口味画像
//! - 开启 `auto_recommend` 时以两种节奏静默插入推荐曲：
//!   - **队尾续播**：非列表循环下，当前曲后余量 ≤ 1 首时往队尾补曲（已是最后一首时
//!     先毫秒级插 1 首本地相似曲防断音）
//!   - **播中穿插**：长队列每播 3 首非推荐曲，自动在下一首（`cur + 1`）插 1 首推荐曲
//!
//! 推荐来源（全部零 LLM）：
//! 1. **Last.fm 协同过滤**（读取 `<data_dir>/lastfm.json`）：
//!    - 多粒种子（当前播放曲 + 长期高分心头好 / Last.fm 红心常听）调 `track.getSimilar`
//!      做单曲级跨种子交集召回
//!    - `artist.getSimilar` 找跨歌手相似艺人 → 拉 `artist.getTopTracks` + 酷我/酷狗无损直搜
//! 2. **Deezer 相似歌手 + 热门曲**（免 key 互补）
//! 3. **酷我 / 酷狗同歌手热门无损** + **Deezer 榜单兜底**
//! 4. **本地曲库智能关联兜底**（同歌手 + 同歌单共现 + 同专辑 + 听歌亲和分）
//!
//! 所有候选都会排除曲库已有、当前队列、近期播放、会话失败墓碑与永久拉黑名单 `blocked.json`。

use crate::Library;
use crate::history::{self, TasteProfile};
use rmus::engine::queue::{QueueItem, next_rand, simple_rng};
use rmus::engine::{RecommendContext, ToEngine};
use rmus::library::LibEntry;
use rmus::proto::{Event, PlayState, RepeatMode};
use rmus_fm::deezer::{self, Candidate};
use rmus_fm::lastfm;
use rmus_plugin_sdk::runtime::{self, HostContext};
use rmus_sources::download::title_artist_matches;
use rmus_sources::{SearchResult, split_artists};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use tokio::sync::{broadcast, oneshot};

/// 长队列播中穿插间隔：每播这么多首非推荐曲，在下一首插 1 首推荐曲。
const INTERLEAVE_EVERY: usize = 3;
/// 单次在线拉新最多尝试下载的候选数（防网络慢或无无损时拖太久）。
const ONLINE_MAX_TRIES: usize = 4;
/// 会话去重窗口容量。
const RECENT_CAP: usize = 80;

/// 会话级记忆（避免重复推荐 / 反复撞失败候选）。
#[derive(Default)]
struct Memory {
    recent_paths: VecDeque<PathBuf>,
    recent_keys: VecDeque<String>,
    failed_keys: HashSet<String>,
    last_inserted_artist: Option<String>,
    last_seed_path: Option<PathBuf>,
    tracks_since_insert: usize,
}

impl Memory {
    fn record_played(&mut self, item: &QueueItem) {
        self.push_recent(item.path.clone(), dedup_key(&item.title, &item.artist));
    }

    fn record_inserted(&mut self, item: &QueueItem) {
        self.push_recent(item.path.clone(), dedup_key(&item.title, &item.artist));
        if !item.artist.trim().is_empty() {
            self.last_inserted_artist = Some(item.artist.clone());
        }
    }

    fn push_recent(&mut self, path: PathBuf, key: String) {
        if !self.recent_paths.contains(&path) {
            if self.recent_paths.len() >= RECENT_CAP {
                self.recent_paths.pop_front();
            }
            self.recent_paths.push_back(path);
        }
        if !key.is_empty() && !self.recent_keys.contains(&key) {
            if self.recent_keys.len() >= RECENT_CAP {
                self.recent_keys.pop_front();
            }
            self.recent_keys.push_back(key);
        }
    }
}

/// 启动后台自动推荐与听歌行为记录任务（随 `rmusd` 常驻）。
pub(crate) fn spawn(
    cmd_tx: std_mpsc::Sender<ToEngine>,
    mut events_rx: broadcast::Receiver<Event>,
    library: Library,
) {
    let memory = Arc::new(Mutex::new(Memory::default()));
    let in_flight = Arc::new(AtomicBool::new(false));

    tokio::spawn(async move {
        loop {
            let ev = match events_rx.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if let Event::TrackFinished {
                path,
                title,
                artist,
                played_ms,
                duration_ms,
                eof,
            } = ev
            {
                let data_dir = rmus::plugin::data_root();
                tokio::task::spawn_blocking(move || {
                    history::record(&data_dir, path, title, artist, played_ms, duration_ms, eof);
                });
                continue;
            }
            let is_song_change = matches!(ev, Event::SongChanged { .. });
            let is_mode_or_play = matches!(
                ev,
                Event::StateChanged {
                    state: PlayState::Playing
                } | Event::ModeChanged {
                    auto_recommend: true,
                    ..
                }
            );
            if !is_song_change && !is_mode_or_play {
                continue;
            }
            maybe_recommend(&cmd_tx, &library, &memory, &in_flight, is_song_change).await;
        }
    });
}

async fn snapshot(cmd_tx: &std_mpsc::Sender<ToEngine>) -> Option<RecommendContext> {
    let (tx, rx) = oneshot::channel();
    cmd_tx.send(ToEngine::RecommendSnapshot { reply: tx }).ok()?;
    rx.await.ok()
}

async fn maybe_recommend(
    cmd_tx: &std_mpsc::Sender<ToEngine>,
    library: &Library,
    memory: &Arc<Mutex<Memory>>,
    in_flight: &Arc<AtomicBool>,
    is_song_change: bool,
) {
    let Some(snap) = snapshot(cmd_tx).await else {
        return;
    };
    if !snap.auto_recommend
        || snap.state != PlayState::Playing
        || snap.peeking
        || snap.repeat == RepeatMode::One
    {
        return;
    }
    let Some(cur) = snap.current else {
        return;
    };
    let Some(seed) = snap.queue.get(cur).cloned() else {
        return;
    };
    let remaining = snap.queue.len().saturating_sub(cur + 1);
    let next_is_recommended = snap.queue.get(cur + 1).is_some_and(|t| t.recommended);

    let plan: Option<(bool, bool)> = {
        let mut mem = memory.lock().unwrap();
        let is_new_track = mem.last_seed_path.as_ref() != Some(&seed.path);
        if is_new_track {
            mem.last_seed_path = Some(seed.path.clone());
            mem.record_played(&seed);
            if is_song_change {
                if seed.recommended {
                    mem.tracks_since_insert = 0;
                } else {
                    mem.tracks_since_insert += 1;
                }
            }
        }

        if remaining <= 1 && snap.repeat != RepeatMode::All {
            Some((false, remaining == 0))
        } else if remaining >= 2 && !next_is_recommended && mem.tracks_since_insert >= INTERLEAVE_EVERY
        {
            mem.tracks_since_insert = 0;
            Some((true, false))
        } else {
            None
        }
    };

    let Some((after_current, urgent_local_first)) = plan else {
        return;
    };

    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    if urgent_local_first {
        let local_item = {
            let lib = library.read().unwrap();
            let mem = memory.lock().unwrap();
            let profile = history::taste_profile(&rmus::plugin::data_root());
            pick_local(
                &seed,
                &snap.queue,
                cur,
                &snap.playlists,
                &lib,
                &mem.recent_paths,
                mem.last_inserted_artist.as_deref(),
                &profile.artist_affinity,
            )
        };
        if let Some(item) = local_item {
            memory.lock().unwrap().record_inserted(&item);
            let _ = cmd_tx.send(ToEngine::InsertAuto {
                items: vec![item],
                after_current: false,
            });
        }
    }

    let cmd_tx = cmd_tx.clone();
    let library = library.clone();
    let memory = memory.clone();
    let in_flight = in_flight.clone();
    tokio::task::spawn_blocking(move || {
        struct ResetFlag(Arc<AtomicBool>);
        impl Drop for ResetFlag {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = ResetFlag(in_flight);

        let lib_snapshot: Vec<LibEntry> = library.read().unwrap().clone();
        let (recent_paths, recent_keys, failed_keys, last_artist) = {
            let mem = memory.lock().unwrap();
            (
                mem.recent_paths.clone(),
                mem.recent_keys.clone(),
                mem.failed_keys.clone(),
                mem.last_inserted_artist.clone(),
            )
        };
        let profile = history::taste_profile(&rmus::plugin::data_root());

        let mut new_failed = Vec::new();
        let picked = pick_online(
            &seed,
            &snap.queue,
            &lib_snapshot,
            &profile,
            &recent_keys,
            &failed_keys,
            last_artist.as_deref(),
            &mut new_failed,
        )
        .or_else(|| {
            if urgent_local_first {
                return None;
            }
            pick_local(
                &seed,
                &snap.queue,
                cur,
                &snap.playlists,
                &lib_snapshot,
                &recent_paths,
                last_artist.as_deref(),
                &profile.artist_affinity,
            )
        });

        {
            let mut mem = memory.lock().unwrap();
            for k in new_failed {
                mem.failed_keys.insert(k);
            }
            if let Some(item) = &picked {
                mem.record_inserted(item);
            }
        }

        if let Some(item) = picked {
            println!(
                "[rmusd] 自动推荐插入{}：{} - {}",
                if after_current { "（下一首）" } else { "（队尾）" },
                item.title,
                item.artist
            );
            let _ = cmd_tx.send(ToEngine::InsertAuto {
                items: vec![item],
                after_current,
            });
        }
    });
}

/// 标准化去重键（与 `rmus_plugin_sdk::blocklist::dedup_key` 保持一致）。
pub(crate) fn dedup_key(title: &str, artist: &str) -> String {
    rmus_plugin_sdk::blocklist::dedup_key(title, artist)
}

/// 本地曲库相似推荐（毫秒级，零网络）：
/// 同歌手（含合唱）+ 同歌单共现 + 同专辑 + 近期歌手 + 长期听歌亲和分 + 多样性惩罚。
#[allow(clippy::too_many_arguments)]
pub(crate) fn pick_local(
    seed: &QueueItem,
    queue: &[QueueItem],
    cur: usize,
    playlists: &BTreeMap<String, Vec<QueueItem>>,
    library: &[LibEntry],
    recent_paths: &VecDeque<PathBuf>,
    last_inserted_artist: Option<&str>,
    artist_affinity: &HashMap<String, f64>,
) -> Option<QueueItem> {
    let in_queue: HashSet<&PathBuf> = queue.iter().map(|q| &q.path).collect();
    let recent: HashSet<&PathBuf> = recent_paths.iter().collect();

    let seed_artists: HashSet<String> = split_artists(&seed.artist)
        .map(|a| a.to_lowercase())
        .collect();
    let seed_album = seed.album.trim().to_lowercase();

    let context_artists: HashSet<String> = queue[cur.saturating_sub(3)..cur]
        .iter()
        .flat_map(|q| split_artists(&q.artist))
        .map(|a| a.to_lowercase())
        .collect();

    let mut co_occur: HashSet<&PathBuf> = HashSet::new();
    for list in playlists.values() {
        if list.iter().any(|it| it.path == seed.path) {
            for it in list {
                if it.path != seed.path {
                    co_occur.insert(&it.path);
                }
            }
        }
    }

    let last_artist_set: HashSet<String> = last_inserted_artist
        .map(|a| split_artists(a).map(|s| s.to_lowercase()).collect())
        .unwrap_or_default();

    let mut rng = simple_rng();
    let mut best: Option<(i64, &QueueItem)> = None;

    for entry in library {
        let cand = &entry.item;
        if in_queue.contains(&cand.path) || recent.contains(&cand.path) {
            continue;
        }
        let mut score: i64 = (next_rand(&mut rng) % 24) as i64;
        let cand_artists: HashSet<String> = split_artists(&cand.artist)
            .map(|a| a.to_lowercase())
            .collect();

        if !seed_artists.is_empty() && cand_artists.iter().any(|a| seed_artists.contains(a)) {
            score += 100;
        }
        if co_occur.contains(&cand.path) {
            score += 80;
        }
        if !seed_album.is_empty() && cand.album.trim().to_lowercase() == seed_album {
            score += 45;
        }
        if !context_artists.is_empty() && cand_artists.iter().any(|a| context_artists.contains(a)) {
            score += 30;
        }
        // 长期听歌亲和分（完播加分、频繁秒切扣分，限幅 [-40, +50]）
        let affinity: f64 = cand_artists
            .iter()
            .filter_map(|a| artist_affinity.get(a))
            .copied()
            .sum();
        score += (affinity * 8.0).clamp(-40.0, 50.0) as i64;

        if !last_artist_set.is_empty() && cand_artists.iter().any(|a| last_artist_set.contains(a)) {
            score -= 25;
        }

        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, cand));
        }
    }

    best.map(|(_, item)| {
        let mut item = item.clone();
        item.recommended = true;
        item
    })
}

/// 在线多源协同过滤发现（零 LLM）：
/// 1. Last.fm 单曲相似（当前曲 + 长期心头好种子交集加权）
/// 2. Last.fm 相似歌手（`artist.getSimilar` → 热门曲 + 酷我/酷狗无损直搜，华语跨歌手主力）
/// 3. Deezer 相似歌手 + 热门曲
/// 4. 酷我/酷狗同歌手直搜 → Deezer 全球榜单兜底
///
/// 命中后过双向匹配验证 + 无损下载管线，落盘 `~/Music` 并扫描成 `QueueItem`。
#[allow(clippy::too_many_arguments)]
fn pick_online(
    seed: &QueueItem,
    queue: &[QueueItem],
    library: &[LibEntry],
    profile: &TasteProfile,
    recent_keys: &VecDeque<String>,
    failed_keys: &HashSet<String>,
    last_inserted_artist: Option<&str>,
    new_failed: &mut Vec<String>,
) -> Option<QueueItem> {
    let lib_dir = rmus::library::root();
    if !lib_dir.is_dir() {
        return None;
    }
    let data_dir = rmus::plugin::data_root();

    struct CtxGuard;
    impl Drop for CtxGuard {
        fn drop(&mut self) {
            runtime::clear_context();
        }
    }
    runtime::install_context(HostContext {
        lib_dir: lib_dir.clone(),
        data_dir: data_dir.clone(),
        ..Default::default()
    });
    let _ctx_guard = CtxGuard;

    let mut excluded: HashSet<String> = library
        .iter()
        .map(|e| dedup_key(&e.item.title, &e.item.artist))
        .collect();
    for q in queue {
        excluded.insert(dedup_key(&q.title, &q.artist));
    }
    for k in recent_keys {
        excluded.insert(k.clone());
    }
    for k in failed_keys {
        excluded.insert(k.clone());
    }
    for k in rmus_plugin_sdk::blocklist::load_keys(&data_dir) {
        excluded.insert(k);
    }

    let mut rng = simple_rng();
    let primary_artist = split_artists(&seed.artist).next().unwrap_or("").trim();
    let last_artist_low = last_inserted_artist.map(|a| a.to_lowercase());

    // 带分数的候选池：同曲被多个种子共同命中时分数叠加（真交集加权）
    let mut scored_pool: HashMap<String, (Candidate, f64)> = HashMap::new();
    let mut add_cand = |title: String, artist: String, from: &'static str, weight: f64| {
        let key = dedup_key(&title, &artist);
        if excluded.contains(&key) {
            return;
        }
        let mut w = weight;
        // 长期亲和分修正（经常秒切的歌手降权，爱听的歌手小幅加权）
        let aff: f64 = split_artists(&artist)
            .filter_map(|a| profile.artist_affinity.get(&a.to_lowercase()))
            .copied()
            .sum();
        w += (aff * 0.08).clamp(-0.6, 0.4);
        if last_artist_low
            .as_deref()
            .is_some_and(|la| artist.to_lowercase().contains(la))
        {
            w -= 0.35;
        }
        scored_pool
            .entry(key)
            .and_modify(|(_, s)| *s += w)
            .or_insert_with(|| (Candidate { title, artist, from }, w));
    };

    // 跨歌手相似艺人名（供酷我/酷狗无损直搜，解决 Last.fm 繁体/缺曲名问题）
    let mut similar_artists: Vec<String> = Vec::new();

    // ---- 1. Last.fm 协同过滤（单曲相似 + 相似歌手 + 云端红心/常听种子）----
    if let Some(lfm) = lastfm::load_config(&data_dir) {
        // 准备多粒种子：当前曲（1.0）+ 本地长期心头好随机 1 首（0.75）
        let mut track_seeds: Vec<(String, String, f64)> = Vec::new();
        if !seed.title.trim().is_empty() && !primary_artist.is_empty() {
            track_seeds.push((seed.title.clone(), primary_artist.to_string(), 1.0));
        }
        if !profile.top_tracks.is_empty() {
            let top_n = profile.top_tracks.len().min(6);
            let idx = (next_rand(&mut rng) as usize) % top_n;
            let (t, a, _) = &profile.top_tracks[idx];
            if let Some(pa) = split_artists(a).next()
                && dedup_key(t, pa) != dedup_key(&seed.title, primary_artist)
            {
                track_seeds.push((t.clone(), pa.to_string(), 0.75));
            }
        } else if !lfm.username.trim().is_empty() {
            // 本地账本还没攒够时，直接借你 Last.fm 账号的红心/近 3 月常听当种子
            let cloud = lastfm::user_loved_tracks(&lfm.api_key, &lfm.username, 6)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| lastfm::user_top_tracks(&lfm.api_key, &lfm.username, 6).ok())
                .unwrap_or_default();
            if !cloud.is_empty() {
                let idx = (next_rand(&mut rng) as usize) % cloud.len();
                let c = &cloud[idx];
                track_seeds.push((c.title.clone(), c.artist.clone(), 0.75));
            }
        }

        // 1a. 单曲级相似（track.getSimilar）
        for (t, a, seed_w) in &track_seeds {
            if let Ok(sims) = lastfm::track_similar(&lfm.api_key, t, a, 12) {
                for s in sims {
                    add_cand(s.title, s.artist, "lastfm_track", seed_w * (0.6 + s.score));
                }
            }
        }

        // 1b. 歌手级相似（artist.getSimilar → 热门曲 + 相似歌手列表）
        let mut artist_seeds: Vec<String> = Vec::new();
        if !primary_artist.is_empty() {
            artist_seeds.push(primary_artist.to_string());
        }
        if let Some((_, a, _)) = track_seeds.get(1)
            && !artist_seeds.iter().any(|x| x.eq_ignore_ascii_case(a))
        {
            artist_seeds.push(a.clone());
        }
        for a_seed in artist_seeds {
            if let Ok(mut sims) = lastfm::artist_similar(&lfm.api_key, &a_seed, 10) {
                // 前 8 位相似歌手轻度洗牌，保证跨歌手多样性
                let n = sims.len().min(8);
                for i in (1..n).rev() {
                    let j = (next_rand(&mut rng) as usize) % (i + 1);
                    sims.swap(i, j);
                }
                for (sim_name, m) in sims.into_iter().take(3) {
                    if !similar_artists
                        .iter()
                        .any(|x| x.eq_ignore_ascii_case(&sim_name))
                    {
                        similar_artists.push(sim_name.clone());
                    }
                    if let Ok(tops) = lastfm::artist_top_tracks(&lfm.api_key, &sim_name, 4) {
                        for c in tops {
                            add_cand(c.title, c.artist, "lastfm_artist", 0.55 + 0.4 * m);
                        }
                    }
                }
            }
        }
    }

    // ---- 2. Deezer 相似歌手 + 当前歌手热门（补充外语与免 key 兜底）----
    if !primary_artist.is_empty()
        && let Ok(Some(artist_id)) = deezer::search_artist_id(primary_artist)
    {
        if let Ok(related) = deezer::artist_related(artist_id, 4) {
            for (rel_id, rel_name) in related.into_iter().take(2) {
                if !similar_artists
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(&rel_name))
                {
                    similar_artists.push(rel_name);
                }
                if let Ok(tops) = deezer::artist_top(rel_id, 3) {
                    for c in tops {
                        add_cand(c.title, c.artist, "deezer_related", 0.7);
                    }
                }
            }
        }
        if let Ok(own) = deezer::artist_top(artist_id, 3) {
            for c in own {
                add_cand(c.title, c.artist, "deezer_own", 0.45);
            }
        }
    }

    // 排序候选池（加微量随机抖动，防止每次死板取第一条）
    let mut ranked: Vec<(Candidate, f64)> = scored_pool
        .into_values()
        .map(|(c, s)| {
            let jitter = ((next_rand(&mut rng) % 100) as f64) * 0.002;
            (c, s + jitter)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    // ---- 3. 逐条过双向匹配验证 + 无损下载管线 ----
    for (cand, _) in ranked.into_iter().take(ONLINE_MAX_TRIES) {
        let key = dedup_key(&cand.title, &cand.artist);
        let (hits, _) = rmus_sources::search_all(&format!("{} {}", cand.title, cand.artist), true);
        let pick = hits
            .iter()
            .filter(|h| {
                title_artist_matches(
                    &h.title,
                    h.artist.as_deref().unwrap_or(""),
                    &cand.title,
                    &cand.artist,
                )
            })
            .find(|h| flac_ok(h));
        let Some(pick) = pick else {
            new_failed.push(key);
            continue;
        };
        if let Some(item) = download_as_queue_item(pick, &lib_dir) {
            return Some(item);
        }
        new_failed.push(key);
    }

    // ---- 4. 相似歌手酷我/酷狗无损直搜（解决 Last.fm 繁体/外文译名对不上问题）----
    for sim_artist in similar_artists.iter().take(2) {
        if let Some(item) = search_artist_direct(sim_artist, &excluded, &lib_dir, &mut rng, new_failed)
        {
            return Some(item);
        }
    }

    // ---- 5. 当前歌手酷我/酷狗直搜兜底 ----
    if !primary_artist.is_empty()
        && let Some(item) =
            search_artist_direct(primary_artist, &excluded, &lib_dir, &mut rng, new_failed)
    {
        return Some(item);
    }

    // ---- 6. Deezer 全球榜单最终兜底 ----
    if let Ok(chart) = deezer::chart_tracks(15) {
        for cand in chart.into_iter().take(2) {
            let key = dedup_key(&cand.title, &cand.artist);
            if excluded.contains(&key) {
                continue;
            }
            let (hits, _) =
                rmus_sources::search_all(&format!("{} {}", cand.title, cand.artist), true);
            if let Some(pick) = hits
                .iter()
                .filter(|h| {
                    title_artist_matches(
                        &h.title,
                        h.artist.as_deref().unwrap_or(""),
                        &cand.title,
                        &cand.artist,
                    )
                })
                .find(|h| flac_ok(h))
                && let Some(item) = download_as_queue_item(pick, &lib_dir)
            {
                return Some(item);
            }
            new_failed.push(key);
        }
    }

    None
}

fn search_artist_direct(
    artist_name: &str,
    excluded: &HashSet<String>,
    lib_dir: &std::path::Path,
    rng: &mut u64,
    new_failed: &mut Vec<String>,
) -> Option<QueueItem> {
    let (hits, _) = rmus_sources::search_all(artist_name, true);
    let mut filtered: Vec<SearchResult> = hits
        .into_iter()
        .filter(|h| {
            let artist = h.artist.as_deref().unwrap_or("");
            let key = dedup_key(&h.title, artist);
            !excluded.contains(&key)
                && flac_ok(h)
                && split_artists(artist).any(|a| a.eq_ignore_ascii_case(artist_name))
        })
        .collect();
    let n = filtered.len().min(10);
    for i in (1..n).rev() {
        let j = (next_rand(rng) as usize) % (i + 1);
        filtered.swap(i, j);
    }
    for hit in filtered.into_iter().take(2) {
        let key = dedup_key(&hit.title, hit.artist.as_deref().unwrap_or(""));
        if let Some(item) = download_as_queue_item(&hit, lib_dir) {
            return Some(item);
        }
        new_failed.push(key);
    }
    None
}

fn flac_ok(h: &SearchResult) -> bool {
    h.source != "kuwo"
        || h.format
            .as_deref()
            .is_none_or(|f| f.eq_ignore_ascii_case("flac"))
}

fn download_as_queue_item(hit: &SearchResult, lib_dir: &std::path::Path) -> Option<QueueItem> {
    let result = rmus_sources::download(hit, lib_dir, true).ok()?;
    let path = PathBuf::from(result.get("path")?.as_str()?);
    let mut item = rmus::scan::collect(&[path]).into_iter().next()?;
    item.recommended = true;
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str, artist: &str, album: &str) -> QueueItem {
        QueueItem {
            path: PathBuf::from(format!("/music/{name}.flac")),
            title: name.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms: 200_000,
            recommended: false,
        }
    }

    #[test]
    fn local_prefers_same_artist_and_playlist_co_occurrence() {
        let seed = track("晴天", "周杰伦", "叶惠美");
        let same_artist = track("七里香", "周杰伦", "七里香");
        let co_occur_song = track("红豆", "王菲", "唱游");
        let unrelated = track("Other", "Nobody", "Album");

        let lib: Vec<LibEntry> = [
            seed.clone(),
            same_artist.clone(),
            co_occur_song.clone(),
            unrelated.clone(),
        ]
        .into_iter()
        .map(LibEntry::new)
        .collect();

        let mut playlists = BTreeMap::new();
        playlists.insert("深夜".into(), vec![seed.clone(), co_occur_song.clone()]);
        let affinity = HashMap::new();

        let picked = pick_local(
            &seed,
            std::slice::from_ref(&seed),
            0,
            &playlists,
            &lib,
            &VecDeque::new(),
            None,
            &affinity,
        )
        .unwrap();
        assert_eq!(picked.title, "七里香");
        assert!(picked.recommended);

        let picked2 = pick_local(
            &seed,
            &[seed.clone(), same_artist],
            0,
            &playlists,
            &lib,
            &VecDeque::new(),
            None,
            &affinity,
        )
        .unwrap();
        assert_eq!(picked2.title, "红豆");
    }

    #[test]
    fn dedup_key_normalizes_case_and_duet_order() {
        assert_eq!(
            dedup_key("知否知否", "胡夏 / 郁可唯"),
            dedup_key("知否知否", "郁可唯&胡夏")
        );
    }
}
