//! 打底：铺底色、1px 线、色块进度条、列表行底。

use super::super::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// 用一块区域铺底色：先清掉原有字符，再压上样式。
///
/// 只改样式不清字符的话，浮层会透出底下的列表（`set_style` 不动符号）。
pub fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    let area = buf.area.intersection(area);
    if area.is_empty() {
        return;
    }
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.reset();
            cell.set_style(style);
        }
    }
}

/// 用同一字形填满一块区域（单色模式画描边/进度条用）。
pub fn fill_glyph(buf: &mut Buffer, area: Rect, glyph: &str, style: Style) {
    let area = buf.area.intersection(area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.reset();
            cell.set_symbol(glyph);
            cell.set_style(style);
        }
    }
}

/// 整屏底色。先铺它，后面所有绘制都在这层之上。
pub fn canvas(frame: &mut Frame, theme: &Theme) {
    let area = frame.area();
    fill(frame.buffer_mut(), area, theme.style_canvas());
}

/// 1px 分隔线：有颜色时用背景色铺，无颜色时退化成字形线。
pub fn rule(frame: &mut Frame, theme: &Theme, area: Rect) {
    if theme.mono {
        fill_glyph(frame.buffer_mut(), area, "─", theme.style_faint());
    } else {
        fill(frame.buffer_mut(), area, theme.style_rule());
    }
}

/// 水平进度条：有颜色时铺色块，无颜色时用 `█`/`░` 两个字形。
pub fn bar(
    theme: &Theme,
    buf: &mut Buffer,
    area: Rect,
    ratio: f64,
    fill_color: Color,
    track_color: Color,
) {
    if area.is_empty() {
        return;
    }
    let ratio = ratio.clamp(0.0, 1.0);
    let filled = (area.width as f64 * ratio).round() as u16;
    if theme.mono {
        fill(buf, area, Style::new());
        for i in 0..area.width {
            let (glyph, color) = if i < filled {
                ("█", theme.text)
            } else {
                ("░", theme.faint)
            };
            buf[(area.x + i, area.y)]
                .set_symbol(glyph)
                .set_style(Style::new().fg(color));
        }
        return;
    }
    fill(buf, area, Style::new().bg(track_color));
    if filled > 0 {
        let mut head = area;
        head.width = filled.min(area.width);
        fill(buf, head, Style::new().bg(fill_color));
    }
}

/// 列表行底：选中铺抬起的底，正在播放铺左边一格强调条。
pub fn list_row_bg(buf: &mut Buffer, theme: &Theme, area: Rect, selected: bool, playing: bool) {
    list_row_bg_visual(buf, theme, area, selected, playing, false)
}

/// 带 visual 选段位的版本：visual 行铺冷色底，光标行的选中底盖在上面。
pub fn list_row_bg_visual(
    buf: &mut Buffer,
    theme: &Theme,
    area: Rect,
    selected: bool,
    playing: bool,
    visual: bool,
) {
    if visual {
        fill(buf, area, theme.style_visual());
    }
    if selected {
        fill(buf, area, theme.style_selected());
    }
    if playing {
        let mut mark = area;
        mark.width = 1;
        if theme.mono {
            fill(buf, mark, Style::new().add_modifier(Modifier::REVERSED));
        } else {
            fill(buf, mark, Style::new().bg(theme.accent));
        }
    }
}
