//! 队列 ↔ 播放列表的边界。整个模块就做三件事，而且各走各的路：
//!
//! - `save`：队列内容 → 列表（唯一的写入口，同名要显式覆盖）
//! - `load`：列表 → 队列的一份副本（播放）
//! - `append`：若干曲 → 列表（完全不碰队列）
//!
//! 反方向永远不自动发生：队列的增删移排序都不回写列表（想留就 `save`），
//! 列表的增删也不拿队列当默认目标。放一个新模块而不是散在 handlers 里，
//! 是为了让"谁写谁"一眼可见——上一版就是这里串了线。

use super::Engine;
use super::queue::QueueItem;
use std::collections::BTreeMap;

pub(super) type Playlists = BTreeMap<String, Vec<QueueItem>>;

/// 队列内容是否已偏离它的来源列表。
///
/// 只比路径序列：队列被增删排序都算偏离；来源列表被删、或队列本就没有
/// 出处（匿名）时不算——没得比就不该报"变了"。
pub(super) fn is_modified(
    playlists: &Playlists,
    origin: Option<&str>,
    items: &[QueueItem],
) -> bool {
    let Some(name) = origin else {
        return false;
    };
    let Some(saved) = playlists.get(name) else {
        return false;
    };
    saved.len() != items.len() || saved.iter().zip(items).any(|(a, b)| a.path != b.path)
}

/// 把队列写进名为 `name` 的列表。同名已存在时必须 `overwrite` 才落盘：
/// 否则"队列改了几首，顺手 save 一下"就会静默冲掉整张列表。
pub(super) fn save(
    playlists: &mut Playlists,
    name: &str,
    items: &[QueueItem],
    overwrite: bool,
) -> Result<(), String> {
    if playlists.contains_key(name) && !overwrite {
        return Err(format!(
            "播放列表 {name} 已存在（覆盖请显式确认，或换个名字另存）"
        ));
    }
    playlists.insert(name.to_string(), items.to_vec());
    Ok(())
}

/// 取列表内容作为队列副本。
pub(super) fn load(playlists: &Playlists, name: &str) -> Result<Vec<QueueItem>, String> {
    let Some(items) = playlists.get(name) else {
        return Err(format!("播放列表 {name} 不存在"));
    };
    if items.is_empty() {
        return Err(format!("播放列表 {name} 是空的"));
    }
    Ok(items.clone())
}

/// 把曲目追加进列表，按路径去重（同一条目的重复加入没有意义）。
/// 返回 `(newly_added, skipped)`。
pub(super) fn append(
    playlists: &mut Playlists,
    name: &str,
    items: Vec<QueueItem>,
) -> (usize, usize) {
    let list = playlists.entry(name.to_string()).or_default();
    let mut added = 0;
    let mut skipped = 0;
    for item in items {
        if list.iter().any(|existing| existing.path == item.path) {
            skipped += 1;
        } else {
            list.push(item);
            added += 1;
        }
    }
    (added, skipped)
}

impl Engine {
    /// 队列内容是否已偏离它的来源列表（顶栏 `*`、pl list 的 `*`）
    pub(super) fn queue_modified(&self) -> bool {
        is_modified(
            &self.playlists,
            self.queue_origin.as_deref(),
            &self.queue.items,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: usize) -> QueueItem {
        QueueItem {
            path: format!("/{n}.flac").into(),
            title: format!("曲{n}"),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
        }
    }

    fn lists() -> Playlists {
        let mut m = Playlists::new();
        m.insert("A".into(), vec![item(0), item(1)]);
        m
    }

    #[test]
    fn 没改过就不算偏离() {
        let l = lists();
        assert!(!is_modified(&l, Some("A"), &[item(0), item(1)]));
        // 匿名队列永远不算偏离
        assert!(!is_modified(&l, None, &[item(9)]));
        // 出处列表被删了也不算
        assert!(!is_modified(&l, Some("B"), &[item(9)]));
    }

    #[test]
    fn 增删排序都算偏离() {
        let l = lists();
        assert!(is_modified(&l, Some("A"), &[item(0)]), "少一首");
        assert!(
            is_modified(&l, Some("A"), &[item(0), item(1), item(2)]),
            "多一首"
        );
        assert!(is_modified(&l, Some("A"), &[item(1), item(0)]), "顺序变了");
    }

    #[test]
    fn 保存默认不覆盖同名() {
        let mut l = lists();
        let err = save(&mut l, "A", &[item(7)], false).unwrap_err();
        assert!(err.contains("已存在"), "{err}");
        assert_eq!(l["A"].len(), 2, "被拒绝的保存不能动原列表");
    }

    #[test]
    fn 显式覆盖才写进去() {
        let mut l = lists();
        save(&mut l, "A", &[item(7)], true).unwrap();
        assert_eq!(l["A"].len(), 1);
        assert_eq!(l["A"][0].path, item(7).path);
    }

    #[test]
    fn 覆盖后不再偏离() {
        let mut l = lists();
        let mut q = vec![item(0), item(2)];
        assert!(is_modified(&l, Some("A"), &q));
        save(&mut l, "A", &q, true).unwrap();
        assert!(!is_modified(&l, Some("A"), &q));
        // 再动队列又偏离
        q.push(item(3));
        assert!(is_modified(&l, Some("A"), &q));
    }

    #[test]
    fn 追加按路径去重() {
        let mut l = lists();
        let (added, skipped) = append(&mut l, "A", vec![item(1), item(2), item(2)]);
        assert_eq!((added, skipped), (1, 2));
        assert_eq!(l["A"].len(), 3);
        assert_eq!(l["A"][2].path, item(2).path);
    }

    #[test]
    fn 追加到不存在的名字等于新建() {
        let mut l = lists();
        let (added, skipped) = append(&mut l, "B", vec![item(5)]);
        assert_eq!((added, skipped), (1, 0));
        assert_eq!(l["B"].len(), 1);
    }

    #[test]
    fn 空列表不能播() {
        let mut l = lists();
        l.insert("空".into(), Vec::new());
        assert!(load(&l, "空").unwrap_err().contains("空的"));
        assert!(load(&l, "没这张").unwrap_err().contains("不存在"));
    }
}
