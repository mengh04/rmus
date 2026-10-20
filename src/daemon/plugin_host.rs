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
use rmus::proto::{Event, PluginLine};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinSet;
use wasm_host::Plugin as WasmPlugin;

use rmus::engine::ToEngine;

/// 已发现的一个插件。
struct Discovered {
    manifest: PluginManifest,
    path: PathBuf,
    /// wasm 插件（wasm 会话）；false = 进程外可执行（NDJSON 会话）
    wasm: bool,
}

/// 插件宿主（廉价克隆，内部共享）。
#[derive(Clone)]
pub(crate) struct PluginHost(Arc<Inner>);

struct Inner {
    plugins: Vec<Discovered>,
    /// 会话表单独 Arc：stdout 读任务退出时要回来摘除自己
    sessions: Arc<tokio::sync::Mutex<HashMap<String, Arc<Session>>>>,
    /// wasm 插件会话表：wasmi 实例独占线程，消息经 channel 注入
    wasm_sessions: Arc<Mutex<HashMap<String, WasmSession>>>,
    events_tx: broadcast::Sender<Event>,
    /// 引擎命令通道：插件 daemon RPC（白名单）经它进引擎
    cmd_tx: std::sync::mpsc::Sender<ToEngine>,
}

/// wasm 插件会话的一帧输入：UI 按键、方法调用。（取消不进通道——
/// 插件线程阻塞在长任务里收不到，经 [`wasm_host::CancelHandle`] 直通。）
enum WasmMsg {
    Key {
        key: i32,
        rows: u16,
        cols: u16,
        /// 跳过 update 直接 render（`PluginViewReq` 的视图刷新）
        force_render: bool,
    },
    Call {
        method: String,
        params: serde_json::Value,
        call_id: u64,
        task: bool,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// 后台任务事件（worker 实例的进度/done/failed）：JSON 行。UI 线程
    /// 写进 `set_last_event` 并以 `KIND_DATA` 事件通知插件
    Data { json: String },
}

/// wasm 插件会话：wasmi 实例独占一个线程，保证单线程同步模型（Zellij
/// 同款假设——事件顺序由宿主单一消费点保证）。UI 视图与通知经
/// `Event::PluginView` / `Event::Plugin` 广播；方法回包走 oneshot。
struct WasmSession {
    tx: std::sync::mpsc::Sender<WasmMsg>,
    /// 取消标记直通：插件线程阻塞在长任务里收不到 channel 消息，
    /// 取消必须绕过 channel 直接写共享集合（插件自查生效）
    cancels: wasm_host::CancelHandle,
    next_id: AtomicU64,
    /// 只用于退出时判存活（std 线程无法 kill；靠 drop tx 让线程自然退出）
    _join: std::thread::JoinHandle<()>,
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
    /// `.wasm` 文件走 wasm 握手（实例化 + manifest export），其余按可执行
    /// 文件处理（`--rmus-manifest` 子进程握手）。
    pub async fn discover(
        events_tx: broadcast::Sender<Event>,
        cmd_tx: std::sync::mpsc::Sender<ToEngine>,
    ) -> Self {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for dir in plugin::plugin_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "wasm") || is_executable_file(&path) {
                    candidates.push(path);
                }
            }
        }

        let mut handles: JoinSet<anyhow::Result<Option<Discovered>>> = JoinSet::new();
        for path in candidates {
            if path.extension().is_some_and(|e| e == "wasm") {
                // wasm 握手是同步 CPU 工作，丢阻塞线程池
                handles.spawn(async move {
                    tokio::task::spawn_blocking(move || wasm_handshake(path))
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("join: {e}")))
                });
            } else {
                handles.spawn(async move { handshake(path).await });
            }
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
                Ok(Ok(None)) => {}
                Ok(Err(e)) => eprintln!("[rmusd] 插件握手失败: {e:#}"),
                Err(e) => eprintln!("[rmusd] 插件握手任务异常: {e}"),
            }
        }

        let host = Self(Arc::new(Inner {
            plugins,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            wasm_sessions: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
            cmd_tx,
        }));
        spawn_daemon_event_bridge(host.0.events_tx.clone(), host.0.wasm_sessions.clone());
        tokio::spawn(reaper(host.clone()));
        host
    }

    /// 已发现插件的清单（客户端 `plugin list`）。
    pub fn list(&self) -> serde_json::Value {
        serde_json::json!({
            "plugins": self.0.plugins.iter().map(|d| serde_json::json!({
                "name": d.manifest.name,
                "version": d.manifest.version,
                "ui": d.manifest.ui,
                "tab": d.manifest.tab,
                "methods": d.manifest.methods,
            })).collect::<Vec<_>>()
        })
    }

    /// 锁内取（或懒启动）wasm 会话并执行操作。call_id 分配也在锁内，
    /// 保证并发调用不撞号。
    fn with_wasm_session<T>(
        &self,
        name: &str,
        f: impl FnOnce(&WasmSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut sessions = self.0.wasm_sessions.lock().unwrap();
        if !sessions.contains_key(name) {
            let found = self
                .0
                .plugins
                .iter()
                .find(|p| p.manifest.name == name)
                .ok_or_else(|| format!("未知插件 “{name}”"))?;
            if !found.wasm {
                return Err(format!("插件 “{name}” 不是 wasm 插件"));
            }
            let session = spawn_wasm_session(
                name.to_string(),
                found.path.clone(),
                self.0.events_tx.clone(),
                self.0.cmd_tx.clone(),
            )
            .map_err(|e| e.to_string())?;
            sessions.insert(name.to_string(), session);
        }
        f(sessions.get(name).unwrap())
    }

    /// UI 插件：注入数据（搜索关键词等）。插件以 `KIND_DATA` 事件收到。
    pub fn send_ui_data(&self, name: &str, data: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::json!({ "data": data }).to_string();
        self.with_wasm_session(name, |s| {
            s.tx
                .send(WasmMsg::Data { json })
                .map_err(|_| "wasm 插件线程已退出".to_string())
        })
    }

    /// UI 插件：注入按键（或 `force_render` 时直接刷新视图）。会话懒启动；
    /// 视图经 `Event::PluginView` 广播，本调用只做投递确认。
    pub fn send_ui_key(        &self,
        name: &str,
        key: i32,
        rows: u16,
        cols: u16,
        force_render: bool,
    ) -> Result<(), String> {
        self.with_wasm_session(name, |s| {
            s.tx
                .send(WasmMsg::Key {
                    key,
                    rows,
                    cols,
                    force_render,
                })
                .map_err(|_| "wasm 插件线程已退出".to_string())
        })
    }

    /// 是否一个插件都没有。
    pub fn is_empty(&self) -> bool {
        self.0.plugins.is_empty()
    }

    /// 调用插件方法。按插件类型分流：wasm 走实例线程，进程外走 NDJSON。
    /// 短调用等响应；长任务立即返回 `{"call":N}`，结果随后以
    /// `Event::Plugin`（done/failed）广播。
    pub async fn call(
        &self,
        name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let is_wasm = self
            .0
            .plugins
            .iter()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| format!("未知插件 “{name}”"))?
            .wasm;
        if is_wasm {
            self.wasm_call(name, method, params).await
        } else {
            self.process_call(name, method, params).await
        }
    }

    /// wasm 插件的方法调用。
    async fn wasm_call(
        &self,
        name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let task = self
            .0
            .plugins
            .iter()
            .find(|p| p.manifest.name == name)
            .and_then(|p| p.manifest.method_kind(method))
            == Some(MethodKind::Task);
        let (call_id, rx) = self.with_wasm_session(name, |s| {
            let call_id = s.next_id.fetch_add(1, Ordering::Relaxed);
            let (reply, rx) = oneshot::channel();
            s.tx
                .send(WasmMsg::Call {
                    method: method.to_string(),
                    params,
                    call_id,
                    task,
                    reply,
                })
                .map_err(|_| "wasm 插件线程已退出".to_string())?;
            Ok((call_id, rx))
        })?;

        if task {
            return Ok(serde_json::json!({"call": call_id}));
        }
        match tokio::time::timeout(plugin::CALL_TIMEOUT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("wasm 插件线程已中断".into()),
            Err(_) => Err("插件响应超时".into()),
        }
    }

    /// 进程外插件的方法调用（原 call 主体）。
    async fn process_call(
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

    /// 取消长任务。按插件类型分流；wasm 插件标记取消集（插件循环自查）。
    pub async fn cancel(&self, name: &str, call: u64) -> Result<serde_json::Value, String> {
        let found = self
            .0
            .plugins
            .iter()
            .find(|p| p.manifest.name == name);
        match found {
            Some(found) if found.wasm => {
                // 无会话 = 无在途任务，标记也无害（乐观回）
                self.with_wasm_session(name, |s| {
                    s.cancels.cancel(call);
                    Ok(())
                })?;
                Ok(serde_json::json!({"cancelled": true}))
            }
            Some(_) => {
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
                        let line = serde_json::json!({"id": id, "method": "cancel", "params": {"call": call}}).to_string();
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
            None => Err(format!("未知插件 “{name}”")),
        }
    }

    /// 退出前清理：杀掉所有会话子进程（process::exit 不跑析构）。
    pub fn shutdown(&self) {
        // wasm 会话：drop 发送端让线程在 rx.recv() 处自然退出
        self.0.wasm_sessions.lock().unwrap().clear();
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
/// wasm 插件握手：实例化 → 读 manifest export → 校验。
fn wasm_handshake(path: PathBuf) -> anyhow::Result<Option<Discovered>> {
    let wasm = std::fs::read(&path).context("无法读取 wasm 文件")?;
    let mut instance = WasmPlugin::new(&wasm, Box::new(|_, _| {})).context("实例化失败")?;
    let manifest: PluginManifest =
        serde_json::from_str(&instance.manifest().context("读取 manifest 失败")?)
            .context("manifest 不是合法 JSON")?;
    manifest
        .validate()
        .map_err(|e| anyhow::anyhow!("manifest 不合规（{}）: {e}", path.display()))?;
    Ok(Some(Discovered {
        manifest,
        path,
        wasm: true,
    }))
}

/// wasm 插件会话线程：实例化后循环消费消息（按键 / 方法调用）。
/// 方法执行的 notify 经 on_notify 实时转发为 `Event::Plugin`；长任务
/// 完成包装成 done/failed 广播，短调用走 oneshot。实例化在调用线程完成
/// （μs 级），CancelHandle 因此无竞态地带出给会话表。
/// daemon RPC 桥：插件线程阻塞调用（`rmus.daemon`）→ 专用通道 → 独立
/// 线程白名单执行。不复用插件自身的 WasmMsg channel——插件线程正阻塞在
/// RPC 里，消费不了它，复用必死锁。
fn daemon_rpc_bridge(
    name: String,
    cmd_tx: std::sync::mpsc::Sender<ToEngine>,
) -> wasm_host::DaemonFn {
    let (rpc_tx, rpc_rx) = std::sync::mpsc::channel::<(
        serde_json::Value,
        tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    )>();
    std::thread::Builder::new()
        .name(format!("rmus-daemon-rpc-{name}"))
        .spawn(move || {
            while let Ok((request, rtx)) = rpc_rx.recv() {
                let reply = execute_daemon_rpc(&cmd_tx, request);
                let _ = rtx.send(reply);
            }
        })
        .expect("daemon rpc 线程启动失败");
    std::sync::Arc::new(move |request| {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        rpc_tx
            .send((request, rtx))
            .map_err(|_| "daemon RPC 通道已关闭".to_string())?;
        rrx.blocking_recv().map_err(|_| "daemon RPC 无响应".to_string())?
    })
}

/// 插件的 daemon RPC 白名单执行。请求是协议 Request 的 tagged JSON——
/// 先解析（形态非法即拒绝），再按变体过白名单。v1：Status（进度采样）、
/// Peek（点播/连播的无缝切集）、Back（结束试听）。其余一律拒绝——插件
/// 不该摸到管理类操作（清队列/重扫库/退出）。
fn execute_daemon_rpc(
    cmd_tx: &std::sync::mpsc::Sender<ToEngine>,
    request: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use rmus::proto::Request;
    let request: Request =
        serde_json::from_value(request).map_err(|e| format!("请求不是合法 Request: {e}"))?;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let sent = match request {
        // 直通引擎的查询/控制
        Request::Status | Request::QueueList | Request::Back => cmd_tx
            .send(ToEngine::Request {
                id: 0,
                request,
                reply: reply_tx,
            })
            .is_ok(),
        // Peek 是 daemon 层处理（先扫描再进引擎试听态）；插件场景是
        // 单集文件，同步扫描足够快（目录也能容忍）
        Request::Peek { paths } => {
            let items = rmus::scan::collect(&paths);
            if items.is_empty() {
                return Err("没有找到可播放的音频文件".into());
            }
            cmd_tx
                .send(ToEngine::PeekScanned {
                    id: 0,
                    items,
                    reply: reply_tx,
                })
                .is_ok()
        }
        _other => {
            return Err("插件无权调用该请求（白名单仅 Status/Peek/Back）".into());
        }
    };
    if !sent {
        return Err("引擎不可达".into());
    }
    let reply = reply_rx
        .blocking_recv()
        .map_err(|_| "引擎回包通道关闭".to_string())??;
    serde_json::to_value(reply).map_err(|e| e.to_string())
}

/// daemon 事件桥：切歌/停止广播推给所有 wasm 会话（插件的连播接管与
/// 播完标记以此为信号源）。复用 worker 事件通道（KIND_DATA），JSON 加
/// `kind:"daemon"` 与任务事件区分；Position 每秒一条不转发（太吵，
/// 插件进度记忆靠自己的采样循环）。
fn spawn_daemon_event_bridge(
    events_tx: broadcast::Sender<Event>,
    wasm_sessions: Arc<Mutex<HashMap<String, WasmSession>>>,
) {
    let mut bridge_rx = events_tx.subscribe();
    tokio::spawn(async move {
        loop {
            match bridge_rx.recv().await {
                Ok(ev @ Event::SongChanged { .. }) | Ok(ev @ Event::StateChanged { .. }) => {
                    let mut value = serde_json::to_value(&ev).unwrap_or_default();
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("kind".into(), serde_json::json!("daemon"));
                    }
                    let json = value.to_string();
                    for session in wasm_sessions.lock().unwrap().values() {
                        let _ = session.tx.send(WasmMsg::Data { json: json.clone() });
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[rmusd] 插件事件桥落后 {n} 条（丢弃；进度有采样兜底）");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_wasm_session(
    name: String,
    path: PathBuf,
    events_tx: broadcast::Sender<Event>,
    cmd_tx: std::sync::mpsc::Sender<ToEngine>,
) -> anyhow::Result<WasmSession> {
    let (tx, rx) = std::sync::mpsc::channel::<WasmMsg>();
    let wasm = std::fs::read(&path).context("无法读取 wasm 文件")?;
    // notify → Event::Plugin 实时转发（进度等）
    let notify_events = {
        let events_tx = events_tx.clone();
        let name = name.clone();
        Box::new(move |call: Option<u64>, data: serde_json::Value| {
            let _ = events_tx.send(Event::Plugin {
                plugin: name.clone(),
                call,
                data,
            });
        })
    };
    let mut plugin = WasmPlugin::new(&wasm, notify_events).context("实例化失败")?;
    plugin.set_lib_dir(rmus::library::root());
    plugin.set_daemon_call(daemon_rpc_bridge(name.clone(), cmd_tx.clone()));
    plugin.set_data_dir(rmus::plugin::data_root());
    // 最近一次按键的视口（Data 事件重绘用；默认 24×80）
    let mut viewport: u64 = ((24u64) << 16) | 80;
    let cancels = plugin.cancel_handle();
    // 后台任务源：插件经 spawn_task 发起时，宿主用同 wasm 的新实例跑，
    // 事件经此桥接回 UI 线程（写 last_event + KIND_DATA 通知）
    let worker_tx = tx.clone();
    let worker_events: wasm_host::TaskEvents = Arc::new(move |json: String| {
        let _ = worker_tx.send(WasmMsg::Data { json });
    });
    plugin.set_worker_source(path.clone(), worker_events);
    let join = std::thread::Builder::new()
        .name(format!("rmus-wasm-{name}"))
        .spawn(move || {
            let mut run = || -> anyhow::Result<()> {
                while let Ok(msg) = rx.recv() {
                    match msg {
                        WasmMsg::Key {
                            key,
                            rows,
                            cols,
                            force_render,
                        } => {
                            viewport = ((rows as u64) << 16) | cols as u64;
                            let redraw = if force_render {
                                true
                            } else {
                                plugin.update(key).context("update 失败")?
                            };
                            if !redraw || rows == 0 || cols == 0 {
                                continue; // 区域为空时跳过渲染，保留上次视图
                            }
                            let lines: Vec<PluginLine> = plugin
                                .render(rows, cols)?
                                .iter()
                                .map(|line| PluginLine {
                                    spans: line
                                        .spans
                                        .iter()
                                        .map(|s| rmus::proto::PluginSpan {
                                            text: s.text.clone(),
                                            style: s.style.clone(),
                                        })
                                        .collect(),
                                })
                                .collect();
                            let selected = plugin.take_selected();
                            let _ = events_tx.send(Event::PluginView {
                                plugin: name.clone(),
                                lines,
                                selected,
                            });
                        }
                        WasmMsg::Call {
                            method,
                            params,
                            call_id,
                            task,
                            reply,
                        } => {
                            plugin.set_current_call(Some(call_id));
                            let outcome = plugin.call_method(&method, &params);
                            plugin.set_current_call(None);
                            match outcome {
                                Ok(Ok(result)) => {
                                    if task {
                                        let _ = events_tx.send(Event::Plugin {
                                            plugin: name.clone(),
                                            call: Some(call_id),
                                            data: serde_json::json!({
                                                "event": "done", "result": result
                                            }),
                                        });
                                    } else {
                                        let _ = reply.send(Ok(result));
                                    }
                                }
                                Ok(Err(error)) => {
                                    if task {
                                        let _ = events_tx.send(Event::Plugin {
                                            plugin: name.clone(),
                                            call: Some(call_id),
                                            data: serde_json::json!({
                                                "event": "failed", "error": error
                                            }),
                                        });
                                    } else {
                                        let _ = reply.send(Err(error));
                                    }
                                }
                                Err(error) => {
                                    let error = error.to_string();
                                    if task {
                                        let _ = events_tx.send(Event::Plugin {
                                            plugin: name.clone(),
                                            call: Some(call_id),
                                            data: serde_json::json!({
                                                "event": "failed", "error": error
                                            }),
                                        });
                                    } else {
                                        let _ = reply.send(Err(error));
                                    }
                                }
                            }
                        }
                        WasmMsg::Data { json } => {
                            // 后台任务事件：写进 last_event 缓冲并以 KIND_DATA
                            // 通知插件；任务期事件同时以 Event::Plugin 兼容广播
                            let _ = events_tx.send(Event::Plugin {
                                plugin: name.clone(),
                                call: None,
                                data: serde_json::from_str(&json)
                                    .unwrap_or(serde_json::json!({"raw": json})),
                            });
                            plugin.set_last_event(json.as_bytes());
                            if plugin.update(rmus::proto::plugin_key::KIND_DATA)? {
                                let (rows, cols) = ((viewport >> 16) as u16, viewport as u16);
                                let lines: Vec<PluginLine> = plugin
                                    .render(rows, cols)?
                                    .iter()
                                    .map(|line| PluginLine {
                                        spans: line
                                            .spans
                                            .iter()
                                            .map(|s| rmus::proto::PluginSpan {
                                                text: s.text.clone(),
                                                style: s.style.clone(),
                                            })
                                            .collect(),
                                    })
                                    .collect();
                                let selected = plugin.take_selected();
                                let _ = events_tx.send(Event::PluginView {
                                    plugin: name.clone(),
                                    lines,
                                    selected,
                                });
                            }
                        }
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                eprintln!("[rmusd] wasm 插件 {name} 会话结束: {e:#}");
            }
        })
        .expect("spawn wasm 插件线程");
    Ok(WasmSession {
        tx,
        cancels,
        next_id: AtomicU64::new(1),
        _join: join,
    })
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
    Ok(Some(Discovered {
        manifest,
        path,
        wasm: false,
    }))
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
