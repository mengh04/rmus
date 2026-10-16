//! 队列添加时的文件收集：递归扫描目录、按扩展名过滤、lofty 读元数据。

use crate::engine::queue::QueueItem;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Accessor;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// 支持的音频扩展名（小写）。opus/aac 容器先收录，解码器支持情况见 decoder。
const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "ogg", "oga", "opus", "wav", "m4a", "aac", "aiff", "aif", "alac",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// 收集路径列表中的音频文件（目录递归，按路径排序），读取元数据。
/// 不存在的文件直接跳过（相对路径错误/已删除），单文件元数据读不动时
/// 用文件名兜底，不让一首坏歌挡住整个 add。
pub fn collect(paths: &[PathBuf]) -> Vec<QueueItem> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            collect_dir(path, &mut files);
        } else if is_audio_file(path) && path.is_file() {
            files.push(path.clone());
        }
    }
    files.sort();
    files.iter().map(|p| item_for(p)).collect()
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    // 迭代式遍历，避免深目录递归栈溢出
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("[rmusd] 无法读取目录: {}", dir.display());
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                queue.push_back(path);
            } else if is_audio_file(&path) {
                out.push(path);
            }
        }
    }
}

fn item_for(path: &Path) -> QueueItem {
    let (title, artist, album, duration_ms) = read_metadata(path).unwrap_or_else(|| {
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        (title, String::new(), String::new(), 0)
    });
    QueueItem {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        duration_ms,
    }
}

fn read_metadata(path: &Path) -> Option<(String, String, String, u64)> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let duration_ms = tagged.properties().duration().as_millis() as u64;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    Some((
        tag.title().map(|t| t.into_owned()).unwrap_or_default(),
        tag.artist().map(|t| t.into_owned()).unwrap_or_default(),
        tag.album().map(|t| t.into_owned()).unwrap_or_default(),
        duration_ms,
    ))
}
