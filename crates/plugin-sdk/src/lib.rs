//! rmus wasm 插件 SDK：插件与宿主之间 ABI 的插件侧封装。
//!
//! 架构取向学 Zellij：插件是 wasm32 core module，宿主（rmusd）用解释器
//! 跑实例并**提供全部宿主函数**——插件不直接链接 WASI，能力（渲染输出、
//! 方法回包、通知、取消查询、网络、文件、时钟）一律经宿主代理，便于做
//! 权限白名单。
//!
//! host functions 只在 `wasm32` target 真实链接；native target 下是
//! no-op stub——插件 crate 因此能在主机上直接 `cargo test` / 类型检查，
//! 不需要 wasm 工具链（API 调用在 native 下什么都不做）。
//!
//! # 插件 ABI（v2）
//!
//! 插件导出（按 manifest 类型）：
//!
//! ```text
//! load() -> ()                     实例化后调用一次，初始化状态
//! update(event: i32) -> i32        UI 插件：宿主推事件；非 0 = 需要重绘
//! render(rows: i32, cols: i32)     UI 插件：宿主在重绘前调用
//! alloc(len: i32) -> ptr           方法插件：宿主请求一块参数缓冲
//! call(ptr: i32, len: i32) -> i32  方法插件：执行请求行 {"method","params"}，
//!                                  回包经 rmus.reply 送出；返回非 0 = 异常失败
//! manifest_ptr() / manifest_len()  manifest 静态字符串
//! ```
//!
//! 方法回包是 `{"result":...}` 或 `{"error":"..."}`——形状与进程外插件的
//! NDJSON 协议一致（只是传输介质从 stdio 换成共享内存 + 宿主函数）。
//!
//! 单线程 + 同步模型（Zellij 同款假设）：插件内不需要任何并发原语的
//! 正确性保证，事件顺序由宿主保证。`alloc` 复用同一份缓冲也依赖这个
//! 假设。

/// 宿主函数的原始导入（wasm）/ no-op stub（native）。
mod host {
    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "rmus")]
    #[allow(dead_code)] // 未用导入无害：能力面全集声明，按插件需求取用
    unsafe extern "C" {
        #[link_name = "print_line"]
        pub fn rmus_print_line(ptr: *const u8, len: u32);
        #[link_name = "print_spans"]
        pub fn rmus_print_spans(ptr: *const u8, len: u32);
        #[link_name = "set_selected"]
        pub fn rmus_set_selected(idx: i32);
        #[link_name = "notify"]
        pub fn rmus_notify(ptr: *const u8, len: u32);
        #[link_name = "cancelled"]
        pub fn rmus_cancelled() -> i32;
        #[link_name = "reply"]
        pub fn rmus_reply(ptr: *const u8, len: u32);
        #[link_name = "sleep_ms"]
        pub fn rmus_sleep_ms(ms: u32);
        #[link_name = "log"]
        pub fn rmus_log(ptr: *const u8, len: u32);
        #[link_name = "spawn_task"]
        pub fn rmus_spawn_task(ptr: *const u8, len: u32) -> i32;
        #[link_name = "last_event"]
        pub fn rmus_last_event(out: *mut u8, cap: u32) -> i32;
        #[link_name = "cancel_task"]
        pub fn rmus_cancel_task(task_id: i64) -> i32;        #[link_name = "now_ms"]
        pub fn rmus_now_ms() -> i64;
        #[link_name = "library_dir"]
        pub fn rmus_library_dir(out: *mut u8, cap: u32) -> i32;
        // daemon RPC（白名单直通引擎：Status / Peek / Back）
        #[link_name = "daemon"]
        pub fn rmus_daemon(ptr: *const u8, len: u32) -> i32;
        #[link_name = "daemon_read"]
        pub fn rmus_daemon_read(out: *mut u8, cap: u32) -> i32;
        #[link_name = "data_dir"]
        pub fn rmus_data_dir(out: *mut u8, cap: u32) -> i32;
        // 网络
        #[link_name = "http_open"]
        pub fn rmus_http_open(ptr: *const u8, len: u32) -> i32;
        #[link_name = "http_status"]
        pub fn rmus_http_status(h: i32) -> i32;
        #[link_name = "http_url"]
        pub fn rmus_http_url(h: i32, out: *mut u8, cap: u32) -> i32;
        #[link_name = "http_header"]
        pub fn rmus_http_header(h: i32, name: *const u8, nl: u32, out: *mut u8, cap: u32) -> i32;
        #[link_name = "http_read"]
        pub fn rmus_http_read(h: i32, out: *mut u8, cap: u32) -> i32;
        #[link_name = "http_close"]
        pub fn rmus_http_close(h: i32);
        // 文件
        #[link_name = "file_open"]
        pub fn rmus_file_open(ptr: *const u8, len: u32, mode: i32) -> i32;
        #[link_name = "file_read"]
        pub fn rmus_file_read(h: i32, out: *mut u8, cap: u32) -> i32;
        #[link_name = "file_write"]
        pub fn rmus_file_write(h: i32, ptr: *const u8, len: u32) -> i32;
        #[link_name = "file_seek"]
        pub fn rmus_file_seek(h: i32, off: i64, whence: i32) -> i64;
        #[link_name = "file_truncate"]
        pub fn rmus_file_truncate(h: i32, len: i64) -> i32;
        #[link_name = "file_len"]
        pub fn rmus_file_len(h: i32) -> i64;
        #[link_name = "file_close"]
        pub fn rmus_file_close(h: i32);
        #[link_name = "fs_exists"]
        pub fn rmus_fs_exists(ptr: *const u8, len: u32) -> i32;
        #[link_name = "fs_remove"]
        pub fn rmus_fs_remove(ptr: *const u8, len: u32) -> i32;
        #[link_name = "fs_rename"]
        pub fn rmus_fs_rename(fp: *const u8, fl: u32, tp: *const u8, tl: u32) -> i32;
    }

    /// wasm32 下导入在 host 根（extern 块本身），直接可见；
    /// native 下统一走 native stub。
    #[cfg(not(target_arch = "wasm32"))]
    pub use native::*;

    /// native stub：签名与 wasm 导入一一对应。**标成 unsafe fn**——调用处
    /// 的 unsafe 块在两个 target 下都合法（native 下其实安全，仅为对齐）。
    #[cfg(not(target_arch = "wasm32"))]
    mod native {
        #![allow(dead_code)] // native 下只是 stub，保证插件 crate 能原生编译/测试
        pub unsafe fn rmus_print_line(_ptr: *const u8, _len: u32) {}
        pub unsafe fn rmus_print_spans(_ptr: *const u8, _len: u32) {}
        pub unsafe fn rmus_set_selected(_idx: i32) {}
        pub unsafe fn rmus_notify(_ptr: *const u8, _len: u32) {}
        pub unsafe fn rmus_cancelled() -> i32 {
            0
        }
        pub unsafe fn rmus_reply(_ptr: *const u8, _len: u32) {}
        pub unsafe fn rmus_sleep_ms(_ms: u32) {}
        pub unsafe fn rmus_spawn_task(_ptr: *const u8, _len: u32) -> i32 {
            0
        }
        pub unsafe fn rmus_last_event(_out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_cancel_task(_task_id: i64) -> i32 {
            0
        }
        pub unsafe fn rmus_now_ms() -> i64 {
            0
        }
        pub unsafe fn rmus_library_dir(_out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_daemon(_ptr: *const u8, _len: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_daemon_read(_out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_data_dir(_out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_http_open(_ptr: *const u8, _len: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_http_status(_h: i32) -> i32 {
            -1
        }
        pub unsafe fn rmus_http_url(_h: i32, _out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_http_header(
            _h: i32,
            _n: *const u8,
            _nl: u32,
            _out: *mut u8,
            _cap: u32,
        ) -> i32 {
            -1
        }
        pub unsafe fn rmus_http_read(_h: i32, _out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_http_close(_h: i32) {}
        pub unsafe fn rmus_file_open(_ptr: *const u8, _len: u32, _mode: i32) -> i32 {
            -1
        }
        pub unsafe fn rmus_file_read(_h: i32, _out: *mut u8, _cap: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_file_write(_h: i32, _ptr: *const u8, _len: u32) -> i32 {
            -1
        }
        pub unsafe fn rmus_file_seek(_h: i32, _off: i64, _whence: i32) -> i64 {
            -1
        }
        pub unsafe fn rmus_file_truncate(_h: i32, _len: i64) -> i32 {
            0
        }
        pub unsafe fn rmus_file_len(_h: i32) -> i64 {
            -1
        }
        pub unsafe fn rmus_file_close(_h: i32) {}
        pub unsafe fn rmus_fs_exists(_ptr: *const u8, _len: u32) -> i32 {
            0
        }
        pub unsafe fn rmus_fs_remove(_ptr: *const u8, _len: u32) -> i32 {
            0
        }
        pub unsafe fn rmus_fs_rename(_fp: *const u8, _fl: u32, _tp: *const u8, _tl: u32) -> i32 {
            0
        }
    }
}

/// 输出一行渲染内容。UTF-8、不含换行；宿主按调用次数分行收集，
/// 在每次 `render()` 开始时清空上一轮的输出。
pub fn print_line(text: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_print_line(text.as_ptr(), text.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text; // native 下 no-op（单元测试/类型检查用）
    }
}

/// 方法执行期间发通知（如下载进度）。宿主实时转发给所有客户端。
pub fn notify(data: &serde_json::Value) {
    let line = serde_json::json!({"event": "progress", "data": data});
    let bytes = match serde_json::to_vec(&line) {
        Ok(b) => b,
        Err(_) => return,
    };
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_notify(bytes.as_ptr(), bytes.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = bytes;
    }
}

/// 当前方法调用是否已被取消（长任务循环里轮询；取消后应尽快回 error）。
pub fn cancelled() -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_cancelled() != 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// 睡眠。wasm32-unknown-unknown 没有 std 时钟实现，经宿主代理阻塞
/// 插件线程（同步模型下等价于 std::thread::sleep）。
pub fn sleep(ms: u64) {
    let ms = ms.min(u32::MAX as u64) as u32;
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_sleep_ms(ms);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ms;
    }
}

/// 后台任务：把长任务（下载/搜索）丢给宿主的 worker 实例跑，UI 线程
/// 保持响应。worker 的进度/完成以 `KIND_DATA` 事件注入，用
/// [`last_event`] 取数据。
pub mod task {
    use super::host;

    /// 发起后台任务：宿主用同一个 wasm 的**新实例**执行
    /// `call_method(method, params)`，任务事件（进度/done/failed）以
    /// `KIND_DATA` 事件回注当前插件实例。返回 false = 宿主拒绝。
    pub fn spawn(task_id: u64, method: &str, params: &serde_json::Value) -> bool {
        let spec = serde_json::json!({
            "task_id": task_id,
            "method": method,
            "params": params,
        })
        .to_string();
        #[cfg(target_arch = "wasm32")]
        unsafe {
            host::rmus_spawn_task(spec.as_ptr(), spec.len() as u32) != 0
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = spec;
            false
        }
    }

    /// 取消后台任务（worker 循环自查生效）。
    pub fn cancel(task_id: u64) -> bool {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            host::rmus_cancel_task(task_id as i64) != 0
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = task_id;
            false
        }
    }

    /// 取最近一条后台任务事件：`{"task_id":N,"event":"progress|done|failed",
    /// "data":...}`。仅在 `KIND_DATA` 事件的 update 调用内有效。
    pub fn last_event() -> Option<serde_json::Value> {
        // 宿主返回完整长度、写入按 cap 截断——截断则扩容重读
        // （read 可重复调用，内容保持到下一次 set_last_event 覆盖）
        let mut cap = 64 * 1024;
        loop {
            let mut buf = vec![0u8; cap];
            let n = unsafe { host::rmus_last_event(buf.as_mut_ptr(), buf.len() as u32) };
            if n < 0 {
                return None;
            }
            if (n as usize) <= cap {
                buf.truncate(n as usize);
                return serde_json::from_slice(&buf).ok();
            }
            cap = n as usize + 64;
        }
    }
}

/// 插件调试日志（经宿主 stderr 写出；wasm32 没有 stderr，eprintln 无效）。
pub fn debug_log(text: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_log(text.as_ptr(), text.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("[插件] {text}");
    }
}

/// 发送方法回包（`declare_plugin_manifest!` 生成的 `call` export 内部用）。
#[doc(hidden)]
pub fn send_reply(line: &[u8]) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_reply(line.as_ptr(), line.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = line;
    }
}

/// 墙钟毫秒（进度节流、临时文件名）。
pub fn now_ms() -> u64 {
    // native 下用真实时间（stub 形同虚设，方便单测）
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host::rmus_now_ms().max(0) as u64
    }
}

/// 下载产物根目录（rmusd 的音乐库根目录）。
pub fn library_dir() -> String {
    read_into_host(|out, cap| unsafe { host::rmus_library_dir(out, cap) }).unwrap_or_default()
}

/// daemon 数据根目录（`~/.local/share/rmus`）。播客等插件数据的落点；
/// 放这里而不是 `library_dir`——库根下的音频文件会被音乐库扫描收编。
pub fn data_dir() -> String {
    read_into_host(|out, cap| unsafe { host::rmus_data_dir(out, cap) }).unwrap_or_default()
}

/// daemon RPC（宿主代理，白名单直通引擎）。
///
/// 白名单 v1：`Status`（进度采样）/ `Peek`（试听态播放，播客点播与连播
/// 都靠它）/ `Back`（结束试听）。其余请求宿主侧一律拒绝。
pub mod daemon {
    use super::host;

    /// 发起 RPC。`request` 是协议 Request 的 tagged JSON
    /// （`{"method":..,"params":..}`），返回 Reply 的 JSON（untagged 形态）；
    /// Err = daemon 侧错误信息。
    pub fn call(request: serde_json::Value) -> Result<serde_json::Value, String> {
        let text = serde_json::to_string(&request).map_err(|e| e.to_string())?;
        let rc = unsafe { host::rmus_daemon(text.as_ptr(), text.len() as u32) };
        if rc != 0 {
            return Err("daemon RPC 宿主不可用".into());
        }
        // 响应读取：daemon_read 返回完整长度、写入按 cap 截断——
        // 截断则按完整长度扩容重读（read 可重复调用，直到下次 daemon 覆盖）
        let mut cap = 4096usize;
        loop {
            let mut buf = vec![0u8; cap];
            let n = unsafe { host::rmus_daemon_read(buf.as_mut_ptr(), buf.len() as u32) };
            if n < 0 {
                return Err("daemon RPC 响应读取失败".into());
            }
            if (n as usize) <= cap {
                buf.truncate(n as usize);
                let line: serde_json::Value =
                    serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
                return match line.get("ok").and_then(|v| v.as_bool()) {
                    Some(true) => Ok(line.get("reply").cloned().unwrap_or(serde_json::Value::Null)),
                    _ => Err(line
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("未知错误")
                        .to_string()),
                };
            }
            cap = n as usize + 64;
        }
    }

    /// 当前播放状态快照（Reply::Status：state/position_ms/duration_ms/track…）。
    pub fn status() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "status" }))
    }

    /// 试听态播放这些文件（存档当前队列，播完自动恢复）。已在试听态时
    /// 只换临时队列、原存档不动——播客连播的无缝切集就靠这个语义。
    pub fn peek(paths: Vec<String>) -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "peek", "params": { "paths": paths } }))
    }

    /// 提前结束试听，恢复进入前的状态。
    pub fn back() -> Result<serde_json::Value, String> {
        call(serde_json::json!({ "method": "back" }))
    }
}

/// 宿主把字符串写进插件缓冲的通用封装（4KB 上限，够 URL/header/路径用）。
fn read_into_host(f: impl FnOnce(*mut u8, u32) -> i32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = f(buf.as_mut_ptr(), buf.len() as u32);
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// HTTP 客户端（宿主代理；非 2xx 也返回 Response，状态码由插件判定）。
pub mod http {
    use super::host;

    /// 一个请求的描述。
    pub struct Request {
        pub url: String,
        pub method: String,
        /// header 名小写化由宿主负责
        pub headers: Vec<(String, String)>,
        /// 文本 body（form / JSON；二进制上传暂无需求）
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

        fn to_json(&self) -> Vec<u8> {
            let mut headers = serde_json::Map::new();
            for (k, v) in &self.headers {
                headers.insert(k.clone(), serde_json::json!(v));
            }
            serde_json::json!({
                "url": self.url,
                "method": self.method,
                "headers": headers,
                "body": self.body,
            })
            .to_string()
            .into_bytes()
        }
    }

    /// 已发起的响应。Drop 时自动关闭句柄；body 用 [`Response::read`] 流式读。
    pub struct Response(i32);

    impl Response {
        /// 发起请求。网络层失败（连接拒绝等）返回 Err；HTTP 错误码不算失败。
        pub fn send(req: &Request) -> anyhow::Result<Self> {
            let spec = req.to_json();
            let handle = unsafe { host::rmus_http_open(spec.as_ptr(), spec.len() as u32) };
            if handle < 0 {
                anyhow::bail!("http {} {} 失败", req.method, req.url);
            }
            Ok(Response(handle))
        }

        pub fn status(&self) -> u16 {
            unsafe { host::rmus_http_status(self.0).max(0) as u16 }
        }

        /// 重定向跟随后的最终 URL。
        pub fn url(&self) -> String {
            super::read_into_host(|out, cap| unsafe { host::rmus_http_url(self.0, out, cap) })
                .unwrap_or_default()
        }

        pub fn header(&self, name: &str) -> Option<String> {
            let name = name.to_string();
            super::read_into_host(|out, cap| unsafe {
                host::rmus_http_header(self.0, name.as_ptr(), name.len() as u32, out, cap)
            })
        }

        /// 读一段 body；返回 0 表示 EOF。
        pub fn read(&self, buf: &mut [u8]) -> anyhow::Result<usize> {
            let n = unsafe { host::rmus_http_read(self.0, buf.as_mut_ptr(), buf.len() as u32) };
            if n < 0 {
                anyhow::bail!("读 body 失败");
            }
            Ok(n as usize)
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

    impl Drop for Response {
        fn drop(&mut self) {
            unsafe { host::rmus_http_close(self.0) };
        }
    }
}

/// 文件系统（宿主代理；写入目标应是 [`library_dir`] 之下）。
///
/// [`File`] 实现了 [`std::io::Read`]/[`Write`]/[`Seek`]（经宿主代理），
/// lofty 这类需要 Read+Seek+Write 的库可以直接使用。
pub mod fs {
    use super::host;
    use std::io::SeekFrom;

    /// 文件句柄。Drop 时自动关闭（flush）。
    pub struct File(i32);

    impl File {
        /// 只写打开（create + truncate）。
        pub fn create(path: &str) -> anyhow::Result<Self> {
            Self::open_mode(path, 0, "创建")
        }

        /// 只读打开。
        pub fn open(path: &str) -> anyhow::Result<Self> {
            Self::open_mode(path, 1, "打开")
        }

        /// 读写打开（不截断；lofty tag 就地修改用）。
        pub fn open_rw(path: &str) -> anyhow::Result<Self> {
            Self::open_mode(path, 2, "打开")
        }

        fn open_mode(path: &str, mode: i32, action: &str) -> anyhow::Result<Self> {
            let handle = unsafe { host::rmus_file_open(path.as_ptr(), path.len() as u32, mode) };
            if handle < 0 {
                anyhow::bail!("{action} {path} 失败");
            }
            Ok(File(handle))
        }

        pub fn write_all(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
            let n = unsafe { host::rmus_file_write(self.0, bytes.as_ptr(), bytes.len() as u32) };
            if n < 0 {
                anyhow::bail!("写文件失败");
            }
            Ok(())
        }
    }

    impl std::io::Write for File {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = unsafe { host::rmus_file_write(self.0, buf.as_ptr(), buf.len() as u32) };
            if n < 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "宿主写文件失败",
                ));
            }
            Ok(n as usize)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(()) // 宿主侧 close（drop File）即落盘
        }
    }

    impl std::io::Read for File {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = unsafe { host::rmus_file_read(self.0, buf.as_mut_ptr(), buf.len() as u32) };
            if n < 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "宿主读文件失败",
                ));
            }
            Ok(n as usize)
        }
    }

    impl std::io::Seek for File {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            let (off, whence) = match pos {
                SeekFrom::Start(n) => (n as i64, 0),
                SeekFrom::Current(n) => (n, 1),
                SeekFrom::End(n) => (n, 2),
            };
            let p = unsafe { host::rmus_file_seek(self.0, off, whence) };
            if p < 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "宿主 seek 失败",
                ));
            }
            Ok(p as u64)
        }
    }

    // lofty 的 FileLike 要求（Read + Write + Seek + Truncate + Length）：
    // `lofty-io` feature 下为自家类型实现 lofty 的两个 IO trait，
    // 插件即可把 SDK File 直接交给 lofty 做 tag 读写
    #[cfg(feature = "lofty-io")]
    impl lofty::io::Truncate for File {
        fn truncate(&mut self, new_len: u64) -> std::io::Result<()> {
            let ok = unsafe { host::rmus_file_truncate(self.0, new_len as i64) };
            if ok == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "宿主 truncate 失败",
                ));
            }
            Ok(())
        }
    }

    #[cfg(feature = "lofty-io")]
    impl lofty::io::Length for File {
        fn len(&self) -> std::io::Result<u64> {
            let len = unsafe { host::rmus_file_len(self.0) };
            if len < 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "宿主取文件长度失败",
                ));
            }
            Ok(len as u64)
        }
    }

    impl Drop for File {
        fn drop(&mut self) {
            unsafe { host::rmus_file_close(self.0) };
        }
    }

    pub fn exists(path: &str) -> bool {
        unsafe { host::rmus_fs_exists(path.as_ptr(), path.len() as u32) != 0 }
    }

    pub fn remove(path: &str) -> bool {
        unsafe { host::rmus_fs_remove(path.as_ptr(), path.len() as u32) != 0 }
    }

    pub fn rename(from: &str, to: &str) -> bool {
        unsafe {
            host::rmus_fs_rename(from.as_ptr(), from.len() as u32, to.as_ptr(), to.len() as u32)
                != 0
        }
    }
}

/// v0 事件集：整数枚举，复杂载荷（键值字符串等）后续版本经共享内存传递。
///
/// 编码：`event = kind | (payload << 8)`——kind 低 8 位，payload 高 24 位
/// （字符码点直接放 payload，CJK 也在 24 位内）。`TICK`/`ENTER` 的数值与
/// 旧版枚举兼容。
pub mod event {
    pub const KIND_TICK: i32 = 1;
    pub const KIND_ENTER: i32 = 2;
    pub const KIND_UP: i32 = 3;
    pub const KIND_DOWN: i32 = 4;
    /// 普通字符键（payload = Unicode 码点）
    pub const KIND_CHAR: i32 = 5;
    /// 空格键（独立 kind，便于当"确认/播放"用）
    pub const KIND_SPACE: i32 = 6;
    /// 后台任务事件（数据在 `last_event` 缓冲，SDK `task::last_event()` 取）
    pub const KIND_DATA: i32 = 7;

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

/// UI 组件表 v0：把组件渲染成带样式的行序列（SDK 侧纯计算，native 可测）。
///
/// 设计取向：**组件表是 SDK 的绘图库，不是宿主协议**——宿主只认
/// "带样式的行"（`PluginLine`），组件在插件内存里展开成行再输出。
/// 宿主渲染器按语义样式令牌上色（自动跟随主题）并负责截断兜底。
///
/// ```ignore
/// use rmus_plugin_sdk::ui::{self, style};
/// ui::header("搜索结果", "d 下载 · x 取消");
/// ui::list_row("晴天 - 周杰伦", i == selected);
/// ui::progress(received, total, "下载中", cols as usize);
/// ```
pub mod ui {
    /// 语义样式令牌（宿主映射到主题色；未认识的令牌落回正文色）。
    use unicode_width::UnicodeWidthStr;

    pub mod style {
        pub const BASE: &str = "base";
        pub const STRONG: &str = "strong";
        pub const FAINT: &str = "faint";
        pub const ACCENT: &str = "accent";
        pub const DANGER: &str = "danger";
        /// 成功/高品质（flac 标签、完成 ✓）。
        pub const OK: &str = "ok";
        /// 成功加粗（完成 ✓）。
        pub const OK_BOLD: &str = "ok_bold";
        /// 冷色弱化（来源标签）。
        pub const COOL: &str = "cool";
        /// 失败加粗（✗）。
        pub const DANGER_BOLD: &str = "danger_bold";
        /// 分隔线。
        pub const RULE: &str = "rule";
        /// 进度条填充（成功色底、深色字）。
        pub const OK_BG: &str = "ok_bg";
    }

    /// 一个带样式的文本片段。
    #[derive(Debug, Clone)]
    pub struct Span {
        pub style: &'static str,
        pub text: String,
    }

    /// 正文色片段。
    pub fn text(s: impl Into<String>) -> Span {
        Span { style: style::BASE, text: s.into() }
    }
    /// 指定样式片段。
    pub fn span(style: &'static str, s: impl Into<String>) -> Span {
        Span { style, text: s.into() }
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

    /// 左对齐填充到宽度（列布局：先 fit 再补空格）。
    pub fn pad_right(text: &str, w: usize) -> String {
        let mut out = fit(text, w);
        let width: usize = out.width();
        out.extend(core::iter::repeat(' ').take(w.saturating_sub(width)));
        out
    }

    /// 右对齐填充到宽度（时长/音质等右侧列）。
    pub fn pad_left(text: &str, w: usize) -> String {
        let cut = fit(text, w);
        let width: usize = cut.width();
        let mut out: String = core::iter::repeat(' ').take(w.saturating_sub(width)).collect();
        out.push_str(&cut);
        out
    }

    /// 结果表列布局（与 TUI 队列/音乐库同款：曲名永远保留，宽度不够
    /// 时先砍专辑、再砍艺术家；极窄先砍时长）。
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

    /// 按画布宽度算列（画布从 x=0 起，与宿主渲染行对齐）。
    pub fn columns(width: u16) -> Cols {
        let right = width;
        let index_x = 2;
        let index_w = 3;
        let title_x = index_x + index_w + 2;
        // 宽度不够 40：时长整列让位
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

    /// 声明画布的选中行（画布行号，含表头等偏移）。宿主据此铺选中
    /// 底色并驱动滚动投影；每帧 `render` 里都要调用（宿主在帧开始
    /// 时清空上帧声明）。
    pub fn set_selected(idx: usize) {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            super::host::rmus_set_selected(idx as i32);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = idx;
    }

    /// 列表窗口化：返回可视区首行下标，保证 `selected` 落在窗口内。
    /// 插件画布自行滚动时用（画布高度 = `render` 的 `rows`）。
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

    /// 下载进度条：把行内前 `filled` 个显示宽度的片段换成 `ok_bg`
    /// 样式（跨界片段按显示宽度切开，前半进度后半正文）。`filled`
    /// 为 0 或超出行宽时原样返回。
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
                out.push(Span { style: style::OK_BG, text: s.text.clone() });
                used += total;
                continue;
            }
            // 跨界：按显示宽度切文本
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
            out.push(Span { style: style::OK_BG, text: head });
            out.push(Span { style: s.style, text: tail });
            used = filled;
        }
        out
    }

    /// 输出一行（多段、带样式）。行首空位（列 x 坐标）用
    /// [`pad_left`] 拼进首段文本实现——画布行是流式排列，没有绝对
    /// 定位；`columns` 给出的列宽就是用来在插件侧拼行的。
    pub fn print_line(spans: &[Span]) {
        #[cfg(target_arch = "wasm32")]
        {
            let payload: Vec<serde_json::Value> = spans
                .iter()
                .map(|s| serde_json::json!({ "text": s.text, "style": s.style }))
                .collect();
            let line = serde_json::Value::Array(payload).to_string();
            unsafe {
                super::host::rmus_print_spans(line.as_ptr(), line.len() as u32);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = spans;
        }
    }

    /// 输出一行纯文本（单段正文色；`print_line` 的便捷形态）。
    pub fn print(s: &str) {
        print_line(&[text(s)]);
    }

    // ---- 组件 ----

    /// 标题行：左标题（strong）+ 右提示（faint）。
    pub fn header(title: &str, hint: &str) {
        print_line(&[
            span(style::STRONG, title),
            span(style::FAINT, format!("  ·  {hint}")),
        ]);
    }

    /// 列表行：选中态加 `▸ ` 前缀并高亮，否则暗色 `  ` 前缀。
    pub fn list_row(row: &str, selected: bool) {
        if selected {
            print_line(&[
                span(style::ACCENT, "▸ "),
                span(style::STRONG, row),
            ]);
        } else {
            print_line(&[span(style::FAINT, "  "), text(row)]);
        }
    }

    /// 带前缀的列表行（行号 / 来源标签等由插件自行拼装）。
    pub fn list_row_custom(prefix: &str, text: &str, selected: bool) {
        let (mark, body_style) = if selected {
            (span(style::ACCENT, format!("▸ {prefix}")), style::STRONG)
        } else {
            (span(style::FAINT, format!("  {prefix}")), style::BASE)
        };
        print_line(&[mark, super::ui::span(body_style, text)]);
    }

    /// 进度条：`label ▕▊▊▋..... 42%`（块字符按比例填充）。
    pub fn progress(received: u64, total: u64, label: &str, width: usize) {
        let pct = if total > 0 {
            (received.min(total) * 100 / total) as usize
        } else {
            0
        };
        // 条宽 = width - 标签与百分比的空间（粗略预算，宿主 fit 兜底）
        let bar_width = width.saturating_sub(label.chars().count() + 8).max(4);
        let filled = bar_width * pct / 100;
        let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
        print_line(&[
            span(style::FAINT, format!("{label} ")),
            span(style::ACCENT, bar),
            span(style::STRONG, format!(" {pct}%")),
        ]);
    }

    /// 键位提示行：`(key, 说明)` 交替着色。
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

/// methods 数组的 JSON 拼接（`declare_plugin_manifest!` 内部用）。
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

/// 声明插件 manifest 并导出宿主所需的全部 export。
///
/// UI 插件（自带视图，宿主提供按键/渲染通道）：
///
/// ```
/// rmus_plugin_sdk::declare_plugin_manifest! {
///     name: "hello", version: "0.2.0", ui: true,
/// }
/// ```
///
/// 方法插件（search/download 这类能力，无界面）——`handle` 是同模块里
/// 用户定义的 `fn(&str, &serde_json::Value) -> Result<serde_json::Value, E>`（E: Display，如 String 或 anyhow::Error）：
///
/// ```
/// fn handle(method: &str, _params: &serde_json::Value) -> Result<serde_json::Value, String> {
///     Ok(serde_json::json!({}))
/// }
///
/// rmus_plugin_sdk::declare_plugin_manifest! {
///     name: "rmus-dl", version: "0.1.0", ui: false,
///     methods: [("search", "call"), ("download", "task"), ("cancel", "call")],
///     handle: handle,
/// }
/// ```
///
/// 用在 crate 根；一个 crate 只能声明一次。
#[macro_export]
macro_rules! declare_plugin_manifest {
    // UI 插件 + tab 注册（manifest 带 tab 字段，TUI tab 栏随插件动态增减）
    (name: $name:literal, version: $ver:literal, ui: true, tab: $tab:literal $(,)?) => {
        static MANIFEST: &str = concat!(
            "{\"name\":\"", $name, "\",\"version\":\"", $ver,
            "\",\"ui\":true,\"tab\":\"", $tab, "\",\"methods\":[]}"
        );

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_ptr() -> *const u8 {
            MANIFEST.as_ptr()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_len() -> u32 {
            MANIFEST.len() as u32
        }
    };

    // 混合形态 + tab 注册
    (
        name: $name:literal, version: $ver:literal, ui: true, tab: $tab:literal,
        methods: $methods:tt, handle: $handle:path $(,)?
    ) => {
        static MANIFEST: &str = concat!(
            "{\"name\":\"", $name, "\",\"version\":\"", $ver,
            "\",\"ui\":true,\"tab\":\"", $tab, "\",\"methods\":[",
            $crate::methods_json!$methods,
            "]}"
        );

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_ptr() -> *const u8 {
            MANIFEST.as_ptr()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_len() -> u32 {
            MANIFEST.len() as u32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn load() {}

        /// 请求缓冲：宿主经 alloc 拿指针写参数。单线程模型下复用一份。
        static mut REQUEST: Vec<u8> = Vec::new();

        #[unsafe(no_mangle)]
        pub extern "C" fn alloc(len: i32) -> *mut u8 {
            if len <= 0 {
                return core::ptr::null_mut();
            }
            // 单线程假设（见模块文档）；static_mut_refs 规避用 addr_of_mut
            let buf: &mut Vec<u8> = unsafe { &mut *core::ptr::addr_of_mut!(REQUEST) };
            buf.clear();
            buf.resize(len as usize, 0);
            buf.as_mut_ptr()
        }

        /// 执行一个请求行。恰好发一个回包；返回非 0 = 异常失败（宿主直接报错）。
        #[unsafe(no_mangle)]
        pub extern "C" fn call(ptr: *const u8, len: i32) -> i32 {
            if len < 0 {
                return 1;
            }
            let bytes: &[u8] = unsafe {
                let buf: &mut Vec<u8> = &mut *core::ptr::addr_of_mut!(REQUEST);
                if ptr != buf.as_ptr() {
                    return 1;
                }
                buf.get(..len as usize).unwrap_or(&[])
            };
            let run = || -> Result<(), String> {
                let request: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                let method = request
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string();
                let params = request.get("params").cloned().unwrap_or_default();
                // 错误按 Display 转 String（String / anyhow::Error 都兼容）
                let reply = match $handle(&method, &params) {
                    Ok(result) => serde_json::json!({ "result": result }),
                    Err(error) => serde_json::json!({ "error": error.to_string() }),
                };
                let line = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
                $crate::send_reply(&line);
                Ok(())
            };
            match run() {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
    };

    // UI 插件（不注册 tab）：只有渲染通道，待在「插件」清单里
    (name: $name:literal, version: $ver:literal, ui: true $(,)?) => {
        static MANIFEST: &str = concat!(
            "{\"name\":\"", $name, "\",\"version\":\"", $ver,
            "\",\"ui\":true,\"methods\":[]}"
        );

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_ptr() -> *const u8 {
            MANIFEST.as_ptr()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_len() -> u32 {
            MANIFEST.len() as u32
        }
    };

    // 混合形态（不注册 tab）：UI 通道 + 方法通道
    (
        name: $name:literal, version: $ver:literal, ui: true,
        methods: $methods:tt, handle: $handle:path $(,)?
    ) => {
        static MANIFEST: &str = concat!(
            "{\"name\":\"", $name, "\",\"version\":\"", $ver,
            "\",\"ui\":true,\"methods\":[",
            $crate::methods_json!$methods,
            "]}"
        );

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_ptr() -> *const u8 {
            MANIFEST.as_ptr()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_len() -> u32 {
            MANIFEST.len() as u32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn load() {}

        /// 请求缓冲：宿主经 alloc 拿指针写参数。单线程模型下复用一份。
        static mut REQUEST: Vec<u8> = Vec::new();

        #[unsafe(no_mangle)]
        pub extern "C" fn alloc(len: i32) -> *mut u8 {
            if len <= 0 {
                return core::ptr::null_mut();
            }
            // 单线程假设（见模块文档）；static_mut_refs 规避用 addr_of_mut
            let buf: &mut Vec<u8> = unsafe { &mut *core::ptr::addr_of_mut!(REQUEST) };
            buf.clear();
            buf.resize(len as usize, 0);
            buf.as_mut_ptr()
        }

        /// 执行一个请求行。恰好发一个回包；返回非 0 = 异常失败（宿主直接报错）。
        #[unsafe(no_mangle)]
        pub extern "C" fn call(ptr: *const u8, len: i32) -> i32 {
            if len < 0 {
                return 1;
            }
            let bytes: &[u8] = unsafe {
                let buf: &mut Vec<u8> = &mut *core::ptr::addr_of_mut!(REQUEST);
                if ptr != buf.as_ptr() {
                    return 1;
                }
                buf.get(..len as usize).unwrap_or(&[])
            };
            let run = || -> Result<(), String> {
                let request: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                let method = request
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let params = request.get("params").cloned().unwrap_or_default();
                // 错误按 Display 转 String（String / anyhow::Error 都兼容）
                let reply = match $handle(&method, &params) {
                    Ok(result) => serde_json::json!({ "result": result }),
                    Err(error) => serde_json::json!({ "error": error.to_string() }),
                };
                let line = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
                $crate::send_reply(&line);
                Ok(())
            };
            match run() {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
    };

    // 方法插件：call/alloc export + 回包缓冲 + 空 load（宿主 lifecycle 统一）
    (
        name: $name:literal, version: $ver:literal, ui: false,
        methods: $methods:tt, handle: $handle:path $(,)?
    ) => {
        static MANIFEST: &str = concat!(
            "{\"name\":\"", $name, "\",\"version\":\"", $ver,
            "\",\"ui\":false,\"methods\":[",
            $crate::methods_json!$methods,
            "]}"
        );

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_ptr() -> *const u8 {
            MANIFEST.as_ptr()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn manifest_len() -> u32 {
            MANIFEST.len() as u32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn load() {}

        /// 请求缓冲：宿主经 alloc 拿指针写参数。单线程模型下复用一份。
        static mut REQUEST: Vec<u8> = Vec::new();

        #[unsafe(no_mangle)]
        pub extern "C" fn alloc(len: i32) -> *mut u8 {
            if len <= 0 {
                return core::ptr::null_mut();
            }
            // 单线程假设（见模块文档）；static_mut_refs 规避用 addr_of_mut
            let buf: &mut Vec<u8> = unsafe { &mut *core::ptr::addr_of_mut!(REQUEST) };
            buf.clear();
            buf.resize(len as usize, 0);
            buf.as_mut_ptr()
        }

        /// 执行一个请求行。恰好发一个回包；返回非 0 = 异常失败（宿主直接报错）。
        #[unsafe(no_mangle)]
        pub extern "C" fn call(ptr: *const u8, len: i32) -> i32 {
            if len < 0 {
                return 1;
            }
            let bytes: &[u8] = unsafe {
                let buf: &mut Vec<u8> = &mut *core::ptr::addr_of_mut!(REQUEST);
                if ptr != buf.as_ptr() {
                    return 1;
                }
                buf.get(..len as usize).unwrap_or(&[])
            };
            let run = || -> Result<(), String> {
                let request: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                let method = request
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string();
                let params = request.get("params").cloned().unwrap_or_default();
                // 错误按 Display 转 String（String / anyhow::Error 都兼容）
                let reply = match $handle(&method, &params) {
                    Ok(result) => serde_json::json!({ "result": result }),
                    Err(error) => serde_json::json!({ "error": error.to_string() }),
                };
                let line = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
                $crate::send_reply(&line);
                Ok(())
            };
            match run() {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
    };
}

#[cfg(test)]
mod ui_tests {
    use super::ui::{self, span, style};

    #[test]
    fn paint_progress_前段换底色_跨界切分() {
        let spans = [
            span(style::STRONG, "标题"),
            span(style::FAINT, "  艺术家"),
        ];
        // 全覆盖第一段（"标题" 4 宽）：整段换 ok_bg
        let out = ui::paint_progress(&spans, 4);
        assert_eq!(out[0].style, style::OK_BG);
        assert_eq!(out[0].text, "标题");
        assert_eq!(out[1].style, style::FAINT);

        // 跨界（5 宽：标题 4 + 空格 1）：第二段切开（第 1 个空格进进度段）
        let out = ui::paint_progress(&spans, 5);
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].style, style::OK_BG);
        assert_eq!(out[1].text, " ");
        assert_eq!(out[2].style, style::FAINT);
        assert_eq!(out[2].text, " 艺术家");

        // 0 宽 / 超宽：原样
        assert_eq!(ui::paint_progress(&spans, 0).len(), 2);
        assert_eq!(ui::paint_progress(&spans, 999).len(), 2);
    }

    #[test]
    fn window_选中滚入窗口() {
        let mut scroll = 0;
        // 选中在窗口内：不动
        assert_eq!(ui::window(&mut scroll, 2, 100, 10), 0);
        // 选中下移超窗：翻页
        assert_eq!(ui::window(&mut scroll, 15, 100, 10), 6);
        // 选中上移超窗：贴顶
        assert_eq!(ui::window(&mut scroll, 1, 100, 10), 1);
        // 空列表归零
        assert_eq!(ui::window(&mut scroll, 0, 0, 10), 0);
    }
}
