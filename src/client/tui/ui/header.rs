//! 顶栏：品牌、视图切换、队列名，右侧是循环/随机/音量这些常驻状态。
//!
//! 只占一行：把纵向空间留给列表，播放状态交给底部那一条"正在播放"。

use super::super::app::{App, Tab};
use super::PAGE_MARGIN;
use super::theme::Theme;
use crate::proto::{PlayState, StatusInfo};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width < 10 {
        return;
    }
    let theme = Theme::get();
    let row = Rect {
        x: area.x + PAGE_MARGIN.min(area.width),
        y: area.y,
        width: area.width.saturating_sub(PAGE_MARGIN * 2),
        height: 1,
    };
    if row.width < 4 {
        return;
    }

    let right = right_line(app, theme, row.width);
    let right_w = right.width() as u16;
    // 左侧至少要放得下品牌 + 一个 tab，否则右侧状态整体让位
    let left_min = 6 + 12;
    let show_right = right_w > 0 && row.width > left_min + right_w + 2;

    let mut left = row;
    if show_right {
        left.width = row.width.saturating_sub(right_w + 2);
        frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), row);
    }
    frame.render_widget(Paragraph::new(left_line(app, theme, left.width)), left);
}

/// 左侧：`rmus` 品牌 + 视图 tab（带计数）+ 队列名。
fn left_line(app: &App, theme: &Theme, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("rmus", theme.style_accent_strong()),
        Span::raw("    "),
    ];
    let mut used: u16 = 8;

    for (i, tab) in Tab::ALL.iter().enumerate() {
        let active = *tab == app.tab;
        let count = match tab {
            Tab::Queue => app.queue.len(),
            Tab::Playlists => app.playlists.len(),
        };
        let label = format!(" {} {} {} ", i + 1, tab.title(), count);
        let w = label.width() as u16;
        if !active && used + w + 2 > width {
            continue;
        }
        let style = if active {
            theme
                .style_chip(theme.accent, theme.raised)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("  "));
        used += w + 2;
    }

    // 队列名：匿名临时队列不显示，省得每帧都在喊“临时”
    if let Some(name) = app
        .status
        .as_ref()
        .and_then(|s| s.queue_name.as_deref())
        .filter(|n| !n.is_empty())
    {
        let tail = format!("· {name}");
        let w = tail.width() as u16;
        if used + w < width {
            spans.push(Span::styled(tail, theme.style_muted()));
        }
    }
    Line::from(spans)
}

/// 右侧：播放状态 + 循环 / 随机 / 音量。按宽度分档，宁缺毋滥。
fn right_line(app: &App, theme: &Theme, width: u16) -> Line<'static> {
    let Some(status) = app.status.as_ref() else {
        return Line::from(Span::styled("未同步", theme.style_faint()));
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    if width >= 52 {
        spans.extend(state_span(status, theme));
    }
    if width >= 70 {
        spans.push(Span::styled("循环 ", theme.style_faint()));
        spans.push(Span::styled(
            App::repeat_label(status.repeat).to_owned(),
            theme.style_text(),
        ));
        spans.push(Span::raw("   "));
        spans.push(Span::styled("随机 ", theme.style_faint()));
        spans.push(Span::styled(
            if status.random { "开" } else { "关" }.to_owned(),
            if status.random {
                theme.style_accent()
            } else {
                theme.style_text()
            },
        ));
        spans.push(Span::raw("   "));
    }
    if width >= 34 {
        let volume = (status.volume.clamp(0.0, 1.0) * 100.0).round() as u32;
        if width >= 92 {
            // 音量条：6 格色块，比数字更快读懂
            let filled = ((status.volume.clamp(0.0, 1.0) * 6.0).round() as usize).min(6);
            spans.push(Span::styled("音量 ", theme.style_faint()));
            if theme.mono {
                let mut meter = "█".repeat(filled);
                meter.push_str(&"░".repeat(6 - filled));
                spans.push(Span::styled(meter, theme.style_text()));
            } else {
                spans.push(Span::styled(
                    " ".repeat(filled),
                    Style::new().bg(theme.accent_dim),
                ));
                spans.push(Span::styled(
                    " ".repeat(6 - filled),
                    Style::new().bg(theme.line),
                ));
            }
            spans.push(Span::styled(format!(" {volume}%"), theme.style_text()));
        } else {
            spans.push(Span::styled("音量 ", theme.style_faint()));
            spans.push(Span::styled(format!("{volume}%"), theme.style_text()));
        }
    }

    // 尾部去空白，右对齐时才贴得紧
    if spans.last().is_some_and(|s| s.content.ends_with(' '))
        && let Some(last) = spans.last_mut()
    {
        let trimmed = last.content.trim_end().to_owned();
        last.content = trimmed.into();
    }
    Line::from(spans)
}

fn state_span(status: &StatusInfo, theme: &Theme) -> Vec<Span<'static>> {
    let (label, style) = match status.state {
        PlayState::Playing => ("播放中", theme.style_accent_strong()),
        PlayState::Paused => ("已暂停", Style::new().fg(theme.cool)),
        PlayState::Stopped => ("已停止", theme.style_faint()),
    };
    vec![Span::styled(format!("{label}  "), style)]
}
