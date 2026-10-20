//! daemon RPC / 数据目录宿主函数的测试桩：每个方法对应一条能力验证。
//!
//! 配合 `wasm-host` 的 `tests/daemon.rs`（注入 mock daemon_call）运行：
//! - `status`：调 daemon::status()，回包原样透传（Reply::Status 的 JSON）
//! - `peek`：调 daemon::peek(paths)，验证试听态请求往返
//! - `denied`：调白名单外的请求（QueueAdd），应收到错误
//! - `datadir`：回读宿主注入的数据根目录

use anyhow::Context;
use rmus_plugin_sdk::{daemon, data_dir, declare_plugin_manifest};

declare_plugin_manifest! {
    name: "daemontest", version: "0.1.0", ui: false,
    methods: [("status", "call"), ("peek", "call"),
              ("denied", "call"), ("datadir", "call"),
              ("worker_status", "call")],
    handle: handle,
}

fn handle(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, anyhow::Error> {
    match method {
        // 透传 Reply JSON，测试侧断言注入的 mock 值
        "status" => daemon::status().map_err(anyhow::Error::msg),
        "peek" => {
            let paths: Vec<String> = params
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .context("缺少 paths 参数")?;
            daemon::peek(paths).map_err(anyhow::Error::msg)
        }
        // 白名单外：daemon 侧应拒绝
        "denied" => daemon::call(serde_json::json!({
            "method": "lib_rescan"
        }))
        .map(|v| serde_json::json!({"unexpected_ok": v}))
        .map_err(anyhow::Error::msg),
        "datadir" => {
            let dir = data_dir();
            anyhow::ensure!(!dir.is_empty(), "data_dir 未注入或为空");
            Ok(serde_json::json!({ "dir": dir }))
        }
        // worker 全链路：worker 实例里再调 daemon RPC（宿主必须把
        // daemon_call 透传给 worker，播客的进度采样/连播全靠它）
        "worker_status" => {
            anyhow::ensure!(
                rmus_plugin_sdk::task::spawn(77, "status", &serde_json::json!({})),
                "spawn_task 失败"
            );
            Ok(serde_json::json!({ "spawned": true }))
        }
        _ => anyhow::bail!("未知方法 {method}"),
    }
}
