//! 库目录监听：文件变化后防抖、全量重建索引（inotify / FSEvents / RDCW）。

use crate::Library;
use anyhow::{Context, Result};
use rmus::library;
use std::sync::mpsc as std_mpsc;

const LIB_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

/// 起后台线程监听库目录：文件变化后防抖、全量重建索引。
/// 专用阻塞线程（事件等待 + 扫描都是阻塞操作，不占 tokio worker）。
/// 静息态零轮询（inotify 推送）；监听失败只降级警告，手动 rescan 仍可用。
pub(crate) fn spawn_library_watcher(library: Library) -> Result<()> {
    let root = library::root();
    std::thread::Builder::new()
        .name("lib-watch".into())
        .spawn(move || {
            let (tx, rx) = std_mpsc::channel::<()>();
            use notify::Watcher;
            let mut watcher = match notify::recommended_watcher(move |_res| {
                // 事件内容不重要（增删改都导致重扫），只用来喂防抖
                let _ = tx.send(());
            }) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("[rmusd] 库监听不可用（rmus lib rescan 手动更新）: {e}");
                    return;
                }
            };
            if let Err(e) = watcher.watch(root.as_path(), notify::RecursiveMode::Recursive) {
                eprintln!("[rmusd] 监听 {} 失败: {e}", root.display());
                return;
            }
            loop {
                if rx.recv().is_err() {
                    break;
                }
                while rx.recv_timeout(LIB_DEBOUNCE).is_ok() {}
                let items = library::rescan();
                println!("[rmusd] 库已自动更新：{} 首", items.len());
                *library.write().unwrap() = items;
            }
        })
        .context("启动库监听线程失败")?;
    Ok(())
}
