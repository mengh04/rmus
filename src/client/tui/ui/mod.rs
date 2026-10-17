//! 渲染层：把 `App` 的状态画成一帧。这里不做任何 IO / 请求。
//!
//! 布局：
//! ```text
//! ┌ 正在播放 ──────────────────────────┐
//! │ ▶ 曲名 - 艺术家                     │
//! │ [██████████░░░░░░] 1:23 / 4:56     │
//! │ 音量 65%  循环 列表  随机 关 ...     │
//! ├────────────────────────────────────┤
//! │  队列 │ 播放列表                    │
//! │  ...列表主体...                     │
//! ├────────────────────────────────────┤
//! │ 提示 / 消息 / 输入行                │
//! └────────────────────────────────────┘
//! ```
//! 搜索面板与帮助是居中浮层。

mod header;
mod help;
mod palette;
mod playlists;
mod queue;
mod search;

use super::app::{App, Mode, Tab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Tabs};

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.tick(); // 顺手清掉过期的底部消息

    let [header_area, tabs_area, body_area, status_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    header::draw(frame, app, header_area);
    draw_tabs(frame, app, tabs_area);
    draw_body(frame, app, body_area);
    draw_status_line(frame, app, status_area);

    // 浮层最后画（盖在主体上）
    if let Some(search) = &app.search {
        search::draw(frame, search);
    }
    if let Some(palette) = &app.palette {
        palette::draw(frame, palette);
    }
    if app.help_open {
        help::draw(frame);
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<String> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| format!(" {} {} ", i + 1, tab.title()))
        .collect();
    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(selected)
        .highlight_style(Style::default().fg(ratatui::style::Color::Yellow).bold());
    frame.render_widget(tabs, area);
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Queue => queue::draw(frame, app, area),
        Tab::Playlists => playlists::draw(frame, app, area),
    }
}

/// 底部一行：输入命令行 > 临时消息 > 键位速览。
fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    let message_line = |app: &App| {
        app.message
            .as_ref()
            .filter(|m| m.alive())
            .map(|msg| {
                let style = if msg.error {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Green)
                };
                Line::from(Span::styled(format!(" {}", msg.text), style))
            })
    };
    let line = match app.mode {
        Mode::Input(purpose) => Line::from(vec![
            Span::styled(purpose.prefix(), Style::default().fg(Color::Yellow).bold()),
            Span::raw(app.input_buffer()),
            Span::styled("▏", Style::default().add_modifier(Modifier::REVERSED)),
        ]),
        // 搜索/命令面板自带输入行，底部只在有消息时显示
        Mode::Search | Mode::Command => message_line(app).unwrap_or_else(|| Line::from("")),
        Mode::Normal => message_line(app).unwrap_or_else(|| {
            let peek = if app.status.as_ref().is_some_and(|s| s.peeking) {
                "  B 恢复原队列"
            } else {
                ""
            };
            Line::from(Span::styled(
                format!(
                    " j/k 移动  空格 播放  n/p 切曲  / 搜索  w 存队列  Tab 切换  ? 帮助  q 退出{peek}"
                ),
                Style::default().add_modifier(Modifier::DIM),
            ))
        }),
    };
    frame.render_widget(line, area);
}

/// 带边框标题的公共块。
pub(crate) fn block(title: &str) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {title} "), Style::default().bold()))
}

/// 当前队列展示名（无名字的临时队列显示占位）。
pub(crate) fn queue_label_from(status: &crate::proto::StatusInfo) -> String {
    status.queue_name.clone().unwrap_or_else(|| "临时".into())
}
