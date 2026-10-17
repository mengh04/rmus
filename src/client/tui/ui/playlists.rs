//! 播放列表 tab：列出所有命名队列，标记当前正在用的。

use super::block;
use crate::client::tui::app::App;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = if app.playlists.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " （没有播放列表：按 w 把当前队列存为一个）",
            Style::default().add_modifier(Modifier::DIM),
        )))]
    } else {
        app.playlists
            .iter()
            .map(|name| {
                let current = app.current_playlist.as_deref() == Some(name.as_str());
                let (marker, style) = if current {
                    ("▶ ", Style::new().green().bold())
                } else {
                    ("  ", Style::new())
                };
                ListItem::new(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(name.clone(), style),
                ]))
            })
            .collect()
    };

    let len = app.playlists.len();
    let selected = (app.playlists_selected < len).then_some(app.playlists_selected);
    let title = format!("播放列表 {len} 个");
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
