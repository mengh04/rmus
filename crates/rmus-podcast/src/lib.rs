//! rmus-podcast：播客订阅插件（wasm，TUI「播客」tab 画布）。
//!
//! 架构分工（方案 docs/podcast-plugin-plan.md，宿主只当解码播放器）：
//! - 订阅/退订/单集浏览/状态全部插件自治（data_dir 下 JSON 存档）
//! - feed 抓取解析在 worker（task::spawn → fetch_feed → quick-xml），
//!   done/failed 经 KIND_DATA 回注重绘，UI 线程零阻塞
//! - 下载与连播接管在后续版本（v1 纯浏览；d/p/x 中 d 预留）
//!
//! 画布两级：订阅列表（a 订阅 / x 退订 / Enter 进单集）→ 单集列表
//! （p 标已播 / h 返回）。字符输入无退格通道（TUI 只转发 Char，
//! Backspace 事件丢失）——输错 URL 用 h/Esc 重来（TODO v2 加
//! KIND_BACKSPACE 事件编码）。

pub mod feed;
pub mod store;

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use rmus_plugin_sdk::{data_dir, declare_plugin_manifest, http, task, ui};
use store::{Episode, Store};
use ui::style;

declare_plugin_manifest! {
    name: "rmus-podcast", version: "0.1.0", ui: true, tab: "播客",
    methods: [("fetch_feed", "task")],
    handle: handle,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum View {
    Subs,
    Eps,
}

struct UiState {
    store: Store,
    data_dir: PathBuf,
    view: View,
    subs_sel: usize,
    eps_sel: usize,
    /// 两级视图各自的窗口滚动（切换后各自保留）
    subs_scroll: usize,
    eps_scroll: usize,
    /// a 键的 feed URL 输入态（None = 非输入态）
    input: Option<String>,
    /// 底部状态行（订阅中/退订提示/错误信息），一次性消费
    status: Option<String>,
    /// 在途 feed 刷新：feed_url -> task_id（去重 + 回执分发）
    fetching: Vec<(String, u64)>,
    next_task: u64,
}

impl UiState {
    fn new() -> Self {
        let dir = PathBuf::from(data_dir());
        let store = store::load(&dir);
        Self {
            store,
            data_dir: dir,
            view: View::Subs,
            subs_sel: 0,
            eps_sel: 0,
            subs_scroll: 0,
            eps_scroll: 0,
            input: None,
            status: None,
            fetching: Vec::new(),
            next_task: 100,
        }
    }

    fn save(&self) {
        if let Err(e) = store::save(&self.data_dir, &self.store) {
            eprintln!("[rmus-podcast] 订阅存档失败: {e:#}");
        }
    }

    fn spawn_fetch(&mut self, feed_url: &str) -> bool {
        if self.fetching.iter().any(|(u, _)| u == feed_url) {
            return true; // 在途不重复发起，视为成功
        }
        let task_id = self.next_task;
        self.next_task += 1;
        if task::spawn(
            task_id,
            "fetch_feed",
            &serde_json::json!({ "url": feed_url }),
        ) {
            self.fetching.push((feed_url.to_string(), task_id));
            true
        } else {
            self.status = Some("宿主拒绝刷新任务".into());
            false
        }
    }
}

static UI: Mutex<Option<UiState>> = Mutex::new(None);

fn with_ui<R>(f: impl FnOnce(&mut UiState) -> R) -> R {
    let mut guard = UI.lock().unwrap();
    let state = guard.get_or_insert_with(UiState::new);
    f(state)
}

fn handle(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, String> {
    match method {
        "fetch_feed" => fetch_feed(params).map_err(|e| format!("{e:#}")),
        other => Err(format!("未知方法 “{other}”")),
    }
}

/// worker：抓 feed XML 并解析成结构化 JSON（合并/存档在 UI 侧做）。
fn fetch_feed(params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let url = params
        .get("url")
        .and_then(|v| v.as_str())
        .context("缺少 url 参数")?
        .to_string();
    let body = http::Response::send(&http::Request::get(url.clone()))?
        .error_for_status()?
        .read_to_end()?;
    let text = String::from_utf8_lossy(&body);
    let parsed = feed::parse(&text)?;
    Ok(serde_json::json!({
        "feed_url": url,
        "title": parsed.title,
        "author": parsed.author,
        "episodes": parsed.episodes,
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn update(ev: i32) -> i32 {
    use rmus_plugin_sdk::event::{char_of, encode, kind, KIND_CHAR, KIND_DATA, KIND_DOWN, KIND_ENTER, KIND_UP};

    match kind(ev) {
        KIND_DATA => on_data(),
        KIND_UP => {
            with_ui(|s| match s.view {
                View::Subs => s.subs_sel = s.subs_sel.saturating_sub(1),
                View::Eps => s.eps_sel = s.eps_sel.saturating_sub(1),
            });
            1
        }
        KIND_DOWN => {
            with_ui(|s| match s.view {
                View::Subs => {
                    if s.subs_sel + 1 < s.store.podcasts.len() {
                        s.subs_sel += 1;
                    }
                }
                View::Eps => {
                    let len = current_episodes(s).len();
                    if s.eps_sel + 1 < len {
                        s.eps_sel += 1;
                    }
                }
            });
            1
        }
        KIND_ENTER => {
            // 输入态：提交订阅
            let submitted = with_ui(|s| match s.input.take() {
                Some(url) => {
                    if !url.starts_with("http://") && !url.starts_with("https://") {
                        s.status = Some("feed 地址要以 http(s):// 开头".into());
                        s.input = Some(url);
                        None
                    } else if s.store.podcasts.iter().any(|p| p.feed_url == url) {
                        s.status = Some("这个播客已在订阅里".into());
                        None
                    } else {
                        Some(url)
                    }
                }
                None => None,
            });
            if let Some(url) = submitted {
                return with_ui(|s| {
                    if s.spawn_fetch(&url) {
                        s.status = Some(format!("订阅中：{url}"));
                    }
                    1
                });
            }
            // 非输入态：订阅列表 Enter 进单集（顺带异步刷新该 feed）
            return with_ui(|s| {
                if s.view == View::Subs {
                    let feed_url = s
                        .store
                        .podcasts
                        .get(s.subs_sel)
                        .map(|p| p.feed_url.clone());
                    if let Some(feed_url) = feed_url {
                        s.view = View::Eps;
                        s.eps_sel = 0;
                        s.spawn_fetch(&feed_url);
                        return 1;
                    }
                }
                0
            });
        }
        KIND_CHAR => {
            let ch = char_of(ev).unwrap_or('\0');
            // 输入态：收字符（无退格通道，见模块注释）
            if with_ui(|s| s.input.is_some()) {
                if ch.is_ascii_graphic() {
                    with_ui(|s| {
                        if let Some(input) = s.input.as_mut() {
                            input.push(ch);
                        }
                    });
                    return 1;
                }
                return 0;
            }
            match ch {
                'j' => return update(encode(KIND_DOWN, 0)),
                'k' => return update(encode(KIND_UP, 0)),
                'a' => {
                    with_ui(|s| s.input = Some(String::new()));
                    1
                }
                'h' => {
                    with_ui(|s| {
                        if s.view == View::Eps {
                            s.view = View::Subs;
                        }
                    });
                    1
                }
                'p' => {
                    with_ui(|s| {
                        if s.view != View::Eps {
                            return 0;
                        }
                        let sel = s.eps_sel;
                        let Some(ep) = current_episodes(s).get_mut(sel) else {
                            return 0;
                        };
                        ep.played = true;
                        ep.position_ms = 0;
                        s.save();
                        s.status = Some("已标记已播".into());
                        1
                    })
                }
                'x' => {
                    with_ui(|s| {
                        if s.view == View::Subs
                            && s.subs_sel < s.store.podcasts.len()
                        {
                            let p = s.store.podcasts.remove(s.subs_sel);
                            s.subs_sel = s.subs_sel.min(s.store.podcasts.len().saturating_sub(1));
                            s.save();
                            s.status = Some(format!("已退订：{}", p.title));
                            return 1;
                        }
                        0
                    })
                }
                _ => 0,
            }
        }
        _ => 0,
    }
}

/// 当前单集视图对应的播客单集（可变借用侧写）。
fn current_episodes(s: &mut UiState) -> &mut Vec<Episode> {
    let idx = s.subs_sel.min(s.store.podcasts.len().saturating_sub(1));
    &mut s.store.podcasts[idx].episodes
}

/// 后台事件：feed 刷新回执（done/failed）与 daemon 事件（v1 忽略，
/// 连播接管接入后在此处理播完标记）。
fn on_data() -> i32 {
    use rmus_plugin_sdk::event::KIND_DATA;
    let _ = KIND_DATA;
    let Some(data) = task::last_event() else {
        return 0;
    };
    // daemon 事件桥（kind:"daemon"）：v1 无消费者
    if data.get("kind").and_then(|v| v.as_str()) == Some("daemon") {
        return 0;
    }
    let task_id = data.get("task_id").and_then(|v| v.as_u64()).unwrap_or(0);
    let event = data.get("event").and_then(|v| v.as_str()).unwrap_or("");
    let inner = data.get("data").cloned().unwrap_or_default();

    let feed_url = with_ui(|s| {
        s.fetching
            .iter()
            .find(|(_, id)| *id == task_id)
            .map(|(u, _)| u.clone())
    });
    let Some(feed_url) = feed_url else {
        return 0;
    };

    match event {
        "done" => {
            let title = inner["title"].as_str().unwrap_or("未命名播客").to_string();
            let author = inner["author"].as_str().unwrap_or("").to_string();
            let episodes: Vec<Episode> = inner
                .get("episodes")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            with_ui(|s| {
                s.fetching.retain(|(_, id)| *id != task_id);
                s.store.merge_episodes(&feed_url, title, author, episodes);
                s.save();
                s.status = Some("订阅完成".into());
            });
            1
        }
        "failed" => {
            let err = inner.as_str().unwrap_or("未知错误").to_string();
            with_ui(|s| {
                s.fetching.retain(|(_, id)| *id != task_id);
                // 新订阅失败：半途条目不该留（merge 没发生就没有）
                s.status = Some(format!("feed 获取失败：{err}"));
            });
            1
        }
        _ => 0,
    }
}

/// 毫秒 -> "m:ss"（超 1 小时折算成总分钟，保持列宽稳定）。
fn fmt_mmss(ms: u64) -> String {
    let secs = ms / 1000;
    let (m, s) = (secs / 60, secs % 60);
    format!("{m:02}:{s:02}")
}

#[unsafe(no_mangle)]
pub extern "C" fn render(rows: i32, cols: i32) {
    let width = cols.max(20) as usize;
    let mut state = UI.lock().unwrap();
    let Some(s) = state.as_mut() else {
        ui::print_line(&[ui::span(style::STRONG, "按 a 订阅播客 feed")]);
        return;
    };

    // 头行：标题/输入态/状态消息
    match (&s.view, &s.input) {
        (_, Some(input)) => {
            ui::print_line(&[
                ui::span(style::FAINT, "订阅 feed："),
                ui::span(style::ACCENT, input.clone()),
                ui::span(style::FAINT, "_  (Enter 提交 · h 取消)"),
            ]);
        }
        (View::Subs, _) => {
            let n = s.store.podcasts.len();
            let mut spans = vec![
                ui::span(style::STRONG, "播客订阅".to_string()),
                ui::span(style::FAINT, format!("  ·  {n} 个")),
            ];
            if let Some(msg) = s.status.take() {
                spans.push(ui::span(style::ACCENT, format!("  ·  {msg}")));
            }
            ui::print_line(&spans);
        }
        (View::Eps, _) => {
            let idx = s.subs_sel.min(s.store.podcasts.len().saturating_sub(1));
            let (title, n, fetching) = match s.store.podcasts.get(idx) {
                Some(p) => (
                    p.title.clone(),
                    p.episodes.len(),
                    s.fetching.iter().any(|(u, _)| u == &p.feed_url),
                ),
                None => (String::new(), 0, false),
            };
            let mut spans = vec![
                ui::span(style::STRONG, title),
                ui::span(style::FAINT, format!("  ·  {n} 集")),
            ];
            if fetching {
                spans.push(ui::span(style::ACCENT, "  ·  刷新中…"));
            }
            if let Some(msg) = s.status.take() {
                spans.push(ui::span(style::ACCENT, format!("  ·  {msg}")));
            }
            ui::print_line(&spans);
        }
    }

    // 输入态不画列表
    if s.input.is_some() {
        return;
    }

    match s.view {
        View::Subs => render_subs(s, width, rows),
        View::Eps => render_eps(s, width, rows),
    }
}

/// 订阅列表：标题<flex> 作者<16> 单集<5> 未播<5>。
fn render_subs(s: &mut UiState, width: usize, rows: i32) {
    if s.store.podcasts.is_empty() {
        ui::print_line(&[
            ui::span(style::FAINT, "还没有订阅。"),
            ui::span(style::BASE, "按 a 贴一个 RSS feed 地址"),
            ui::span(style::FAINT, "（iTunes/泛用型客户端里都能复制到）"),
        ]);
        return;
    }
    let author_w = (width / 4).clamp(8, 20);
    let count_w = 5;
    let title_w = width
        .saturating_sub(author_w + count_w * 2 + 6)
        .max(10);

    ui::print_line(&[
        ui::span(style::FAINT, ui::pad_right("标题", title_w)),
        ui::span(style::FAINT, format!("  {}", ui::pad_right("作者", author_w))),
        ui::span(style::FAINT, format!("  {}", ui::pad_left("单集", count_w))),
        ui::span(style::FAINT, format!("  {}", ui::pad_left("未播", count_w))),
    ]);
    ui::print_line(&[ui::span(style::RULE, "─".repeat(width))]);

    let list_h = (rows.max(4) as usize).saturating_sub(3);
    let total = s.store.podcasts.len();
    let first = ui::window(&mut s.subs_scroll, s.subs_sel, total, list_h);
    ui::set_selected(3 + s.subs_sel - first);

    let snapshot: Vec<_> = s.store.podcasts.clone();
    for (i, p) in snapshot[first..].iter().take(list_h).enumerate() {
        let i = first + i;
        let selected = i == s.subs_sel;
        let title_style = if selected { style::STRONG } else { style::BASE };
        let unplayed = p.episodes.iter().filter(|e| !e.played).count();
        ui::print_line(&[
            ui::span(style::FAINT, ui::pad_left(&(i + 1).to_string(), 3)),
            ui::span(title_style, format!("  {}", ui::pad_right(&p.title, title_w.saturating_sub(3)))),
            ui::span(style::BASE, format!("  {}", ui::pad_right(&p.author, author_w))),
            ui::span(style::FAINT, format!("  {}", ui::pad_left(&p.episodes.len().to_string(), count_w))),
            ui::span(
                if unplayed > 0 { style::ACCENT } else { style::FAINT },
                format!("  {}", ui::pad_left(&unplayed.to_string(), count_w)),
            ),
        ]);
    }
}

/// 单集列表：日期<6> 标题<flex> 时长<6> 状态<8>。
fn render_eps(s: &mut UiState, width: usize, rows: i32) {
    let date_w = 6;
    let dur_w = 6;
    let st_w = 8;
    let title_w = width.saturating_sub(date_w + dur_w + st_w + 6).max(10);

    ui::print_line(&[
        ui::span(style::FAINT, ui::pad_right("日期", date_w)),
        ui::span(style::FAINT, format!("  {}", ui::pad_right("标题", title_w))),
        ui::span(style::FAINT, format!("  {}", ui::pad_left("时长", dur_w))),
        ui::span(style::FAINT, format!("  {}", ui::pad_left("状态", st_w))),
    ]);
    ui::print_line(&[ui::span(style::RULE, "─".repeat(width))]);

    let episodes: Vec<Episode> = current_episodes(s).clone();
    if episodes.is_empty() {
        ui::print_line(&[ui::span(style::FAINT, "没有单集（feed 为空或还在刷新）")]);
        return;
    }

    let list_h = (rows.max(4) as usize).saturating_sub(3);
    let total = episodes.len();
    let first = ui::window(&mut s.eps_scroll, s.eps_sel, total, list_h);
    ui::set_selected(3 + s.eps_sel - first);

    for (i, ep) in episodes[first..].iter().take(list_h).enumerate() {
        let i = first + i;
        let selected = i == s.eps_sel;
        let title_style = if selected { style::STRONG } else { style::BASE };
        let (status_text, status_style): (String, &str) = if ep.played {
            ("已播".into(), style::FAINT)
        } else if ep.in_progress() {
            let pct = if ep.duration_ms > 0 {
                (ep.position_ms * 100 / ep.duration_ms).min(99)
            } else {
                0
            };
            (format!("▶ {pct}%"), style::ACCENT)
        } else if ep.downloaded.is_some() {
            ("⤓ 本地".into(), style::OK)
        } else {
            ("新".into(), style::STRONG)
        };
        ui::print_line(&[
            ui::span(style::FAINT, ui::pad_right(&ep.pub_date, date_w)),
            ui::span(title_style, format!("  {}", ui::pad_right(&ep.title, title_w))),
            ui::span(style::FAINT, format!("  {}", ui::pad_left(&fmt_mmss(ep.duration_ms), dur_w))),
            ui::span(status_style, format!("  {}", ui::pad_left(&status_text, st_w))),
        ]);
    }
}
