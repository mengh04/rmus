//! 渲染层快照测试：靠 `TestBackend` 把一帧变成纯文本，
//! 覆盖各尺寸分档、浮层、空态和消息条，防止布局改动把对齐弄坏。

use super::draw;
use crate::client::tui::app::{App, CmdPalette, Message, Mode, SearchMode, SearchState};
use crate::client::tui::conn::{Daemon, Feed};
use crate::proto::{PlayState, RepeatMode, StatusInfo, TrackInfo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn track(index: usize, title: &str, artist: &str, album: &str, dur: u64) -> TrackInfo {
    TrackInfo {
        index,
        path: std::path::PathBuf::from(format!("/tmp/{title}.flac")),
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        duration_ms: dur,
    }
}

fn demo_app() -> App {
    let mut app = App::new(Daemon::detached());
    app.status = Some(StatusInfo {
        state: PlayState::Playing,
        current: Some(1),
        position_ms: 83_000,
        duration_ms: 276_000,
        volume: 0.7,
        queue_len: 4,
        track: Some(track(1, "红豆", "王菲", "唱游", 276_000)),
        repeat: RepeatMode::All,
        random: false,
        queue_name: Some("深夜循环".into()),
        queue_modified: true,
        peeking: false,
    });
    app.queue = vec![
        track(0, "晚风", "陈婧霏", "陈婧霏", 252_000),
        track(1, "红豆", "王菲", "唱游", 276_000),
        track(2, "流年", "王菲", "王菲", 280_000),
        track(
            3,
            "A Very Long Track Title That Should Be Truncated Gracefully",
            "Some Artist",
            "An Album Name",
            3_723_000,
        ),
    ];
    app.playlists = vec!["深夜循环".into(), "我喜欢的".into(), "通勤".into()];
    app.current_playlist = Some("深夜循环".into());
    app.queue_selected = 1;
    app
}

/// 把一帧渲染成纯文本行，方便人工检查对齐。
///
/// 宽字符占两格：跳过后面的空占位格，拼出来才是人能读的一行。
fn render(app: &mut App, width: u16, height: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0u16;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                x += (symbol.width() as u16).max(1);
                line.push_str(symbol);
            }
            line.trim_end().to_owned()
        })
        .collect()
}

#[test]
fn 各尺寸都不panic() {
    let mut app = demo_app();
    for (w, h) in [(120, 34), (80, 24), (60, 18), (40, 12), (20, 8), (10, 4)] {
        let lines = render(&mut app, w, h);
        assert_eq!(lines.len(), h as usize);
    }
}

#[test]
fn 浮层都能渲染() {
    let mut app = demo_app();
    app.mode = Mode::Search;
    app.search = Some(SearchState {
        query: "晚".into(),
        results: vec![track(0, "晚风", "陈婧霏", "陈婧霏", 252_000)],
        selected: 0,
        mode: SearchMode::Input,
        pending_g: false,
    });
    let lines = render(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains('晚')), "搜索结果应出现");

    // normal 子模式也要能渲染（字母键是动作，无光标）
    if let Some(search) = &mut app.search {
        search.mode = SearchMode::Normal;
    }
    let lines = render(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains("NORMAL")), "normal 模式标示应出现");
    if let Some(search) = &mut app.search {
        search.mode = SearchMode::Input;
    }

    app.mode = Mode::Command;
    app.palette = Some(CmdPalette {
        input: "seek".into(),
        selected: 0,
        error: None,
    });
    let lines = render(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains("seek")), "命令名应出现");

    app.mode = Mode::Normal;
    app.palette = None;
    app.help_open = true;
    let lines = render(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains("帮助")), "帮助标题应出现");
}

#[test]
fn 消息与空态() {
    let mut app = demo_app();
    app.message = Some(Message {
        text: "已加入队列".into(),
        error: false,
        at: std::time::Instant::now(),
    });
    let lines = render(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains("已加入队列")));

    app.queue.clear();
    app.playlists.clear();
    app.status = None;
    let lines = render(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains("队列是空的")));
}

/// 播放错误（Event::Error）要落到状态行，且按错误样式停留更久。
#[test]
fn 播放错误显示在状态行() {
    let mut app = demo_app();
    app.handle_feed(Feed::Daemon(crate::proto::Event::Error {
        message: "无法播放，已跳过：/tmp/x.mp3: 不支持的编码".into(),
    }));
    let msg = app.message.as_ref().expect("错误应写进消息条");
    assert!(msg.error, "错误消息要按错误样式显示");
    assert!(msg.text.contains("不支持的编码"), "消息内容: {}", msg.text);
    assert!(msg.alive(), "错误消息应立即可见");
}

/// 打印一份快照（`cargo test -- --nocapture` 时人工看）。
#[test]
fn 快照_120x32() {
    let mut app = demo_app();
    let lines = render(&mut app, 120, 32);
    println!("\n┌{}┐", "─".repeat(120));
    for line in &lines {
        println!("│{line:<120}│");
    }
    println!("└{}┘", "─".repeat(120));
}
