//! 命令面板：`:` 打开，输入即过滤，Tab 补全，Enter 执行。
//!
//! 和搜索浮层共用同一套骨架：标题行就是输入行，中间是列表，底部是键位。

use super::super::app::CmdPalette;
use super::super::cmd;
use super::theme::Theme;
use super::widgets::{POPUP_CHROME, fit, input_line, key_hints, list_row_bg, popup};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

const MAX_ROWS: u16 = 12;
/// 命令名列宽（含别名，如 `pl / playlist`）。
const NAME_W: u16 = 16;

pub fn draw(frame: &mut Frame, palette: &CmdPalette) {
    let theme = Theme::get();
    let filtered = cmd::filter(&palette.input);
    let rows = (filtered.len() as u16).clamp(1, MAX_ROWS);
    let p = popup(frame, theme, 68, rows + POPUP_CHROME);
    if p.title.is_empty() {
        return;
    }

    // ---- 标题行 = 输入行；右侧显示计数或错误 ------------------------------
    let meta_style = if palette.error.is_some() {
        theme.style_danger()
    } else {
        theme.style_faint()
    };
    let meta = match &palette.error {
        Some(err) => err.clone(),
        None => format!("{} 个命令", filtered.len()),
    };
    let meta_w = meta.width() as u16;
    let room_for_meta = p.title.width > meta_w + 28;
    let input_w = p
        .title
        .width
        .saturating_sub(4)
        .saturating_sub(if room_for_meta { meta_w + 2 } else { 0 });
    let input = Rect::new(p.title.x + 2, p.title.y, input_w, 1);
    let (cx, cy) = input_line(frame, theme, input, ":", &palette.input);
    frame.set_cursor_position((cx, cy));
    if room_for_meta {
        let meta_area = Rect::new(p.title.right() - 2 - meta_w, p.title.y, meta_w, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(fit(&meta, meta_w), meta_style)))
                .alignment(Alignment::Right),
            meta_area,
        );
    }

    // ---- 命令列表 ---------------------------------------------------------
    if filtered.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "没有匹配的命令，试试 :help",
                theme.style_muted(),
            )))
            .alignment(Alignment::Center),
            Rect::new(p.body.x, p.body.y + p.body.height / 2, p.body.width, 1),
        );
    } else {
        let height = p.body.height as usize;
        let selected = palette.selected.min(filtered.len().saturating_sub(1));
        let first = if selected >= height {
            selected + 1 - height
        } else {
            0
        };
        for (row, i) in (first..filtered.len()).take(height).enumerate() {
            let y = p.body.y + row as u16;
            let spec = filtered[i];
            let row_area = Rect::new(p.body.x, y, p.body.width, 1);
            let is_selected = i == selected;
            list_row_bg(
                frame.buffer_mut(),
                theme,
                row_area,
                is_selected,
                is_selected,
            );

            let name_w = NAME_W.min(p.body.width.saturating_sub(8).max(8));
            let name_style = if is_selected {
                Style::new().fg(theme.accent)
            } else {
                theme.style_text()
            };
            let names = spec.names.join(" / ");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    fit(&names, name_w),
                    name_style.add_modifier(ratatui::style::Modifier::BOLD),
                ))),
                Rect::new(p.body.x + 2, y, name_w, 1),
            );
            let desc_x = p.body.x + 2 + name_w + 2;
            let desc_w = p.body.right().saturating_sub(2 + desc_x);
            if desc_w > 0 {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        fit(spec.desc, desc_w),
                        theme.style_muted(),
                    ))),
                    Rect::new(desc_x, y, desc_w, 1),
                );
            }
        }
    }

    // ---- 底部键位 ---------------------------------------------------------
    if !p.hint.is_empty() {
        let hints = key_hints(
            theme,
            &[
                ("↑↓", "历史"),
                ("Tab", "补全"),
                ("Enter", "执行"),
                ("Esc", "关闭"),
            ],
            p.hint.width.saturating_sub(4),
        );
        frame.render_widget(
            Paragraph::new(hints),
            Rect::new(p.hint.x + 3, p.hint.y, p.hint.width.saturating_sub(4), 1),
        );
    }
}
