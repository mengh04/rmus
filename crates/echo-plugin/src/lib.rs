//! 方法/UI 混合插件 POC：假 search/download + worker 全链路演示。
//!
//! - `search`（call）：秒回一条假结果
//! - `download`（task）：5 段 × 300ms 假下载，逐段 notify 进度，每段前查
//!   cancelled
//! - `cancel`（call）：协议占位——真正的取消是宿主标记 + 插件自查
//!
//! UI 通道演示 worker：`CHAR 'g'` 经 `task::spawn` 发起后台 "work" 任务，
//! 进度以 `KIND_DATA` 事件回注，`task::last_event()` 取数据画进度条；
//! `SPACE` 清空收集。

use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};

use serde_json::Value;

use rmus_plugin_sdk::{
    cancelled, declare_plugin_manifest, event, notify, print_line, sleep, task, ui,
    ui::style,
};

declare_plugin_manifest! {
    name: "echo", version: "0.2.0", ui: true, tab: "演示",
    methods: [("search", "call"), ("download", "task"), ("cancel", "call")],
    handle: handle,
}

/// 方法通道：真正干活的地方（worker 实例也执行这份代码）。
fn handle(method: &str, _params: &Value) -> Result<Value, String> {
    match method {
        "search" => Ok(serde_json::json!({
            "songs": [{
                "title": "echo 之歌", "artist": "wasm", "album": "poc",
                "source": "echo", "song_id": "1", "duration_ms": 3000,
                "format": "flac", "size_bytes": 10240,
            }]
        })),
        "download" => {
            let (total, steps) = (10240u64, 10u64);
            for i in 1..=steps {
                if cancelled() {
                    return Err("已取消".into());
                }
                sleep(300);
                notify(&serde_json::json!({"received": i * total / steps, "total": total}));
            }
            Ok(serde_json::json!({"path": "/tmp/echo-之歌.flac"}))
        }
        "work" => {
            // 后台任务演示：5 段假进度
            for i in 1..=5 {
                sleep(300);
                notify(&serde_json::json!({"step": i * 20, "total": 100}));
            }
            Ok(serde_json::json!({"message": "work 完成"}))
        }
        "cancel" => Ok(serde_json::json!({"cancelled": true})),
        other => Err(format!("未知方法 “{other}”")),
    }
}

// ---- UI 通道：worker 事件的收集与渲染 ----

static LAST_STEP: AtomicU32 = AtomicU32::new(0);
static LAST_MSG: AtomicU64 = AtomicU64::new(0); // 完成标记（1 = done）
static WORKER_RUNS: AtomicU32 = AtomicU32::new(0);

static SELECTED: AtomicU32 = AtomicU32::new(0);
static DOWNLOADED: AtomicU64 = AtomicU64::new(0);

const DEMO_ITEMS: [&str; 4] = ["群青 - YOASOBI", "晴天 - 周杰伦", "晚风 - 王菲", "Amsterdam - 冷玩"];

/// 返回非 0 表示状态变了、需要重绘。
#[unsafe(no_mangle)]
pub extern "C" fn update(ev: i32) -> i32 {
    use event::{kind, char_of, KIND_CHAR, KIND_DATA, KIND_DOWN, KIND_ENTER, KIND_SPACE, KIND_UP};
    match kind(ev) {
        KIND_DATA => {
            // 后台任务事件：取数据更新状态
            if let Some(data) = task::last_event() {
                match data.get("event").and_then(|e| e.as_str()) {
                    Some("progress") => {
                        if let Some(step) = data.pointer("/data/step").and_then(|v| v.as_u64()) {
                            LAST_STEP.store(step as u32, Ordering::Relaxed);
                            return 1;
                        }
                        return 0;
                    }
                    Some("done") => {
                        LAST_STEP.store(100, Ordering::Relaxed);
                        LAST_MSG.store(1, Ordering::Relaxed);
                        return 1;
                    }
                    Some("failed") => {
                        LAST_MSG.store(2, Ordering::Relaxed);
                        return 1;
                    }
                    _ => return 0,
                }
            }
            0
        }
        KIND_UP => {
            let cur = SELECTED.load(Ordering::Relaxed);
            SELECTED.store(cur.saturating_sub(1), Ordering::Relaxed);
            1
        }
        KIND_DOWN => {
            let cur = SELECTED.load(Ordering::Relaxed);
            if (cur as usize) < DEMO_ITEMS.len() - 1 {
                SELECTED.store(cur + 1, Ordering::Relaxed);
            }
            1
        }
        KIND_SPACE => {
            let cur = DOWNLOADED.load(Ordering::Relaxed);
            DOWNLOADED.store((cur + 25).min(100), Ordering::Relaxed);
            1
        }
        KIND_ENTER => {
            // Enter：发起后台 work 任务（worker 实例执行，UI 不卡）
            let run = WORKER_RUNS.fetch_add(1, Ordering::Relaxed) + 1;
            LAST_MSG.store(0, Ordering::Relaxed);
            LAST_STEP.store(0, Ordering::Relaxed);
            let ok = task::spawn(run as u64, "work", &serde_json::json!({}));
            if !ok {
                LAST_MSG.store(2, Ordering::Relaxed);
            }
            1
        }
        KIND_CHAR => match char_of(ev) {
            Some('j') => update(event::encode(KIND_DOWN, 0)),
            Some('k') => update(event::encode(KIND_UP, 0)),
            Some('g') => update(event::encode(KIND_ENTER, 0)),
            _ => 0,
        },
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn render(_rows: i32, cols: i32) {
    let selected = SELECTED.load(Ordering::Relaxed) as usize;
    let step = LAST_STEP.load(Ordering::Relaxed);
    let msg = LAST_MSG.load(Ordering::Relaxed);
    let downloaded = DOWNLOADED.load(Ordering::Relaxed);
    let width = cols.max(20) as usize;

    ui::header("echo 组件演示", "worker + 组件表");

    for (i, item) in DEMO_ITEMS.iter().enumerate() {
        ui::list_row_custom(&format!("{}. ", i + 1), item, i == selected);
    }

    // 后台任务进度：worker 事件驱动（UI 线程零阻塞的证明）
    ui::progress(step as u64, 100, "work", width);
    let status = match msg {
        1 => ui::span(style::ACCENT, "worker 完成 ✓"),
        2 => ui::span(style::DANGER, "worker 失败 ✗"),
        _ => ui::span(style::BASE, "worker 运行中…"),
    };
    ui::print_line(&[ui::span(style::FAINT, "状态: "), status]);

    // 下载进度（SPACE 步进的假进度）
    ui::progress(downloaded, 100, "下载", width);

    ui::hints(&[
        ("↑↓/jk", "导航"),
        ("g/Enter", "发起 worker"),
        ("空格", "下载步进"),
    ]);
    print_line("（echo 混合插件：方法 + UI + worker）");
}
