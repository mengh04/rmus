//! 顶部"正在播放"面板：曲目信息、进度条、音量/模式/队列信息。

use super::super::fmt_mmss;
use super::block;
use crate::client::tui::app::App;
use crate::proto::{PlayState, StatusInfo};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph};

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let block = match app.status.as_ref().and_then(|s| s.track.as_ref()) {
        Some(_) => block("正在播放"),
        None => block("正在播放（空闲）"),
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [title_area, gauge_area, info_area] =
        Layout::vertical([Constraint::Length(1); 3]).areas(inner);

    let Some(status) = app.status.as_ref() else {
        frame.render_widget(Paragraph::new(Line::from(" 未连接状态").dim()), title_area);
        return;
    };

    draw_title(frame, status, title_area);
    draw_gauge(frame, status, gauge_area);
    draw_info(frame, status, info_area);
}

fn draw_title(frame: &mut Frame, status: &StatusInfo, area: Rect) {
    let (symbol, color) = match status.state {
        PlayState::Playing => ("▶ ", Color::Green),
        PlayState::Paused => ("‖ ", Color::Yellow),
        PlayState::Stopped => ("■ ", Color::DarkGray),
    };
    let line = match &status.track {
        Some(track) => {
            let artist = if track.artist.is_empty() {
                String::new()
            } else {
                format!(" — {}", track.artist)
            };
            Line::from(vec![
                Span::styled(symbol, Style::default().fg(color)),
                Span::styled(&track.title, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(artist, Style::default().add_modifier(Modifier::DIM)),
            ])
        }
        None => Line::from(Span::styled(
            " 队列为空：rmus add <文件/目录>，或按 / 搜索播放",
            Style::default().add_modifier(Modifier::DIM),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_gauge(frame: &mut Frame, status: &StatusInfo, area: Rect) {
    let label = match &status.track {
        Some(_) => format!(
            " {} / {} ",
            fmt_mmss(status.position_ms),
            fmt_mmss(status.duration_ms)
        ),
        None => String::new(),
    };
    let ratio = if status.duration_ms > 0 {
        (status.position_ms as f64 / status.duration_ms as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (fg, bg) = match status.state {
        PlayState::Playing => (Color::Green, Color::DarkGray),
        _ => (Color::DarkGray, Color::DarkGray),
    };
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .gauge_style(Style::default().fg(fg).bg(bg));
    frame.render_widget(gauge, area);
}

fn draw_info(frame: &mut Frame, status: &StatusInfo, area: Rect) {
    let volume = (status.volume.clamp(0.0, 1.0) * 100.0).round() as u32;
    let pos = status.current.map(|i| i + 1).unwrap_or(0);
    let peek = if status.peeking {
        "  ♪ 试听中"
    } else {
        ""
    };
    let queue_name = super::queue_label_from(status);
    let line = Line::from(vec![
        Span::raw(" 音量 "),
        Span::styled(format!("{volume}%"), Style::new().cyan()),
        Span::raw("  循环 "),
        Span::styled(App::repeat_label(status.repeat), Style::new().yellow()),
        Span::raw("  随机 "),
        Span::styled(
            if status.random { "开" } else { "关" },
            Style::new().yellow(),
        ),
        Span::raw(format!("  队列[{queue_name} {pos}/{}]", status.queue_len)),
        Span::styled(peek, Style::new().magenta()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
