//! 队列 tab：曲目列表，标记正在播放的一首。

use super::super::fmt_mmss;
use super::block;
use crate::client::tui::app::App;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let current = app.status.as_ref().and_then(|s| s.current);
    let items: Vec<ListItem> = if app.queue.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " （队列是空的：rmus add <文件/目录> 添加，或按 / 搜索播放）",
            Style::default().add_modifier(Modifier::DIM),
        )))]
    } else {
        app.queue
            .iter()
            .map(|track| {
                let playing = Some(track.index) == current;
                let marker = if playing { "▶" } else { " " };
                let artist = if track.artist.is_empty() {
                    String::new()
                } else {
                    format!(" - {}", track.artist)
                };
                // 正在播放的曲目整行绿色高亮
                let (marker_span, title_style) = if playing {
                    (
                        Span::styled(format!("{marker} "), Style::new().green()),
                        Style::new().green().bold(),
                    )
                } else {
                    (Span::raw(format!("{marker} ")), Style::new().bold())
                };
                ListItem::new(Line::from(vec![
                    marker_span,
                    Span::raw(format!("{:>3} ", track.index + 1)),
                    Span::styled(track.title.clone(), title_style),
                    Span::styled(artist, Style::new().dim()),
                    Span::raw(format!("  [{}]", fmt_mmss(track.duration_ms))),
                ]))
            })
            .collect()
    };

    let queue_len = app.queue.len();
    let selected = (app.queue_selected < queue_len).then_some(app.queue_selected);
    let title = format!("队列 {queue_len} 首");
    let list = List::new(items)
        .block(block(&title))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("» ");
    frame.render_stateful_widget(
        list,
        area,
        &mut ListState::default().with_selected(selected),
    );
}
