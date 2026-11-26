//! Last.fm 公开推荐 API（免费 API Key，零大模型）：
//! - `track.getSimilar`：单曲级协同过滤相似推荐
//! - `artist.getSimilar`：歌手级协同过滤相似推荐（华语/外语覆盖都很广）
//! - `artist.getTopTracks`：歌手热门曲
//! - `user.getTopTracks` / `user.getLovedTracks`：用户云端常听与红心曲（可选）
//!
//! 凭据统一保存在本机 `<data_dir>/lastfm.json`（权限 600，绝不入库）。

use anyhow::{Context, Result};
use rmus_plugin_sdk::http;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::deezer::{Candidate, clean_title};

const BASE: &str = "https://ws.audioscrobbler.com/2.0/";

/// 本机 Last.fm 凭据配置（`<data_dir>/lastfm.json`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastFmConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub shared_secret: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub session_key: String,
}

impl LastFmConfig {
    pub fn is_configured(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    pub fn can_scrobble(&self) -> bool {
        !self.api_key.trim().is_empty()
            && !self.shared_secret.trim().is_empty()
            && !self.session_key.trim().is_empty()
    }
}

pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("lastfm.json")
}

/// 读取本机 Last.fm 凭据；文件缺失/损坏/未配 key 返回 `None`。
pub fn load_config(data_dir: &Path) -> Option<LastFmConfig> {
    let text = std::fs::read_to_string(config_path(data_dir)).ok()?;
    let cfg: LastFmConfig = serde_json::from_str(&text).ok()?;
    cfg.is_configured().then_some(cfg)
}

/// 原子保存本机 Last.fm 凭据，并锁定权限 0600（仅当前用户可读）。
pub fn save_config(data_dir: &Path, cfg: &LastFmConfig) -> Result<()> {
    let path = config_path(data_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("创建 Last.fm 配置目录失败")?;
    }
    let text = serde_json::to_string_pretty(cfg)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).context("写入临时配置失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).context("原子替换 lastfm.json 失败")?;
    Ok(())
}

/// 一条带匹配分数的相似单曲。
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarTrack {
    pub title: String,
    pub artist: String,
    pub score: f64,
}

fn urlencode(input: &str) -> String {
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

fn get_json(params: &[(&str, &str)]) -> Result<Value> {
    let query: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("{BASE}?{query}&format=json");
    let resp = http::Response::send(&http::Request::get(url))
        .context("Last.fm 请求失败")?
        .error_for_status()
        .context("Last.fm 返回非 2xx")?;
    let body = resp.read_to_end().context("读 Last.fm 响应失败")?;
    let v: Value = serde_json::from_slice(&body).context("解析 Last.fm JSON 失败")?;
    if let Some(err) = v.get("message").and_then(|m| m.as_str()) {
        anyhow::bail!("Last.fm API 错误：{err}");
    }
    Ok(v)
}

/// Last.fm 的 `match` 字段有时是 JSON 数字（`track.getSimilar`），有时是字符串（`artist.getSimilar`）。
fn parse_match(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn parse_similar_tracks(v: &Value, limit: usize) -> Vec<SimilarTrack> {
    v.get("similartracks")
        .and_then(|s| s.get("track"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let title = clean_title(item.get("name")?.as_str()?);
                    let artist = item
                        .get("artist")
                        .and_then(|a| a.get("name"))
                        .and_then(|n| n.as_str())?
                        .trim()
                        .to_string();
                    if title.is_empty() || artist.is_empty() {
                        return None;
                    }
                    let score = parse_match(item.get("match"));
                    Some(SimilarTrack {
                        title,
                        artist,
                        score,
                    })
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_similar_artists(v: &Value, limit: usize) -> Vec<(String, f64)> {
    v.get("similarartists")
        .and_then(|s| s.get("artist"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let name = item.get("name")?.as_str()?.trim().to_string();
                    if name.is_empty() {
                        return None;
                    }
                    let score = parse_match(item.get("match"));
                    Some((name, score))
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_track_list(v: &Value, root: &str, from: &'static str, limit: usize) -> Vec<Candidate> {
    v.get(root)
        .and_then(|s| s.get("track"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let title = clean_title(item.get("name")?.as_str()?);
                    let artist = item
                        .get("artist")
                        .and_then(|a| a.get("name"))
                        .and_then(|n| n.as_str())?
                        .trim()
                        .to_string();
                    if title.is_empty() || artist.is_empty() {
                        return None;
                    }
                    Some(Candidate {
                        title,
                        artist,
                        from,
                    })
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

/// 单曲级相似推荐（`track.getSimilar`）。
pub fn track_similar(
    api_key: &str,
    title: &str,
    artist: &str,
    limit: usize,
) -> Result<Vec<SimilarTrack>> {
    let limit_s = limit.to_string();
    let v = get_json(&[
        ("method", "track.getsimilar"),
        ("artist", artist),
        ("track", title),
        ("autocorrect", "1"),
        ("limit", &limit_s),
        ("api_key", api_key),
    ])?;
    Ok(parse_similar_tracks(&v, limit))
}

/// 歌手级相似推荐（`artist.getSimilar`）。
pub fn artist_similar(api_key: &str, artist: &str, limit: usize) -> Result<Vec<(String, f64)>> {
    let limit_s = limit.to_string();
    let v = get_json(&[
        ("method", "artist.getsimilar"),
        ("artist", artist),
        ("autocorrect", "1"),
        ("limit", &limit_s),
        ("api_key", api_key),
    ])?;
    Ok(parse_similar_artists(&v, limit))
}

/// 歌手热门曲（`artist.getTopTracks`）。
pub fn artist_top_tracks(api_key: &str, artist: &str, limit: usize) -> Result<Vec<Candidate>> {
    let limit_s = limit.to_string();
    let v = get_json(&[
        ("method", "artist.gettoptracks"),
        ("artist", artist),
        ("autocorrect", "1"),
        ("limit", &limit_s),
        ("api_key", api_key),
    ])?;
    Ok(parse_track_list(&v, "toptracks", "lastfm", limit))
}

/// 用户近 3 个月常听曲（`user.getTopTracks`，无需登录授权）。
pub fn user_top_tracks(api_key: &str, username: &str, limit: usize) -> Result<Vec<Candidate>> {
    let limit_s = limit.to_string();
    let v = get_json(&[
        ("method", "user.gettoptracks"),
        ("user", username),
        ("period", "3month"),
        ("limit", &limit_s),
        ("api_key", api_key),
    ])?;
    Ok(parse_track_list(&v, "toptracks", "lastfm_user", limit))
}

/// 用户红心曲（`user.getLovedTracks`，无需登录授权）。
pub fn user_loved_tracks(api_key: &str, username: &str, limit: usize) -> Result<Vec<Candidate>> {
    let limit_s = limit.to_string();
    let v = get_json(&[
        ("method", "user.getlovedtracks"),
        ("user", username),
        ("limit", &limit_s),
        ("api_key", api_key),
    ])?;
    Ok(parse_track_list(&v, "lovedtracks", "lastfm_loved", limit))
}

// ---- 签名、桌面授权与 Scrobble --------------------------------------------

/// 计算 Last.fm API 2.0 请求签名：参数按键名 ASCII 升序拼接 `k+v`，末尾拼 `secret` 后取小写 MD5。
pub fn api_sig(params: &[(&str, &str)], shared_secret: &str) -> String {
    let mut sorted: Vec<(&str, &str)> = params
        .iter()
        .copied()
        .filter(|(k, _)| *k != "format" && *k != "callback")
        .collect();
    sorted.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut buf = String::new();
    for (k, v) in sorted {
        buf.push_str(k);
        buf.push_str(v);
    }
    buf.push_str(shared_secret);
    md5_hex(buf.as_bytes())
}

fn post_signed(params: &[(&str, &str)], shared_secret: &str) -> Result<Value> {
    let sig = api_sig(params, shared_secret);
    let mut form_parts: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect();
    form_parts.push(format!("api_sig={sig}"));
    form_parts.push("format=json".to_string());
    let form = form_parts.join("&");

    let resp = http::Response::send(&http::Request::post_form(BASE, &form))
        .context("Last.fm POST 请求失败")?
        .error_for_status()
        .context("Last.fm POST 返回非 2xx")?;
    let body = resp.read_to_end().context("读 Last.fm POST 响应失败")?;
    let v: Value = serde_json::from_slice(&body).context("解析 Last.fm JSON 失败")?;
    if let Some(err) = v.get("message").and_then(|m| m.as_str()) {
        anyhow::bail!("Last.fm API 错误：{err}");
    }
    Ok(v)
}

/// 桌面授权第 1 步：申请一次性未授权 token。
pub fn get_token(cfg: &LastFmConfig) -> Result<String> {
    if cfg.api_key.trim().is_empty() || cfg.shared_secret.trim().is_empty() {
        anyhow::bail!("lastfm.json 缺少 api_key 或 shared_secret");
    }
    let params = [("api_key", cfg.api_key.as_str()), ("method", "auth.getToken")];
    let sig = api_sig(&params, &cfg.shared_secret);
    let v = get_json(&[
        ("method", "auth.getToken"),
        ("api_key", cfg.api_key.as_str()),
        ("api_sig", sig.as_str()),
    ])?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .context("Last.fm 未返回 token")
}

/// 桌面授权第 2 步：浏览器确认页链接。
pub fn auth_url(api_key: &str, token: &str) -> String {
    format!(
        "https://www.last.fm/api/auth/?api_key={}&token={}",
        urlencode(api_key),
        urlencode(token)
    )
}

/// 桌面授权第 3 步：用户浏览器点允许后，用 token 换永久 `session_key` 与用户名。
pub fn get_session(cfg: &LastFmConfig, token: &str) -> Result<(String, String)> {
    let params = [
        ("api_key", cfg.api_key.as_str()),
        ("method", "auth.getSession"),
        ("token", token),
    ];
    let sig = api_sig(&params, &cfg.shared_secret);
    let v = get_json(&[
        ("method", "auth.getSession"),
        ("api_key", cfg.api_key.as_str()),
        ("token", token),
        ("api_sig", sig.as_str()),
    ])?;
    let sess = v.get("session").context("Last.fm 未返回 session（请确认已在浏览器点击允许）")?;
    let key = sess
        .get("key")
        .and_then(|k| k.as_str())
        .context("session 缺少 key")?
        .to_string();
    let name = sess
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(&cfg.username)
        .to_string();
    Ok((name, key))
}

/// 上报「正在播放」（`track.updateNowPlaying`）。
pub fn update_now_playing(
    cfg: &LastFmConfig,
    title: &str,
    artist: &str,
    album: &str,
    duration_secs: u64,
) -> Result<()> {
    if !cfg.can_scrobble() || title.trim().is_empty() || artist.trim().is_empty() {
        return Ok(());
    }
    let dur_s = duration_secs.to_string();
    let mut params: Vec<(&str, &str)> = vec![
        ("api_key", cfg.api_key.as_str()),
        ("artist", artist),
        ("method", "track.updateNowPlaying"),
        ("sk", cfg.session_key.as_str()),
        ("track", title),
    ];
    if !album.trim().is_empty() {
        params.push(("album", album));
    }
    if duration_secs > 0 {
        params.push(("duration", &dur_s));
    }
    let _ = post_signed(&params, &cfg.shared_secret)?;
    Ok(())
}

/// 上报一条已听完的曲目（`track.scrobble`）。`timestamp_secs` 是开播 UNIX 秒。
pub fn scrobble(
    cfg: &LastFmConfig,
    title: &str,
    artist: &str,
    album: &str,
    duration_secs: u64,
    timestamp_secs: u64,
) -> Result<()> {
    if !cfg.can_scrobble() || title.trim().is_empty() || artist.trim().is_empty() {
        return Ok(());
    }
    let ts_s = timestamp_secs.to_string();
    let dur_s = duration_secs.to_string();
    let mut params: Vec<(&str, &str)> = vec![
        ("api_key", cfg.api_key.as_str()),
        ("artist", artist),
        ("method", "track.scrobble"),
        ("sk", cfg.session_key.as_str()),
        ("timestamp", &ts_s),
        ("track", title),
    ];
    if !album.trim().is_empty() {
        params.push(("album", album));
    }
    if duration_secs > 0 {
        params.push(("duration", &dur_s));
    }
    let _ = post_signed(&params, &cfg.shared_secret)?;
    Ok(())
}

/// 纯 Rust RFC 1321 MD5（仅用于 Last.fm API 签名，避免引入额外外部依赖）。
fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0): (u32, u32, u32, u32) =
        (0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476);

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_and_number_match_scores() {
        let artists = serde_json::json!({
            "similarartists": {
                "artist": [
                    { "name": "周深", "match": "1" },
                    { "name": "张碧晨", "match": 0.72 }
                ]
            }
        });
        let parsed = parse_similar_artists(&artists, 5);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "周深");
        assert!((parsed[0].1 - 1.0).abs() < 1e-6);
        assert!((parsed[1].1 - 0.72).abs() < 1e-6);

        let tracks = serde_json::json!({
            "similartracks": {
                "track": [
                    { "name": "大鱼 - 唱片版", "match": 0.85, "artist": { "name": "周深" } }
                ]
            }
        });
        let parsed_tracks = parse_similar_tracks(&tracks, 5);
        assert_eq!(parsed_tracks.len(), 1);
        assert_eq!(parsed_tracks[0].title, "大鱼");
        assert_eq!(parsed_tracks[0].artist, "周深");
    }

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
    }

    #[test]
    fn api_sig_sorts_params_and_ignores_format() {
        let sig1 = api_sig(
            &[("method", "auth.getToken"), ("api_key", "k1"), ("format", "json")],
            "sec",
        );
        let sig2 = api_sig(&[("api_key", "k1"), ("method", "auth.getToken")], "sec");
        assert_eq!(sig1, sig2);
        assert_eq!(sig1, md5_hex(b"api_keyk1methodauth.getTokensec"));
    }
}
