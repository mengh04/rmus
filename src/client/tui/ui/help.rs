//! 帮助浮层：直接从 `keymap::HELP` 生成，和实际键位不会对不上。

use super::block;
use crate::client::tui::keymap;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

pub fn draw(frame: &mut Frame) {
    let rows = keymap::HELP.len() as u16;
    let height = (rows + 2).min(frame.area().height);
    let area = popup_area(frame.area(), 50, height);
    frame.render_widget(Clear, area);

    let block = block("键位（Esc 关闭）");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = keymap::HELP
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(format!(" {key:<14}"), Style::new().yellow()),
                Span::styled(*desc, Style::new().add_modifier(Modifier::DIM)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn popup_area(total: Rect, width_pct: u16, height: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(ratatui::layout::Flex::Center)
        .areas(total);
    let [area] = Layout::horizontal([Constraint::Percentage(width_pct)])
        .flex(ratatui::layout::Flex::Center)
        .areas(area);
    area
}
