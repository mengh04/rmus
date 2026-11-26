//! Last.fm 自动 Scrobble 与「正在播放」上报（零阻塞后台任务）。
//!
//! - 每次开始播放音乐库曲目 → `track.updateNowPlaying`
//! - 曲目结束满足 Last.fm 官方规则（曲长 > 30s，且播放 ≥ 一半时长或 ≥ 4 分钟，或 EOF）
//!   → `track.scrobble`
//! - 只上报 `~/Music` 音乐库里的音乐（跳过播客与试听），不污染音乐主页
//! - 断网或接口临时失败时自动暂存到 `<data_dir>/scrobble_queue.json`（上限 50 条），
//!   下次上报时一并补发；热读 `<data_dir>/lastfm.json`，授权后无需重启 `rmusd` 即刻生效。

use rmus::engine::ToEngine;
use rmus::proto::{Event, PlayState, Reply, Request};
use rmus_fm::lastfm;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use tokio::sync::{broadcast, oneshot};

const QUEUE_CAP: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingScrobble {
    title: String,
    artist: String,
    album: String,
    duration_secs: u64,
    timestamp_secs: u64,
}

/// Last.fm 官方 Scrobble 门槛：时长 > 30s，且已播 ≥ 一半或 ≥ 4 分钟（或完整 EOF）。
pub(crate) fn should_scrobble(played_ms: u64, duration_ms: u64, eof: bool) -> bool {
    if duration_ms > 0 && duration_ms <= 30_000 {
        return false;
    }
    if duration_ms == 0 {
        return played_ms >= 30_000;
    }
    eof || played_ms >= 240_000 || played_ms * 2 >= duration_ms
}

fn queue_path(data_dir: &Path) -> PathBuf {
    data_dir.join("scrobble_queue.json")
}

fn load_queue(data_dir: &Path) -> Vec<PendingScrobble> {
    std::fs::read_to_string(queue_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_queue(data_dir: &Path, items: &[PendingScrobble]) {
    let path = queue_path(data_dir);
    if items.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(items) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn is_music_track(path: &Path) -> bool {
    let lib_root = rmus::library::root();
    path.starts_with(&lib_root)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 启动 Last.fm Scrobble 后台监听任务。
pub(crate) fn spawn(
    cmd_tx: std_mpsc::Sender<ToEngine>,
    mut events_rx: broadcast::Receiver<Event>,
) {
    tokio::spawn(async move {
        let mut last_album = String::new();
        let mut last_path = PathBuf::new();

        loop {
            let ev = match events_rx.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };

            match ev {
                Event::SongChanged { index: Some(_) } => {
                    let (tx, rx) = oneshot::channel();
                    if cmd_tx
                        .send(ToEngine::Request {
                            id: 0,
                            request: Request::Status,
                            reply: tx,
                        })
                        .is_err()
                    {
                        continue;
                    }
                    let Ok(Ok(Reply::Status(status))) = rx.await else {
                        continue;
                    };
                    if status.state != PlayState::Playing || status.peeking {
                        continue;
                    }
                    let Some(track) = status.track else {
                        continue;
                    };
                    if !is_music_track(&track.path)
                        || track.title.trim().is_empty()
                        || track.artist.trim().is_empty()
                    {
                        continue;
                    }
                    last_path = track.path.clone();
                    last_album = track.album.clone();

                    let title = track.title;
                    let artist = track.artist;
                    let album = track.album;
                    let dur_secs = track.duration_ms / 1000;
                    tokio::task::spawn_blocking(move || {
                        let data_dir = rmus::plugin::data_root();
                        let Some(cfg) = lastfm::load_config(&data_dir) else {
                            return;
                        };
                        if !cfg.can_scrobble() {
                            return;
                        }
                        if let Err(e) =
                            lastfm::update_now_playing(&cfg, &title, &artist, &album, dur_secs)
                        {
                            eprintln!("[rmusd] Last.fm Now Playing 上报失败: {e:#}");
                        }
                    });
                }
                Event::TrackFinished {
                    path,
                    title,
                    artist,
                    played_ms,
                    duration_ms,
                    eof,
                } => {
                    if !is_music_track(&path)
                        || title.trim().is_empty()
                        || artist.trim().is_empty()
                        || !should_scrobble(played_ms, duration_ms, eof)
                    {
                        continue;
                    }
                    let album = if path == last_path {
                        last_album.clone()
                    } else {
                        String::new()
                    };
                    let timestamp_secs = now_secs().saturating_sub(played_ms / 1000);
                    let item = PendingScrobble {
                        title,
                        artist,
                        album,
                        duration_secs: duration_ms / 1000,
                        timestamp_secs,
                    };
                    tokio::task::spawn_blocking(move || {
                        let data_dir = rmus::plugin::data_root();
                        let Some(cfg) = lastfm::load_config(&data_dir) else {
                            return;
                        };
                        if !cfg.can_scrobble() {
                            return;
                        }
                        let mut queue = load_queue(&data_dir);
                        queue.push(item);
                        if queue.len() > QUEUE_CAP {
                            let drop_n = queue.len() - QUEUE_CAP;
                            queue.drain(..drop_n);
                        }
                        let mut remaining = Vec::new();
                        let mut failed_already = false;
                        for entry in queue {
                            if failed_already {
                                remaining.push(entry);
                                continue;
                            }
                            match lastfm::scrobble(
                                &cfg,
                                &entry.title,
                                &entry.artist,
                                &entry.album,
                                entry.duration_secs,
                                entry.timestamp_secs,
                            ) {
                                Ok(()) => {
                                    println!(
                                        "[rmusd] Last.fm Scrobble：{} - {}",
                                        entry.title, entry.artist
                                    );
                                }
                                Err(e) => {
                                    eprintln!("[rmusd] Last.fm Scrobble 暂存稍后重试: {e:#}");
                                    failed_already = true;
                                    remaining.push(entry);
                                }
                            }
                        }
                        save_queue(&data_dir, &remaining);
                    });
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::should_scrobble;

    #[test]
    fn enforces_lastfm_scrobble_thresholds() {
        // <= 30s 的短音频不上报
        assert!(!should_scrobble(25_000, 25_000, true));
        // 未满一半且不足 4 分钟不上报
        assert!(!should_scrobble(60_000, 200_000, false));
        // 满一半上报
        assert!(should_scrobble(100_000, 200_000, false));
        // 长曲播满 4 分钟上报
        assert!(should_scrobble(240_000, 600_000, false));
        // 正常 EOF 上报
        assert!(should_scrobble(180_000, 200_000, true));
    }
}
