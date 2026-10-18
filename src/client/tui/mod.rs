//! rmus TUI：无参数启动的交互界面。
//!
//! 分层（自底向上，依赖只朝一个方向）：
//! - [`conn`]：与 rmusd 的连接封装（主线程请求 + 后台线程收事件推送）
//! - [`keymap`]：按键 -> [`keymap::Action`] 的 vim 风格映射，唯一改键入口
//! - [`cmd`]：`:` 命令面板的命令表 / 解析 / 补全（执行在 app）
//! - [`app`]：界面状态与动作执行（全部业务逻辑；不直接碰终端）
//! - [`ui`]：纯渲染（app 状态 -> 一帧画面；不做任何 IO / 请求）
//!
//! 拓展新功能的套路：
//! 1. `keymap.rs`：加一个 `Action` 变体并绑定按键（帮助页自动跟着变）
//! 2. `app.rs` 的 `App::execute`：加一个 match 分支发请求 / 改状态
//! 3. 需要新视图时在 `ui/` 加一个渲染模块，`ui::draw` 里安排位置

mod app;
mod cmd;
mod conn;
mod keymap;
mod ui;

use anyhow::Result;
use std::path::PathBuf;

/// 连接 rmusd 并进入 TUI 主循环，返回后终端已恢复原样。
pub fn run(socket: Option<PathBuf>, no_spawn: bool) -> Result<()> {
    let conn = conn::Daemon::connect(socket, no_spawn)?;
    // ratatui::init 会装好 panic hook：即使 panic 也恢复终端
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, conn);
    ratatui::restore();
    // 退出后窗口标题别再挂着歌名
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle("rmus"));
    result
}

/// 主循环：键盘事件 + daemon 推送 -> app 状态 -> 渲染。
fn event_loop(terminal: &mut ratatui::DefaultTerminal, conn: conn::Daemon) -> Result<()> {
    use crossterm::event::{Event as TermEvent, KeyEventKind, poll, read};
    use std::time::Duration;

    let mut app = app::App::new(conn);
    app.startup();
    let mut title: Option<String> = None;

    loop {
        // 键盘输入（100ms 轮询窗口，期间也能让出 CPU）
        if poll(Duration::from_millis(100))?
            && let TermEvent::Key(key) = read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key);
        }
        // daemon 推送（非阻塞取空队列）
        app.pump_events();
        // 按需向 daemon 拉取最新状态
        app.refresh_if_needed();

        if app.should_quit {
            return Ok(());
        }
        if let Some(fatal) = app.take_fatal() {
            anyhow::bail!("{fatal}");
        }

        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        sync_title(&mut title, &app);
    }
}

/// 终端窗口标题跟随当前曲目：只在变化时写一次。
fn sync_title(last: &mut Option<String>, app: &app::App) {
    let want = match app.status.as_ref().and_then(|s| s.track.as_ref()) {
        Some(track) if !track.artist.is_empty() => {
            format!("{} · {} — rmus", track.title, track.artist)
        }
        Some(track) => format!("{} — rmus", track.title),
        None => "rmus".to_owned(),
    };
    if last.as_deref() != Some(want.as_str()) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(&want));
        *last = Some(want);
    }
}

/// 毫秒 -> "m:ss" / "h:mm:ss"。
pub(crate) fn fmt_mmss(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
