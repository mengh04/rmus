//! Deezer 公开 API（免 key）：全球榜单 + 歌手热门曲。
//!
//! 只做候选提名，下载仍走 rmus-sources 搜索源验证——榜单数据本身
//! 不落盘。API 形状（均返回 `{"data": [...]}`）：
//! - `GET /chart/0/tracks` → 热门曲（含 `artist.name`）
//! - `GET /search/artist?q=` → 歌手检索（取 id）
//! - `GET /artist/{id}/top?limit=` → 歌手热门曲

use anyhow::{Context, Result};
use rmus_plugin_sdk::http;
use serde_json::Value;

const BASE: &str = "https://api.deezer.com";

/// Deezer 查询参数编码（中文歌手名等非 ASCII 百分号化）。
fn urlencode_lite(input: &str) -> String {
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

/// 一条候选提名（来源标注用于展示与排序）。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub title: String,
    pub artist: String,
    /// `chart`（全球榜单）或 `artist`（歌手热门）
    pub from: &'static str,
}

fn get_json(path: &str) -> Result<Value> {
    let resp = http::Response::send(&http::Request::get(format!("{BASE}{path}")))
        .context("Deezer 请求失败")?
        .error_for_status()
        .context("Deezer 返回非 2xx")?;
    let body = resp.read_to_end().context("读 Deezer 响应失败")?;
    serde_json::from_slice(&body).context("解析 Deezer JSON 失败")
}

/// Deezer 歌名常带 ` - Single Version` / `(Live at …)` 后缀，
/// 拿去搜索源验证会坏事，切掉再提名。
pub(crate) fn clean_title(t: &str) -> String {
    let mut out = t.to_string();
    for sep in [" - ", " (", " ["] {
        if let Some(pos) = out.find(sep) {
            out.truncate(pos);
        }
    }
    out.trim().to_string()
}

fn track_to_candidate(v: &Value, from: &'static str) -> Option<Candidate> {
    let title = clean_title(v.get("title")?.as_str()?);
    let artist = v
        .get("artist")
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())?
        .trim()
        .to_string();
    if title.is_empty() || artist.is_empty() {
        return None;
    }
    Some(Candidate { title, artist, from })
}

/// 全球榜单热门曲（默认前 50，`data` 顺序即热度序）。
pub fn chart_tracks(limit: usize) -> Result<Vec<Candidate>> {
    let v = get_json("/chart/0/tracks")?;
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| track_to_candidate(t, "chart"))
                .take(limit)
                .collect()
        })
        .unwrap_or_default())
}

/// 歌手名 → Deezer id。要求名字严格一致（忽略大小写），防止
/// “周深” 搜出无关歌手把热门曲全带偏。
pub fn search_artist_id(name: &str) -> Result<Option<u64>> {
    let q = urlencode_lite(name);
    let v = get_json(&format!("/search/artist?q={q}"))?;
    let id = v
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|a| {
                let hit_name = a.get("name")?.as_str()?.trim();
                if hit_name.eq_ignore_ascii_case(name.trim()) {
                    a.get("id")?.as_u64()
                } else {
                    None
                }
            })
        });
    Ok(id)
}

/// 歌手热门曲（按 Deezer 播放量序）。
pub fn artist_top(id: u64, limit: usize) -> Result<Vec<Candidate>> {
    let v = get_json(&format!("/artist/{id}/top?limit={limit}"))?;
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| track_to_candidate(t, "artist"))
                .take(limit)
                .collect()
        })
        .unwrap_or_default())
}

/// 相似歌手（Deezer 免费协同过滤接口，免 key）。返回 `(artist_id, name)`。
pub fn artist_related(id: u64, limit: usize) -> Result<Vec<(u64, String)>> {
    let v = get_json(&format!("/artist/{id}/related?limit={limit}"))?;
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let aid = a.get("id")?.as_u64()?;
                    let name = a.get("name")?.as_str()?.trim().to_string();
                    (!name.is_empty()).then_some((aid, name))
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_suffixes() {
        assert_eq!(clean_title("Lemon - TV Size"), "Lemon");
        assert_eq!(clean_title("Viva La Vida (Live at Barclay)"), "Viva La Vida");
        assert_eq!(clean_title("大鱼"), "大鱼");
        assert_eq!(clean_title("  消愁  "), "消愁");
    }
}
