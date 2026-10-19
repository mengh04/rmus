//! 方法通道测试：call / task / progress / cancel 全链路。
//!
//! 前置：echo 插件已编译到 wasm target——
//!
//! ```text
//! cargo build -p echo-plugin --target wasm32-unknown-unknown
//! ```

use std::sync::{Arc, Mutex};

use serde_json::Value;
use wasm_host::{Plugin, event};

fn echo_wasm() -> Vec<u8> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest)
        .join("../../target/wasm32-unknown-unknown/debug/echo_plugin.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("读 {path:?} 失败（先 cargo build -p echo-plugin --target wasm32-unknown-unknown）: {e}"))
}

type Log = Arc<Mutex<Vec<(Option<u64>, Value)>>>;

fn echo_plugin(log: Log) -> Plugin {
    let sink = log.clone();
    Plugin::new(
        &echo_wasm(),
        Box::new(move |call, data| sink.lock().unwrap().push((call, data))),
    )
    .unwrap()
}

#[test]
fn search_短调用返回结果() {
    let mut plugin = echo_plugin(Arc::new(Mutex::new(Vec::new())));
    let reply = plugin
        .call_method("search", &serde_json::json!({"query": "test"}))
        .unwrap()
        .unwrap();
    let songs = reply.get("songs").and_then(|s| s.as_array()).unwrap();
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0]["source"], "echo");
}

#[test]
fn download_长任务带进度() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut plugin = echo_plugin(log.clone());
    plugin.set_current_call(Some(7));
    let reply = plugin
        .call_method("download", &serde_json::json!({"song": {}}))
        .unwrap()
        .unwrap();
    plugin.set_current_call(None);
    assert_eq!(reply["path"], "/tmp/echo-之歌.flac");

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 10, "应有 10 段进度");
    assert!(entries.iter().all(|(call, _)| *call == Some(7)));
    let last = entries.last().unwrap().1.clone();
    assert_eq!(last["received"], 10240);
}

#[test]
fn download_可取消() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut plugin = echo_plugin(log.clone());
    plugin.set_current_call(Some(9));
    // 取消句柄 clone 给另一线程：模拟会话线程在任务执行中标记取消
    // （daemon 里 Cancel 消息就是这么绕过阻塞中的 call_method 生效的）
    let handle = plugin.cancel_handle();
    let marker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        handle.cancel(9);
    });
    let reply = plugin
        .call_method("download", &serde_json::json!({"song": {}}))
        .unwrap();
    marker.join().unwrap();
    plugin.set_current_call(None);
    assert_eq!(reply, Err("已取消".into()));
    assert!(
        log.lock().unwrap().len() < 10,
        "取消后不应再有进度（实际 {} 段）",
        log.lock().unwrap().len()
    );
}


/// worker 全链路：UI 通道发起 spawn_task → 同 wasm 的新实例执行 →
/// 进度/done 经 TaskEvents 出口回注 → set_last_event + KIND_DATA →
/// 插件渲染反映 worker 进度（UI 线程零阻塞的完整证明）。
#[test]
fn worker_全链路_进度与完成回注() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut plugin = echo_plugin(log);

    // 后台任务出口：收集 worker 事件 JSON 行（daemon 侧桥接成 Data 消息）
    let outlet: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = outlet.clone();
    plugin.set_worker_source(
        std::path::Path::new(&echo_wasm_path()).to_path_buf(),
        Arc::new(move |json: String| sink.lock().unwrap().push(json)),
    );

    // CHAR 'g'（echo 的 UI 分支）：发起后台 work 任务
    assert!(plugin
        .update(event::encode(event::KIND_CHAR, 'g' as i32))
        .unwrap());

    // 模拟 daemon：把出口事件逐条写进 last_event + KIND_DATA 通知插件
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut done = false;
    while std::time::Instant::now() < deadline && !done {
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
            plugin.set_last_event(line.as_bytes());
            plugin.update(event::KIND_DATA).unwrap();
            if ev["event"] == "done" {
                done = true;
            }
        }
    }
    assert!(done, "5 秒内没收到 worker done 事件");

    // 插件画出了 worker 的最终进度与完成状态
    let lines = plugin.render(24, 80).unwrap();
    let text = |i: usize| -> String {
        lines[i].spans.iter().map(|s| s.text.as_str()).collect()
    };
    let work_progress = text(5);
    assert!(work_progress.contains("100%"), "worker 进度条: {work_progress}");
    let status = text(6);
    assert!(status.contains("worker 完成 ✓"), "状态行: {status}");
}

fn echo_wasm_path() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest)
        .join("../../target/wasm32-unknown-unknown/debug/echo_plugin.wasm")
        .display()
        .to_string()
}