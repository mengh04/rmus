//! 插件宿主：发现插件、托管会话、转发调用与事件。
//!
//! 会话 = 一个 `--rmus-plugin` 长驻子进程，stdin/stdout 走 NDJSON。宿主
//! 对插件能力零知识：方法名与参数原样透传，只有 manifest 里的
//! `kind`（call/task）决定宿主等待响应还是立即返回任务号。
//!
//! 并发模型：每会话一个 stdin 写任务 + 一个 stdout 读任务；请求挂到
//! `pending` 表（id -> Pending），读任务按 id 路由响应——短调用唤醒
//! 等待者，长任务包装成 `Event::Plugin` 的 done/failed 广播；插件的
//! 通知行原样转发为事件。会话懒启动、空闲回收、退出时清理。

use anyhow::Context;
use rmus::plugin::{self, MethodKind, PluginManifest, PluginNotify, PluginReply};
use rmus::proto::Event;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinSet;

/// 已发现的一个插件。
struct Discovered {
    manifest: PluginManifest,
    path: PathBuf,
}

/// 插件宿主（廉价克隆，内部共享）。
#[derive(Clone)]
pub(crate) struct PluginHost(Arc<Inner>);

struct Inner {
    plugins: Vec<Discovered>,
    /// 会话表单独 Arc：stdout 读任务退出时要回来摘除自己
    sessions: Arc<tokio::sync::Mutex<HashMap<String, Arc<Session>>>>,
    events_tx: broadcast::Sender<Event>,
}

/// 挂起的请求：短调用等回执，长任务只等事件。
struct Pending {
    reply: oneshot::Sender<Result<serde_json::Value, String>>,
    task: bool,
}

/// 一个存活中的插件会话。
struct Session {
    name: String,
    /// 只为 kill 服务；stdin/stdout 在 spawn 时已被拆走
    child: Mutex<tokio::process::Child>,
    /// 协议行队列，由独立写任务串行写出
    stdin: mpsc::Sender<String>,
    /// 发出后未收到响应的请求
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    last_used: Mutex<std::time::Instant>,
}

impl Session {
    fn touch(&self) {
        *self.last_used.lock().unwrap() = std::time::Instant::now();
    }

    /// 无 pending 且久未活动才允许回收（下载中不许回收）。
    fn is_idle(&self) -> bool {
        self.pending.lock().unwrap().is_empty()
            && self.last_used.lock().unwrap().elapsed() > plugin::IDLE_TIMEOUT
    }
}

impl PluginHost {
    /// 扫描插件目录并逐个握手。失败/不合规的候选静默跳过（stderr 留痕）。
    pub async fn discover(events_tx: broadcast::Sender<Event>) -> Self {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for dir in plugin::plugin_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if is_executable_file(&path) {
                    candidates.push(path);
                }
            }
        }

        let mut handles = JoinSet::new();
        for path in candidates {
            handles.spawn(async move { handshake(path).await });
        }
        let mut plugins: Vec<Discovered> = Vec::new();
        while let Some(joined) = handles.join_next().await {
            match joined {
                Ok(Ok(Some(found))) => {
                    if plugins
                        .iter()
                        .any(|p| p.manifest.name == found.manifest.name)
                    {
                        eprintln!(
                            "[rmusd] 插件 {} 重复发现，忽略 {}",
                            found.manifest.name,
                            found.path.display()
                        );
                        continue;
                    }
                    println!(
                        "[rmusd] 插件: {} {}（{}）",
                        found.manifest.name,
                        found.manifest.version,
                        found
                            .manifest
                            .methods
                            .iter()
                            .map(|m| m.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    plugins.push(found);
                }
                Ok(Ok(None)) | Ok(Err(_)) => {}
                Err(e) => eprintln!("[rmusd] 插件握手任务异常: {e}"),
            }
        }

        let host = Self(Arc::new(Inner {
            plugins,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            events_tx,
        }));
        tokio::spawn(reaper(host.clone()));
        host
    }

    /// 已发现插件的清单（客户端 `plugin list`）。
    pub fn list(&self) -> serde_json::Value {
        serde_json::json!({
            "plugins": self.0.plugins.iter().map(|d| serde_json::json!({
                "name": d.manifest.name,
                "version": d.manifest.version,
                "methods": d.manifest.methods,
            })).collect::<Vec<_>>()
        })
    }

    /// 是否一个插件都没有。
    pub fn is_empty(&self) -> bool {
        self.0.plugins.is_empty()
    }

    /// 调用插件方法。短调用等响应；长任务立即返回 `{"call":N}`，
    /// 结果随后以 `Event::Plugin`（done/failed）广播。
    pub async fn call(
        &self,
        name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let session = self.session(name).await?;
        let task = self
            .0
            .plugins
            .iter()
            .find(|p| p.manifest.name == name)
            .and_then(|p| p.manifest.method_kind(method))
            == Some(MethodKind::Task);

        let id = session.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        session
            .pending
            .lock()
            .unwrap()
            .insert(id, Pending { reply: tx, task });
        session.touch();
        let line = serde_json::json!({"id": id, "method": method, "params": params}).to_string();
        if session.stdin.send(line).await.is_err() {
            session.pending.lock().unwrap().remove(&id);
            return Err("插件进程已退出".into());
        }

        if task {
            Ok(serde_json::json!({"call": id}))
        } else {
            match tokio::time::timeout(plugin::CALL_TIMEOUT, rx).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => Err("插件会话已中断".into()),
                Err(_) => {
                    session.pending.lock().unwrap().remove(&id);
                    Err("插件响应超时".into())
                }
            }
        }
    }

    /// 取消长任务。会话不存在 = 无事可做。
    pub async fn cancel(&self, name: &str, call: u64) -> Result<serde_json::Value, String> {
        let existing = self.0.sessions.lock().await.get(name).cloned();
        match existing {
            None => Ok(serde_json::json!({"cancelled": false})),
            Some(session) => {
                let id = session.next_id.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = oneshot::channel();
                session.pending.lock().unwrap().insert(
                    id,
                    Pending {
                        reply: tx,
                        task: false,
                    },
                );
                session.touch();
                let line =
                    serde_json::json!({"id": id, "method": "cancel", "params": {"call": call}})
                        .to_string();
                if session.stdin.send(line).await.is_err() {
                    session.pending.lock().unwrap().remove(&id);
                    return Err("插件进程已退出".into());
                }
                match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
                    Ok(Ok(reply)) => reply,
                    Ok(Err(_)) => Err("插件会话已中断".into()),
                    Err(_) => {
                        session.pending.lock().unwrap().remove(&id);
                        Err("取消请求超时".into())
                    }
                }
            }
        }
    }

    /// 退出前清理：杀掉所有会话子进程（process::exit 不跑析构）。
    pub fn shutdown(&self) {
        if let Ok(sessions) = self.0.sessions.try_lock() {
            for (name, session) in sessions.iter() {
                if let Ok(mut child) = session.child.lock() {
                    let _ = child.start_kill();
                }
                eprintln!("[rmusd] 插件 {name} 会话已终止");
            }
        }
    }

    /// 取会话（懒启动）。启动失败不缓存，下次调用重试。
    async fn session(&self, name: &str) -> Result<Arc<Session>, String> {
        let mut sessions = self.0.sessions.lock().await;
        if let Some(s) = sessions.get(name) {
            return Ok(Arc::clone(s));
        }
        let found = self
            .0
            .plugins
            .iter()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| format!("未知插件 “{name}”"))?;
        let mut command = tokio::process::Command::new(&found.path);
        command
            .arg("--rmus-plugin")
            .env("RMUS_PLUGIN_NAME", name)
            .env("RMUS_LIBRARY_DIR", rmus::library::root())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr 直通 daemon 日志：插件的调试输出 / 错误栈都在这
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| format!("启动插件 {name} 失败: {e}"))?;
        let stdin = child.stdin.take().ok_or("插件缺少 stdin")?;
        let stdout = child.stdout.take().ok_or("插件缺少 stdout")?;

        let (line_tx, line_rx) = mpsc::channel::<String>(32);
        tokio::spawn(stdin_writer(stdin, line_rx));
        let session = Arc::new(Session {
            name: name.to_string(),
            child: Mutex::new(child),
            stdin: line_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            last_used: Mutex::new(std::time::Instant::now()),
        });
        tokio::spawn(stdout_reader(
            Arc::clone(&session),
            stdout,
            Arc::clone(&self.0.sessions),
            self.0.events_tx.clone(),
        ));
        sessions.insert(name.to_string(), Arc::clone(&session));
        Ok(session)
    }
}
/// 握手：以 `--rmus-manifest` 调用候选，解析第一行 JSON。
async fn handshake(path: PathBuf) -> anyhow::Result<Option<Discovered>> {
    use std::process::Stdio;
    let output = tokio::process::Command::new(&path)
        .arg("--rmus-manifest")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(plugin::MANIFEST_TIMEOUT, output)
        .await
        .context("握手超时")?
        .context("无法执行")?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    let manifest: PluginManifest =
        serde_json::from_str(line).with_context(|| format!("manifest 不是合法 JSON: {line}"))?;
    manifest.validate().map_err(|e| {
        anyhow::anyhow!("manifest 不合规（{}）: {e}", path.display())
    })?;
    Ok(Some(Discovered { manifest, path }))
}

/// 会话空闲回收：久无活动且无在途请求的会话杀掉。
async fn reaper(host: PluginHost) {
    loop {
        tokio::time::sleep(plugin::IDLE_TIMEOUT).await;
        let mut sessions = host.0.sessions.lock().await;
        let stale: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.is_idle())
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            if let Some(session) = sessions.remove(&name)
                && let Ok(mut child) = session.child.lock()
            {
                let _ = child.start_kill();
            }
            eprintln!("[rmusd] 插件 {name} 空闲超时，会话已回收");
        }
    }
}

/// stdin 写任务：协议行串行写出。
async fn stdin_writer(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::Receiver<String>) {
    while let Some(line) = rx.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err()
            || stdin.write_all(b"\n").await.is_err()
            || stdin.flush().await.is_err()
        {
            break;
        }
    }
}

/// stdout 读任务：响应按 id 路由（短调用回执 / 长任务包装成事件），
/// 通知行原样转发；EOF 时未决请求全部失败并把自己从会话表摘除
/// （除非已被新会话顶替）。
async fn stdout_reader(
    session: Arc<Session>,
    stdout: tokio::process::ChildStdout,
    sessions: Arc<tokio::sync::Mutex<HashMap<String, Arc<Session>>>>,
    events_tx: broadcast::Sender<Event>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                session.touch();
                if value.get("id").is_some() {
                    let Ok(reply) = serde_json::from_value::<PluginReply>(value) else {
                        continue;
                    };
                    let pending = session.pending.lock().unwrap().remove(&reply.id);
                    let Some(pending) = pending else { continue };
                    let outcome = match reply.error {
                        Some(error) => Err(error),
                        None => Ok(reply.result),
                    };
                    if pending.task {
                        let data = match &outcome {
                            Ok(result) => {
                                serde_json::json!({"event": "done", "result": result})
                            }
                            Err(error) => {
                                serde_json::json!({"event": "failed", "error": error})
                            }
                        };
                        let _ = events_tx.send(Event::Plugin {
                            plugin: session.name.clone(),
                            call: Some(reply.id),
                            data,
                        });
                    } else {
                        let _ = pending.reply.send(outcome);
                    }
                } else if let Ok(notify) = serde_json::from_value::<PluginNotify>(value) {
                    let _ = events_tx.send(Event::Plugin {
                        plugin: session.name.clone(),
                        call: notify.call,
                        data: notify.data,
                    });
                }
            }
            Err(_) => break,
        }
    }
    for (id, pending) in session.pending.lock().unwrap().drain() {
        let _ = pending.reply.send(Err("插件进程已退出".into()));
        if pending.task {
            let _ = events_tx.send(Event::Plugin {
                plugin: session.name.clone(),
                call: Some(id),
                data: serde_json::json!({"event": "failed", "error": "插件进程已退出"}),
            });
        }
    }
    let mut map = sessions.lock().await;
    if map
        .get(&session.name)
        .is_some_and(|current| Arc::ptr_eq(current, &session))
    {
        map.remove(&session.name);
    }
    eprintln!("[rmusd] 插件 {} 会话已结束", session.name);
}

/// 可执行文件判定（POSIX x 位）。
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}
