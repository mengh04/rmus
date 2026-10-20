//! worker 抓取链路定位测试：UI 通道发起订阅 → worker 跑 fetch_feed →
//! 事件回注。真网络（机核 feed），把 daemon/TUI 全部剥掉。
//!
//! 前置：cargo build -p rmus-podcast --target wasm32-unknown-unknown
//! 跑法：cargo test -p wasm-host --test fetch_worker -- --nocapture

use std::sync::{Arc, Mutex};

use wasm_host::{Plugin, event};

fn podcast_wasm_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-unknown-unknown/debug/rmus_podcast.wasm")
}

/// 真网络（机核 feed ~2.7MB）：功能冒烟用，不进常规套件——
/// `cargo test -p wasm-host --test fetch_worker -- --ignored --nocapture`
#[test]
#[ignore = "真网络，直连可能 30s+"]
fn worker_抓取_机核_feed() {
    let wasm = std::fs::read(podcast_wasm_path()).unwrap();
    let mut plugin = Plugin::new(&wasm, Box::new(|_, _| {})).unwrap();
    plugin.set_data_dir("/tmp/opencode/rmus-pod-test".into());

    let outlet: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = outlet.clone();
    plugin.set_worker_source(podcast_wasm_path(), Arc::new(move |json: String| {
        sink.lock().unwrap().push(json);
    }));

    // a → 输入 URL → Enter（订阅提交）
    assert!(plugin.update(event::encode(event::KIND_CHAR, 'a' as i32)).unwrap());
    let url = "https://alioss.gcores.com/feeds/apple_podcast_v1.xml";
    for ch in url.chars() {
        assert!(plugin.update(event::encode(event::KIND_CHAR, ch as i32)).unwrap());
    }
    assert!(plugin.update(event::encode(event::KIND_ENTER, 0)).unwrap());
    println!("订阅已提交，等 worker 回注…");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut seen_done = false;
    while std::time::Instant::now() < deadline {
        let pending: Vec<String> = {
            let mut q = outlet.lock().unwrap();
            std::mem::take(&mut *q)
        };
        for line in pending {
            println!("  事件: {line}");
            let ev: serde_json::Value = serde_json::from_str(&line).unwrap();
            if ev["event"] == "done" || ev["event"] == "failed" {
                seen_done = true;
            }
        }
        if seen_done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(seen_done, "60 秒内 worker 没有回注 done/failed");
}
