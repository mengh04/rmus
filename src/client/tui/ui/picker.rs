//! 候选列表浮层：输入即模糊过滤（音乐库选曲加入播放列表等）。
//!
//! 标题行 = "标题 · 输入行"，候选列表跟随高亮，底部一行键位提示。

use super::super::app::CandidatePicker;
use super::theme::Theme;
use super::widgets::{POPUP_CHROME, empty_state, fit, key_hints, list_row_bg, popup};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// 候选最多展示的行数（超出靠滚动跟随选中项）。
const MAX_ROWS: u16 = 10;

pub fn draw(frame: &mut Frame, picker: &CandidatePicker) {
    let theme = Theme::get();
    let filtered = picker.filtered();
    let rows = (filtered.len() as u16).clamp(1, MAX_ROWS);
    let p = popup(frame, theme, 56, rows + POPUP_CHROME);
    if p.title.is_empty() {
        return;
    }

    // ---- 标题行 = 标题 + 输入行 -------------------------------------------
    let line_rect = Rect::new(p.title.x + 2, p.title.y, p.title.width.saturating_sub(4), 1);
    let prefix = format!("{} · ", picker.title);
    let prefix_w = (prefix.width() as u16).min(line_rect.width.saturating_sub(4));
    let avail = line_rect.width.saturating_sub(prefix_w);
    let shown = if (picker.input.width() as u16) > avail {
        fit(&picker.input, avail)
    } else {
        picker.input.clone()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            ratatui::text::Span::styled(prefix, theme.style_faint()),
            ratatui::text::Span::styled(shown.clone(), theme.style_text()),
        ])),
        line_rect,
    );
    let cx = line_rect.x + prefix_w + (shown.width() as u16).min(avail.saturating_sub(1));
    frame.set_cursor_position((cx.min(line_rect.right().saturating_sub(1)), line_rect.y));

    // ---- 候选 --------------------------------------------------------------
    if filtered.is_empty() {
        // 没有匹配：提示直接回车按输入名新建
        let hint = if picker.input.trim().is_empty() {
            "还没有播放列表，输入名字回车新建"
        } else {
            "没有匹配的播放列表 · 回车按输入名新建并加入"
        };
        empty_state(frame, theme, p.body, hint, "Esc 取消");
    } else {
        let height = p.body.height as usize;
        let selected = picker.selected.min(filtered.len().saturating_sub(1));
        let first = if selected >= height { selected + 1 - height } else { 0 };
        for (row, i) in filtered.iter().enumerate().skip(first).take(height) {
            let item = &picker.items[*i];
            let y = p.body.y + (row - first) as u16;
            let row_area = Rect::new(p.body.x, y, p.body.width, 1);
            let is_selected = row == selected;
            list_row_bg(frame.buffer_mut(), theme, row_area, is_selected, false);
            frame.render_widget(
                Paragraph::new(Line::from(fit(&item.label, p.body.width.saturating_sub(4)))),
                Rect::new(p.body.x + 2, y, p.body.width.saturating_sub(4), 1),
            );
        }
    }

    // ---- 底部键位 ----------------------------------------------------------
    if !p.hint.is_empty() {
        let items: &[(&str, &str)] = &[
            ("输入", "过滤"),
            ("Enter", "确认"),
            ("C-j/k", "移动"),
            ("Esc", "取消"),
        ];
        let hints = key_hints(theme, items, p.hint.width.saturating_sub(4));
        frame.render_widget(
            Paragraph::new(hints),
            Rect::new(p.hint.x + 3, p.hint.y, p.hint.width.saturating_sub(4), 1),
        );
    }
}
