//! 搜索结果浮层：`/` 输入查询回车后弹出，j/k 选中、Enter 插播、Esc 关闭。

use super::super::fmt_mmss;
use super::block;
use crate::client::tui::app::SearchState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};

pub fn draw(frame: &mut Frame, search: &SearchState) {
    let area = popup_area(frame.area(), 70, (search.results.len() as u16 + 2).min(16));
    frame.render_widget(Clear, area);

    let title = format!("搜索：{}（Enter 插播，Esc 关闭）", search.query);
    let block = block(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = search
        .results
        .iter()
        .map(|track| {
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(track.title.clone(), Style::new().bold()),
                Span::styled(format!(" - {}", track.artist), Style::new().dim()),
                Span::raw(format!("  [{}]", fmt_mmss(track.duration_ms))),
            ]))
        })
        .collect();

    let selected = (search.selected < search.results.len()).then_some(search.selected);
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("» ");
    frame.render_stateful_widget(
        list,
        inner,
        &mut ListState::default().with_selected(selected),
    );
}

/// 居中、宽 `width_pct`%、高 `height` 行的浮层区域。
fn popup_area(total: Rect, width_pct: u16, height: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Length(height.min(total.height))])
        .flex(ratatui::layout::Flex::Center)
        .areas(total);
    let [area] = Layout::horizontal([Constraint::Percentage(width_pct)])
        .flex(ratatui::layout::Flex::Center)
        .areas(area);
    area
}
