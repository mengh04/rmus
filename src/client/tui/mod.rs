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

/// 主循环事件源（统一队列：键盘 / resize / daemon 推送，谁到处理谁）。
enum UiEvent {
    Key(crossterm::event::KeyEvent),
    Resize(u16, u16),
    Feed(conn::Feed),
}

/// 主循环：事件驱动（无轮询）——键盘与 daemon 推送各占一个读取线程，
/// 汇入同一 channel；`recv` 阻塞等任意源，事件到达即刻处理并重绘。
/// 无事件时零唤醒（进度等周期刷新由 daemon 的推送驱动）。
fn event_loop(terminal: &mut ratatui::DefaultTerminal, mut conn: conn::Daemon) -> Result<()> {
    use crossterm::event::{Event as TermEvent, KeyEventKind, read};
    use std::sync::mpsc::channel;
    use std::thread;

    let feeds = conn.take_feeds();
    let mut app = app::App::new(conn);
    app.startup();
    // 初始数据拉取（消费 startup 置位的 need_*）：事件驱动后 recv 阻塞
    // 等事件，daemon 停止态没有推送——不在这里拉，首帧永远"未同步"
    app.refresh_if_needed();
    let mut title: Option<String> = None;

    let (tx, rx) = channel::<UiEvent>();
    // 键盘/resize：阻塞 read（crossterm 官方支持的跨线程用法），
    // 程序退出后本线程随进程消亡
    {
        let tx = tx.clone();
        thread::Builder::new()
            .name("rmus-input".into())
            .spawn(move || loop {
                match read() {
                    Ok(TermEvent::Key(key)) => {
                        if key.kind == KeyEventKind::Press
                            && tx.send(UiEvent::Key(key)).is_err()
                        {
                            break;
                        }
                    }
                    Ok(TermEvent::Resize(cols, rows)) => {
                        if tx.send(UiEvent::Resize(cols, rows)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            })?;
    }
    // daemon 推送转发：feeds 耗尽（events 线程退出）即断连
    {
        let tx = tx.clone();
        thread::Builder::new()
            .name("rmus-feed-fwd".into())
            .spawn(move || {
                for feed in feeds {
                    if tx.send(UiEvent::Feed(feed)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(UiEvent::Feed(conn::Feed::Disconnected));
            })?;
    }
    drop(tx);

    // 首帧立即画（否则要等第一条推送才见画面）
    terminal.draw(|frame| ui::draw(frame, &mut app))?;

    loop {
        let ev = rx.recv()?;
        match ev {
            UiEvent::Key(key) => app.handle_key(key),
            UiEvent::Resize(cols, rows) => app.on_resize(cols, rows),
            UiEvent::Feed(feed) => app.handle_feed(feed),
        }

        if app.should_quit {
            return Ok(());
        }
        if let Some(fatal) = app.take_fatal() {
            anyhow::bail!("{fatal}");
        }
        // 按需向 daemon 拉取最新状态（触发条件由推送置位）
        app.refresh_if_needed();
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
