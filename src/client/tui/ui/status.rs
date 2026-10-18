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

    if let Mode::Input(purpose) = &app.mode {
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

    let hints: &[(&str, &str)] = if app.visual.is_some() {
        &[
            ("j/k", "选段"),
            ("d", "剪切"),
            ("y", "复制"),
            ("o", "对调"),
            ("Esc", "退出"),
        ]
    } else {
        keys_for(app.tab)
    };

    frame.render_widget(
        Paragraph::new(key_hints(theme, hints, left.width)),
        left,
    );
}

/// 键位速览随当前视图变化。
fn keys_for(tab: Tab) -> &'static [(&'static str, &'static str)] {
    match tab {
        Tab::Queue => &[
            ("j/k", "移动"),
            ("Enter", "播放"),
            ("J/K", "调序"),
            ("/", "搜索"),
            (":", "命令"),
            ("?", "帮助"),
            ("q", "退出"),
        ],
        Tab::Library => &[
            ("j/k", "移动"),
            ("Enter", "插播"),
            ("a", "入队"),
            ("/", "搜索"),
            (":", "命令"),
            ("?", "帮助"),
        ],
        Tab::Playlists => &[
            ("j/k", "移动"),
            ("Enter", "播放"),
            ("c", "改名"),
            ("d", "删除"),
            ("w", "存队列"),
            (":", "命令"),
            ("?", "帮助"),
        ],
        Tab::Online => &[
            ("/", "搜索"),
            ("j/k", "移动"),
            ("d", "下载"),
            ("Enter", "下载+试听"),
            ("x", "取消"),
            ("?", "帮助"),
        ],
    }
}

/// 右下角统计：队列/音乐库报曲目数和总时长，播放列表报个数，
/// 在线视图报搜索结果数与在途下载。
fn stats_line(app: &App) -> String {
    match app.tab {
        Tab::Queue | Tab::Library => {
            let (count, tracks) = match app.tab {
                Tab::Library => (app.library.len(), &app.library),
                _ => (app.queue.len(), &app.queue),
            };
            if count == 0 {
                return String::new();
            }
            let total: u64 = tracks.iter().map(|t| t.duration_ms).sum();
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
        Tab::Online => {
            use super::super::app::DownloadStatus;
            let running = app
                .downloads
                .values()
                .filter(|d| d.status == DownloadStatus::Running)
                .count();
            match (app.online.as_ref().map(|o| o.songs.len()).unwrap_or(0), running) {
                (0, 0) => String::new(),
                (results, 0) => format!("{results} 个结果"),
                (0, _) => format!("下载中 {running}"),
                (results, _) => format!("{results} 个结果 · 下载中 {running}"),
            }
        }
    }
}
