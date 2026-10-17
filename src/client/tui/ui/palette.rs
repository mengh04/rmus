//! 命令面板浮层：telescope 风格，输入即过滤，选中代入，Enter 执行。

use super::super::cmd;
use crate::client::tui::app::CmdPalette;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

pub fn draw(frame: &mut Frame, palette: &CmdPalette) {
    let filtered = cmd::filter(&palette.input);
    let total = frame.area();
    let err_extra = u16::from(palette.error.is_some());
    // 提示行 1 + 错误行 ?1 + 命令若干 + 上下边框 2，最多 20 行
    let height = (1 + err_extra + filtered.len() as u16 + 2)
        .min(20)
        .min(total.height);
    let area = popup_area(total, 70, height);
    frame.render_widget(Clear, area);

    let block = Block::new()
        .borders(Borders::ALL)
        .title(Span::styled(" 命令面板 ", Style::default().bold()))
        .title_bottom(
            Line::from(Span::styled(
                " C-j/C-k 选择  Tab 补全  ↑↓ 历史  Esc 关闭 ",
                Style::default().add_modifier(Modifier::DIM),
            ))
            .alignment(ratatui::layout::Alignment::Center),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut constraints = vec![Constraint::Length(1)];
    if palette.error.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let mut constraints = vec![Constraint::Length(1)];
    if palette.error.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let chunks = Layout::vertical(constraints).split(inner);
    let prompt_area = chunks[0];
    let error_area = palette.error.is_some().then_some(chunks[1]);
    let list_area = chunks[chunks.len() - 1];

    let prompt = Line::from(vec![
        Span::styled(":", Style::default().fg(Color::Yellow).bold()),
        Span::raw(palette.input.clone()),
        Span::styled("▏", Style::default().add_modifier(Modifier::REVERSED)),
    ]);
    frame.render_widget(Paragraph::new(prompt), prompt_area);

    if let (Some(error), Some(area)) = (&palette.error, error_area) {
        let line = Paragraph::new(Line::styled(
            format!(" {error}"),
            Style::default().fg(Color::Red),
        ));
        frame.render_widget(line, area);
    }

    if filtered.is_empty() {
        let empty = Paragraph::new(Line::styled(
            " （无匹配命令）",
            Style::default().add_modifier(Modifier::DIM),
        ));
        frame.render_widget(empty, list_area);
        return;
    }

    let items: Vec<ListItem> = filtered
        .iter()
        .map(|spec| {
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{:<20}", spec.names.join(" / ")),
                    Style::new().bold(),
                ),
                Span::styled(spec.desc.to_string(), Style::new().dim()),
            ]))
        })
        .collect();

    let selected = (palette.selected < filtered.len()).then_some(palette.selected);
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
