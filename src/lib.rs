//! rmus：客户端/服务器分离的终端音乐播放器。
//!
//! - `rmusd`：后台守护进程（解码 + 输出 + 队列），监听 Unix socket
//! - `rmus`：TUI 客户端（第二阶段加入）
//! - 协议：NDJSON，见 [`proto`]

pub mod client;
pub mod engine;
pub mod proto;
pub mod scan;

use std::path::PathBuf;

/// rmusd 监听的 socket 路径。
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("rmus.sock")
}

/// 状态（命名队列等）目录。
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local").join("state"))
                .unwrap_or_else(|| PathBuf::from(".local").join("state"))
        })
        .join("rmus")
}
