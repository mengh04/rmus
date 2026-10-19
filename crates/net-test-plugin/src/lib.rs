//! 网络/文件宿主函数的测试桩：每个方法对应一条能力验证。
//!
//! 配合 `wasm-host` 的 `tests/http.rs`（本地 TCP 假服务器）运行：
//! - `fetch`：GET 回显端点，返回 body 文本
//! - `status`：GET 404 端点，返回状态码
//! - `redirect`：GET 302 端点，返回最终 URL
//! - `post`：POST 回显端点，返回 body
//! - `file`：写文件 → exists → rename → remove，逐步验证
//! - `libdir`：回读宿主注入的库目录

use anyhow::Context;
use rmus_plugin_sdk::{declare_plugin_manifest, fs, http, library_dir};

declare_plugin_manifest! {
    name: "nettest", version: "0.1.0", ui: false,
    methods: [("fetch", "call"), ("status", "call"), ("redirect", "call"),
              ("post", "call"), ("file", "call"), ("libdir", "call")],
    handle: handle,
}

fn handle(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, anyhow::Error> {
    let base = params
        .get("base")
        .and_then(|v| v.as_str())
        .context("缺少 base 参数")?
        .to_string();
    match method {
        "fetch" => {
            let resp = http::Response::send(&http::Request::get(format!("{base}/text")))
                .map_err(anyhow::Error::msg)?;
            let status = resp.status();
            let body = resp.error_for_status()?.read_to_end()?;
            Ok(serde_json::json!({
                "status": status,
                "body": String::from_utf8_lossy(&body),
            }))
        }
        "status" => {
            let resp = http::Response::send(&http::Request::get(format!("{base}/missing")))
                .map_err(anyhow::Error::msg)?;
            Ok(serde_json::json!({ "status": resp.status() }))
        }
        "redirect" => {
            let resp = http::Response::send(&http::Request::get(format!("{base}/redir")))
                .map_err(anyhow::Error::msg)?;
            Ok(serde_json::json!({
                "status": resp.status(),
                "url": resp.url(),
            }))
        }
        "post" => {
            let req = http::Request::post_form(format!("{base}/echo"), "a=1&b=x")
                .header("X-Probe", "rmus");
            let resp = http::Response::send(&req).map_err(anyhow::Error::msg)?;
            let body = resp.error_for_status()?.read_to_end()?;
            Ok(serde_json::json!({ "body": String::from_utf8_lossy(&body) }))
        }
        "file" => {
            let dir = library_dir();
            let path = format!("{dir}/.nettest-a.part");
            let mut file = fs::File::create(&path)?;
            file.write_all(b"nettest-payload")?;
            let existed = fs::exists(&path);
            let final_path = format!("{dir}/nettest-final.txt");
            let renamed = fs::rename(&path, &final_path);
            let final_existed = fs::exists(&final_path);
            let removed = fs::remove(&final_path);
            let removed_gone = !fs::exists(&final_path);
            Ok(serde_json::json!({
                "dir": dir,
                "existed": existed,
                "renamed": renamed,
                "final_existed": final_existed,
                "removed": removed,
                "removed_gone": removed_gone,
            }))
        }
        "libdir" => Ok(serde_json::json!({ "dir": library_dir() })),
        other => anyhow::bail!("未知方法 “{other}”"),
    }
}
