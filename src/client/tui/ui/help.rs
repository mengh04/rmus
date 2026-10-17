//! 帮助浮层：上半是 `keymap::HELP` 键位表，下半是 `cmd::SPECS` 命令表。
//! 两张表都是单一事实来源，这里只负责排版。

use super::block;
use crate::client::tui::{cmd, keymap};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

pub fn draw(frame: &mut Frame) {
    let rows = (keymap::HELP.len() + cmd::SPECS.len() + 3) as u16; // 空行 + 命令区标题
    let height = (rows + 2).min(frame.area().height);
    let area = popup_area(frame.area(), 60, height);
    frame.render_widget(Clear, area);

    let block = block("帮助（Esc 关闭）");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = keymap
        ::HELP
        .iter()
        .map(|(key, desc)| key_line(key, desc))
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " 命令（: 前缀，Tab 补全）",
        Style::new().yellow().add_modifier(Modifier::BOLD),
    )));
    lines.extend(cmd::SPECS.iter().map(|spec| {
        key_line(&spec.names.join(" / "), spec.desc)
    }));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn key_line(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {key:<24}"), Style::new().yellow()),
        Span::styled(desc.to_string(), Style::new().add_modifier(Modifier::DIM)),
    ])
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
