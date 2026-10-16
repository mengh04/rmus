//! MPRIS（org.mpris.MediaPlayer2.rmus）服务端。
//!
//! 桌面环境/媒体键/playerctl 通过 D-Bus 控制 rmusd：
//! - MPRIS 方法 -> ToEngine 命令（等待引擎回执再返回）
//! - 引擎事件 -> 缓存最新状态 + properties_changed / Seeked 信号
//!
//! 状态缓存：事件桥任务维护一份 StatusInfo 快照（Position 事件就地更新
//! position，其余事件拉一次全量），MPRIS 属性查询全部读缓存，不查引擎。

use mpris_server::{
    LoopStatus, Metadata as MprisMetadata, PlaybackStatus, PlayerInterface, Property, RootInterface,
    Server, Signal, Time, TrackId, Volume,
};
use rmus::engine::ToEngine;
use rmus::proto::{Event, PlayState, RepeatMode, Request, StatusInfo};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, oneshot};
use zbus::fdo;

/// Position 事件间隔的公差：超过它视为用户 seek（发 Seeked 信号）
const SEEK_DETECT_MS: i64 = 2000;

/// 封面临时文件前缀：/tmp/rmus-art-{pid}-{counter}.{ext}
const ART_PREFIX: &str = "rmus-art-";

/// 当前曲封面的临时文件管理。
///
/// 每次切歌写一个新文件（counter 递增，文件名变化让 MPRIS 客户端
/// 重新加载图片），并删除上一张。daemon 崩溃残留的孤儿文件在下次
/// 启动时按 /proc 探活清理。
struct ArtCache {
    counter: u64,
    last_file: Option<std::path::PathBuf>,
}

impl ArtCache {
    fn new() -> Self {
        Self {
            counter: 0,
            last_file: None,
        }
    }

    fn store(&mut self, data: &[u8], ext: &str) -> Option<String> {
        let path = std::env::temp_dir().join(format!(
            "{ART_PREFIX}{}-{}.{ext}",
            std::process::id(),
            self.counter
        ));
        self.counter += 1;
        std::fs::write(&path, data).ok()?;
        if let Some(old) = self.last_file.take() {
            let _ = std::fs::remove_file(old);
        }
        self.last_file = Some(path.clone());
        url::Url::from_file_path(path).ok().map(|u| u.to_string())
    }

    /// 删除残留的孤儿封面（pid 已不存在的）
    fn cleanup_orphans() {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(suffix) = name.strip_prefix(ART_PREFIX) else {
                continue;
            };
            let Some((pid, _)) = suffix.split_once('-') else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            // 自己 pid 的文件可能是刚启动就切歌写下的，不清理
            if pid == std::process::id() || std::path::Path::new(&format!("/proc/{pid}")).exists()
            {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 从音频文件 tag 提取内嵌封面（优先 CoverFront，否则第一张）
fn extract_art(path: &std::path::Path) -> Option<(Vec<u8>, String)> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pictures = tag.pictures();
    let pic = pictures
        .iter()
        .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
        .or_else(|| pictures.first())?;
    let ext = pic
        .mime_type()
        .and_then(|m| m.ext())
        .unwrap_or("jpg")
        .to_string();
    Some((pic.data().to_vec(), ext))
}

struct Shared {
    status: StatusInfo,
    /// 上一次 Position 事件的值，用于检测 seek 跳变
    last_position: Option<u64>,
    /// 当前曲封面的 file:// URL
    art_url: Option<String>,
    /// 封面临时文件管理
    art: ArtCache,
}

pub struct Mpris {
    cmd_tx: std_mpsc::Sender<ToEngine>,
    shared: Arc<RwLock<Shared>>,
}

/// 启动 MPRIS 服务与事件桥。失败（如无 D-Bus 会话）由调用方决定是否致命。
pub async fn spawn(
    cmd_tx: std_mpsc::Sender<ToEngine>,
    mut events: broadcast::Receiver<Event>,
) -> anyhow::Result<()> {
    ArtCache::cleanup_orphans();
    let shared = Arc::new(RwLock::new(Shared {
        status: fetch_status(&cmd_tx).await.unwrap_or_default(),
        last_position: None,
        art_url: None,
        art: ArtCache::new(),
    }));

    let server = Server::new("rmus", Mpris {
        cmd_tx: cmd_tx.clone(),
        shared: shared.clone(),
    })
    .await?;

    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => handle_event(&server, &shared, &cmd_tx, event).await,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(())
}

async fn fetch_status(cmd_tx: &std_mpsc::Sender<ToEngine>) -> Option<StatusInfo> {
    let (reply_tx, reply_rx) = oneshot::channel();
    cmd_tx
        .send(ToEngine::Request {
            id: 0,
            request: Request::Status,
            reply: reply_tx,
        })
        .ok()?;
    match reply_rx.await {
        Ok(Ok(rmus::proto::Reply::Status(status))) => Some(*status),
        _ => None,
    }
}

async fn handle_event(
    server: &Server<Mpris>,
    shared: &Arc<RwLock<Shared>>,
    cmd_tx: &std_mpsc::Sender<ToEngine>,
    event: Event,
) {
    match event {
        Event::Position { position_ms } => {
            let mut data = shared.write().await;
            let jumped = data
                .last_position
                .is_some_and(|prev| (position_ms as i64 - prev as i64).abs() > SEEK_DETECT_MS);
            data.status.position_ms = position_ms;
            data.last_position = Some(position_ms);
            if jumped {
                drop(data);
                let _ = server
                    .emit(Signal::Seeked {
                        position: Time::from_millis(position_ms as i64),
                    })
                    .await;
            }
        }
        Event::StateChanged { .. } | Event::SongChanged { .. } | Event::QueueChanged => {
            if let Some(status) = fetch_status(cmd_tx).await {
                let mut data = shared.write().await;
                data.status = status.clone();

                // 切歌时按需提取内嵌封面到临时文件（几~几十 ms，切歌不频繁）
                let art_url = if matches!(event, Event::SongChanged { .. }) {
                    let art = data
                        .status
                        .track
                        .as_ref()
                        .and_then(|t| extract_art(&t.path));
                    let url = art.and_then(|(bytes, ext)| data.art.store(&bytes, &ext));
                    data.art_url = url.clone();
                    url
                } else {
                    data.art_url.clone()
                };
                drop(data);

                let props = match event {
                    Event::StateChanged { .. } => {
                        vec![Property::PlaybackStatus(playback_status(&status))]
                    }
                    Event::SongChanged { .. } => vec![Property::Metadata(build_metadata(
                        &status,
                        art_url.as_deref(),
                    ))],
                    _ => vec![],
                };
                if !props.is_empty() {
                    let _ = server.properties_changed(props).await;
                }
            }
        }
        Event::VolumeChanged { value } => {
            shared.write().await.status.volume = value;
            let _ = server
                .properties_changed([Property::Volume(f64::from(value))])
                .await;
        }
        Event::ModeChanged { repeat, random } => {
            let mut data = shared.write().await;
            data.status.repeat = repeat;
            data.status.random = random;
            drop(data);
            let _ = server
                .properties_changed([
                    Property::LoopStatus(loop_status(repeat)),
                    Property::Shuffle(random),
                ])
                .await;
        }
    }
}

fn playback_status(status: &StatusInfo) -> PlaybackStatus {
    match status.state {
        PlayState::Playing => PlaybackStatus::Playing,
        PlayState::Paused => PlaybackStatus::Paused,
        PlayState::Stopped => PlaybackStatus::Stopped,
    }
}

fn loop_status(repeat: RepeatMode) -> LoopStatus {
    match repeat {
        RepeatMode::Off => LoopStatus::None,
        RepeatMode::All => LoopStatus::Playlist,
        RepeatMode::One => LoopStatus::Track,
    }
}

fn build_metadata(status: &StatusInfo, art_url: Option<&str>) -> MprisMetadata {
    let mut m = MprisMetadata::new();
    let track_path = format!("/dev/rmus/queue/{}", status.current.unwrap_or(0));
    if let Ok(path) = zbus::zvariant::ObjectPath::try_from(track_path) {
        m.set_trackid(Some(TrackId::from(path)));
    }
    if let Some(art_url) = art_url {
        m.set_art_url(Some(art_url));
    }
    if let Some(track) = &status.track {
        m.set_title(Some(track.title.clone()));
        if !track.artist.is_empty() {
            m.set_artist(Some([track.artist.clone()]));
        }
        if !track.album.is_empty() {
            m.set_album(Some(track.album.clone()));
        }
        if let Ok(url) = url::Url::from_file_path(&track.path) {
            m.set_url(Some(url.to_string()));
        }
    }
    if status.duration_ms > 0 {
        m.set_length(Some(Time::from_millis(status.duration_ms as i64)));
    }
    m
}

impl Mpris {
    async fn request(&self, request: Request) -> fdo::Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(ToEngine::Request {
                id: 0,
                request,
                reply: reply_tx,
            })
            .map_err(|_| fdo::Error::Failed("引擎不可用".into()))?;
        match reply_rx.await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(fdo::Error::Failed(e)),
            Err(_) => Err(fdo::Error::Failed("引擎无响应".into())),
        }
    }

    async fn status(&self) -> StatusInfo {
        self.shared.read().await.status.clone()
    }
}

impl RootInterface for Mpris {
    async fn raise(&self) -> fdo::Result<()> {
        // rmus 无 GUI 可 raise
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        let _ = std::fs::remove_file(rmus::socket_path());
        std::process::exit(0);
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _: bool) -> zbus::Result<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("rmus".into())
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok("rmus".into())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec!["file".into()])
    }
}

impl PlayerInterface for Mpris {
    async fn next(&self) -> fdo::Result<()> {
        self.request(Request::Next).await
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.request(Request::Prev).await
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.request(Request::TogglePlay).await
    }

    async fn play(&self) -> fdo::Result<()> {
        self.request(Request::Play { index: None }).await
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.status().await.queue_len > 0)
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.request(Request::Pause).await
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        self.request(Request::SeekRelative {
            delta_ms: offset.as_millis(),
        })
        .await
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(loop_status(self.status().await.repeat))
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> zbus::Result<()> {
        let mode = match loop_status {
            LoopStatus::None => RepeatMode::Off,
            LoopStatus::Track => RepeatMode::One,
            LoopStatus::Playlist => RepeatMode::All,
        };
        self.request(Request::SetRepeat { mode: Some(mode) })
            .await
            .map_err(|e| zbus::Error::Failure(e.to_string()))
    }

    async fn set_position(&self, _track_id: TrackId, position: Time) -> fdo::Result<()> {
        self.request(Request::Seek {
            position_ms: position.as_millis().max(0) as u64,
        })
        .await
    }

    async fn set_rate(&self, _rate: mpris_server::PlaybackRate) -> zbus::Result<()> {
        Ok(())
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn metadata(&self) -> fdo::Result<MprisMetadata> {
        let data = self.shared.read().await;
        Ok(build_metadata(&data.status, data.art_url.as_deref()))
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        // TODO: 解析 file:// URI 走 Open 路径
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(playback_status(&self.status().await))
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_millis(
            self.status().await.position_ms as i64,
        ))
    }

    async fn rate(&self) -> fdo::Result<mpris_server::PlaybackRate> {
        Ok(1.0)
    }

    async fn set_shuffle(&self, shuffle: bool) -> zbus::Result<()> {
        self.request(Request::SetRandom { on: Some(shuffle) })
            .await
            .map_err(|e| zbus::Error::Failure(e.to_string()))
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.status().await.random)
    }

    async fn set_volume(&self, volume: Volume) -> zbus::Result<()> {
        self.request(Request::SetVolume {
            value: volume.min(1.0) as f32,
        })
        .await
        .map_err(|e| zbus::Error::Failure(e.to_string()))
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(f64::from(self.status().await.volume))
    }

    async fn stop(&self) -> fdo::Result<()> {
        self.request(Request::Stop).await
    }

    async fn maximum_rate(&self) -> fdo::Result<mpris_server::PlaybackRate> {
        Ok(1.0)
    }

    async fn minimum_rate(&self) -> fdo::Result<mpris_server::PlaybackRate> {
        Ok(1.0)
    }
}
