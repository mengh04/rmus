//! 封面临时文件：MPRIS 只接受 URL，所以把 tag 里的图片落到临时目录。

/// 封面临时文件前缀：/tmp/rmus-art-{pid}-{counter}.{ext}
const ART_PREFIX: &str = "rmus-art-";

/// 当前曲封面的临时文件管理。
///
/// 每次切歌写一个新文件（counter 递增，文件名变化让 MPRIS 客户端
/// 重新加载图片），并删除上一张。daemon 崩溃残留的孤儿文件在下次
/// 启动时按 /proc 探活清理。
pub(super) struct ArtCache {
    counter: u64,
    last_file: Option<std::path::PathBuf>,
}

impl ArtCache {
    pub(super) fn new() -> Self {
        Self {
            counter: 0,
            last_file: None,
        }
    }

    pub(super) fn store(&mut self, data: &[u8], ext: &str) -> Option<String> {
        let path = std::env::temp_dir().join(format!(
            "{ART_PREFIX}{}-{}.{ext}",
            std::process::id(),
            self.counter
        ));
        self.counter += 1;
        std::fs::write(&path, data).ok()?;
        if let Some(old) = self.last_file.take() {
            let _ = std::fs::remove_file(old);
        }
        self.last_file = Some(path.clone());
        url::Url::from_file_path(path).ok().map(|u| u.to_string())
    }

    /// 删除残留的孤儿封面（pid 已不存在的）
    pub(super) fn cleanup_orphans() {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(suffix) = name.strip_prefix(ART_PREFIX) else {
                continue;
            };
            let Some((pid, _)) = suffix.split_once('-') else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            // 自己 pid 的文件可能是刚启动就切歌写下的，不清理
            if pid == std::process::id() || std::path::Path::new(&format!("/proc/{pid}")).exists() {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
