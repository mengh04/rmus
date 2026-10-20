//! daemon RPC / 数据目录链路测试：mock daemon_call 注入 → wasm 宿主函数
//! → SDK 封装的往返，外加 worker 实例的透传验证。
//!
//! 前置：插件已编译到 wasm target——
//!
//! ```text
//! cargo build -p daemon-test-plugin --target wasm32-unknown-unknown
//! ```

use std::sync::{Arc, Mutex};

use serde_json::Value;
use wasm_host::{DaemonFn, Plugin};

fn daemontest_wasm() -> Vec<u8> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest)
        .join("../../target/wasm32-unknown-unknown/debug/daemon_test_plugin.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!(
        "读 {path:?} 失败（先 cargo build -p daemon-test-plugin --target wasm32-unknown-unknown）: {e}"
    ))
}

/// mock daemon 侧桥：模拟白名单行为（status/peek 直通、其余拒绝），
/// 收到的请求留档供断言。
fn mock_daemon(seen: Arc<Mutex<Vec<Value>>>, status: Value) -> DaemonFn {
    Arc::new(move |request: Value| {
        seen.lock().unwrap().push(request.clone());
        match request.get("method").and_then(|v| v.as_str()) {
            Some("status") | Some("peek") => Ok(status.clone()),
            _ => Err("插件无权调用该请求（白名单仅 Status/Peek/Back）".into()),
        }
    })
}

fn plugin_with(daemon: DaemonFn) -> Plugin {
    let mut p = Plugin::new(&daemontest_wasm(), Box::new(|_, _| {})).unwrap();
    p.set_daemon_call(daemon);
    p.set_data_dir("/tmp/rmus-test-data".into());
    p
}

#[test]
fn status_往返_带注入值() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let status = serde_json::json!({
        "state": "playing", "position_ms": 12345, "duration_ms": 240000,
        "track": {"path": "/data/podcasts/foo/ep1.mp3", "title": "EP1"}
    });
    let mut p = plugin_with(mock_daemon(seen.clone(), status));

    let reply = p.call_method("status", &serde_json::json!({})).unwrap().unwrap();
    assert_eq!(reply["position_ms"], 12345, "注入的进度应原样到达插件");
    assert_eq!(reply["track"]["path"], "/data/podcasts/foo/ep1.mp3");

    // 插件发出的请求是协议 Request 的 tagged 形态
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["method"], "status");
}

#[test]
fn peek_往返_参数形态正确() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let status = serde_json::json!({"state": "playing"});
    let mut p = plugin_with(mock_daemon(seen.clone(), status));

    p.call_method(
        "peek",
        &serde_json::json!({"paths": ["/tmp/rmus-test-data/podcasts/foo/ep2.mp3"]}),
    )
    .unwrap()
    .unwrap();

    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["method"], "peek");
    assert_eq!(
        sent["params"]["paths"][0],
        "/tmp/rmus-test-data/podcasts/foo/ep2.mp3"
    );
}

#[test]
fn 白名单外_错误传回插件() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut p = plugin_with(mock_daemon(seen.clone(), serde_json::json!(null)));

    let reply = p.call_method("denied", &serde_json::json!({})).unwrap();
    let err = reply.err().expect("白名单外请求应失败");
    assert!(err.contains("插件无权调用"), "错误信息: {err}");
}

#[test]
fn data_dir_注入读取() {
    let mut p = plugin_with(mock_daemon(
        Arc::new(Mutex::new(Vec::new())),
        serde_json::json!(null),
    ));
    let reply = p.call_method("datadir", &serde_json::json!({})).unwrap().unwrap();
    assert_eq!(reply["dir"], "/tmp/rmus-test-data");
}

/// worker 全链路：方法里 spawn_task → worker 实例（同 wasm 新实例）执行
/// status → daemon_call 必须透传给 worker（播客进度采样/连播的依赖）→
/// done 事件回注。
#[test]
fn worker_实例_daemon_rpc_透传() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let status = serde_json::json!({
        "state": "playing", "position_ms": 42, "duration_ms": 1000
    });
    let mut p = plugin_with(mock_daemon(seen.clone(), status));

    // 后台任务出口：收集 worker 事件（daemon 侧桥接成 Data 消息）
    let outlet: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = outlet.clone();
    let wasm_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/wasm32-unknown-unknown/debug/daemon_test_plugin.wasm");
    p.set_worker_source(wasm_path, Arc::new(move |json: String| {
        sink.lock().unwrap().push(json);
    }));

    p.call_method("worker_status", &serde_json::json!({})).unwrap().unwrap();

    // 模拟 daemon：出口事件写 last_event + KIND_DATA
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut done_result: Option<Value> = None;
    while std::time::Instant::now() < deadline && done_result.is_none() {
        let pending: Vec<String> = {
            let mut q = outlet.lock().unwrap();
            std::mem::take(&mut *q)
        };
        if pending.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        for line in pending {
            let ev: Value = serde_json::from_str(&line).unwrap();
            if ev["event"] == "done" {
                done_result = Some(ev["data"].clone());
            }
        }
    }
    let result = done_result.expect("5 秒内没收到 worker done 事件");
    assert_eq!(result["position_ms"], 42, "worker 里的 daemon RPC 应通");
    // 请求确实经过了 mock（worker 发出的 status）
    assert!(
        seen.lock().unwrap().iter().any(|r| r["method"] == "status"),
        "worker 的 status 请求没到 daemon 侧"
    );
}
