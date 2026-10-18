//! `org.mpris.MediaPlayer2.Player`：播放控制与属性查询。

use super::Mpris;
use super::metadata::{build_metadata, loop_status, playback_status};
use mpris_server::{
    LoopStatus, Metadata as MprisMetadata, PlaybackStatus, PlayerInterface, Time, TrackId, Volume,
};
use rmus::proto::{RepeatMode, Request};
use zbus::fdo;

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
        Ok(build_metadata(
            &data.status,
            data.art_url.as_deref(),
            data.lyrics.as_deref(),
        ))
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        // TODO: 解析 file:// URI 走 Open 路径
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(playback_status(&self.status().await))
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_millis(self.status().await.position_ms as i64))
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
