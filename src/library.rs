//! 音乐库：~/Music 全量索引的持久化缓存 + 模糊匹配。
//!
//! 索引是 Vec<QueueItem>（复用 scan 的元数据读取），落盘
//! $XDG_STATE_HOME/rmus/library.json。daemon 启动载入缓存即用，
//! `rmus lib rescan` 全量重建（扫描放 daemon 层阻塞线程池）。
//!
//! 匹配是 zoxide 式的：查询词（空格分词，AND 语义）对 title/artist/album
//! 打分，字段权重 title > artist > album，得分档位 完全相等 > 前缀 >
//! 子串 > subsequence；所有词得分之和为总分，按总分降序输出。

use crate::engine::queue::QueueItem;
use crate::scan;
use std::path::{Path, PathBuf};

/// 库根目录。单根设计：整个库就是 ~/Music。
pub fn root() -> PathBuf {
    let home = std::env::var_os("HOME").expect("无法确定 HOME");
    PathBuf::from(home).join("Music")
}

fn library_file() -> PathBuf {
    crate::state_dir().join("library.json")
}

pub fn load() -> Vec<QueueItem> {
    let Ok(text) = std::fs::read_to_string(library_file()) else {
        return Vec::new();
    };
    match serde_json::from_str(&text) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("[rmusd] 库索引损坏，忽略: {e}");
            Vec::new()
        }
    }
}

pub fn save(items: &[QueueItem]) {
    let path = library_file();
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        eprintln!("[rmusd] 无法创建状态目录");
        return;
    }
    let text = match serde_json::to_string(items) {
        Ok(t) => t,
        Err(_) => return,
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        eprintln!("[rmusd] 库索引落盘失败: {}", path.display());
    }
}

/// 全量重建索引（阻塞：读全库 tag，调用方放阻塞线程池）。
pub fn rescan() -> Vec<QueueItem> {
    let root: &Path = &root();
    let items = if root.is_dir() {
        scan::collect(std::slice::from_ref(&root.to_path_buf()))
    } else {
        Vec::new()
    };
    save(&items);
    items
}

/// 模糊匹配：返回按相关度降序的引用列表。
pub fn matches<'a>(items: &'a [QueueItem], query: &str) -> Vec<&'a QueueItem> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(i64, usize, &QueueItem)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| score_track(item, &words).map(|s| (s, i, item)))
        .collect();
    // 分数降序；同分按库序稳定
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

/// 每个词都必须命中（AND），总分为各词最佳字段得分之和。
fn score_track(item: &QueueItem, words: &[String]) -> Option<i64> {
    let fields = [
        (item.title.to_lowercase(), 3),
        (item.artist.to_lowercase(), 2),
        (item.album.to_lowercase(), 1),
    ];
    let mut total = 0;
    for word in words {
        let best = fields
            .iter()
            .filter_map(|(field, weight)| score_field(field, word).map(|s| s * weight))
            .max()?;
        total += best;
    }
    Some(total)
}

/// 单字段得分档位。命中不了任何档位返回 None。
fn score_field(field: &str, word: &str) -> Option<i64> {
    if field == word {
        Some(1000)
    } else if field.starts_with(word) {
        Some(100)
    } else if field.contains(word) {
        Some(50)
    } else if is_subsequence(word, field) {
        Some(10)
    } else {
        None
    }
}

/// needle 的字符按序出现在 haystack 中（不必连续）。
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}
