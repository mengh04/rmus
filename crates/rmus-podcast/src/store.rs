//! 订阅数据模型与 JSON 存取（`data_dir()/podcasts/subscriptions.json`）。
//!
//! 模型抄 AntennaPod 的三层精简版：Podcast（feed_url 即身份）/
//! Episode（guid 即身份，缺失回退 hash(enclosure_url)）。进度/已播/
//! 下载状态挂在 Episode 上，feed 刷新时按 guid 合并保留。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub podcasts: Vec<Podcast>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Podcast {
    /// feed URL，订阅身份（退订/刷新的寻址键）
    pub feed_url: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    /// 身份；feed 缺 guid 时由 enclosure URL 哈希补位
    pub guid: String,
    pub title: String,
    /// 显示用日期（MM-DD；解析失败存原文）
    #[serde(default)]
    pub pub_date: String,
    pub enclosure_url: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub played: bool,
    /// 听到哪（毫秒；worker 采样回写）
    #[serde(default)]
    pub position_ms: u64,
    /// 已下载的落盘文件名（相对本播客目录；下载功能后续版本接入）
    #[serde(default)]
    pub downloaded: Option<String>,
}

impl Episode {
    /// guid 缺失时的身份回退：hash(enclosure_url)。
    pub fn fallback_guid(enclosure_url: &str) -> String {
        let mut hasher = DefaultHasher::new();
        enclosure_url.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// 未播完判定（播放中的显示 ▶ 百分比）。
    pub fn in_progress(&self) -> bool {
        !self.played && self.position_ms > 0
    }
}

impl Store {
    pub fn find_mut(&mut self, feed_url: &str) -> Option<&mut Podcast> {
        self.podcasts.iter_mut().find(|p| p.feed_url == feed_url)
    }

    /// feed 刷新合并：按 guid 匹配保留本地状态（played/position/
    /// downloaded），新集按 feed 顺序插入，feed 里已消失的集丢弃。
    pub fn merge_episodes(&mut self, feed_url: &str, title: String, author: String, fresh: Vec<Episode>) {
        let Some(p) = self.find_mut(feed_url) else {
            self.podcasts.push(Podcast {
                feed_url: feed_url.to_string(),
                title,
                author,
                episodes: fresh,
            });
            return;
        };
        p.title = title;
        p.author = author;
        p.episodes = fresh
            .into_iter()
            .map(|ep| {
                match p.episodes.iter().find(|old| old.guid == ep.guid) {
                    Some(old) => Episode {
                        played: old.played,
                        position_ms: old.position_ms,
                        downloaded: old.downloaded.clone(),
                        ..ep
                    },
                    None => ep,
                }
            })
            .collect();
    }
}

fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join("podcasts").join("subscriptions.json")
}

/// 读取订阅库；文件不存在/损坏返回空库（不报错——首启体验）。
pub fn load(data_dir: &Path) -> Store {
    let bytes = match std::fs::read(store_path(data_dir)) {
        Ok(b) => b,
        Err(_) => return Store::default(),
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// 原子写：临时文件 + rename（半截文件不会污染订阅库）。
pub fn save(data_dir: &Path, store: &Store) -> anyhow::Result<()> {
    let path = store_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(store)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
