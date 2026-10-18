//! 文字排版：CJK 安全的截断/补白，以及浮层里的输入行。

use super::super::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// 按显示宽度截断，超出部分用省略号收尾（CJK 双宽字符不会被切一半）。
pub fn fit(text: &str, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.to_string().width();
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// 左边补空格到指定显示宽度（CJK 安全）。超宽则截断。
pub fn pad_right(text: &str, width: u16) -> String {
    let fitted = fit(text, width);
    let used = fitted.width() as u16;
    let mut out = fitted;
    for _ in used..width {
        out.push(' ');
    }
    out
}

/// 输入行：前缀（`/`、`:`）+ 输入内容，返回硬件光标位置。
///
/// 光标交给终端画（`Frame::set_cursor_position`），比自己画一个方块更像原生输入框。
pub fn input_line(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    prefix: &str,
    text: &str,
) -> (u16, u16) {
    let prefix_w = (prefix.width() + 1) as u16;
    let avail = area.width.saturating_sub(prefix_w);
    let shown = if (text.width() as u16) > avail {
        fit(text, avail)
    } else {
        text.to_owned()
    };
    let line = Line::from(vec![
        Span::styled(prefix.to_owned(), theme.style_accent_strong()),
        Span::raw(" "),
        Span::styled(shown.clone(), theme.style_text()),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    let caret_x = area.x + prefix_w + (shown.width() as u16).min(avail.saturating_sub(1));
    (caret_x.min(area.right().saturating_sub(1)), area.y)
}
