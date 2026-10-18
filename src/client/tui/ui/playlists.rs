//! 播放列表视图：一列名字，当前正在播的那张带徽标和左侧强调条。

use super::super::app::App;
use super::theme::Theme;
use super::widgets::{empty_state, fit, list_row_bg_visual, rule};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = Theme::get();

    // 空列表：不画表头，整块留给空态
    if app.playlists.is_empty() {
        empty_state(
            frame,
            theme,
            area,
            "还没有播放列表",
            "按 w 把当前队列存成一个，或用 :pl new <名字> 新建",
        );
        return;
    }

    let head = Rect::new(area.x, area.y, area.width, 1);
    draw_head(frame, theme, head, app.playlists.len());
    if area.height >= 2 {
        rule(frame, theme, Rect::new(area.x, area.y + 1, area.width, 1));
    }
    let list = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    if list.is_empty() {
        return;
    }

    let height = list.height as usize;
    let selected = app
        .playlists_selected
        .min(app.playlists.len().saturating_sub(1));
    let first = super::list::window(
        &mut app.scroll_playlists,
        selected,
        app.playlists.len(),
        height,
    );

    for (row, i) in (first..app.playlists.len()).take(height).enumerate() {
        let y = list.y + row as u16;
        let name = &app.playlists[i];
        let is_selected = i == selected;
        let playing = app.current_playlist.as_deref() == Some(name.as_str());
        let is_visual = app.visual_range().is_some_and(|(a, b)| a <= i && i <= b)
            || app.playlist_marks.contains(name);
        let row_area = Rect::new(list.x, y, list.width, 1);
        list_row_bg_visual(frame.buffer_mut(), theme, row_area, is_selected, playing, is_visual);

        // 序号
        let idx = Span::styled(
            format!("{:>3}", i + 1),
            if playing {
                Style::new().fg(theme.accent_dim)
            } else {
                theme.style_faint()
            },
        );
        frame.render_widget(
            Paragraph::new(Line::from(idx)),
            Rect::new(list.x + 2, y, 3, 1),
        );

        // 名字：当前播放中的用强调色
        let name_style = if playing {
            Style::new().fg(theme.accent).bold()
        } else if is_selected {
            theme.style_strong()
        } else {
            theme.style_text()
        };
        let badge = if playing { " 当前 " } else { "" };
        let badge_w = badge.width() as u16;
        let name_w = list.width.saturating_sub(6).saturating_sub(badge_w + 2);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(fit(name, name_w), name_style))),
            Rect::new(list.x + 6, y, name_w, 1),
        );
        if !badge.is_empty() {
            let badge_area = Rect::new(list.right().saturating_sub(badge_w), y, badge_w, 1);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    badge,
                    theme.style_chip(theme.bg, theme.accent_dim).bold(),
                ))),
                badge_area,
            );
        }
    }
}

fn draw_head(frame: &mut Frame, theme: &Theme, area: Rect, count: usize) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("播放列表", theme.style_faint()))),
        Rect::new(area.x + 2, area.y, area.width.saturating_sub(2), 1),
    );
    let tail = format!("共 {count} 个");
    let tail_w = tail.width() as u16;
    if area.width > tail_w + 12 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(tail, theme.style_faint())))
                .alignment(Alignment::Right),
            Rect::new(area.right().saturating_sub(tail_w), area.y, tail_w, 1),
        );
    }
}
