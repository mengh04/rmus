//! rmus-fm：免费 API 推荐（Deezer 全球榜单 + 曲库歌手热门）。
//!
//! 与 rmus-dj 互补：不需要 LLM、不需要密钥，开箱即用。候选来自
//! 人群验证过的榜单/热门数据，再经搜索源双向匹配闸门与共享下载管线
//! 落盘入列表——免费的保底层，LLM 那层在 rmus-dj。

mod config;
pub mod deezer;
mod fm;
pub mod lastfm;
mod store;

use std::sync::Mutex;

use rmus_plugin_sdk::{daemon, declare_native_plugin, notify, now_ms, sleep, task, ui};
use ui::style;

/// 手动一轮的任务号（UI 按 r 触发）。
const RUN_TASK: u64 = 2;
/// 常驻 worker 的任务号（自动轮）。
const WORKER_TASK: u64 = 1;
const TICK_MS: u64 = 5000;
/// 启动后首轮流延迟。
const FIRST_ROUND_MS: u64 = 15 * 60_000;
/// 配置关闭 auto 等情况的重查间隔（worker 不退出，随时能被打开）。
const DISABLED_RETRY_MS: u64 = 6 * 3_600_000;
/// 锁冲突退避：手动轮正在跑，过一会再来。
const LOCK_RETRY_MS: u64 = 10 * 60_000;
/// 通知 worker 尽快排班的延迟（UI 'a' 开 auto 后写 state.json 生效）。
const SOON_MS: u64 = 10_000;

declare_native_plugin! {
    name: "rmus-fm", version: "0.1.0", ui: true, tab: "FM", cli: "fm",
    methods: [("fm", "task"), ("cli", "task"), ("fm_worker", "task")],
    handle: handle, update: ui_update, render: ui_render,
}

fn handle(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, String> {
    match method {
        "fm" => run_round(params),
        "fm_worker" => fm_worker().map_err(|e| format!("{e:#}")),
        "cli" => cli(params).map_err(|e| format!("{e:#}")),
        _ => Err(format!("未知方法 {method}")),
    }
}

/// 跑一轮（worker 实例执行）。UI 与 CLI 都走这。
fn run_round(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let cfg = config::load().map_err(|e| format!("{e:#}"))?;
    fm::run(&cfg).map_err(|e| format!("{e:#}"))
}

/// 常驻 worker：到点自动跑轮（同 rmus-dj 的排班/退避/热更模式）。
fn fm_worker() -> anyhow::Result<serde_json::Value> {
    let iv_now = || {
        config::load()
            .map(|c| c.interval_hours * 3_600_000)
            .unwrap_or(DISABLED_RETRY_MS)
            .min(FIRST_ROUND_MS)
    };
    let mut next_due = now_ms() + iv_now();
    let _ = store::worker_state_write(next_due);
    loop {
        sleep(TICK_MS);
        if rmus_plugin_sdk::cancelled() {
            break;
        }
        // 外部排班提示（拉早生效）
        if let Some(hint) = store::worker_state_read()
            && hint < next_due
        {
            next_due = hint;
        }
        if now_ms() < next_due {
            continue;
        }
        next_due = match config::load() {
            Ok(cfg) if cfg.auto => {
                let iv = cfg.interval_hours * 3_600_000;
                match fm::run(&cfg) {
                    Ok(result) => {
                        notify(&serde_json::json!({
                            "worker": "fm", "phase": "round_done", "result": result,
                        }));
                        if cfg.auto_queue {
                            let paths: Vec<String> = result["added"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x["path"].as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default();
                            if !paths.is_empty()
                                && daemon::add(paths.clone()).is_ok()
                                && let Some(n) = u64::try_from(paths.len()).ok()
                            {
                                notify(&serde_json::json!({
                                    "worker": "fm", "phase": "queued", "count": n,
                                }));
                            }
                        }
                        now_ms() + iv
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.contains(fm::LOCK_BUSY_TEXT) {
                            now_ms() + LOCK_RETRY_MS // 手动轮在跑：静默退避
                        } else {
                            notify(&serde_json::json!({
                                "worker": "fm", "phase": "round_failed", "error": msg,
                            }));
                            now_ms() + iv
                        }
                    }
                }
            }
            _ => now_ms() + DISABLED_RETRY_MS,
        };
        let _ = store::worker_state_write(next_due);
    }
    Ok(serde_json::json!({}))
}

// ---- CLI：rmus -fm [--set k=v …] [--status]（无参跑一轮）----

const USAGE: &str = "\
rmus -fm：免费 API 推荐（Deezer 榜单+歌手热门，零配置）

用法：
  rmus -fm                      跑一轮（榜单/歌手热门 → 验证 → 下载 → 入列表）
  rmus -fm --status             查看状态
  rmus -fm --set k=v [k=v …]    改配置

配置项（都有默认值，可不配直接跑）：
  playlist        入库列表名（默认 FM 推荐）
  count           每轮入库上限（默认 4）
  flac_only       只下无损（默认 true）
  chart           启用全球榜单候选（默认 true）
  artist_top      启用歌手热门候选（默认 true）
  artist_depth    探测曲库高频歌手数（默认 6，1..=20）
  auto            rmus 运行时自动跑轮（默认 true）
  interval_hours  自动轮间隔小时（默认 6）
  auto_queue      自动轮新歌直插当前队列尾（默认 false）";

fn cli(params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let args: Vec<String> = params
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let mut sets: Vec<(String, String)> = Vec::new();
    let mut show_status = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Ok(serde_json::json!({ "usage": USAGE })),
            "--status" => show_status = true,
            "--set" => {
                let Some(kv) = args.get(i + 1) else {
                    anyhow::bail!("--set 需要跟 k=v");
                };
                let Some((k, v)) = kv.split_once('=') else {
                    anyhow::bail!("--set 格式为 k=v，收到 {kv}");
                };
                sets.push((k.trim().to_string(), v.to_string()));
                i += 1;
            }
            other => anyhow::bail!("不认识的参数 {other}（--help 看用法）"),
        }
        i += 1;
    }

    if !sets.is_empty() {
        let mut cfg = config::load().unwrap_or_default();
        for (k, v) in &sets {
            config::set(&mut cfg, k, v)?;
        }
        config::save(&cfg)?;
        return Ok(serde_json::json!({ "saved": config::config_path(), "config": cfg }));
    }

    if show_status {
        let cfg = config::load()?;
        let tombs = store::tombstones_load();
        let history = store::history_load();
        let has_lfm = lastfm::load_config(&std::path::PathBuf::from(rmus_plugin_sdk::data_dir()))
            .is_some();
        let sources = match (cfg.chart, cfg.artist_top, has_lfm) {
            (true, true, true) => "Last.fm 相似 + Deezer 榜单/歌手热门",
            (true, true, false) => "全球榜单 + 歌手热门",
            (true, false, _) => "全球榜单",
            (false, true, true) => "Last.fm 相似 + 歌手热门",
            (false, true, false) => "歌手热门",
            (false, false, _) => "（全关，跑轮会报错）",
        };
        let due_txt = store::worker_state_read()
            .map(|d| {
                let mins = d.saturating_sub(now_ms()) / 60_000 + 1;
                if mins >= 60 {
                    format!("{}.{} 小时后", mins / 60, (mins % 60) / 6)
                } else {
                    format!("{mins} 分钟后")
                }
            })
            .unwrap_or_else(|| "未排程（rmus 会话活动后 worker 生效）".into());
        let auto_line = if cfg.auto {
            format!(
                "  自动  开 · 每 {}h 一轮 · 下次 {}{}",
                cfg.interval_hours,
                due_txt,
                if cfg.auto_queue { " · 新歌直插队列" } else { "" }
            )
        } else {
            "  自动  关（--set auto=true 开启）".to_string()
        };
        let status_text = format!(
            "rmus-fm 状态（免费 API · 零配置）\n  来源  {sources}（探测歌手 {} 位）\n  列表  「{}」 · 每轮 ≤{} 首 · 无损 {}\n{auto_line}\n  墓碑  {} 条 · 历史 {} 轮\n  配置文件  {}",
            cfg.artist_depth,
            cfg.playlist,
            cfg.count,
            if cfg.flac_only { "开" } else { "关" },
            tombs.len(),
            history.len(),
            config::config_path(),
        );
        return Ok(serde_json::json!({
            "status_text": status_text,
            "playlist": cfg.playlist,
            "count": cfg.count,
            "chart": cfg.chart,
            "artist_top": cfg.artist_top,
            "tombstones": tombs.len(),
        }));
    }

    run_round(&serde_json::json!({})).map_err(|e| anyhow::anyhow!("{e}"))
}

// ---- UI 画布：来源/排班状态 + 最近一轮结果 ----

/// 结果行三态。
#[derive(Debug, Clone, Copy, PartialEq)]
enum LineKind {
    Added,
    Skipped,
    Failed,
}

/// UI 状态（wasm 单线程；Mutex 只为满足 static 的 Sync 要求）。
struct UiState {
    running: bool,
    phase: String,
    round_err: Option<String>,
    round_note: String,
    lines: Vec<(LineKind, String)>,
    list: ui::ListState,
    view_rows: usize,
    /// 常驻 worker 只 spawn 一次
    worker_spawned: bool,
}

impl UiState {
    fn new() -> Self {
        Self {
            running: false,
            phase: String::new(),
            round_err: None,
            round_note: String::new(),
            lines: Vec::new(),
            list: ui::ListState::new(),
            view_rows: 0,
            worker_spawned: false,
        }
    }

    /// 常驻 worker 懒 spawn（烓播客/rmus-dj 先例）
    fn ensure_worker(&mut self) {
        if self.worker_spawned {
            return;
        }
        self.worker_spawned = true;
        let _ = task::spawn(WORKER_TASK, "fm_worker", &serde_json::json!({}));
    }
}

static UI: Mutex<Option<UiState>> = Mutex::new(None);

fn with_ui<R>(f: impl FnOnce(&mut UiState) -> R) -> R {
    let mut guard = UI.lock().unwrap();
    let state = guard.get_or_insert_with(UiState::new);
    f(state)
}

fn ui_update(ev: i32) -> i32 {
    use rmus_plugin_sdk::event::{
        char_of, kind, KIND_CHAR, KIND_DATA, KIND_DOWN, KIND_HALF_DOWN, KIND_HALF_UP, KIND_TOP,
        KIND_UP,
    };

    with_ui(|s| s.ensure_worker());

    match kind(ev) {
        KIND_DATA => {
            let Some(data) = task::last_event() else {
                return 0;
            };
            let task_id = data.get("task_id").and_then(|v| v.as_u64()).unwrap_or(0);
            let event = data.get("event").and_then(|v| v.as_str()).unwrap_or("");
            let inner = data.get("data").cloned().unwrap_or_default();
            if task_id != RUN_TASK && task_id != WORKER_TASK {
                return 0;
            }
            let auto = task_id == WORKER_TASK;

            // worker 自己的通知（round_done / round_failed / queued）
            if inner.get("worker").and_then(|v| v.as_str()) == Some("fm") {
                match inner.get("phase").and_then(|v| v.as_str()) {
                    Some("round_done") => {
                        let result = inner.get("result").cloned().unwrap_or_default();
                        with_ui(|s| {
                            s.running = false;
                            s.phase.clear();
                            s.round_err = None;
                            s.lines = result_lines(&result);
                            s.round_note = format!(
                                "自动 · {}",
                                result.get("summary").and_then(|v| v.as_str()).unwrap_or("")
                            );
                            s.list = ui::ListState::new();
                        });
                        return 1;
                    }
                    Some("round_failed") => {
                        let err = inner
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("未知")
                            .to_string();
                        with_ui(|s| {
                            s.running = false;
                            s.phase.clear();
                            s.round_err = Some(format!("自动轮失败：{err}"));
                        });
                        return 1;
                    }
                    Some("queued") => {
                        let n = inner.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                        with_ui(|s| s.phase = format!("新歌 {n} 首已加到当前队列"));
                        return 1;
                    }
                    _ => return 1,
                }
            }

            match event {
                "progress" => {
                    let phase = inner.get("phase").and_then(|v| v.as_str()).unwrap_or("");
                    let label = match phase {
                        "library" => "读取曲库…".to_string(),
                        "chart" => "拉取 Deezer 全球榜单…".to_string(),
                        "artist" => format!(
                            "探测歌手热门（{} 位）…",
                            inner.get("depth").and_then(|v| v.as_u64()).unwrap_or(0)
                        ),
                        "search" => format!(
                            "验证候选：{} - {}",
                            inner.get("title").and_then(|v| v.as_str()).unwrap_or("?"),
                            inner.get("artist").and_then(|v| v.as_str()).unwrap_or("")
                        ),
                        "download" => format!(
                            "下载：{} - {}",
                            inner.get("title").and_then(|v| v.as_str()).unwrap_or("?"),
                            inner.get("artist").and_then(|v| v.as_str()).unwrap_or("")
                        ),
                        "playlist" => format!(
                            "加入列表「{}」（{} 首）…",
                            inner.get("playlist").and_then(|v| v.as_str()).unwrap_or("?"),
                            inner.get("count").and_then(|v| v.as_u64()).unwrap_or(0)
                        ),
                        // rmus-sources 下载管线内部的进度（received/total/metadata）
                        _ => return 1,
                    };
                    with_ui(|s| {
                        if auto && !s.running {
                            s.running = true;
                        }
                        s.phase = if auto { format!("自动 · {label}") } else { label };
                    });
                    1
                }
                "done" => {
                    with_ui(|s| {
                        s.running = false;
                        s.phase.clear();
                        s.round_err = None;
                        s.lines = result_lines(&inner);
                        s.round_note = inner
                            .get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        s.list = ui::ListState::new();
                    });
                    1
                }
                "failed" => {
                    let err = inner.as_str().unwrap_or("未知错误").to_string();
                    with_ui(|s| {
                        s.running = false;
                        s.phase.clear();
                        s.round_err = Some(err);
                    });
                    1
                }
                _ => 0,
            }
        }
        KIND_UP | KIND_DOWN | KIND_TOP | rmus_plugin_sdk::event::KIND_BOTTOM
        | KIND_HALF_UP | KIND_HALF_DOWN => with_ui(|s| {
            let len = s.lines.len();
            let page = (s.view_rows / 2).max(1);
            s.list.handle(ev, len, page) as i32
        }),
        KIND_CHAR => {
            match char_of(ev) {
                Some('j') => ui_update(rmus_plugin_sdk::event::encode(
                    rmus_plugin_sdk::event::KIND_DOWN,
                    0,
                )),
                Some('k') => ui_update(rmus_plugin_sdk::event::encode(
                    rmus_plugin_sdk::event::KIND_UP,
                    0,
                )),
                Some('r') => {
                    if with_ui(|s| s.running) {
                        return 0;
                    }
                    with_ui(|s| {
                        s.running = true;
                        s.phase = "启动…".into();
                        s.round_err = None;
                        s.lines.clear();
                        s.round_note.clear();
                        s.list = ui::ListState::new();
                    });
                    if task::spawn(RUN_TASK, "fm", &serde_json::json!({})) {
                        1
                    } else {
                        with_ui(|s| {
                            s.running = false;
                            s.phase.clear();
                            s.round_err = Some("宿主拒绝任务".into());
                        });
                        1
                    }
                }
                Some('a') => {
                    match config::load() {
                        Ok(mut cfg) => {
                            cfg.auto = !cfg.auto;
                            if config::save(&cfg).is_ok() && cfg.auto {
                                let _ = store::worker_state_write(now_ms() + SOON_MS);
                            }
                            1
                        }
                        Err(_) => 0,
                    }
                }
                Some('x') if with_ui(|s| s.running) && task::cancel(RUN_TASK) => 1,
                _ => 0,
            }
        }
        _ => 0,
    }
}

/// done 数据 → 结果行。
fn result_lines(result: &serde_json::Value) -> Vec<(LineKind, String)> {
    let mut lines = Vec::new();
    for key in ["added", "skipped", "failed"] {
        let kind = match key {
            "added" => LineKind::Added,
            "skipped" => LineKind::Skipped,
            _ => LineKind::Failed,
        };
        if let Some(arr) = result.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let artist = item.get("artist").and_then(|v| v.as_str()).unwrap_or("");
                let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                let line = if reason.is_empty() {
                    format!("{title} - {artist}")
                } else {
                    format!("{title} - {artist} — {reason}")
                };
                lines.push((kind, line));
            }
        }
    }
    lines
}

fn ui_render(rows: i32, cols: i32) {
    let width = cols.max(20) as u16;
    let mut state = UI.lock().unwrap();
    if let Some(s) = state.as_mut() {
        s.ensure_worker();
        render_state(s, rows, width);
        return;
    }
    // 待机：只画提示（首次切到 tab）
    ui::print_line(&[
        ui::span(style::STRONG, "按 r 跑一轮推荐"),
        ui::span(
            style::FAINT,
            "  免费榜单/歌手热门 → 搜索源验证 → 下载 → 自动入列表",
        ),
    ]);
}

fn render_state(s: &mut UiState, rows: i32, width: u16) {
    s.view_rows = rows.max(4) as usize;

    let cfg = config::load().ok().unwrap_or_default();

    // 第一行：标题 + 来源
    let has_lfm =
        lastfm::load_config(&std::path::PathBuf::from(rmus_plugin_sdk::data_dir())).is_some();
    let sources = match (cfg.chart, cfg.artist_top, has_lfm) {
        (true, true, true) => "Last.fm 相似 + Deezer 榜单/歌手热门",
        (true, true, false) => "Deezer 全球榜单 + 歌手热门（免费 · 零配置）",
        (true, false, _) => "Deezer 全球榜单（免费 · 零配置）",
        (false, true, true) => "Last.fm 相似 + Deezer 歌手热门",
        (false, true, false) => "Deezer 歌手热门（免费 · 零配置）",
        (false, false, _) => "候选源全关（--set chart=true artist_top=true）",
    };
    ui::print_line(&[
        ui::span(style::STRONG, "FM 推荐"),
        ui::span(style::FAINT, format!("  ·  {sources}")),
    ]);

    // 第二行：运行状态 / 最近一轮小结 / 错误
    if s.running {
        ui::print_line(&[
            ui::span(style::ACCENT, "进行中"),
            ui::span(style::FAINT, format!("  ·  {}", s.phase)),
        ]);
    } else if let Some(err) = &s.round_err {
        ui::print_line(&[
            ui::span(style::FAINT, "最近一轮"),
            ui::span(style::DANGER, format!("  ·  失败：{err}")),
        ]);
    } else if !s.round_note.is_empty() {
        ui::print_line(&[
            ui::span(style::FAINT, "最近一轮"),
            ui::span(style::OK, format!("  ·  {}", s.round_note)),
        ]);
    } else {
        ui::print_line(&[ui::span(style::FAINT, "还没有跑过——按 r 开始")]);
    }

    // 第三行：自动轮排班
    if cfg.auto {
        let due_txt = store::worker_state_read()
            .map(|d| {
                let mins = d.saturating_sub(now_ms()) / 60_000 + 1;
                if mins >= 60 {
                    format!("{}.{} 小时后", mins / 60, (mins % 60) / 6)
                } else {
                    format!("{mins} 分钟后")
                }
            })
            .unwrap_or_else(|| "…".into());
        let queue = if cfg.auto_queue { " · 新歌直插队列" } else { "" };
        ui::print_line(&[
            ui::span(style::FAINT, "自动  "),
            ui::span(
                style::OK,
                format!("开 · 每 {}h 一轮 · 下次 {}{}", cfg.interval_hours, due_txt, queue),
            ),
        ]);
    } else {
        ui::print_line(&[
            ui::span(style::FAINT, "自动  "),
            ui::span(style::FAINT, "关（a 键开启）"),
        ]);
    }

    ui::print_line(&[ui::span(style::RULE, "─".repeat(width as usize))]);
    ui::hints(&[("r", "跑一轮"), ("x", "取消"), ("a", "自动开/关"), ("j/k", "滚动")]);

    // 结果列表（窗口化，选中行滚入窗口）
    let head = 5;
    let list_h = (rows.max(4) as usize).saturating_sub(head);
    let first = s.list.prepare(s.lines.len(), list_h, 3);
    for (i, (kind, line)) in s.lines[first..].iter().take(list_h).enumerate() {
        let i = first + i;
        let selected = i == s.list.selected;
        let (mark, mark_style) = match kind {
            LineKind::Added => ("✓ ", style::OK),
            LineKind::Skipped => ("· ", style::FAINT),
            LineKind::Failed => ("✗ ", style::DANGER),
        };
        if selected {
            ui::print_line(&[
                ui::span(style::ACCENT, format!("▸{mark}")),
                ui::span(style::STRONG, line.clone()),
            ]);
        } else {
            ui::print_line(&[
                ui::span(mark_style, format!("  {mark}")),
                ui::span(style::BASE, line.clone()),
            ]);
        }
    }
    if s.lines.is_empty() && !s.running {
        ui::print_line(&[ui::span(
            style::FAINT,
            "结果会出现在这里（✓ 入库 · 跳过 · ✗ 失败）",
        )]);
    }
}
