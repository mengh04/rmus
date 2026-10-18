//! 引擎事件桥：就地更新缓存，并把变化推成 D-Bus 信号。

use super::metadata::{build_metadata, extract_extras, loop_status, playback_status};
use super::{Mpris, SEEK_DETECT_MS, Shared, fetch_status};
use mpris_server::{Property, Server, Signal, Time};
use rmus::engine::ToEngine;
use rmus::proto::Event;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use tokio::sync::RwLock;

pub(super) async fn handle_event(
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

                // 切歌时按需提取内嵌封面与歌词（几~几十 ms，切歌不频繁）
                let (art_url, lyrics) = if matches!(event, Event::SongChanged { .. }) {
                    let extras = data
                        .status
                        .track
                        .as_ref()
                        .map(|t| extract_extras(&t.path))
                        .unwrap_or_default();
                    let url = extras
                        .0
                        .as_ref()
                        .and_then(|(bytes, ext)| data.art.store(bytes, ext));
                    data.art_url = url.clone();
                    data.lyrics = extras.1.clone();
                    (url, extras.1)
                } else {
                    (data.art_url.clone(), data.lyrics.clone())
                };
                drop(data);

                let props = match event {
                    Event::StateChanged { .. } => {
                        vec![Property::PlaybackStatus(playback_status(&status))]
                    }
                    Event::SongChanged { .. } => vec![Property::Metadata(build_metadata(
                        &status,
                        art_url.as_deref(),
                        lyrics.as_deref(),
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
