//! 键位速览条与空态。

use super::super::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// 键位速览：`j/k` 用键帽样式，说明用次要色。放不下就整组丢掉。
pub fn key_hints(theme: &Theme, items: &[(&str, &str)], max_width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0u16;
    for (key, desc) in items {
        let cap = format!(" {key} ");
        let text = format!(" {desc} ");
        let need = cap.width() as u16 + text.width() as u16;
        if used + need > max_width && !spans.is_empty() {
            break;
        }
        spans.push(Span::styled(
            cap,
            theme.style_chip(theme.text, theme.raised),
        ));
        spans.push(Span::styled(text, theme.style_muted()));
        used += need;
    }
    Line::from(spans)
}

/// 空态：一句结论 + 一句下一步该按什么，居中偏上摆放。
pub fn empty_state(frame: &mut Frame, theme: &Theme, area: Rect, title: &str, hint: &str) {
    if area.height < 2 || area.width < 8 {
        return;
    }
    let top = area.y + (area.height / 3).min(area.height.saturating_sub(2));
    let title_area = Rect::new(area.x, top, area.width, 1);
    let hint_area = Rect::new(area.x, top + 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::styled(title.to_owned(), theme.style_strong()))
            .alignment(Alignment::Center),
        title_area,
    );
    if area.height >= 3 {
        frame.render_widget(
            Paragraph::new(Line::styled(hint.to_owned(), theme.style_faint()))
                .alignment(Alignment::Center),
            hint_area,
        );
    }
}
