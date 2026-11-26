//! rmus 原生插件 SDK 与线程本地运行时上下文。
//!
//! 所有插件 crate（`rmus-dl` / `rmus-podcast` / `rmus-fm` / `rmus-dj`）直接
//! 静态链接进 `rmusd`，不再经 wasm32 / wasmi 解释执行：
//! - HTTP 直走 `ureq`（带代理与 cookie 支持）
//! - 文件 IO 直走 `std::fs::File`
//! - 渲染输出、daemon RPC、后台任务与取消标记挂在每个插件线程 / worker 线程的
//!   thread-local [`runtime::HostContext`] 上，保留单线程顺序事件模型，零 unsafe。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

/// 原生插件的运行时支撑（供 `rmusd` 宿主与测试安装线程上下文）。
pub mod runtime {
    use super::*;

    /// 渲染行的一个带样式片段。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RenderSpan {
        pub text: String,
        pub style: String,
    }

    /// 渲染行（`render()` 的输出单位）。
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RenderLine {
        pub spans: Vec<RenderSpan>,
    }

    /// 一帧 `render()` 收集到的完整结果。
    #[derive(Debug, Default)]
    pub struct FrameOutput {
        pub lines: Vec<RenderLine>,
        pub selected: Option<usize>,
        pub list: Option<(usize, usize)>,
    }

    /// 跨线程取消句柄：标记某个 call_id / worker 已被取消。
    #[derive(Clone, Default)]
    pub struct CancelHandle(pub(crate) Arc<Mutex<HashSet<u64>>>);

    impl CancelHandle {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn cancel(&self, call: u64) {
            self.0.lock().unwrap().insert(call);
        }

        pub(crate) fn is_cancelled(&self, current_call: Option<u64>, worker_cancel: bool) -> bool {
            let set = self.0.lock().unwrap();
            match current_call {
                Some(id) => set.contains(&id),
                None => worker_cancel && !set.is_empty(),
            }
        }
    }

    pub type NotifyFn = Arc<dyn Fn(Option<u64>, serde_json::Value) + Send + Sync>;
    pub type DaemonFn = Arc<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;
    pub type SpawnTaskFn = Arc<dyn Fn(u64, String, serde_json::Value) -> bool + Send + Sync>;

    /// 原生插件描述符：每个内建插件 crate 导出 `pub fn plugin() -> NativePlugin`。
    #[derive(Clone, Copy)]
    pub struct NativePlugin {
        /// 与 `rmus::plugin::PluginManifest` 同形的静态 JSON 字符串。
        pub manifest: &'static str,
        /// UI 插件事件回调；返回非 0 表示需要重绘。
        pub update: Option<fn(i32) -> i32>,
        /// UI 插件渲染回调。
        pub render: Option<fn(i32, i32)>,
        /// 方法/任务入口。
        pub handle: fn(&str, &serde_json::Value) -> Result<serde_json::Value, String>,
    }

    /// 当前插件线程或 worker 线程的宿主上下文。
    #[derive(Default)]
    pub struct HostContext {
        pub lib_dir: PathBuf,
        pub data_dir: PathBuf,
        pub current_call: Option<u64>,
        pub cancelled: CancelHandle,
        pub worker_cancel: bool,
        pub on_notify: Option<NotifyFn>,
        pub daemon_call: Option<DaemonFn>,
        pub spawn_task: Option<SpawnTaskFn>,
        pub worker_cancels: Arc<Mutex<HashMap<u64, CancelHandle>>>,
        pub last_event: Option<serde_json::Value>,
        pub frame: FrameOutput,
    }

    thread_local! {
        pub(crate) static CTX: RefCell<Option<HostContext>> = const { RefCell::new(None) };
    }

    /// 在当前线程安装上下文（插件会话线程 / worker 线程启动时调用）。
    pub fn install_context(ctx: HostContext) {
        CTX.with(|cell| {
            *cell.borrow_mut() = Some(ctx);
        });
    }

    /// 清除当前线程上下文（测试收尾 / 线程退出）。
    pub fn clear_context() {
        CTX.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }

    /// 更新当前方法调用编号（供 `notify` 与 `cancelled()` 使用）。
    pub fn set_current_call(call: Option<u64>) {
        CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.current_call = call;
            }
        });
    }

    /// 写入最近一条事件（`KIND_DATA` 前调用，`task::last_event()` 读取）。
    pub fn set_last_event(event: serde_json::Value) {
        CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.last_event = Some(event);
            }
        });
    }

    /// 清空上一帧渲染缓冲（每次 `render()` 前调用）。
    pub fn begin_render() {
        CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.frame = FrameOutput::default();
            }
        });
    }

    /// 取出本帧渲染结果（每次 `render()` 后调用）。
    pub fn take_render() -> FrameOutput {
        CTX.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map(|ctx| std::mem::take(&mut ctx.frame))
                .unwrap_or_default()
        })
    }
}

pub use runtime::{HostContext, NativePlugin};

/// 输出一行纯文本渲染内容（单段正文色）。
pub fn print_line(text: &str) {
    ui::print(text);
}

/// 方法执行期间发通知（如下载进度）。宿主实时转发给所有客户端或回注 UI 线程。
pub fn notify(data: &serde_json::Value) {
    let callback = runtime::CTX.with(|cell| {
        let borrow = cell.borrow();
        let ctx = borrow.as_ref()?;
        Some((ctx.on_notify.clone()?, ctx.current_call))
    });
    if let Some((cb, call)) = callback {
        cb(call, data.clone());
    }
}

/// 当前方法调用是否已被取消（长任务循环里轮询；取消后应尽快回 error）。
pub fn cancelled() -> bool {
    runtime::CTX.with(|cell| {
        cell.borrow().as_ref().is_some_and(|ctx| {
            ctx.cancelled
                .is_cancelled(ctx.current_call, ctx.worker_cancel)
        })
    })
}

/// 阻塞当前插件/worker 线程指定毫秒数。
pub fn sleep(ms: u64) {
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// 后台任务：把长任务（下载/搜索/常驻 worker）丢给独立线程跑，UI 线程保持响应。
/// worker 的进度/完成以 `KIND_DATA` 事件注入，用 [`last_event`] 取数据。
pub mod task {
    use super::runtime;

    /// 发起后台任务：宿主在新线程执行本插件的 `handle(method, params)`，
    /// 任务事件（progress / done / failed）以 `KIND_DATA` 回注插件 UI 线程。
    pub fn spawn(task_id: u64, method: &str, params: &serde_json::Value) -> bool {
        let spawner = runtime::CTX.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|ctx| ctx.spawn_task.clone())
        });
        match spawner {
            Some(f) => f(task_id, method.to_string(), params.clone()),
            None => false,
        }
    }

    /// 取消后台任务（worker 循环自查 `cancelled()` 生效）。
    pub fn cancel(task_id: u64) -> bool {
        let cancels = runtime::CTX.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|ctx| ctx.worker_cancels.clone())
        });
        let Some(cancels) = cancels else {
            return false;
        };
        let guard = cancels.lock().unwrap();
        if let Some(handle) = guard.get(&task_id) {
            handle.cancel(task_id);
            true
        } else {
            false
        }
    }

    /// 取最近一条后台任务/daemon 事件。仅在 `KIND_DATA` 事件的 update 调用内有效。
    pub fn last_event() -> Option<serde_json::Value> {
        runtime::CTX.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|ctx| ctx.last_event.clone())
        })
    }
}

/// 插件调试日志（直接写到 daemon stderr）。
pub fn debug_log(text: &str) {
    eprintln!("[rmusd] 插件日志: {text}");
}

/// 墙钟毫秒（进度节流、临时文件名、排班）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 下载产物根目录（rmusd 的音乐库根目录）。
pub fn library_dir() -> String {
    runtime::CTX.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|ctx| ctx.lib_dir.display().to_string())
            .unwrap_or_default()
    })
}

/// daemon 数据根目录（`~/.local/share/rmus`）。播客/DJ/FM 等插件数据的落点；
/// 放这里而不是 `library_dir`——库根下的音频文件会被音乐库扫描收编。
pub fn data_dir() -> String {
    runtime::CTX.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|ctx| ctx.data_dir.display().to_string())
            .unwrap_or_default()
    })
}

/// daemon RPC（直通引擎/曲库，白名单校验在宿主侧）。
pub mod daemon {
    use super::runtime;

    /// 发起 RPC。`request` 是协议 Request 的 tagged JSON，返回 Reply 的 JSON。
    pub fn call(request: serde_json::Value) -> Result<serde_json::Value, String> {
        let f = runtime::CTX
            .with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|ctx| ctx.daemon_call.clone())
            })
            .ok_or_else(|| "daemon RPC 宿主不可用".to_string())?;
        f(request)
    }

    /// 当前播放状态快照（Reply::Status）。
    pub fn status() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "status" }))
    }

    /// 试听态播放这些文件（存档当前队列，播完自动恢复）。
    pub fn peek(paths: Vec<String>) -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "peek", "params": { "paths": paths } }))
    }

    /// 提前结束试听，恢复进入前的状态。
    pub fn back() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "back" }))
    }

    /// 当前队列（Reply::Queue：`{"tracks":[TrackInfo…]}`）。
    pub fn queue_list() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "queue_list" }))
    }

    /// 播放列表名与当前来源（`{"names":[…],"current":…}`）。
    pub fn playlists() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "playlists_list" }))
    }

    /// 库模糊搜索，限 10 条（`{"tracks":[TrackInfo…]}`，相关度降序）。
    pub fn find(query: &str) -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "find", "params": { "query": query } }))
    }

    /// 整库列表（`{"tracks":[TrackInfo…]}`；推荐建口味画像用）。
    pub fn lib_list() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "lib_list" }))
    }

    /// 加文件/目录到队列尾部（扫描在宿主侧完成）。
    pub fn add(paths: Vec<String>) -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "add", "params": { "paths": paths } }))
    }

    /// 加文件/目录到播放列表（不存在则创建；返回 `{"added":N}`）。
    pub fn playlist_add(name: &str, paths: Vec<String>) -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "playlist_add", "params": { "name": name, "paths": paths } }))
    }
}

/// 进程级共享 HTTP 客户端（自动读取系统代理 / socks 代理，统一 UA）。
static HTTP_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36")
        .build()
});

/// 原生 HTTP 客户端（直连 `ureq`；非 2xx 也返回 Response，状态码由调用方判定）。
pub mod http {
    use super::HTTP_AGENT;
    use std::cell::RefCell;
    use std::io::{BufRead, BufReader};

    /// 一个请求的描述。
    pub struct Request {
        pub url: String,
        pub method: String,
        pub headers: Vec<(String, String)>,
        pub body: Option<String>,
    }

    impl Request {
        pub fn get(url: impl Into<String>) -> Self {
            Self {
                url: url.into(),
                method: "GET".into(),
                headers: Vec::new(),
                body: None,
            }
        }

        pub fn post_form(url: impl Into<String>, form: &str) -> Self {
            Self {
                url: url.into(),
                method: "POST".into(),
                headers: vec![(
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                )],
                body: Some(form.to_string()),
            }
        }

        pub fn header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_string(), value.to_string()));
            self
        }
    }

    /// 已发起的响应。body 用 [`Response::read`] 流式读。
    pub struct Response {
        status: u16,
        url: String,
        headers: Vec<(String, String)>,
        reader: RefCell<Box<dyn BufRead + Send>>,
    }

    impl Response {
        /// 发起请求。网络层失败返回 Err；HTTP 4xx/5xx 照常返回 Response。
        pub fn send(req: &Request) -> anyhow::Result<Self> {
            let mut request = HTTP_AGENT.request(&req.method, &req.url);
            for (k, v) in &req.headers {
                request = request.set(k, v);
            }
            let response = match req.body.as_deref() {
                Some(body) => request.send_string(body),
                None => request.call(),
            };
            let response = match response {
                Ok(r) => r,
                Err(ureq::Error::Status(_, r)) => r,
                Err(e) => anyhow::bail!("http {} {} 失败: {e}", req.method, req.url),
            };
            let status = response.status();
            let url = response.get_url().to_string();
            let headers = response
                .headers_names()
                .into_iter()
                .filter_map(|name| {
                    response
                        .header(&name)
                        .map(|v| (name.to_ascii_lowercase(), v.to_string()))
                })
                .collect();
            Ok(Self {
                status,
                url,
                headers,
                reader: RefCell::new(Box::new(BufReader::new(response.into_reader()))),
            })
        }

        pub fn status(&self) -> u16 {
            self.status
        }

        /// 重定向跟随后的最终 URL。
        pub fn url(&self) -> String {
            self.url.clone()
        }

        pub fn header(&self, name: &str) -> Option<String> {
            let key = name.to_ascii_lowercase();
            self.headers
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        }

        /// 读一段 body；返回 0 表示 EOF。
        pub fn read(&self, buf: &mut [u8]) -> anyhow::Result<usize> {
            let n = std::io::Read::read(&mut *self.reader.borrow_mut(), buf)?;
            Ok(n)
        }

        /// 读完整个 body（内存式；大文件请用 [`Response::read`] 流式）。
        pub fn read_to_end(&self) -> anyhow::Result<Vec<u8>> {
            let mut out = Vec::new();
            let mut chunk = [0u8; 64 * 1024];
            loop {
                let n = self.read(&mut chunk)?;
                if n == 0 {
                    return Ok(out);
                }
                out.extend_from_slice(&chunk[..n]);
            }
        }

        /// 状态码非 2xx 时报错（reqwest::error_for_status 同款语义）。
        pub fn error_for_status(self) -> anyhow::Result<Self> {
            let status = self.status();
            if (400..600).contains(&status) {
                anyhow::bail!("HTTP {status}");
            }
            Ok(self)
        }
    }
}

/// 持久化拉黑名单（`<data_dir>/blocked.json`）：用户手动删除原文件后自动记入，
/// 所有推荐器（自动推荐 / rmus-fm / rmus-dj）共用同一份黑名单，不再重复下载。
pub mod blocklist {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    const SEPARATORS: [char; 7] = ['&', '、', '/', ',', '，', ';', '；'];

    /// 归一化去重键（标题 + 排序后的歌手集，忽略空白与大小写）。
    pub fn dedup_key(title: &str, artist: &str) -> String {
        let norm = |s: &str| -> String {
            s.chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect()
        };
        let t = norm(title);
        let mut artists: Vec<String> = artist
            .split(SEPARATORS)
            .map(norm)
            .filter(|a| !a.is_empty())
            .collect();
        artists.sort();
        artists.dedup();
        format!("{t}\0{}", artists.join("&"))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Entry {
        pub title: String,
        pub artist: String,
        pub at: u64,
    }

    fn file_path(data_dir: &Path) -> PathBuf {
        data_dir.join("blocked.json")
    }

    /// 读取全部拉黑条目；文件缺失或损坏返回空表。
    pub fn load_entries(data_dir: &Path) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(file_path(data_dir)) else {
            return Vec::new();
        };
        let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(&text) else {
            return Vec::new();
        };
        arr.into_iter()
            .filter_map(|v| {
                let title = v.get("title")?.as_str()?.trim().to_string();
                let artist = v
                    .get("artist")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let at = v.get("at").and_then(|a| a.as_u64()).unwrap_or(0);
                (!title.is_empty()).then_some(Entry { title, artist, at })
            })
            .collect()
    }

    /// 读取拉黑键集合（供推荐器快速去重）。
    pub fn load_keys(data_dir: &Path) -> HashSet<String> {
        load_entries(data_dir)
            .into_iter()
            .map(|e| dedup_key(&e.title, &e.artist))
            .collect()
    }

    /// 追加一条拉黑记录（按归一化键去重，原子落盘）。
    pub fn add(data_dir: &Path, title: &str, artist: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let artist = artist.trim();
        let mut entries = load_entries(data_dir);
        let key = dedup_key(title, artist);
        if entries.iter().any(|e| dedup_key(&e.title, &e.artist) == key) {
            return;
        }
        entries.push(Entry {
            title: title.to_string(),
            artist: artist.to_string(),
            at: super::now_ms(),
        });
        let path = file_path(data_dir);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let json_arr: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "title": e.title,
                    "artist": e.artist,
                    "at": e.at,
                })
            })
            .collect();
        if let Ok(text) = serde_json::to_string_pretty(&json_arr) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// 原生文件系统封装（写模式自动建父目录；实现 `lofty` 所需的 IO trait）。
pub mod fs {
    use anyhow::Context;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::Path;

    /// 文件句柄，直接包装 `std::fs::File`。
    pub struct File(std::fs::File);

    impl File {
        /// 写打开（create + truncate，自动建父目录）。
        pub fn create(path: &str) -> anyhow::Result<Self> {
            ensure_parent(path)?;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .with_context(|| format!("创建 {path} 失败"))?;
            Ok(Self(f))
        }

        /// 只读打开。
        pub fn open(path: &str) -> anyhow::Result<Self> {
            let f = std::fs::File::open(path).with_context(|| format!("打开 {path} 失败"))?;
            Ok(Self(f))
        }

        /// 读写打开（不截断；lofty tag 就地修改用）。
        pub fn open_rw(path: &str) -> anyhow::Result<Self> {
            ensure_parent(path)?;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .with_context(|| format!("打开 {path} 失败"))?;
            Ok(Self(f))
        }

        pub fn write_all(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
            self.0.write_all(bytes).context("写文件失败")
        }
    }

    fn ensure_parent(path: &str) -> anyhow::Result<()> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {}", parent.display()))?;
        }
        Ok(())
    }

    impl Write for File {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    impl Read for File {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Seek for File {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(pos)
        }
    }

    #[cfg(feature = "lofty-io")]
    impl lofty::io::Truncate for File {
        fn truncate(&mut self, new_len: u64) -> std::io::Result<()> {
            self.0.set_len(new_len)
        }
    }

    #[cfg(feature = "lofty-io")]
    impl lofty::io::Length for File {
        fn len(&self) -> std::io::Result<u64> {
            Ok(self.0.metadata()?.len())
        }
    }

    pub fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }

    pub fn remove(path: &str) -> bool {
        std::fs::remove_file(path).is_ok()
    }

    pub fn rename(from: &str, to: &str) -> bool {
        std::fs::rename(from, to).is_ok()
    }
}

/// 插件事件编码：`kind | (payload << 8)`（与 `rmus::proto::plugin_key` 对齐）。
pub mod event {
    pub const KIND_TICK: i32 = 1;
    pub const KIND_ENTER: i32 = 2;
    pub const KIND_UP: i32 = 3;
    pub const KIND_DOWN: i32 = 4;
    /// 普通字符键（payload = Unicode 码点）
    pub const KIND_CHAR: i32 = 5;
    /// 空格键
    pub const KIND_SPACE: i32 = 6;
    /// 后台任务 / daemon 事件（经 `task::last_event()` 取数据）
    pub const KIND_DATA: i32 = 7;
    /// 退格键（无 payload）
    pub const KIND_BACKSPACE: i32 = 8;
    /// 语义导航：跳到首项 / 末项 / 上半页 / 下半页
    pub const KIND_TOP: i32 = 9;
    pub const KIND_BOTTOM: i32 = 10;
    pub const KIND_HALF_UP: i32 = 11;
    pub const KIND_HALF_DOWN: i32 = 12;

    pub fn encode(kind: i32, payload: i32) -> i32 {
        kind | (payload << 8)
    }
    pub fn kind(event: i32) -> i32 {
        event & 0xFF
    }
    pub fn payload(event: i32) -> i32 {
        (event >> 8) & 0xFF_FFFF
    }
    /// CHAR 事件里的字符。
    pub fn char_of(event: i32) -> Option<char> {
        if kind(event) == KIND_CHAR {
            char::from_u32(payload(event) as u32)
        } else {
            None
        }
    }
}

/// UI 组件库：把组件渲染成带语义样式令牌的行序列，写入当前线程的渲染缓冲。
pub mod ui {
    use super::runtime::{self, RenderLine, RenderSpan};
    use unicode_width::UnicodeWidthStr;

    pub mod style {
        pub const BASE: &str = "base";
        pub const STRONG: &str = "strong";
        pub const FAINT: &str = "faint";
        pub const ACCENT: &str = "accent";
        pub const DANGER: &str = "danger";
        pub const OK: &str = "ok";
        pub const OK_BOLD: &str = "ok_bold";
        pub const COOL: &str = "cool";
        pub const DANGER_BOLD: &str = "danger_bold";
        pub const RULE: &str = "rule";
        pub const OK_BG: &str = "ok_bg";
    }

    /// 一个带样式的文本片段。
    #[derive(Debug, Clone)]
    pub struct Span {
        pub style: &'static str,
        pub text: String,
    }

    pub fn text(s: impl Into<String>) -> Span {
        Span {
            style: style::BASE,
            text: s.into(),
        }
    }

    pub fn span(style: &'static str, s: impl Into<String>) -> Span {
        Span {
            style,
            text: s.into(),
        }
    }

    /// 按显示宽度截断（超宽加省略号；与宿主渲染器同款规则）。
    pub fn fit(text: &str, w: usize) -> String {
        if w == 0 {
            return String::new();
        }
        let width: usize = text.width();
        if width <= w {
            return text.to_string();
        }
        let mut out = String::new();
        let mut used = 0usize;
        for ch in text.chars() {
            let cw = ch.to_string().width();
            if used + cw + 1 > w {
                break;
            }
            out.push(ch);
            used += cw;
        }
        out.push('…');
        out
    }

    /// 左对齐填充到宽度。
    pub fn pad_right(text: &str, w: usize) -> String {
        let mut out = fit(text, w);
        let width: usize = out.width();
        out.push_str(&" ".repeat(w.saturating_sub(width)));
        out
    }

    /// 右对齐填充到宽度。
    pub fn pad_left(text: &str, w: usize) -> String {
        let cut = fit(text, w);
        let width: usize = cut.width();
        let mut out = " ".repeat(w.saturating_sub(width));
        out.push_str(&cut);
        out
    }

    /// 结果表列布局（与 TUI 队列/音乐库同款）。
    #[derive(Debug, Clone, Copy)]
    pub struct Cols {
        pub index_x: u16,
        pub index_w: u16,
        pub title_x: u16,
        pub title_w: u16,
        pub artist_x: u16,
        pub artist_w: u16,
        pub album_x: u16,
        pub album_w: u16,
        pub dur_x: u16,
        pub dur_w: u16,
    }

    pub fn columns(width: u16) -> Cols {
        let right = width;
        let index_x = 2;
        let index_w = 3;
        let title_x = index_x + index_w + 2;
        let dur_w = if width >= 40 {
            7.min(width.saturating_sub(10)).max(4)
        } else {
            0
        };
        let dur_x = right.saturating_sub(dur_w);
        let meta_end = dur_x.saturating_sub(2);
        let span = meta_end.saturating_sub(title_x);

        let (title_w, artist_w, album_w) = if span >= 64 {
            let artist_w = span * 30 / 100;
            let album_w = span * 26 / 100;
            (
                span.saturating_sub(artist_w + album_w + 4),
                artist_w,
                album_w,
            )
        } else if span >= 12 {
            let artist_w = (span * 40 / 100).max(6);
            (span.saturating_sub(artist_w + 2), artist_w, 0)
        } else {
            (span, 0, 0)
        };

        let artist_x = title_x + title_w + 2;
        let album_x = artist_x + artist_w + 2;
        Cols {
            index_x,
            index_w,
            title_x,
            title_w,
            artist_x,
            artist_w,
            album_x,
            album_w,
            dur_x,
            dur_w,
        }
    }

    /// 声明画布选中行（画布行号，含表头偏移）。
    pub fn set_selected(idx: usize) {
        runtime::CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.frame.selected = Some(idx);
            }
        });
    }

    /// 声明当前列表语义：总条数与选中下标（0 起）。
    pub fn set_list(total: usize, index: usize) {
        runtime::CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.frame.list = Some((total, index.min(total.saturating_sub(1))));
            }
        });
    }

    /// 列表光标与窗口状态。
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct ListState {
        pub selected: usize,
        pub scroll: usize,
    }

    impl ListState {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn clamp(&mut self, len: usize) {
            if len == 0 {
                self.selected = 0;
                self.scroll = 0;
            } else {
                self.selected = self.selected.min(len - 1);
            }
        }

        pub fn up(&mut self) {
            self.selected = self.selected.saturating_sub(1);
        }

        pub fn down(&mut self, len: usize) {
            if self.selected + 1 < len {
                self.selected += 1;
            }
        }

        pub fn top(&mut self) {
            self.selected = 0;
        }

        pub fn bottom(&mut self, len: usize) {
            self.selected = len.saturating_sub(1);
        }

        pub fn handle(&mut self, event: i32, len: usize, page: usize) -> bool {
            let page = page.max(1);
            match super::event::kind(event) {
                super::event::KIND_UP => self.up(),
                super::event::KIND_DOWN => self.down(len),
                super::event::KIND_TOP => self.top(),
                super::event::KIND_BOTTOM => self.bottom(len),
                super::event::KIND_HALF_UP => {
                    self.selected = self.selected.saturating_sub(page);
                }
                super::event::KIND_HALF_DOWN => {
                    self.selected = (self.selected + page).min(len.saturating_sub(1));
                }
                _ => return false,
            }
            true
        }

        pub fn prepare(&mut self, len: usize, height: usize, header_rows: usize) -> usize {
            self.clamp(len);
            let first = window(&mut self.scroll, self.selected, len, height);
            if len > 0 {
                set_selected(header_rows + self.selected - first);
            }
            set_list(len, self.selected);
            first
        }
    }

    pub fn window(scroll: &mut usize, selected: usize, len: usize, height: usize) -> usize {
        if len == 0 || height == 0 {
            *scroll = 0;
            return 0;
        }
        let selected = selected.min(len - 1);
        if selected < *scroll {
            *scroll = selected;
        }
        if selected >= *scroll + height {
            *scroll = selected + 1 - height;
        }
        let max = len.saturating_sub(height);
        *scroll = (*scroll).min(max);
        *scroll
    }

    pub fn paint_progress(spans: &[Span], filled: usize) -> Vec<Span> {
        if filled == 0 {
            return spans.to_vec();
        }
        let mut out = Vec::with_capacity(spans.len() + 1);
        let mut used = 0usize;
        for s in spans {
            if used >= filled {
                out.push(s.clone());
                continue;
            }
            let total: usize = s.text.width();
            if used + total <= filled {
                out.push(Span {
                    style: style::OK_BG,
                    text: s.text.clone(),
                });
                used += total;
                continue;
            }
            let take = filled - used;
            let mut head = String::new();
            let mut w = 0usize;
            let mut tail = String::new();
            for ch in s.text.chars() {
                let cw = ch.to_string().width();
                if w + cw <= take {
                    head.push(ch);
                    w += cw;
                } else {
                    tail.push(ch);
                }
            }
            out.push(Span {
                style: style::OK_BG,
                text: head,
            });
            out.push(Span {
                style: s.style,
                text: tail,
            });
            used = filled;
        }
        out
    }

    /// 输出一行（多段、带样式）到当前线程渲染缓冲。
    pub fn print_line(spans: &[Span]) {
        runtime::CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.frame.lines.push(RenderLine {
                    spans: spans
                        .iter()
                        .map(|s| RenderSpan {
                            text: s.text.clone(),
                            style: s.style.to_string(),
                        })
                        .collect(),
                });
            }
        });
    }

    pub fn print(s: &str) {
        print_line(&[text(s)]);
    }

    pub fn header(title: &str, hint: &str) {
        print_line(&[
            span(style::STRONG, title),
            span(style::FAINT, format!("  ·  {hint}")),
        ]);
    }

    pub fn list_row(row: &str, selected: bool) {
        if selected {
            print_line(&[span(style::ACCENT, "▸ "), span(style::STRONG, row)]);
        } else {
            print_line(&[span(style::FAINT, "  "), text(row)]);
        }
    }

    pub fn list_row_custom(prefix: &str, text: &str, selected: bool) {
        let (mark, body_style) = if selected {
            (span(style::ACCENT, format!("▸ {prefix}")), style::STRONG)
        } else {
            (span(style::FAINT, format!("  {prefix}")), style::BASE)
        };
        print_line(&[mark, super::ui::span(body_style, text)]);
    }

    pub fn progress(received: u64, total: u64, label: &str, width: usize) {
        let pct = (received.min(total) * 100)
            .checked_div(total)
            .map_or(0, |p| p as usize);
        let bar_width = width.saturating_sub(label.chars().count() + 8).max(4);
        let filled = bar_width * pct / 100;
        let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
        print_line(&[
            span(style::FAINT, format!("{label} ")),
            span(style::ACCENT, bar),
            span(style::STRONG, format!(" {pct}%")),
        ]);
    }

    pub fn hints(hints: &[(&str, &str)]) {
        let mut spans = Vec::with_capacity(hints.len() * 3);
        for (i, (key, desc)) in hints.iter().enumerate() {
            if i > 0 {
                spans.push(span(style::FAINT, "   "));
            }
            spans.push(span(style::ACCENT, *key));
            spans.push(span(style::FAINT, format!(" {desc}")));
        }
        print_line(&spans);
    }
}

/// methods 数组的 JSON 拼接（`declare_native_plugin!` 内部用）。
#[macro_export]
#[doc(hidden)]
macro_rules! methods_json {
    [] => { "" };
    [ ($m:literal, $k:literal) ] => {
        concat!("{\"name\":\"", $m, "\",\"kind\":\"", $k, "\"}")
    };
    [ ($m:literal, $k:literal), $($rest:tt)+ ] => {
        concat!(
            "{\"name\":\"", $m, "\",\"kind\":\"", $k, "\"},",
            $crate::methods_json![$($rest)+]
        )
    };
}

/// 声明原生插件并生成 `pub fn plugin() -> rmus_plugin_sdk::NativePlugin`。
#[macro_export]
macro_rules! declare_native_plugin {
    // 混合形态 + tab 注册（无 CLI 入口，如播客）
    (
        name: $name:literal, version: $ver:literal, ui: true, tab: $tab:literal,
        methods: $methods:tt, handle: $handle:path, update: $update:path, render: $render:path $(,)?
    ) => {
        pub fn plugin() -> $crate::NativePlugin {
            static MANIFEST: &str = concat!(
                "{\"name\":\"", $name, "\",\"version\":\"", $ver,
                "\",\"ui\":true,\"tab\":\"", $tab, "\",\"methods\":[",
                $crate::methods_json!$methods,
                "]}"
            );
            $crate::NativePlugin {
                manifest: MANIFEST,
                update: Some($update),
                render: Some($render),
                handle: $handle,
            }
        }
    };

    // 混合形态 + tab 注册 + 命令行入口（如 dl / fm / dj）
    (
        name: $name:literal, version: $ver:literal, ui: true, tab: $tab:literal,
        cli: $cli:literal, methods: $methods:tt, handle: $handle:path, update: $update:path, render: $render:path $(,)?
    ) => {
        pub fn plugin() -> $crate::NativePlugin {
            static MANIFEST: &str = concat!(
                "{\"name\":\"", $name, "\",\"version\":\"", $ver,
                "\",\"ui\":true,\"tab\":\"", $tab,
                "\",\"cli\":\"", $cli, "\",\"methods\":[",
                $crate::methods_json!$methods,
                "]}"
            );
            $crate::NativePlugin {
                manifest: MANIFEST,
                update: Some($update),
                render: Some($render),
                handle: $handle,
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dummy_handle(_m: &str, _p: &serde_json::Value) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }
    fn dummy_update(_ev: i32) -> i32 {
        0
    }
    fn dummy_render(_r: i32, _c: i32) {}

    declare_native_plugin! {
        name: "demo", version: "0.1.0", ui: true, tab: "演示", cli: "demo",
        methods: [("search", "call"), ("cli", "task")],
        handle: dummy_handle, update: dummy_update, render: dummy_render,
    }

    #[test]
    fn native_manifest_parses_cleanly() {
        let p = plugin();
        let v: serde_json::Value = serde_json::from_str(p.manifest).unwrap();
        assert_eq!(v["name"], "demo");
        assert_eq!(v["tab"], "演示");
        assert_eq!(v["cli"], "demo");
        assert_eq!(v["methods"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn runtime_render_and_daemon_and_cancel() {
        let cancel = runtime::CancelHandle::new();
        let ctx = HostContext {
            lib_dir: PathBuf::from("/tmp/lib"),
            data_dir: PathBuf::from("/tmp/data"),
            cancelled: cancel.clone(),
            daemon_call: Some(Arc::new(|req| Ok(serde_json::json!({ "echo": req })))),
            ..Default::default()
        };
        runtime::install_context(ctx);

        assert_eq!(library_dir(), "/tmp/lib");
        assert_eq!(data_dir(), "/tmp/data");

        runtime::begin_render();
        ui::header("标题", "提示");
        ui::set_selected(2);
        ui::set_list(10, 3);
        let frame = runtime::take_render();
        assert_eq!(frame.lines.len(), 1);
        assert_eq!(frame.selected, Some(2));
        assert_eq!(frame.list, Some((10, 3)));

        let reply = daemon::status().unwrap();
        assert_eq!(reply["echo"]["method"], "status");

        runtime::set_current_call(Some(42));
        assert!(!cancelled());
        cancel.cancel(42);
        assert!(cancelled());

        runtime::clear_context();
    }

    #[test]
    fn blocklist_roundtrip_and_dedup() {
        let dir = std::env::temp_dir().join(format!("rmus-blocklist-test-{}", now_ms()));
        let _ = std::fs::remove_dir_all(&dir);
        blocklist::add(&dir, "知否知否", "胡夏 / 郁可唯");
        blocklist::add(&dir, "知否知否", "郁可唯&胡夏");
        let entries = blocklist::load_entries(&dir);
        assert_eq!(entries.len(), 1);
        let keys = blocklist::load_keys(&dir);
        assert!(keys.contains(&blocklist::dedup_key("知否知否", "胡夏、郁可唯")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
