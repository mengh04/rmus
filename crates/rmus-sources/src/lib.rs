//! rmus 插件共享音乐源库：酷我/酷狗搜索、下载管线、tag 写入。
//!
//! 原 rmus-dl 的 `sources/` + `download.rs` 平移而来（kuwo/kugou 的
//! `super::` 引用天然指向 crate root，无需改动），另加高层入口：
//! [`search_all`]（串行搜双源）、[`download`]（完整下载管线）、
//! [`title_artist_matches`]（标题/歌手双向模糊匹配，LLM 候选验证用
//! ——rmus-dj 从搜索结果里找到 LLM 点名的歌就靠它）。差异：async →
//! 同步（SDK http 阻塞调用）；`reqwest::Url` → `String`（SDK http 用
//! 字符串 URL）；文件/tag IO 经 SDK fs 代理。

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use rmus_plugin_sdk::{fs, http};

pub mod kugou;
pub mod kuwo;
pub mod download;

/// 极简 percent-encoding，仅用于拼搜索 URL 的关键词。
pub(crate) fn urlencode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 酷我搜索结果里的 MINFO 品质信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QualityInfo {
    pub flac: bool,
    pub flac_size: Option<u64>,
    pub mp3_label: Option<String>,
}

/// Range 探针结果：真实格式与精确总大小。
pub struct Probe {
    pub is_flac: bool,
    pub total: Option<u64>,
}

/// 下载前的最后确认：取 URL 开头几十字节看魔数，顺带从 Content-Range
/// 拿精确总大小。`expect_flac` 时魔数不是 fLaC 直接报错——"只要无损"
/// 的最终闸门。
pub(crate) fn probe_audio(url: &str, expect_flac: bool) -> Result<Probe> {
    let resp = http::Response::send(
        &http::Request::get(url).header("Range", "bytes=0-15"),
    )
    .context("探针请求失败")?
    .error_for_status()?;

    let mut head: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 64];
    while head.len() < 16 {
        let n = resp.read(&mut chunk).context("探针读 body 失败")?;
        if n == 0 {
            break;
        }
        let want = 16 - head.len();
        head.extend_from_slice(&chunk[..chunk.len().min(want)]);
    }

    let is_flac = head.starts_with(b"fLaC");
    if expect_flac && !is_flac {
        let got = if head.starts_with(b"ID3") || head.first().is_some_and(|b| b & 0xE0 == 0xE0) {
            "mp3"
        } else {
            "未知格式"
        };
        anyhow::bail!("上游返回的不是 FLAC（实际 {got}），已取消下载");
    }
    let total = resp
        .header("content-range")
        .and_then(|value| value.rsplit('/').next().and_then(|s| s.parse::<u64>().ok()));

    Ok(Probe { is_flac, total })
}

pub(crate) fn parse_minfo(minfo: &str) -> QualityInfo {
    let mut quality = QualityInfo { flac: false, flac_size: None, mp3_label: None };
    for entry in minfo.split(';') {
        let mut level = "";
        let mut format = "";
        let mut bitrate: u64 = 0;
        let mut size_mb: Option<f64> = None;
        for pair in entry.split(',') {
            let Some((key, value)) = pair.split_once(':') else { continue };
            match key.trim() {
                "level" => level = value.trim(),
                "format" => format = value.trim(),
                "bitrate" => bitrate = value.trim().parse().unwrap_or(0),
                "size" => {
                    size_mb = value.trim().strip_suffix("Mb").and_then(|s| s.parse().ok());
                }
                _ => {}
            }
        }
        match format {
            "flac" => {
                quality.flac = true;
                if quality.flac_size.is_none() {
                    quality.flac_size = size_mb.map(|mb| (mb * 1024.0 * 1024.0) as u64);
                }
            }
            "mp3" => {
                let better = quality
                    .mp3_label
                    .as_deref()
                    .and_then(|label| label.strip_suffix('k'))
                    .and_then(|k| k.parse::<u64>().ok())
                    .is_none_or(|best| bitrate > best);
                if better && bitrate > 0 {
                    quality.mp3_label = Some(format!("{bitrate}k"));
                }
            }
            _ => {}
        }
        let _ = level;
    }
    quality
}

/// 一条搜索结果：足够展示 + 回源取下载链接和元数据。
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub song_id: String,
    pub source: &'static str,
    pub cover_url: Option<String>,
    pub duration_ms: u64,
    pub format: Option<String>,
    pub size_bytes: Option<u64>,
}

/// 下载所需的补充元数据（歌词、封面）。
#[derive(Debug, Default)]
pub struct SongMetadata {
    pub lyric: Option<String>,
    pub cover_url: Option<String>,
    /// 主封面下载失败时的备选封面链接（例如酷我 500px 404 时退回 songinfo.pic）
    pub fallback_cover_url: Option<String>,
}

impl SongMetadata {
    /// 门禁：必须拿到封面（纯音乐允许无歌词，但封面绝不可缺）。
    pub fn gate(&self) -> Result<()> {
        if self.cover_url.is_none() && self.fallback_cover_url.is_none() {
            anyhow::bail!("metadata gate failed: cover art was not resolved");
        }
        Ok(())
    }
}

/// 一个音乐源（同步版：SDK http 阻塞调用，插件线程独占模型）。
pub trait MusicSource {
    fn search(&self, keyword: &str) -> Result<Vec<SearchResult>>;
    fn get_download_url(&self, song: &SearchResult) -> Result<String>;
    fn get_metadata(&self, song: &SearchResult) -> Result<SongMetadata>;
}

/// 按源名开源实例。rmus-dl 的 CLI `--pick kuwo` 与 rmus-dj 的定向搜索
/// 都用它——多态在这里，不在调用方。
pub fn open_source(name: &str, flac_only: bool) -> Result<Box<dyn MusicSource>> {
    match name {
        "kuwo" => Ok(Box::new(kuwo::Kuwo::new(flac_only)?)),
        "kugou" => Ok(Box::new(kugou::Kugou::new(flac_only)?)),
        other => bail!("未知音源：{other}"),
    }
}

/// 串行搜酷我+酷狗，单源挂了不挡另一源（错误进 notes，凑合用另一源）。
pub fn search_all(keyword: &str, flac_only: bool) -> (Vec<SearchResult>, Vec<String>) {
    let mut all = Vec::new();
    let mut notes = Vec::new();
    for name in ["kuwo", "kugou"] {
        match open_source(name, flac_only).and_then(|s| s.search(keyword)) {
            Ok(mut hits) => all.append(&mut hits),
            Err(e) => notes.push(format!("{name} 源不可用：{e:#}")),
        }
    }
    if all.is_empty() && notes.is_empty() {
        notes.push("两个源都没有匹配结果".into());
    }
    (all, notes)
}

/// 完整下载管线（原 rmus-dl 的 download 门禁 + finish_download 合并，
/// 行为与消息原样保留）：品质门禁 → 同名探测（flac/mp3/m4a/ogg 四扩
/// 展名）→ 直链 → Range 探针（验魔数+精确大小，上游偷偷降级 mp3 在这里
/// 拦下）→ 流式下载（150ms 节流 notify 进度）→ 元数据门禁（拿不到或写
/// 不进去算失败，清理产物）→ 成功返回 `{"path":落盘路径}`。
///
/// rmus-dl 的 CLI/UI 与 rmus-dj 的自动入库都走这一个门面——取消
/// （cancelled()）、进度（notify）语义对两者一致。酷狗源内部自己保证
/// sqhash（无损专属），酷我的 flac 门禁在这里拦。
pub fn download(
    song: &SearchResult,
    dest_dir: &Path,
    flac_only: bool,
) -> Result<serde_json::Value> {
    use download::DownloadOptions;

    if flac_only && song.source == "kuwo" && song.format.as_deref().is_some_and(|f| f != "flac") {
        let label = song.format.as_deref().unwrap_or("未知");
        bail!("无 flac 版本（搜索标注：{label}）");
    }
    let fallback_probe = match &song.artist {
        Some(artist) => format!("{} - {}", song.title, artist),
        None => song.title.clone(),
    };
    for ext in ["flac", "mp3", "m4a", "ogg"] {
        let candidate = dest_dir.join(format!("{fallback_probe}.{ext}"));
        if fs::exists(&candidate.display().to_string()) {
            bail!("同名文件已存在：{}", candidate.display());
        }
    }

    let source = open_source(song.source, flac_only)?;

    // 下载后半程（源已定）：直链 → 探针 → 下载 → 门禁 → 落盘
    let url = source.get_download_url(song)?;
    let probe = probe_audio(&url, flac_only)?;
    if let Some(total) = probe.total {
        rmus_plugin_sdk::notify(&serde_json::json!({ "total": total }));
    }

    let mut received: u64 = 0;
    let mut last_notify = rmus_plugin_sdk::now_ms().saturating_sub(150);
    let mut opts = DownloadOptions {
        dest_dir,
        on_chunk: &mut |chunk, total| {
            received += chunk;
            let now = rmus_plugin_sdk::now_ms();
            let finished = total.is_some_and(|t| received >= t);
            if !finished && now.saturating_sub(last_notify) < 150 {
                return;
            }
            last_notify = now;
            rmus_plugin_sdk::notify(
                &serde_json::json!({ "received": received, "total": total }),
            );
        },
    };
    let file_path = download::download_file(&url, &fallback_probe, song, &mut opts)?;

    // 元数据是门禁：拿不到或写不进去，这次下载就算失败，清理产物
    rmus_plugin_sdk::notify(&serde_json::json!({ "phase": "metadata" }));
    let meta_result = source
        .get_metadata(song)
        .and_then(|meta| write_metadata(&file_path, &meta, song, probe.is_flac));
    if let Err(e) = meta_result {
        let path_str = file_path.display().to_string();
        if !fs::remove(&path_str) {
            // 清理失败只留痕（宿主 stderr），不掩盖主错误
        }
        bail!("元数据门禁失败：{e:#}");
    }

    Ok(serde_json::json!({ "path": file_path }))
}

/// 歌手串的常见分隔符：酷我用 "&"、酷狗用 "、"，CDN tag 里还见 "/"、","。
pub const ARTIST_SEPARATORS: [char; 7] = ['&', '、', '/', ',', '，', ';', '；'];

/// 拆分歌手串，去首尾空白与空项（rmus-dj 统计歌手口味用）。
pub fn split_artists(s: &str) -> impl Iterator<Item = &str> {
    s.split(ARTIST_SEPARATORS)
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// 歌手串统一用 "&" 连接，文件名与 ARTIST 标签保持一致
/// （rmus-dj 也用它做口味统计键）。
pub fn normalize_artists(s: &str) -> String {
    split_artists(s).collect::<Vec<_>>().join("&")
}

/// 把标题/歌手/专辑/歌词/封面写进音频文件的 tag（lofty 经 SDK File 的 IO 代理）。
/// 基础信息以搜索结果为准——CDN 自带 tag 常不完整（合唱只写第一个歌手），
/// 搜索文本是用户确认过的展示文本。`is_flac` 定显式 FileType——temp/无扩展名
/// 文件的自行探测会失败（rmus-dl 同款坑）。门禁：没有歌词也没有封面时直接
/// 失败；写入后读回验证。
pub(crate) fn write_metadata(
    file_path: &Path,
    meta: &SongMetadata,
    search: &SearchResult,
    is_flac: bool,
) -> Result<()> {
    use lofty::{
        config::WriteOptions,
        file::{AudioFile, FileType, TaggedFileExt},
        picture::{Picture, PictureType},
        tag::ItemKey,
    };
    use std::io::Cursor;

    meta.gate()?;
    let path_str = file_path.display().to_string();
    let file_type = if is_flac { FileType::Flac } else { FileType::Mpeg };

    // 读写打开（不截断）：lofty 就地修改 tag（rewind 重写 tag 区、音频原位）
    let mut file = fs::File::open_rw(&path_str)?;
    let mut tagged_file = lofty::probe::Probe::new(&mut file)
        .set_file_type(file_type)
        .read()
        .context("lofty 解析音频文件失败")?;

    let Some(tag) = tagged_file.primary_tag_mut() else {
        anyhow::bail!("No primary tag found in the audio file");
    };

    let lyrics_key = match tag.tag_type() {
        lofty::tag::TagType::Id3v2 => ItemKey::UnsyncLyrics,
        _ => ItemKey::Lyrics,
    };

    // 基础信息覆盖 CDN 自带的残缺 tag（insert_text 替换同 key 旧值）
    tag.insert_text(ItemKey::TrackTitle, search.title.clone());
    if let Some(artist) = search
        .artist
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        tag.insert_text(ItemKey::TrackArtist, normalize_artists(artist));
    }
    if let Some(album) = search
        .album
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        tag.insert_text(ItemKey::AlbumTitle, album.to_string());
    }

    if let Some(lyric) = &meta.lyric {
        tag.insert_text(lyrics_key, lyric.clone());
    }

    // 封面是硬门禁：按主链接 → 备选链接顺序拉取并解码，全部失败则直接报错清理产物
    let cover_urls: Vec<&str> = [
        meta.cover_url.as_deref(),
        meta.fallback_cover_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut picture_ok = None;
    let mut last_cover_err = None;
    for url in cover_urls {
        let fetched = http::Response::send(&http::Request::get(url.to_string()))
            .context("封面下载失败")
            .and_then(|r| r.error_for_status().context("封面 HTTP 错误"))
            .and_then(|r| r.read_to_end().context("封面读取失败"))
            .and_then(|bytes| {
                let mut cursor = Cursor::new(bytes);
                Picture::from_reader(&mut cursor).context("封面解码失败")
            });
        match fetched {
            Ok(mut pic) => {
                pic.set_pic_type(PictureType::CoverFront);
                picture_ok = Some(pic);
                break;
            }
            Err(e) => last_cover_err = Some(e),
        }
    }
    let picture = match picture_ok {
        Some(pic) => pic,
        None => {
            return Err(last_cover_err.unwrap_or_else(|| anyhow::anyhow!("封面缺失")));
        }
    };
    tag.remove_picture_type(PictureType::CoverFront);
    tag.push_picture(picture);

    // 就地保存（同一句柄；lofty rewind 后重写 tag 区）
    tagged_file
        .save_to(&mut file, WriteOptions::default())
        .context("lofty 保存失败")?;
    drop(file);

    // 门禁复核：重新读文件（显式类型），确认封面和基础标签真的在 tag 里
    let mut verified_input = fs::File::open(&path_str).map_err(anyhow::Error::msg)?;
    let verified = lofty::probe::Probe::new(&mut verified_input)
        .set_file_type(file_type)
        .read()?;
    let Some(tag) = verified.primary_tag() else {
        anyhow::bail!("verification failed: no primary tag after save");
    };
    if meta.lyric.is_some() && tag.get_string(lyrics_key).is_none() {
        anyhow::bail!("verification failed: LYRICS missing from tag after save");
    }
    if tag.pictures().is_empty() {
        anyhow::bail!("verification failed: picture missing from tag after save");
    }
    if tag
        .get_string(ItemKey::TrackTitle)
        .is_none_or(|t| t.trim() != search.title.trim())
    {
        anyhow::bail!("verification failed: TITLE missing from tag after save");
    }
    let has_artist = search
        .artist
        .as_deref()
        .map(str::trim)
        .is_some_and(|a| !a.is_empty());
    if has_artist && tag.get_string(ItemKey::TrackArtist).is_none_or(str::is_empty) {
        anyhow::bail!("verification failed: ARTIST missing from tag after save");
    }
    Ok(())
}

/// 临时文件路径工具：`.名字-<随机>.part`（点前缀不会被库扫描收录）。
pub(crate) fn temp_path(dest_dir: &Path, fallback_name: &str) -> PathBuf {
    let unique = rmus_plugin_sdk::now_ms() as u64;
    dest_dir.join(format!(
        ".{}.{unique:x}.part",
        sanitize_filename(&fallback_name.chars().take(40).collect::<String>())
    ))
}

/// 去掉文件名里的路径分隔符等常见非法字符。
pub(crate) fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::normalize_artists;

    #[test]
    fn artist_separators_normalize_to_ampersand() {
        assert_eq!(normalize_artists("胡夏/郁可唯"), "胡夏&郁可唯");
        assert_eq!(normalize_artists("胡夏、郁可唯"), "胡夏&郁可唯");
        assert_eq!(normalize_artists("胡夏&郁可唯"), "胡夏&郁可唯");
        assert_eq!(normalize_artists("胡夏， 郁可唯"), "胡夏&郁可唯");
        assert_eq!(normalize_artists("郁可唯"), "郁可唯");
        assert_eq!(normalize_artists(""), "");
    }

    #[test]
    fn metadata_gate_requires_cover_art() {
        use super::SongMetadata;
        // 仅有歌词没有封面 → 门禁拒绝
        let lyric_only = SongMetadata {
            lyric: Some("[00:00.00]test".into()),
            cover_url: None,
            fallback_cover_url: None,
        };
        assert!(lyric_only.gate().is_err());

        // 有封面（即使纯音乐无歌词）→ 门禁放行
        let with_cover = SongMetadata {
            lyric: None,
            cover_url: Some("https://img1.kuwo.cn/star/albumcover/500/a.jpg".into()),
            fallback_cover_url: None,
        };
        assert!(with_cover.gate().is_ok());
    }
}

