//! 音乐库：~/Music 全量索引的持久化缓存 + 模糊匹配。
//!
//! 索引是 [`LibEntry`]（曲目元数据 + 预计算的拼音检索 key），落盘
//! $XDG_STATE_HOME/rmus/library.json。daemon 启动载入缓存即用，
//! `rmus lib rescan` 全量重建（扫描放 daemon 层阻塞线程池）。
//!
//! 匹配是 zoxide 式的：查询词（空格分词，AND 语义）对汉字字段和拼音 key
//! 打分，字段权重 title > artist > album（拼音 key 比同字段汉字低一档），
//! 得分档位 完全相等 > 前缀 > 子串 > subsequence；所有词得分之和为总分，
//! 按总分降序输出。拼音 key 让全拼（wanfeng）和首字母（wf）都能命中。

use crate::engine::queue::QueueItem;
use crate::scan;
use pinyin::ToPinyin;
use std::path::{Path, PathBuf};

/// 库根目录。单根设计：整个库就是 ~/Music。
pub fn root() -> PathBuf {
    let home = std::env::var_os("HOME").expect("无法确定 HOME");
    PathBuf::from(home).join("Music")
}

/// 库索引条目：曲目元数据 + 拼音检索 key。
/// `key: None` 只出现在旧版缓存里，`load` 时内存补全并回写。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LibEntry {
    #[serde(flatten)]
    pub item: QueueItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<PinyinKey>,
}

impl LibEntry {
    pub fn new(item: QueueItem) -> Self {
        Self {
            key: Some(PinyinKey::build(&item)),
            item,
        }
    }
}

/// 拼音检索 key：各字段的全拼（无声调）与首字母。不含汉字的字段为空串。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PinyinKey {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title_initials: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    artist: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    artist_initials: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    album: String,
}

impl PinyinKey {
    fn build(item: &QueueItem) -> Self {
        let (title, title_initials) = pinyin_keys(&item.title).unwrap_or_default();
        let (artist, artist_initials) = pinyin_keys(&item.artist).unwrap_or_default();
        let (album, _) = pinyin_keys(&item.album).unwrap_or_default();
        Self {
            title,
            title_initials,
            artist,
            artist_initials,
            album,
        }
    }

    /// 打分用的 (文本, 权重) 列表：全拼低于同字段汉字（30/20/10）一档，
    /// 首字母再低一档；专辑首字母太弱不参与。空字段跳过。
    fn fields(&self) -> Vec<(String, i64)> {
        [
            (&self.title, 20),
            (&self.title_initials, 10),
            (&self.artist, 10),
            (&self.artist_initials, 5),
            (&self.album, 5),
        ]
        .into_iter()
        .filter(|(text, _)| !text.is_empty())
        .map(|(text, weight)| (text.to_lowercase(), weight))
        .collect()
    }
}

/// 汉字转 (全拼, 首字母)。非汉字的字母数字穿透并小写，其余字符丢弃；
/// 不含汉字返回 None（纯英文字段无需拼音 key）。
/// 多音字取第一读音（重庆→chongqing）；漏字场景后续可上 ToPinyinMulti。
fn pinyin_keys(text: &str) -> Option<(String, String)> {
    let mut full = String::with_capacity(text.len() * 2);
    let mut initials = String::with_capacity(text.len());
    let mut has_han = false;
    for ch in text.chars() {
        if let Some(py) = ch.to_pinyin() {
            has_han = true;
            let plain = py.plain();
            full.push_str(plain);
            if let Some(initial) = plain.chars().next() {
                initials.push(initial);
            }
        } else if ch.is_ascii_alphanumeric() {
            full.push(ch.to_ascii_lowercase());
            initials.push(ch.to_ascii_lowercase());
        }
    }
    has_han.then_some((full, initials))
}

fn library_file() -> PathBuf {
    crate::state_dir().join("library.json")
}

pub fn load() -> Vec<LibEntry> {
    let Ok(text) = std::fs::read_to_string(library_file()) else {
        return Vec::new();
    };
    let mut entries: Vec<LibEntry> = match serde_json::from_str(&text) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("[rmusd] 库索引损坏，忽略: {e}");
            return Vec::new();
        }
    };
    // 旧版缓存没有拼音 key：内存补全并回写一次，此后启动零转换
    if entries.iter().any(|e| e.key.is_none()) {
        for entry in &mut entries {
            if entry.key.is_none() {
                entry.key = Some(PinyinKey::build(&entry.item));
            }
        }
        save(&entries);
    }
    entries
}

pub fn save(entries: &[LibEntry]) {
    let path = library_file();
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        eprintln!("[rmusd] 无法创建状态目录");
        return;
    }
    let text = match serde_json::to_string(entries) {
        Ok(t) => t,
        Err(_) => return,
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        eprintln!("[rmusd] 库索引落盘失败: {}", path.display());
    }
}

/// 全量重建索引（阻塞：读全库 tag + 转拼音，调用方放阻塞线程池）。
pub fn rescan() -> Vec<LibEntry> {
    let root: &Path = &root();
    let items = if root.is_dir() {
        scan::collect(std::slice::from_ref(&root.to_path_buf()))
    } else {
        Vec::new()
    };
    let entries: Vec<LibEntry> = items.into_iter().map(LibEntry::new).collect();
    save(&entries);
    entries
}

/// 模糊匹配：返回按相关度降序的曲目引用列表。
pub fn matches<'a>(entries: &'a [LibEntry], query: &str) -> Vec<&'a QueueItem> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(i64, usize, &LibEntry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| score_entry(entry, &words).map(|s| (s, i, entry)))
        .collect();
    // 分数降序；同分按库序稳定
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, entry)| &entry.item).collect()
}

/// 每个词都必须命中（AND），总分为各词最佳字段得分之和。
fn score_entry(entry: &LibEntry, words: &[String]) -> Option<i64> {
    let item = &entry.item;
    let mut fields: Vec<(String, i64)> = vec![
        (item.title.to_lowercase(), 30),
        (item.artist.to_lowercase(), 20),
        (item.album.to_lowercase(), 10),
    ];
    if let Some(key) = &entry.key {
        fields.extend(key.fields());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, artist: &str) -> QueueItem {
        QueueItem {
            path: format!("/{title}.flac").into(),
            title: title.into(),
            artist: artist.into(),
            album: "专辑".into(),
            duration_ms: 0,
        }
    }

    #[test]
    fn pinyin_keys_全拼与首字母() {
        assert_eq!(
            pinyin_keys("晚风心里吹"),
            Some(("wanfengxinlichui".into(), "wfxlc".into()))
        );
        // 非 Han 穿透小写；标点丢弃；纯英文没有 key
        assert_eq!(
            pinyin_keys("A-Lin 恋人"),
            Some(("alinlianren".into(), "alinlr".into()))
        );
        assert_eq!(pinyin_keys("Hello World"), None);
    }

    #[test]
    fn 匹配_全拼_首字母_汉字() {
        let lib = vec![LibEntry::new(item("晚风心里吹", "张磊"))];
        for query in ["晚风", "wanfeng", "wfxc", "wf", "wanf", "zhanglei wf"] {
            assert_eq!(matches(&lib, query).len(), 1, "query: {query}");
        }
        assert!(matches(&lib, "zhangwei").is_empty());
    }
}
