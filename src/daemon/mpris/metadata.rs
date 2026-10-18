//! 元数据：从音频 tag 取封面/歌词，按引擎状态拼 MPRIS Metadata。

use mpris_server::{LoopStatus, Metadata as MprisMetadata, PlaybackStatus, Time, TrackId};
use rmus::proto::{PlayState, RepeatMode, StatusInfo};

/// 从音频文件 tag 提取内嵌封面与歌词（一次 parse，切歌只读一遍文件）。
/// 封面优先 CoverFront，否则第一张；歌词尝试 UnsyncLyrics（ID3v2 USLT、
/// Vorbis UNSYNCEDLYRICS）和 Lyrics（Vorbis LYRICS）两个 key。
pub(super) fn extract_extras(
    path: &std::path::Path,
) -> (Option<(Vec<u8>, String)>, Option<String>) {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;
    let Ok(tagged) = lofty::probe::Probe::open(path).and_then(|p| p.read()) else {
        return (None, None);
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return (None, None);
    };

    let art = {
        let pictures = tag.pictures();
        pictures
            .iter()
            .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
            .or_else(|| pictures.first())
            .map(|pic| {
                let ext = pic
                    .mime_type()
                    .and_then(|m| m.ext())
                    .unwrap_or("jpg")
                    .to_string();
                (pic.data().to_vec(), ext)
            })
    };

    let lyrics = tag
        .get_string(ItemKey::UnsyncLyrics)
        .or_else(|| tag.get_string(ItemKey::Lyrics))
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());

    (art, lyrics)
}

pub(super) fn playback_status(status: &StatusInfo) -> PlaybackStatus {
    match status.state {
        PlayState::Playing => PlaybackStatus::Playing,
        PlayState::Paused => PlaybackStatus::Paused,
        PlayState::Stopped => PlaybackStatus::Stopped,
    }
}

pub(super) fn loop_status(repeat: RepeatMode) -> LoopStatus {
    match repeat {
        RepeatMode::Off => LoopStatus::None,
        RepeatMode::All => LoopStatus::Playlist,
        RepeatMode::One => LoopStatus::Track,
    }
}

pub(super) fn build_metadata(
    status: &StatusInfo,
    art_url: Option<&str>,
    lyrics: Option<&str>,
) -> MprisMetadata {
    let mut m = MprisMetadata::new();
    let track_path = format!("/dev/rmus/queue/{}", status.current.unwrap_or(0));
    if let Ok(path) = zbus::zvariant::ObjectPath::try_from(track_path) {
        m.set_trackid(Some(TrackId::from(path)));
    }
    if let Some(art_url) = art_url {
        m.set_art_url(Some(art_url));
    }
    if let Some(lyrics) = lyrics {
        m.set_lyrics(Some(lyrics));
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
