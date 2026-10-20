//! rmus wasm 插件宿主：用 wasmi 实例化插件、注入事件、执行方法调用、
//! 收集渲染输出与进度通知。
//!
//! 插件 ABI 见 `rmus-plugin-sdk` 的模块文档。设计取向：
//!
//! - **不挂 WASI**：插件只 import `rmus.*` 宿主函数。渲染、网络、文件、
//!   时钟等能力一律宿主代理，权限收紧成白名单问题。
//! - **单实例单状态**：`Store` 的 state 携带渲染收集缓冲、当前 call、
//!   取消标记与通知回调。实例独占线程，全部状态无并发访问。
//! - UI 通道：宿主调 `update(event)`，返回非 0 才调 `render` 拉新视图。
//! - 方法通道：宿主写参数进插件内存（`alloc`）→ 调 `call(ptr,len)` →
//!   从 `reply` 缓冲读回包；执行期间的 `notify` 经回调实时抛出。

use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use wasmi::{Caller, Engine, Error, Linker, Module, Store, TypedFunc};

/// 插件事件编码（与 SDK 侧 `event` 模块保持一致；v0 图省事两边各写一份，
/// 正式化后挪到共享 crate）。编码：`kind | (payload << 8)`。
pub mod event {
    pub const KIND_TICK: i32 = 1;
    pub const KIND_ENTER: i32 = 2;
    pub const KIND_UP: i32 = 3;
    pub const KIND_DOWN: i32 = 4;
    pub const KIND_CHAR: i32 = 5;
    pub const KIND_SPACE: i32 = 6;
    /// 后台任务事件（数据经 `set_last_event` + 插件 `rmus.last_event` 读取）
    pub const KIND_DATA: i32 = 7;
    /// 退格键（无 payload）
    pub const KIND_BACKSPACE: i32 = 8;

    pub fn encode(kind: i32, payload: i32) -> i32 {
        kind | (payload << 8)
    }
    pub fn kind(event: i32) -> i32 {
        event & 0xFF
    }
}

type NotifyFn = Box<dyn FnMut(Option<u64>, Value) + Send>;
/// 后台任务事件出口（daemon 侧桥接成 UI 线程消息）。
pub type TaskEvents = Arc<dyn Fn(String) + Send + Sync>;

/// daemon RPC 出口（daemon 侧白名单执行：Status / Peek / Back）。
/// 插件线程阻塞调用，实现在 daemon 的 tokio 侧。请求/响应都是 JSON
/// Value——Request 的解析与白名单校验集中在 daemon 侧桥函数。
pub type DaemonFn = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

/// 取消句柄：跨线程标记某 call 已取消（`rmus.cancelled` 的查询源）。
/// 插件线程阻塞在长任务里时，会话线程也能标记——插件在进度间隙自查生效。
#[derive(Clone, Default)]
pub struct CancelHandle(Arc<Mutex<HashSet<u64>>>);

impl CancelHandle {
    /// 新建独立句柄（后台任务的取消源；经 `Plugin::set_cancel_handle`
    /// 交给 worker 实例）。
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self, call: u64) {
        self.0.lock().unwrap().insert(call);
    }
}

/// 渲染行的一个带样式片段（与协议 PluginSpan 同形状，daemon 侧转换）。
#[derive(Debug, Clone)]
pub struct RenderSpan {
    pub text: String,
    /// 语义样式令牌：base/strong/faint/accent/danger
    pub style: String,
}

/// 渲染行（`render()` 的输出单位）。
#[derive(Debug, Clone, Default)]
pub struct RenderLine {
    pub spans: Vec<RenderSpan>,
}

/// 宿主状态：挂在 `Store` 上，宿主函数与渲染收集共用。
struct HostState {
    /// 本轮 `render` 收集到的行（`print_line` 纯文本 / `print_spans` 带样式）。
    lines: Vec<RenderLine>,
    /// 本轮 `render` 里插件声明的选中行（画布行号；`rmus_set_selected` 写入，
    /// render 开始时清空——插件每帧显式声明，宿主不猜）
    selected: Option<usize>,
    /// 插件的线性内存；实例化后注入（宿主函数走 Caller 取，manifest 走这里）。
    memory: Option<wasmi::Memory>,
    /// 正在执行的方法调用 id（宿主侧编号；None = 无在途调用）
    current_call: Option<u64>,
    /// 被取消的 call id（跨线程共享，见 [`CancelHandle`]）
    cancelled: CancelHandle,
    /// `call` 执行期间插件发的通知（实时抛给回调）
    on_notify: Option<NotifyFn>,
    /// 插件回包（`rmus.reply` 写入；`call_method` 读走）
    reply_line: Vec<u8>,
    /// 最近一条后台任务事件（`KIND_DATA` 事件时插件经 `last_event` 取）
    last_event: Vec<u8>,
    /// worker 实例标记（set_cancel_handle 注入时置位）：cancelled 查询
    /// 按"集合非空即取消"语义（worker 一次只跑一个任务）
    worker_cancel: bool,
    /// 后台任务的事件出口：worker 实例的事件经它回注 UI 线程
    task_events: Option<TaskEvents>,
    /// 在途后台任务的取消句柄（`cancel_task` 用；Arc 共享给 worker 线程，
    /// 任务结束时由 worker 清理自己的条目）
    worker_cancels: Arc<Mutex<HashMap<u64, CancelHandle>>>,
    /// worker 实例化所需的 wasm 模块路径（`set_worker_source` 注入）
    wasm_path: Option<PathBuf>,
    /// 共享 HTTP agent（cookie store 开启，保留会话语义）
    agent: ureq::Agent,
    /// HTTP 句柄表（`rmus.http_open` 发起，`http_read`/`close` 消费）
    http: HashMap<i32, HttpEntry>,
    next_http: i32,
    /// 文件句柄表（`rmus.file_open` 创建，`file_write`/`close` 消费）
    files: HashMap<i32, std::fs::File>,
    next_file: i32,
    /// 下载产物根目录（`rmus.library_dir` 提供给插件）
    lib_dir: PathBuf,
    /// daemon RPC 出口（`rmus.daemon` 调用；未注入时 RPC 返回错误）
    daemon_call: Option<DaemonFn>,
    /// 最近一次 daemon RPC 的响应（`rmus.daemon_read` 读走；下次 `daemon` 覆盖）
    daemon_reply: Vec<u8>,
    /// daemon 数据根目录（`rmus.data_dir` 提供给插件）
    data_dir: PathBuf,
}

/// 一个进行中的 HTTP 响应：元数据在 open 时定格，body 流式读。
struct HttpEntry {
    status: u16,
    url: String,
    headers: Vec<(String, String)>,
    reader: Box<dyn std::io::BufRead + Send>,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            selected: None,
            memory: None,
            current_call: None,
            cancelled: CancelHandle::default(),
            on_notify: None,
            reply_line: Vec::new(),
            last_event: Vec::new(),
            task_events: None,
            worker_cancels: Arc::new(Mutex::new(HashMap::new())),
            worker_cancel: false,
            wasm_path: None,
            lib_dir: PathBuf::new(),
            daemon_call: None,
            daemon_reply: Vec::new(),
            data_dir: PathBuf::new(),
            // cookie store 未开启（rmus-dl 用了但接口实测不强制；TODO 需要时接 CookieStore）
            agent: ureq::AgentBuilder::new()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36")
                .build(),
            http: HashMap::new(),
            next_http: 1,
            files: HashMap::new(),
            next_file: 1,
        }
    }
}

/// 一个已加载的 wasm 插件实例。
pub struct Plugin {
    store: Store<HostState>,
    load: TypedFunc<(), ()>,
    /// UI 通道（方法插件没有，Option 表示不可用）
    update: Option<TypedFunc<i32, i32>>,
    render: Option<TypedFunc<(i32, i32), ()>>,
    manifest_ptr: Option<TypedFunc<(), i32>>,
    manifest_len: Option<TypedFunc<(), i32>>,
    /// 方法调用通道（SDK 的 `declare_plugin_manifest!` 带 handle 时才有）
    alloc: Option<TypedFunc<i32, i32>>,
    call_fn: Option<TypedFunc<(i32, i32), i32>>,
}

/// 从插件内存读 len 字节（宿主函数共用）。
fn read_memory(caller: &Caller<'_, HostState>, ptr: i32, len: i32) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(ptr >= 0 && len >= 0, "内存参数为负");
    let memory = caller
        .get_export("memory")
        .context("插件缺少 memory 导出")?
        .into_memory()
        .context("memory 导出不是线性内存")?;
    let mut buf = vec![0u8; len as usize];
    memory
        .read(caller, ptr as usize, &mut buf)
        .context("读取插件内存越界")?;
    Ok(buf)
}

/// 把字节写进插件缓冲（截断到 cap），返回写入长度。
fn write_guest(
    caller: &mut Caller<'_, HostState>,
    out: i32,
    cap: i32,
    bytes: &[u8],
) -> anyhow::Result<i32> {
    anyhow::ensure!(out >= 0 && cap >= 0, "缓冲参数非法");
    let n = bytes.len().min(cap as usize);
    let memory = caller.data().memory.context("缺少 memory")?;
    memory
        .write(&mut *caller, out as usize, &bytes[..n])
        .context("写入插件内存越界")?;
    Ok(n as i32)
}

impl Plugin {
    /// 从 wasm 字节码加载并调用 `load()`。`on_notify` 接收方法执行期间的
    /// `rmus.notify` 通知（如下载进度）。
    pub fn new(
        wasm: &[u8],
        on_notify: Box<dyn FnMut(Option<u64>, Value) + Send>,
    ) -> anyhow::Result<Self> {
        let engine = Engine::default();
        let module = Module::new(&engine, wasm).context("wasm 模块解析失败")?;
        let mut store = Store::new(&engine, HostState::default());
        store.data_mut().on_notify = Some(on_notify);
        let mut linker = <Linker<HostState>>::new(&engine);
        linker.func_wrap(
            "rmus",
            "print_line",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), Error> {
                // wasmi 的 func_wrap 闭包错误类型是 wasmi::Error；
                // 主体用 anyhow 写，出口统一转换
                let mut run = || -> anyhow::Result<()> {
                    let line = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("渲染行不是 UTF-8")?;
                    caller.data_mut().lines.push(RenderLine {
                        spans: vec![RenderSpan {
                            text: line,
                            style: "base".into(),
                        }],
                    });
                    Ok(())
                };
                run().map_err(|e| Error::new(e.to_string()))
            },
        )?;
        // print_spans：一行多段带样式，JSON `[{"text":"..","style":".."}]`
        linker.func_wrap(
            "rmus",
            "print_spans",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), Error> {
                let mut run = || -> anyhow::Result<()> {
                    let line = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("渲染行不是 UTF-8")?;
                    let spans: Vec<Value> =
                        serde_json::from_str(&line).context("spans 不是合法 JSON")?;
                    let mut out = RenderLine::default();
                    for span in spans {
                        out.spans.push(RenderSpan {
                            text: span
                                .get("text")
                                .and_then(|t| t.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            style: span
                                .get("style")
                                .and_then(|s| s.as_str())
                                .unwrap_or("base")
                                .to_string(),
                        });
                    }
                    caller.data_mut().lines.push(out);
                    Ok(())
                };
                run().map_err(|e| Error::new(e.to_string()))
            },
        )?;
        // set_selected：插件在 render 里声明画布选中行（宿主铺选中底、
        // 键位滚动都以此为准——选中状态归插件所有，宿主只投影）
        linker.func_wrap(
            "rmus",
            "set_selected",
            |mut caller: Caller<'_, HostState>, idx: i32| -> Result<(), Error> {
                caller.data_mut().selected =
                    if idx < 0 { None } else { Some(idx as usize) };
                Ok(())
            },
        )?;
        // 方法执行期间的通知：JSON 行 {"event":...,"data":...}，实时抛回调
        linker.func_wrap(
            "rmus",
            "notify",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), Error> {
                let mut run = || -> anyhow::Result<()> {
                    let line = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("通知不是 UTF-8")?;
                    let value: Value = serde_json::from_str(&line)
                        .context("通知不是合法 JSON")?;
                    let data = value.get("data").cloned().unwrap_or(Value::Null);
                    let current_call = caller.data().current_call;
                    let notify = caller.data_mut().on_notify.as_mut();
                    if let Some(notify) = notify {
                        notify(current_call, data);
                    }
                    Ok(())
                };
                run().map_err(|e| Error::new(e.to_string()))
            },
        )?;
        // 取消查询：当前 call 是否在取消集合里。worker 实例（后台任务，
        // current_call 恒 None）按"集合非空即取消"——它一次只跑一个任务，
        // `cancel_task` 标记的 task_id 就是那个任务。
        linker.func_wrap(
            "rmus",
            "cancelled",
            |caller: Caller<'_, HostState>| -> i32 {
                let state = caller.data();
                let set = state.cancelled.0.lock().unwrap();
                let r = match state.current_call {
                    Some(id) => set.contains(&id) as i32,
                    None => (state.worker_cancel && !set.is_empty()) as i32,
                };
                r
            },
        )?;
        // 方法回包：JSON 行 {"result":...} / {"error":"..."}
        linker.func_wrap(
            "rmus",
            "reply",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), Error> {
                let mut run = || -> anyhow::Result<()> {
                    let line = read_memory(&caller, ptr, len)?;
                    caller.data_mut().reply_line = line;
                    Ok(())
                };
                run().map_err(|e| Error::new(e.to_string()))
            },
        )?;
        // 时钟：阻塞插件线程（同步模型；wasm32-unknown-unknown 无 std 时钟）
        linker.func_wrap("rmus", "sleep_ms", |ms: i32| {
            if ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
            }
        })?;
        // ---- 网络代理（能力面：全部经宿主，插件无原生网络）----
        // http_open：JSON 请求 {"url","method"?,"headers"?{k:v},"body"?} → 句柄。
        // 非 2xx 也给句柄（错误码语义由插件判，reqwest 同款默认）。
        linker.func_wrap(
            "rmus",
            "http_open",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let req = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("请求不是 UTF-8")?;
                    let spec: Value = serde_json::from_str(&req).context("请求不是合法 JSON")?;
                    let url = spec
                        .get("url")
                        .and_then(|v| v.as_str())
                        .context("缺少 url")?
                        .to_string();
                    let method = spec
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("GET")
                        .to_string();
                    let agent = caller.data().agent.clone();
                    let mut request = agent.request(&method, &url);
                    if let Some(headers) = spec.get("headers").and_then(|h| h.as_object()) {
                        for (k, v) in headers {
                            if let Some(v) = v.as_str() {
                                request = request.set(k, v);
                            }
                        }
                    }
                    let body = spec.get("body").and_then(|b| b.as_str());
                    let response = match (method.as_str(), body) {
                        ("POST", Some(body)) => request.send_string(body),
                        ("PUT", Some(body)) => request.send_string(body),
                        (_, Some(body)) => request.send_string(body),
                        _ => request.call(),
                    };
                    let response = match response {
                        Ok(r) => r,
                        // 非 2xx：ureq 把 Response 藏在 Error::Status 里，照常入表
                        Err(ureq::Error::Status(_, r)) => r,
                        Err(e) => anyhow::bail!("http {method} {url}: {e}"),
                    };
                    let entry = HttpEntry {
                        status: response.status(),
                        url: response.get_url().to_string(),
                        headers: response
                            .headers_names()
                            .into_iter()
                            .filter_map(|name| {
                                response.header(&name).map(|v| (name, v.to_string()))
                            })
                            .collect(),
                        reader: Box::new(std::io::BufReader::new(response.into_reader())),
                    };
                    let state = caller.data_mut();
                    let handle = state.next_http;
                    state.next_http += 1;
                    state.http.insert(handle, entry);
                    Ok(handle)
                };
                run().unwrap_or(-1)
            },
        )?;
        linker.func_wrap("rmus", "http_status", |caller: Caller<'_, HostState>, h: i32| -> i32 {
            caller.data().http.get(&h).map(|e| e.status as i32).unwrap_or(-1)
        })?;
        // http_url / http_header：把字符串写进插件缓冲，返回长度（负 = 失败/缺失）
        linker.func_wrap(
            "rmus",
            "http_url",
            |mut caller: Caller<'_, HostState>, h: i32, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let s = caller
                        .data()
                        .http
                        .get(&h)
                        .map(|e| e.url.clone())
                        .context("句柄无效")?;
                    write_guest(&mut caller, out, cap, s.as_bytes())
                };
                run().unwrap_or(-1)
            },
        )?;
        linker.func_wrap(
            "rmus",
            "http_header",
            |mut caller: Caller<'_, HostState>,
             h: i32,
             name: i32,
             nl: i32,
             out: i32,
             cap: i32|
             -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let key = String::from_utf8(read_memory(&caller, name, nl)?)
                        .context("header 名不是 UTF-8")?
                        .to_ascii_lowercase();
                    let value = caller
                        .data()
                        .http
                        .get(&h)
                        .context("句柄无效")?
                        .headers
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v.clone())
                        .context("header 不存在")?;
                    write_guest(&mut caller, out, cap, value.as_bytes())
                };
                run().unwrap_or(-1)
            },
        )?;
        // http_read：读 body 块进插件缓冲；返回 n（0 = EOF，负 = 错误）
        linker.func_wrap(
            "rmus",
            "http_read",
            |mut caller: Caller<'_, HostState>, h: i32, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    anyhow::ensure!(out >= 0 && cap > 0, "缓冲参数非法");
                    let memory = caller.data().memory.context("缺少 memory")?;
                    let mut chunk = vec![0u8; cap as usize];
                    let n = {
                        let entry = caller
                            .data_mut()
                            .http
                            .get_mut(&h)
                            .context("句柄无效")?;
                        entry.reader.read(&mut chunk).context("读 body 失败")?
                    };
                    memory.write(&mut caller, out as usize, &chunk[..n])?;
                    Ok(n as i32)
                };
                run().unwrap_or(-1)
            },
        )?;
        linker.func_wrap("rmus", "http_close", |mut caller: Caller<'_, HostState>, h: i32| {
            caller.data_mut().http.remove(&h);
        })?;
        // ---- 文件代理（产物写库目录；权限白名单 TODO：现按宿主信任插件）----
        // file_open mode：0 = 写（create+truncate），1 = 读
        linker.func_wrap(
            "rmus",
            "file_open",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, mode: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let path = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("路径不是 UTF-8")?;
                    // 写模式隐式创建父目录（podcasts/<slug>/ 这类插落数据
                    // 目录；路径白名单 TODO 一起做，现按宿主信任插件）
                    if let Some(parent) = Path::new(&path).parent()
                        && (mode == 0 || mode == 2)
                    {
                        std::fs::create_dir_all(parent)
                            .with_context(|| format!("创建目录 {}", parent.display()))?;
                    }
                    let file = match mode {
                        // 0 = 写截断（下载临时文件）；1 = 只读；2 = 读写不截断
                        // （lofty tag 就地修改：rewind 重写 tag 区、音频原位保留）
                        0 => std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&path)
                            .with_context(|| format!("创建 {path} 失败"))?,
                        1 => std::fs::File::open(&path)
                            .with_context(|| format!("打开 {path} 失败"))?,
                        2 => std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(&path)
                            .with_context(|| format!("打开 {path} 失败"))?,
                        _ => anyhow::bail!("未知 mode {mode}"),
                    };
                    let state = caller.data_mut();
                    let handle = state.next_file;
                    state.next_file += 1;
                    state.files.insert(handle, file);
                    Ok(handle)
                };
                run().unwrap_or(-1)
            },
        )?;
        // file_read：读模式句柄读一块进插件缓冲；返回 n（0 = EOF，负 = 错误）
        linker.func_wrap(
            "rmus",
            "file_read",
            |mut caller: Caller<'_, HostState>, h: i32, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    anyhow::ensure!(out >= 0 && cap > 0, "缓冲参数非法");
                    let memory = caller.data().memory.context("缺少 memory")?;
                    let mut chunk = vec![0u8; cap as usize];
                    let n = {
                        let file = caller.data_mut().files.get_mut(&h).context("句柄无效")?;
                        std::io::Read::read(file, &mut chunk).context("读文件失败")?
                    };
                    memory.write(&mut caller, out as usize, &chunk[..n])?;
                    Ok(n as i32)
                };
                run().unwrap_or(-1)
            },
        )?;
        // file_seek：whence 0 = SeekStart / 1 = SeekCurrent / 2 = SeekEnd；返回新位置
        linker.func_wrap(
            "rmus",
            "file_seek",
            |mut caller: Caller<'_, HostState>, h: i32, off: i64, whence: i32| -> i64 {
                let mut run = || -> anyhow::Result<i64> {
                    use std::io::Seek;
                    let pos = match whence {
                        0 => std::io::SeekFrom::Start(off.max(0) as u64),
                        1 => std::io::SeekFrom::Current(off),
                        _ => std::io::SeekFrom::End(off),
                    };
                    let file = caller.data_mut().files.get_mut(&h).context("句柄无效")?;
                    Ok(file.seek(pos).context("seek 失败")? as i64)
                };
                run().unwrap_or(-1)
            },
        )?;
        // file_truncate / file_len：lofty 的 Truncate/Length trait 需要
        linker.func_wrap(
            "rmus",
            "file_truncate",
            |mut caller: Caller<'_, HostState>, h: i32, len: i64| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let file = caller.data_mut().files.get_mut(&h).context("句柄无效")?;
                    file.set_len(len.max(0) as u64).context("truncate 失败")?;
                    Ok(1)
                };
                run().unwrap_or(0)
            },
        )?;
        linker.func_wrap(
            "rmus",
            "file_len",
            |caller: Caller<'_, HostState>, h: i32| -> i64 {
                let run = || -> anyhow::Result<i64> {
                    let file = caller.data().files.get(&h).context("句柄无效")?;
                    Ok(file.metadata().context("metadata 失败")?.len() as i64)
                };
                run().unwrap_or(-1)
            },
        )?;
        linker.func_wrap(
            "rmus",
            "file_write",
            |mut caller: Caller<'_, HostState>, h: i32, ptr: i32, len: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    use std::io::Write;
                    let bytes = read_memory(&caller, ptr, len)?;
                    let file = caller
                        .data_mut()
                        .files
                        .get_mut(&h)
                        .context("句柄无效")?;
                    file.write_all(&bytes).context("写文件失败")?;
                    Ok(len)
                };
                run().unwrap_or(-1)
            },
        )?;
        linker.func_wrap(
            "rmus",
            "file_close",
            |mut caller: Caller<'_, HostState>, h: i32| {
                caller.data_mut().files.remove(&h); // drop = flush + close
            },
        )?;
        linker.func_wrap(
            "rmus",
            "fs_exists",
            |caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                match read_memory(&caller, ptr, len)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                {
                    Some(path) => {
                        std::path::Path::new(path.trim_end_matches('\0')).exists() as i32
                    }
                    None => 0,
                }
            },
        )?;
        linker.func_wrap(
            "rmus",
            "fs_remove",
            |caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                match read_memory(&caller, ptr, len)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                {
                    Some(path) => {
                        std::fs::remove_file(path.trim_end_matches('\0')).is_ok() as i32
                    }
                    None => 0,
                }
            },
        )?;
        linker.func_wrap(
            "rmus",
            "fs_rename",
            |caller: Caller<'_, HostState>,
             fp: i32,
             fl: i32,
             tp: i32,
             tl: i32|
             -> i32 {
                let run = || -> anyhow::Result<i32> {
                    let from = String::from_utf8(read_memory(&caller, fp, fl)?)?;
                    let to = String::from_utf8(read_memory(&caller, tp, tl)?)?;
                    std::fs::rename(from.trim_end_matches('\0'), to.trim_end_matches('\0'))
                        .context("rename 失败")?;
                    Ok(1)
                };
                run().unwrap_or(0)
            },
        )?;
        // 环境信息：库目录（下载产物根）
        linker.func_wrap(
            "rmus",
            "library_dir",
            |mut caller: Caller<'_, HostState>, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let dir = caller.data().lib_dir.display().to_string();
                    write_guest(&mut caller, out, cap, dir.as_bytes())
                };
                run().unwrap_or(-1)
            },
        )?;
        // daemon 数据根（插件数据落点；库根之外，躲音乐库扫描）
        linker.func_wrap(
            "rmus",
            "data_dir",
            |mut caller: Caller<'_, HostState>, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let dir = caller.data().data_dir.display().to_string();
                    write_guest(&mut caller, out, cap, dir.as_bytes())
                };
                run().unwrap_or(-1)
            },
        )?;
        // daemon RPC：请求 JSON（协议 Request 的 tagged 形态）进，白名单
        // 由 daemon 侧出口执行；响应暂存 daemon_reply，daemon_read 读走。
        // 响应包装 {"ok":true,"reply":..} / {"ok":false,"error":..}
        linker.func_wrap(
            "rmus",
            "daemon",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let text = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("daemon RPC 请求不是 UTF-8")?;
                    let request: Value =
                        serde_json::from_str(&text).context("daemon RPC 请求不是合法 JSON")?;
                    let f = caller
                        .data()
                        .daemon_call
                        .clone()
                        .context("daemon RPC 未接入")?;
                    let line = match f(request) {
                        Ok(reply) => serde_json::json!({ "ok": true, "reply": reply }),
                        Err(error) => serde_json::json!({ "ok": false, "error": error }),
                    };
                    caller.data_mut().daemon_reply = line.to_string().into_bytes();
                    Ok(0)
                };
                run().unwrap_or(-1)
            },
        )?;
        // 读走上次 daemon RPC 的响应：返回完整长度（截断时按它扩容重读，
        // read 可重复调用），内容写入按 cap 截断
        linker.func_wrap(
            "rmus",
            "daemon_read",
            |mut caller: Caller<'_, HostState>, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let bytes = caller.data().daemon_reply.clone();
                    let total = bytes.len();
                    let n = (cap as usize).min(total);
                    let memory = caller.data().memory.context("缺少 memory")?;
                    memory
                        .write(&mut caller, out as usize, &bytes[..n])
                        .context("写入插件内存越界")?;
                    Ok(total as i32)
                };
                run().unwrap_or(-1)
            },
        )?;
        // 墙钟毫秒（进度节流 / 临时文件名）
        linker.func_wrap("rmus", "now_ms", || -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        })?;
        // 调试日志：wasm32 没有 stderr，插件的日志经宿主写出
        linker.func_wrap(
            "rmus",
            "log",
            |caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), Error> {
                let run = || -> anyhow::Result<()> {
                    let line = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("日志行不是 UTF-8")?;
                    eprintln!("[rmusd] 插件日志: {line}");
                    Ok(())
                };
                run().map_err(|e| Error::new(e.to_string()))
            },
        )?;
        // 后台任务：spec {"task_id","method","params"} → 新实例在新线程执行
        // call_method；进度/完成经 set_worker_source 注入的出口回注 UI 线程。
        linker.func_wrap(
            "rmus",
            "spawn_task",
            |caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let run = || -> anyhow::Result<i32> {
                    let spec_text = String::from_utf8(read_memory(&caller, ptr, len)?)
                        .context("任务 spec 不是 UTF-8")?;
                    let spec: Value =
                        serde_json::from_str(&spec_text).context("任务 spec 不是合法 JSON")?;
                    let task_id = spec.get("task_id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let method = spec
                        .get("method")
                        .and_then(|v| v.as_str())
                        .context("缺少 method")?
                        .to_string();
                    let params = spec.get("params").cloned().unwrap_or(Value::Null);
                    let (wasm_path, lib_dir, events, daemon_call, data_dir) = {
                        let state = caller.data();
                        (
                            state.wasm_path.clone().context("未配置后台任务源")?,
                            state.lib_dir.clone(),
                            state.task_events.clone().context("未配置后台任务源")?,
                            state.daemon_call.clone(),
                            state.data_dir.clone(),
                        )
                    };
                    // 任务取消句柄：宿主先建、注册进表（cancel_task 用）、
                    // 注入 worker 实例——跨线程三方共享
                    let handle = CancelHandle::new();
                    caller
                        .data()
                        .worker_cancels
                        .lock()
                        .unwrap()
                        .insert(task_id, handle.clone());
                    let cancels: Arc<Mutex<HashMap<u64, CancelHandle>>> =
                        caller.data().worker_cancels.clone();
                    std::thread::Builder::new()
                        .name(format!("rmus-worker-{task_id}"))
                        .spawn(move || {
                            let run = || -> anyhow::Result<()> {
                                let wasm = std::fs::read(&wasm_path)?;
                                let emit = {
                                    let events = events.clone();
                                    move |event: &str, data: Value| {
                                        let line = serde_json::json!({
                                            "task_id": task_id,
                                            "event": event,
                                            "data": data,
                                        });
                                        events(line.to_string());
                                    }
                                };
                                // worker 进度：作为 task 事件实时回注
                                let notify = {
                                    let emit = emit.clone();
                                    Box::new(move |_call: Option<u64>, data: Value| {
                                        emit("progress", data);
                                    }) as NotifyFn
                                };
                                let mut worker = Plugin::new(&wasm, notify)?;
                                worker.set_lib_dir(lib_dir);
                                worker.set_cancel_handle(handle);
                                // worker 与主实例同能力面：进度采样 / 连播
                                // 接管都在 worker 里做，daemon RPC 必须可用
                                if let Some(f) = daemon_call {
                                    worker.set_daemon_call(f);
                                }
                                worker.set_data_dir(data_dir);
                                let outcome = worker.call_method(&method, &params);
                                cancels.lock().unwrap().remove(&task_id);
                                match outcome {
                                    Ok(Ok(result)) => emit("done", result),
                                    Ok(Err(error)) => emit("failed", Value::String(error)),
                                    Err(error) => emit("failed", Value::String(error.to_string())),
                                }
                                Ok(())
                            };
                            if let Err(e) = run() {
                                eprintln!("[rmusd] worker 任务 {task_id} 异常: {e:#}");
                                cancels.lock().unwrap().remove(&task_id);
                                events(
                                    serde_json::json!({
                                        "task_id": task_id,
                                        "event": "failed",
                                        "data": e.to_string(),
                                    })
                                    .to_string(),
                                );
                            }
                        })
                        .context("worker 线程启动失败")?;
                    Ok(1)
                };
                run().unwrap_or(0)
            },
        )?;
        // cancel_task：标记后台任务取消（worker 循环自查生效）
        linker.func_wrap(
            "rmus",
            "cancel_task",
            |caller: Caller<'_, HostState>, task_id: i64| -> i32 {
                let found = caller
                    .data()
                    .worker_cancels
                    .lock()
                    .unwrap()
                    .get(&(task_id.max(0) as u64))
                    .is_some();
                if found {
                    caller
                        .data()
                        .worker_cancels
                        .lock()
                        .unwrap()
                        .get(&(task_id.max(0) as u64))
                        .map(|h| h.cancel(task_id.max(0) as u64));
                }
                found as i32
            },
        )?;
        // last_event：最近一条后台任务事件（KIND_DATA 的 update 内读取）
        linker.func_wrap(
            "rmus",
            "last_event",
            |mut caller: Caller<'_, HostState>, out: i32, cap: i32| -> i32 {
                let mut run = || -> anyhow::Result<i32> {
                    let bytes = caller.data().last_event.clone();
                    // 返回完整长度（写入按 cap 截断）——大事件（如整份 feed
                    // 的 done 回执）由插件侧扩容重读
                    let memory = caller.data().memory.context("缺少 memory")?;
                    let n = bytes.len().min(cap as usize);
                    memory
                        .write(&mut caller, out as usize, &bytes[..n])
                        .context("写入插件内存越界")?;
                    Ok(bytes.len() as i32)
                };
                run().unwrap_or(-1)
            },
        )?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)?;
        store.data_mut().memory = instance.get_memory(&store, "memory");

        let load = instance
            .get_typed_func::<(), ()>(&store, "load")
            .context("插件缺少 export: load")?;
        let update = instance.get_typed_func::<i32, i32>(&store, "update").ok();
        let render = instance.get_typed_func::<(i32, i32), ()>(&store, "render").ok();
        let manifest_ptr = instance.get_typed_func::<(), i32>(&store, "manifest_ptr").ok();
        let manifest_len = instance.get_typed_func::<(), i32>(&store, "manifest_len").ok();
        let alloc = instance.get_typed_func::<i32, i32>(&store, "alloc").ok();
        let call_fn = instance.get_typed_func::<(i32, i32), i32>(&store, "call").ok();

        load.call(&mut store, ())?;
        Ok(Self {
            store,
            load,
            update,
            render,
            manifest_ptr,
            manifest_len,
            alloc,
            call_fn,
        })
    }

    /// 设置当前方法调用的宿主编号（`rmus.cancelled` / notify 的 call 字段用）。
    pub fn set_current_call(&mut self, call: Option<u64>) {
        self.store.data_mut().current_call = call;
    }

    /// 写入最近一条后台任务事件（UI 线程收到 worker 数据时；插件经
    /// `rmus.last_event` 读走）。
    pub fn set_last_event(&mut self, json: &[u8]) {
        self.store.data_mut().last_event = json.to_vec();
    }

    /// 让实例使用宿主提供的取消句柄（worker 实例：宿主先建 handle 再注入，
    /// 这样 `cancel_task` 能跨线程标记任务取消）。
    pub fn set_cancel_handle(&mut self, handle: CancelHandle) {
        let state = self.store.data_mut();
        state.cancelled = handle;
        state.worker_cancel = true;
    }

    /// 读取插件 manifest（UTF-8 JSON）。SDK 经 `manifest_ptr`/`manifest_len`
    /// 两个 export 暴露静态字符串；两者缺一视为插件没有 manifest。
    pub fn manifest(&mut self) -> anyhow::Result<String> {
        let (Some(ptr_fn), Some(len_fn)) = (&self.manifest_ptr, &self.manifest_len) else {
            bail!("插件没有 manifest export");
        };
        let ptr = ptr_fn.call(&mut self.store, ())?;
        let len = len_fn.call(&mut self.store, ())?;
        anyhow::ensure!(ptr > 0 && len > 0, "manifest 指针/长度非法");
        let memory = self.store.data().memory.context("插件缺少 memory 导出")?;
        let mut buf = vec![0u8; len as usize];
        memory
            .read(&self.store, ptr as usize, &mut buf)
            .context("读取 manifest 越界")?;
        String::from_utf8(buf).context("manifest 不是 UTF-8")
    }

    /// 注入事件。返回 `true` 表示插件报告了状态变化（需要重绘）。
    pub fn update(&mut self, event: i32) -> anyhow::Result<bool> {
        let Some(update) = &self.update else {
            bail!("插件没有 UI 通道（update export）");
        };
        Ok(update.call(&mut self.store, event)? != 0)
    }

    /// 重绘：清空收集缓冲 → 调 `render` → 返回插件输出的行。
    pub fn render(&mut self, rows: u16, cols: u16) -> anyhow::Result<&[RenderLine]> {
        let Some(render) = &self.render else {
            bail!("插件没有 UI 通道（render export）");
        };
        if rows == 0 || cols == 0 {
            bail!("render 区域为空");
        }
        self.store.data_mut().lines.clear();
        self.store.data_mut().selected = None;
        render.call(&mut self.store, (rows as i32, cols as i32))?;
        Ok(&self.store.data().lines)
    }

    /// 本轮 `render` 里插件声明的选中行（`rmus_set_selected`；未声明 = None）。
    pub fn take_selected(&self) -> Option<usize> {
        self.store.data().selected
    }

    /// 二次 `load`（验证 lifecycle 可重入）。
    pub fn reload(&mut self) -> anyhow::Result<()> {
        self.load.call(&mut self.store, ())?;
        Ok(())
    }

    /// 设置库目录（`rmus.library_dir` 返回给插件；下载产物根）。
    pub fn set_lib_dir(&mut self, dir: PathBuf) {
        self.store.data_mut().lib_dir = dir;
    }

    /// 注入 daemon RPC 出口（`rmus.daemon` 的执行方）。未注入时 RPC 报
    /// "未接入"——纯 UI 插件 / 测试桩可以不接。
    pub fn set_daemon_call(&mut self, f: DaemonFn) {
        self.store.data_mut().daemon_call = Some(f);
    }

    /// 注入 daemon 数据根目录（`rmus.data_dir` 返回值）。
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.store.data_mut().data_dir = dir;
    }

    /// 配置后台任务源：worker 实例从该 wasm 文件实例化；任务事件（进度/
    /// done/failed 的 JSON 行）经 `events` 回注 UI 线程。
    pub fn set_worker_source(&mut self, wasm_path: PathBuf, events: TaskEvents) {
        let state = self.store.data_mut();
        state.wasm_path = Some(wasm_path);
        state.task_events = Some(events);
    }

    /// 取消句柄（clone 给会话线程，长任务执行中也能标记取消）。
    pub fn cancel_handle(&self) -> CancelHandle {
        self.store.data().cancelled.clone()
    }

    /// 标记某 call 已取消（`rmus.cancelled` 之后返回真）。
    pub fn mark_cancelled(&mut self, call: u64) {
        self.cancel_handle().cancel(call);
    }

    /// 执行方法调用。请求行 `{"method","params"}` 写进插件内存 → `call` →
    /// 读回包 `{"result":...}` / `{"error":"..."}`。执行期间的 notify 经
    /// 构造时的回调实时抛出。
    pub fn call_method(
        &mut self,
        method: &str,
        params: &Value,
    ) -> anyhow::Result<Result<Value, String>> {
        let (Some(alloc), Some(call_fn)) = (&self.alloc, &self.call_fn) else {
            bail!("插件不支持方法调用（缺少 alloc/call export）");
        };
        let request = serde_json::json!({"method": method, "params": params});
        let bytes = serde_json::to_vec(&request)?;
        let ptr = alloc.call(&mut self.store, bytes.len() as i32)?;
        anyhow::ensure!(ptr > 0, "alloc 返回非法指针");
        {
            let memory = self.store.data().memory.context("插件缺少 memory 导出")?;
            memory
                .write(&mut self.store, ptr as usize, &bytes)
                .context("写入请求越界")?;
        }
        self.store.data_mut().reply_line.clear();
        let rc = call_fn.call(&mut self.store, (ptr, bytes.len() as i32))?;
        anyhow::ensure!(rc == 0, "call 返回非零退出码 {rc}");
        let reply = std::mem::take(&mut self.store.data_mut().reply_line);
        anyhow::ensure!(!reply.is_empty(), "插件没有回包（每请求必须恰好一个响应）");
        let value: Value =
            serde_json::from_slice(&reply).context("回包不是合法 JSON")?;
        if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
            return Ok(Err(error.to_string()));
        }
        Ok(Ok(value.get("result").cloned().unwrap_or(Value::Null)))
    }
}
