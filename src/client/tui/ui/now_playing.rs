//! 底部"正在播放"：标题行 + 进度行，两行讲完一首歌。
//!
//! 进度条用背景色块画，比 `[████░░░░]` 更干净，也不会被 CJK 宽度坑到；
//! 播放/暂停只靠颜色和字形粗细区分，不再额外堆图标。

use super::super::{app::App, fmt_mmss};
use super::theme::Theme;
use super::widgets::{bar, fit};
use crate::proto::{PlayState, StatusInfo};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width < 12 {
        return;
    }
    let theme = Theme::get();
    let Some(status) = app.status.as_ref() else {
        return;
    };
    // 没在放东西时整条留白：上面有空态和顶栏状态，这里不必再喊一遍
    let Some(track) = status.track.as_ref() else {
        return;
    };
    let title_area = Rect::new(area.x, area.y, area.width, 1);
    draw_title_row(frame, status, track, theme, title_area);
    if area.height >= 2 {
        let progress_area = Rect::new(area.x, area.y + 1, area.width, 1);
        draw_progress_row(frame, status, theme, progress_area);
    }
}

fn draw_title_row(
    frame: &mut Frame,
    status: &StatusInfo,
    track: &crate::proto::TrackInfo,
    theme: &Theme,
    area: Rect,
) {
    if area.width < 8 {
        return;
    }
    // 右侧：试听徽标 + 队列位置
    let mut right = Vec::new();
    if status.peeking {
        right.push(Span::styled(
            " 试听中 ",
            theme.style_chip(theme.bg, theme.accent_dim),
        ));
        right.push(Span::raw(" "));
    }
    if status.queue_len > 0 {
        let pos = status.current.map(|i| i + 1).unwrap_or(0);
        right.push(Span::styled(
            format!("{pos}/{}", status.queue_len),
            theme.style_faint(),
        ));
    }
    let right_line = Line::from(right);
    let right_w = right_line.width() as u16;
    let show_right = right_w > 0 && area.width > right_w + 30;

    let text_area = Rect {
        x: area.x,
        y: area.y,
        width: area
            .width
            .saturating_sub(if show_right { right_w + 2 } else { 0 }),
        height: 1,
    };
    if show_right {
        let right_area = Rect::new(area.right().saturating_sub(right_w), area.y, right_w, 1);
        frame.render_widget(Paragraph::new(right_line), right_area);
    }

    let playing = status.state == PlayState::Playing;
    let title_style = if playing {
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        theme.style_strong()
    };
    let title = fit(&track.title, text_area.width.max(1));
    let mut used = title.width() as u16;
    let mut spans = vec![Span::styled(title, title_style)];
    if !track.artist.is_empty() && used + 4 < text_area.width {
        let artist = format!("  {}", fit(&track.artist, text_area.width - used - 3));
        used += artist.width() as u16;
        spans.push(Span::styled(artist, theme.style_muted()));
    }
    if !track.album.is_empty() && used + 4 < text_area.width {
        let album = format!("  {}", fit(&track.album, text_area.width - used - 3));
        spans.push(Span::styled(album, theme.style_faint()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), text_area);
}

fn draw_progress_row(frame: &mut Frame, status: &StatusInfo, theme: &Theme, area: Rect) {
    if area.width < 12 {
        return;
    }
    let has_track = status.track.is_some();
    let elapsed = if has_track {
        fmt_mmss(status.position_ms)
    } else {
        "--:--".to_owned()
    };
    let total = if has_track && status.duration_ms > 0 {
        fmt_mmss(status.duration_ms)
    } else {
        "--:--".to_owned()
    };

    let elapsed_w = elapsed.width() as u16;
    let total_w = total.width() as u16;
    let show_total = area.width > elapsed_w + total_w + 20;
    let dur_w = if show_total { total_w + 2 } else { 0 };

    let bar_x = area.x + elapsed_w + 1;
    let bar_end = area.right().saturating_sub(dur_w);
    let bar_w = bar_end.saturating_sub(bar_x);
    if bar_w < 6 {
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(elapsed, theme.style_cool()))),
        Rect::new(area.x, area.y, elapsed_w, 1),
    );

    let ratio = if status.duration_ms > 0 {
        status.position_ms as f64 / status.duration_ms as f64
    } else {
        0.0
    };
    let filled = match status.state {
        PlayState::Playing => theme.accent,
        PlayState::Paused => theme.accent_dim,
        PlayState::Stopped => theme.line,
    };
    let bar_area = Rect::new(bar_x, area.y, bar_w, 1);
    bar(
        theme,
        frame.buffer_mut(),
        bar_area,
        ratio,
        filled,
        theme.line,
    );

    if show_total {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(total, theme.style_faint())))
                .alignment(Alignment::Right),
            Rect::new(area.right().saturating_sub(total_w), area.y, total_w, 1),
        );
    }
}
