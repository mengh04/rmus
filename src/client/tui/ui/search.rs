//! 搜索面板：telescope 风格的居中浮层，输入与实时结果一体。
//! `/` 打开，边输入边过滤，Enter 插播选中项，Esc / 空 query 退格关闭。

use super::super::fmt_mmss;
use crate::client::tui::app::SearchState;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

pub fn draw(frame: &mut Frame, search: &SearchState) {
    let total = frame.area();
    // 提示行 1 + 结果若干 + 上下边框 2，最多 20 行
    let height = (search.results.len() as u16 + 3).min(20).min(total.height);
    let area = popup_area(total, 70, height);
    frame.render_widget(Clear, area);

    let block = Block::new()
        .borders(Borders::ALL)
        .title(Span::styled(" 搜索库 ", Style::default().bold()))
        .title_bottom(
            Line::from(Span::styled(
                " C-j/C-k 选择  Enter 插播  Esc 关闭 ",
                Style::default().add_modifier(Modifier::DIM),
            ))
            .alignment(Alignment::Center),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [prompt_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    // 首行：输入提示 + 光标
    let prompt = Line::from(vec![
        Span::styled("/", Style::default().fg(Color::Yellow).bold()),
        Span::raw(search.query.clone()),
        Span::styled("▏", Style::default().add_modifier(Modifier::REVERSED)),
    ]);
    frame.render_widget(Paragraph::new(prompt), prompt_area);

    if search.results.is_empty() && !search.query.trim().is_empty() {
        let empty = Paragraph::new(Line::styled(
            " （无匹配）",
            Style::default().add_modifier(Modifier::DIM),
        ));
        frame.render_widget(empty, list_area);
        return;
    }

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
        list_area,
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
