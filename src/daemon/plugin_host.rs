//! 原生插件宿主：托管内建插件模块（rmus-dl / rmus-podcast / rmus-fm / rmus-dj）。
//!
//! 所有插件已作为 workspace 子 crate 直接编译进 `rmusd`（无 wasm 解释器、无外部子进程）。
//! 每个插件独占一条宿主线程保证顺序事件模型，长任务由 `task::spawn` 派发到独立原生线程；
//! 视图更新经 `Event::PluginView` 广播，任务进度与完结经 `Event::Plugin` 广播，
//! 插件回访引擎/曲库走白名单化的 daemon RPC 通道。

use rmus::engine::ToEngine;
use rmus::plugin::{self, MethodKind, PluginManifest};
use rmus::proto::{Event, PluginLine, PluginList, PluginSpan};
use rmus_plugin_sdk::runtime::{self, CancelHandle, DaemonFn, HostContext, NativePlugin};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};

/// 已注册的一个原生插件。
struct Registered {
    manifest: PluginManifest,
    plugin: NativePlugin,
}

/// 插件宿主（廉价克隆，内部共享）。
#[derive(Clone)]
pub(crate) struct PluginHost(Arc<Inner>);

struct Inner {
    plugins: Vec<Registered>,
    /// 原生插件会话表：每个插件独占一条顺序事件线程
    sessions: Arc<Mutex<HashMap<String, NativeSession>>>,
    events_tx: broadcast::Sender<Event>,
    /// 引擎命令通道：插件 daemon RPC（白名单）经它进引擎
    cmd_tx: std::sync::mpsc::Sender<ToEngine>,
    /// 库索引（Find / LibList 就地回，不进引擎）
    library: crate::Library,
}

/// 插件会话线程的一帧输入：UI 按键、方法调用、后台/daemon 数据事件。
enum PluginMsg {
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
    /// 后台任务事件或 daemon 事件桥消息：写进 `last_event` 并以 `KIND_DATA` 通知插件
    Data { event: serde_json::Value },
}

/// 原生插件会话：独占一条线程消费消息，保证单线程顺序语义。
struct NativeSession {
    tx: std::sync::mpsc::Sender<PluginMsg>,
    /// 方法调用取消句柄（绕过消息通道直通）
    cancels: CancelHandle,
    /// 后台 worker 任务取消表（退出清理用）
    worker_cancels: Arc<Mutex<HashMap<u64, CancelHandle>>>,
    next_id: AtomicU64,
    _join: std::thread::JoinHandle<()>,
}

impl PluginHost {
    /// 注册全部内建原生插件，并启动 daemon 事件桥。
    pub fn new(
        events_tx: broadcast::Sender<Event>,
        cmd_tx: std::sync::mpsc::Sender<ToEngine>,
        library: crate::Library,
    ) -> Self {
        let builtins = [
            rmus_dl::plugin(),
            rmus_podcast::plugin(),
            rmus_fm::plugin(),
            rmus_dj::plugin(),
        ];
        let mut plugins = Vec::with_capacity(builtins.len());
        for plugin in builtins {
            let manifest: PluginManifest = serde_json::from_str(plugin.manifest)
                .expect("内建插件 manifest 必须是合法 JSON");
            manifest
                .validate()
                .expect("内建插件 manifest 必须合法");
            println!(
                "[rmusd] 模块: {} {}（{}）",
                manifest.name,
                manifest.version,
                manifest
                    .methods
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            plugins.push(Registered { manifest, plugin });
        }

        let host = Self(Arc::new(Inner {
            plugins,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
            cmd_tx,
            library,
        }));
        spawn_daemon_event_bridge(host.0.events_tx.clone(), host.0.sessions.clone());
        host
    }

    /// 已注册插件的清单（客户端 `plugin list`）。
    pub fn list(&self) -> serde_json::Value {
        serde_json::json!({
            "plugins": self.0.plugins.iter().map(|d| serde_json::json!({
                "name": d.manifest.name,
                "version": d.manifest.version,
                "ui": d.manifest.ui,
                "tab": d.manifest.tab,
                "cli": d.manifest.cli,
                "methods": d.manifest.methods,
            })).collect::<Vec<_>>()
        })
    }

    /// 锁内取（或懒启动）插件会话并执行操作。
    fn with_session<T>(
        &self,
        name: &str,
        f: impl FnOnce(&NativeSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut sessions = self.0.sessions.lock().unwrap();
        if !sessions.contains_key(name) {
            let found = self
                .0
                .plugins
                .iter()
                .find(|p| p.manifest.name == name)
                .ok_or_else(|| format!("未知插件 “{name}”"))?;
            let session = spawn_native_session(
                name.to_string(),
                found.plugin,
                self.0.events_tx.clone(),
                self.0.cmd_tx.clone(),
                self.0.library.clone(),
            )
            .map_err(|e| e.to_string())?;
            sessions.insert(name.to_string(), session);
        }
        f(sessions.get(name).unwrap())
    }

    /// UI 插件：注入数据（搜索关键词等）。插件以 `KIND_DATA` 事件收到。
    pub fn send_ui_data(&self, name: &str, data: &serde_json::Value) -> Result<(), String> {
        let event = serde_json::json!({ "data": data });
        self.with_session(name, |s| {
            s.tx.send(PluginMsg::Data { event })
                .map_err(|_| "插件线程已退出".to_string())
        })
    }

    /// UI 插件：注入按键（或 `force_render` 时直接刷新视图）。
    pub fn send_ui_key(
        &self,
        name: &str,
        key: i32,
        rows: u16,
        cols: u16,
        force_render: bool,
    ) -> Result<(), String> {
        self.with_session(name, |s| {
            s.tx.send(PluginMsg::Key {
                key,
                rows,
                cols,
                force_render,
            })
            .map_err(|_| "插件线程已退出".to_string())
        })
    }

    /// 调用插件方法。短调用等响应；长任务立即返回 `{"call":N}`，结果以 `Event::Plugin` 广播。
    pub async fn call(
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
            .ok_or_else(|| format!("未知插件 “{name}”"))?
            .manifest
            .method_kind(method)
            == Some(MethodKind::Task);
        let (call_id, rx) = self.with_session(name, |s| {
            let call_id = s.next_id.fetch_add(1, Ordering::Relaxed);
            let (reply, rx) = oneshot::channel();
            s.tx.send(PluginMsg::Call {
                method: method.to_string(),
                params,
                call_id,
                task,
                reply,
            })
            .map_err(|_| "插件线程已退出".to_string())?;
            Ok((call_id, rx))
        })?;

        if task {
            return Ok(serde_json::json!({ "call": call_id }));
        }
        match tokio::time::timeout(plugin::CALL_TIMEOUT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("插件线程已中断".into()),
            Err(_) => Err("插件响应超时".into()),
        }
    }

    /// 取消长任务（标记取消集，插件循环自查 `cancelled()` 生效）。
    pub async fn cancel(&self, name: &str, call: u64) -> Result<serde_json::Value, String> {
        if !self.0.plugins.iter().any(|p| p.manifest.name == name) {
            return Err(format!("未知插件 “{name}”"));
        }
        self.with_session(name, |s| {
            s.cancels.cancel(call);
            Ok(())
        })?;
        Ok(serde_json::json!({ "cancelled": true }))
    }

    /// 退出前清理：标记所有常驻 worker 取消并释放会话发送端。
    pub fn shutdown(&self) {
        let mut sessions = self.0.sessions.lock().unwrap();
        for session in sessions.values() {
            for (task_id, handle) in session.worker_cancels.lock().unwrap().iter() {
                handle.cancel(*task_id);
            }
        }
        sessions.clear();
    }
}

/// daemon RPC 桥：插件线程阻塞调用 → 专用线程白名单执行。
fn daemon_rpc_bridge(
    name: String,
    cmd_tx: std::sync::mpsc::Sender<ToEngine>,
    library: crate::Library,
) -> DaemonFn {
    let (rpc_tx, rpc_rx) = std::sync::mpsc::channel::<(
        serde_json::Value,
        oneshot::Sender<Result<serde_json::Value, String>>,
    )>();
    std::thread::Builder::new()
        .name(format!("rmus-daemon-rpc-{name}"))
        .spawn(move || {
            while let Ok((request, rtx)) = rpc_rx.recv() {
                let reply = execute_daemon_rpc(&cmd_tx, &library, request);
                let _ = rtx.send(reply);
            }
        })
        .expect("daemon rpc 线程启动失败");
    Arc::new(move |request| {
        let (rtx, rrx) = oneshot::channel();
        rpc_tx
            .send((request, rtx))
            .map_err(|_| "daemon RPC 通道已关闭".to_string())?;
        rrx.blocking_recv()
            .map_err(|_| "daemon RPC 无响应".to_string())?
    })
}

/// 插件的 daemon RPC 白名单执行。
/// - Status / QueueList / PlaylistsList / Back：直通引擎
/// - Peek / Add / PlaylistAdd：扫描后进引擎
/// - Find / LibList：查库索引就地回（不进引擎）
fn execute_daemon_rpc(
    cmd_tx: &std::sync::mpsc::Sender<ToEngine>,
    library: &crate::Library,
    request: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use rmus::proto::Request;
    let request: Request =
        serde_json::from_value(request).map_err(|e| format!("请求不是合法 Request: {e}"))?;
    let (reply_tx, reply_rx) = oneshot::channel();
    let sent = match request {
        Request::Status | Request::QueueList | Request::PlaylistsList | Request::Back => cmd_tx
            .send(ToEngine::Request {
                id: 0,
                request,
                reply: reply_tx,
            })
            .is_ok(),
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
        Request::Add { paths } => {
            let items = rmus::scan::collect(&paths);
            cmd_tx
                .send(ToEngine::AddScanned {
                    id: 0,
                    items,
                    reply: reply_tx,
                })
                .is_ok()
        }
        Request::InsertNext { paths } => {
            let items = rmus::scan::collect(&paths);
            cmd_tx
                .send(ToEngine::InsertNextScanned {
                    id: 0,
                    items,
                    reply: reply_tx,
                })
                .is_ok()
        }
        Request::PlaylistAdd { name, paths } => {
            let items = rmus::scan::collect(&paths);
            cmd_tx
                .send(ToEngine::PlaylistAddScanned {
                    id: 0,
                    name,
                    items,
                    reply: reply_tx,
                })
                .is_ok()
        }
        Request::Find { query } => {
            let tracks = {
                let lib = library.read().unwrap();
                rmus::library::matches(&lib, &query)
                    .into_iter()
                    .take(10)
                    .enumerate()
                    .map(|(i, item)| rmus::proto::TrackInfo {
                        index: i,
                        path: item.path.clone(),
                        title: item.title.clone(),
                        artist: item.artist.clone(),
                        album: item.album.clone(),
                        duration_ms: item.duration_ms,
                        recommended: false,
                    })
                    .collect()
            };
            return serde_json::to_value(rmus::proto::Reply::Matches { tracks })
                .map_err(|e| e.to_string());
        }
        Request::LibList => {
            let tracks = {
                let lib = library.read().unwrap();
                lib.iter()
                    .enumerate()
                    .map(|(i, entry)| rmus::proto::TrackInfo {
                        index: i,
                        path: entry.item.path.clone(),
                        title: entry.item.title.clone(),
                        artist: entry.item.artist.clone(),
                        album: entry.item.album.clone(),
                        duration_ms: entry.item.duration_ms,
                        recommended: false,
                    })
                    .collect()
            };
            return serde_json::to_value(rmus::proto::Reply::Matches { tracks })
                .map_err(|e| e.to_string());
        }
        _other => {
            return Err(
                "插件无权调用该请求（白名单：Status/QueueList/PlaylistsList/Peek/Add/PlaylistAdd/Find/LibList/Back）"
                    .into(),
            );
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

/// daemon 事件桥：切歌/停止广播推给所有活跃插件会话（JSON 加 `kind:"daemon"` 区分任务事件）。
fn spawn_daemon_event_bridge(
    events_tx: broadcast::Sender<Event>,
    sessions: Arc<Mutex<HashMap<String, NativeSession>>>,
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
                    for session in sessions.lock().unwrap().values() {
                        let _ = session.tx.send(PluginMsg::Data {
                            event: value.clone(),
                        });
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

fn render_and_broadcast(
    plugin: NativePlugin,
    name: &str,
    rows: u16,
    cols: u16,
    events_tx: &broadcast::Sender<Event>,
) {
    let Some(render_fn) = plugin.render else {
        return;
    };
    if rows == 0 || cols == 0 {
        return;
    }
    runtime::begin_render();
    render_fn(i32::from(rows), i32::from(cols));
    let frame = runtime::take_render();
    let lines: Vec<PluginLine> = frame
        .lines
        .into_iter()
        .map(|line| PluginLine {
            spans: line
                .spans
                .into_iter()
                .map(|s| PluginSpan {
                    text: s.text,
                    style: s.style,
                })
                .collect(),
        })
        .collect();
    let list = frame.list.map(|(total, index)| PluginList { total, index });
    let _ = events_tx.send(Event::PluginView {
        plugin: name.to_string(),
        lines,
        selected: frame.selected,
        list,
    });
}

fn spawn_native_session(
    name: String,
    plugin: NativePlugin,
    events_tx: broadcast::Sender<Event>,
    cmd_tx: std::sync::mpsc::Sender<ToEngine>,
    library: crate::Library,
) -> anyhow::Result<NativeSession> {
    let (tx, rx) = std::sync::mpsc::channel::<PluginMsg>();
    let cancels = CancelHandle::new();
    let worker_cancels: Arc<Mutex<HashMap<u64, CancelHandle>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let lib_dir = rmus::library::root();
    let data_dir = rmus::plugin::data_root();
    let daemon_call = daemon_rpc_bridge(name.clone(), cmd_tx, library);

    let cancels_for_thread = cancels.clone();
    let worker_cancels_for_thread = worker_cancels.clone();
    let worker_tx = tx.clone();
    let thread_name = format!("rmus-plugin-{name}");
    let join = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let on_notify = {
                let events_tx = events_tx.clone();
                let name = name.clone();
                Arc::new(move |call: Option<u64>, data: serde_json::Value| {
                    let _ = events_tx.send(Event::Plugin {
                        plugin: name.clone(),
                        call,
                        data,
                    });
                })
            };

            let spawn_task: runtime::SpawnTaskFn = {
                let worker_cancels = worker_cancels_for_thread.clone();
                let worker_tx = worker_tx.clone();
                let lib_dir = lib_dir.clone();
                let data_dir = data_dir.clone();
                let daemon_call = daemon_call.clone();
                let plugin_name = name.clone();
                Arc::new(move |task_id: u64, method: String, params: serde_json::Value| {
                    let handle = CancelHandle::new();
                    worker_cancels
                        .lock()
                        .unwrap()
                        .insert(task_id, handle.clone());
                    let cancels_map = worker_cancels.clone();
                    let tx = worker_tx.clone();
                    let lib_dir = lib_dir.clone();
                    let data_dir = data_dir.clone();
                    let daemon_call = daemon_call.clone();
                    let spawned = std::thread::Builder::new()
                        .name(format!("rmus-worker-{plugin_name}-{task_id}"))
                        .spawn(move || {
                            let emit = {
                                let tx = tx.clone();
                                move |event: &str, data: serde_json::Value| {
                                    let _ = tx.send(PluginMsg::Data {
                                        event: serde_json::json!({
                                            "task_id": task_id,
                                            "event": event,
                                            "data": data,
                                        }),
                                    });
                                }
                            };
                            let worker_notify = {
                                let emit = emit.clone();
                                Arc::new(move |_call: Option<u64>, data: serde_json::Value| {
                                    emit("progress", data);
                                })
                            };
                            runtime::install_context(HostContext {
                                lib_dir,
                                data_dir,
                                cancelled: handle,
                                worker_cancel: true,
                                on_notify: Some(worker_notify),
                                daemon_call: Some(daemon_call),
                                ..Default::default()
                            });
                            let outcome = (plugin.handle)(&method, &params);
                            runtime::clear_context();
                            cancels_map.lock().unwrap().remove(&task_id);
                            match outcome {
                                Ok(result) => emit("done", result),
                                Err(error) => emit("failed", serde_json::Value::String(error)),
                            }
                        });
                    if spawned.is_err() {
                        worker_cancels.lock().unwrap().remove(&task_id);
                        return false;
                    }
                    true
                })
            };

            runtime::install_context(HostContext {
                lib_dir,
                data_dir,
                cancelled: cancels_for_thread,
                on_notify: Some(on_notify),
                daemon_call: Some(daemon_call),
                spawn_task: Some(spawn_task),
                worker_cancels: worker_cancels_for_thread,
                ..Default::default()
            });

            // 最近一次按键的视口（Data 事件重绘用；默认 24×80）
            let mut viewport: (u16, u16) = (24, 80);

            while let Ok(msg) = rx.recv() {
                match msg {
                    PluginMsg::Key {
                        key,
                        rows,
                        cols,
                        force_render,
                    } => {
                        viewport = (rows, cols);
                        let redraw = if force_render {
                            true
                        } else {
                            plugin.update.is_some_and(|f| f(key) != 0)
                        };
                        if redraw {
                            render_and_broadcast(plugin, &name, rows, cols, &events_tx);
                        }
                    }
                    PluginMsg::Call {
                        method,
                        params,
                        call_id,
                        task,
                        reply,
                    } => {
                        runtime::set_current_call(Some(call_id));
                        let outcome = (plugin.handle)(&method, &params);
                        runtime::set_current_call(None);
                        match outcome {
                            Ok(result) => {
                                if task {
                                    let _ = events_tx.send(Event::Plugin {
                                        plugin: name.clone(),
                                        call: Some(call_id),
                                        data: serde_json::json!({
                                            "event": "done",
                                            "result": result,
                                        }),
                                    });
                                } else {
                                    let _ = reply.send(Ok(result));
                                }
                            }
                            Err(error) => {
                                if task {
                                    let _ = events_tx.send(Event::Plugin {
                                        plugin: name.clone(),
                                        call: Some(call_id),
                                        data: serde_json::json!({
                                            "event": "failed",
                                            "error": error,
                                        }),
                                    });
                                } else {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                    }
                    PluginMsg::Data { event } => {
                        // 后台任务/daemon 事件同时以 Event::Plugin 兼容广播
                        let _ = events_tx.send(Event::Plugin {
                            plugin: name.clone(),
                            call: None,
                            data: event.clone(),
                        });
                        runtime::set_last_event(event);
                        if plugin
                            .update
                            .is_some_and(|f| f(rmus::proto::plugin_key::KIND_DATA) != 0)
                        {
                            render_and_broadcast(plugin, &name, viewport.0, viewport.1, &events_tx);
                        }
                    }
                }
            }
            runtime::clear_context();
        })?;

    Ok(NativeSession {
        tx,
        cancels,
        worker_cancels,
        next_id: AtomicU64::new(1),
        _join: join,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;

    #[tokio::test]
    async fn builtin_native_plugins_register_and_render() {
        let (events_tx, mut events_rx) = broadcast::channel::<Event>(32);
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToEngine>();
        let library: crate::Library = Arc::new(RwLock::new(Vec::new()));

        let host = PluginHost::new(events_tx, cmd_tx, library);
        let list = host.list();
        let plugins = list["plugins"].as_array().unwrap();
        let names: Vec<&str> = plugins
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        assert_eq!(names, ["rmus-dl", "rmus-podcast", "rmus-fm", "rmus-dj"]);

        // 触发初始视图渲染，确认每个原生模块都能输出至少一行
        for name in names {
            host.send_ui_key(name, 0, 24, 80, true).unwrap();
            let ev = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Ok(Event::PluginView { plugin, lines, .. }) = events_rx.recv().await
                        && plugin == name
                    {
                        return lines;
                    }
                }
            })
            .await
            .expect("应在超时内收到 PluginView");
            assert!(!ev.is_empty(), "{name} 初始视图不应为空");
        }
        host.shutdown();
    }
}
