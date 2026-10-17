//! rmus 客户端库：连接 rmusd、发送请求、收取响应。
//!
//! CLI 与以后的 TUI 共用。当前为同步实现（std UnixStream），
//! 每个连接上 daemon 也会推送事件行，读响应时会跳过它们。

use crate::proto::{Event, Request, RequestMsg};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod tui;

pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Client {
    /// 直接连接（不自动拉起 daemon）。
    pub fn connect(socket: Option<PathBuf>) -> Result<Self> {
        let path = socket.unwrap_or_else(crate::socket_path);
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("无法连接 {}（rmusd 未运行？）", path.display()))?;
        Ok(Self::new(stream))
    }

    /// 连接默认 socket；连不上时自动拉起 rmusd 再重试。
    pub fn connect_or_spawn(socket: Option<PathBuf>) -> Result<Self> {
        let path = socket.unwrap_or_else(crate::socket_path);
        match UnixStream::connect(&path) {
            Ok(stream) => Ok(Self::new(stream)),
            Err(first_err) => {
                spawn_daemon(&path, &first_err)?;
                // 等 daemon 起来
                let mut last_err = first_err;
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(stream) = UnixStream::connect(&path) {
                        return Ok(Self::new(stream));
                    }
                    last_err = std::io::Error::other("rmusd 启动超时");
                }
                Err(last_err).context("拉起 rmusd 后仍无法连接")
            }
        }
    }

    fn new(stream: UnixStream) -> Self {
        let reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
        Self {
            stream,
            reader,
            next_id: 1,
        }
    }

    /// 发送请求并等待响应。成功返回 `result` 字段，失败返回 daemon 的错误文本。
    pub fn request(&mut self, request: Request) -> Result<serde_json::Value> {
        let msg = RequestMsg {
            id: self.next_id,
            request,
        };
        self.next_id += 1;
        let line = serde_json::to_string(&msg)?;
        self.stream.write_all(line.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                bail!("与 rmusd 的连接已关闭");
            }
            let value: serde_json::Value = match serde_json::from_str(buf.trim_end()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // 跳过事件推送行，只认自己 id 的响应
            if value.get("id").and_then(|v| v.as_u64()) != Some(msg.id) {
                continue;
            }
            if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                bail!("{error}");
            }
            return Ok(value
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
    }

    /// 在专用事件连接上阻塞等待 daemon 推送的下一条事件。
    ///
    /// 该连接只收不发；若读到响应行（理论不该出现）则跳过。
    /// 返回 Err 表示连接已断开（daemon 退出）。
    pub fn wait_event(&mut self) -> Result<Event> {
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                bail!("与 rmusd 的连接已关闭");
            }
            let value: serde_json::Value = match serde_json::from_str(buf.trim_end()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // 带 id 的是响应行，此连接上不期望出现，跳过
            if value.get("id").is_some() {
                continue;
            }
            return serde_json::from_value(value).context("解析 daemon 事件失败");
        }
    }
}

/// 尝试拉起一个后台 rmusd（detach，独立进程组，不随本客户端退出）。
fn spawn_daemon(socket: &Path, connect_err: &std::io::Error) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    // 陈旧 socket 文件会导致 bind 失败，rmusd 自己会处理（connect 失败则删除），
    // 这里只需要找到可执行文件
    let exe = find_rmusd().with_context(|| {
        format!("无法连接 rmusd（{connect_err}），也找不到 rmusd 可执行文件来自动拉起")
    })?;

    Command::new(exe)
        .arg("--socket")
        .arg(socket)
        .process_group(0) // 独立进程组：终端退出/客户端退出都不影响 daemon
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("拉起 rmusd 失败")?;
    Ok(())
}

/// 查找 rmusd：先 PATH，再本程序所在目录（cargo run / target 布局）。
fn find_rmusd() -> Option<PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("rmusd");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let exe = std::env::current_exe().ok()?;
    let sibling = exe.parent()?.join("rmusd");
    sibling.is_file().then_some(sibling)
}
