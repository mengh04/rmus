//! 底部一行：左边键位速览，右边临时消息 / 统计；输入模式则变成命令行。

use super::super::{app::App, app::Mode, app::Tab, fmt_mmss};
use super::theme::Theme;
use super::widgets::{fit, input_line, key_hints};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn draw(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    if area.is_empty() {
        return;
    }

    if let Mode::Input(purpose) = app.mode {
        let prefix = purpose.prefix().trim_end();
        let (cx, cy) = input_line(frame, theme, area, prefix, app.input_buffer());
        frame.set_cursor_position((cx, cy));
        return;
    }

    // 右侧：有消息就先说消息，没有就报统计
    let right = match app.message.as_ref().filter(|m| m.alive()) {
        Some(msg) => {
            let (fg, bg) = if msg.error {
                (theme.bg, theme.danger)
            } else {
                (theme.bg, theme.ok)
            };
            Line::from(vec![
                Span::styled(
                    format!(" {} ", fit(&msg.text, area.width.saturating_sub(4))),
                    theme.style_chip(fg, bg),
                ),
                Span::styled(" ", theme.style_canvas()),
            ])
        }
        None => Line::from(Span::styled(stats_line(app), theme.style_faint())),
    };
    let right_w = right.width() as u16;

    let mut left = area;
    if right_w > 0 && area.width > right_w + 20 {
        left.width = area.width.saturating_sub(right_w + 2);
        frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
    }

    frame.render_widget(
        Paragraph::new(key_hints(theme, keys_for(app.tab), left.width)),
        left,
    );
}

/// 键位速览随当前视图变化。
fn keys_for(tab: Tab) -> &'static [(&'static str, &'static str)] {
    match tab {
        Tab::Queue => &[
            ("j/k", "移动"),
            ("Enter", "播放"),
            ("/", "搜索"),
            (":", "命令"),
            ("?", "帮助"),
            ("q", "退出"),
        ],
        Tab::Playlists => &[
            ("j/k", "移动"),
            ("Enter", "播放"),
            ("w", "存队列"),
            (":", "命令"),
            ("?", "帮助"),
        ],
    }
}

/// 右下角统计：队列报曲目数和总时长，播放列表报个数。
fn stats_line(app: &App) -> String {
    match app.tab {
        Tab::Queue => {
            let count = app.queue.len();
            if count == 0 {
                return String::new();
            }
            let total: u64 = app.queue.iter().map(|t| t.duration_ms).sum();
            format!("共 {count} 首 · {}", fmt_mmss(total))
        }
        Tab::Playlists => {
            let count = app.playlists.len();
            if count == 0 {
                String::new()
            } else {
                format!("{count} 个播放列表")
            }
        }
    }
}
