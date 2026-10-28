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

    // 右侧：模式提示 > 临时消息 > 统计。模式提示由当前状态推导，
    // 退出直通/visual 的那一帧就消失，不等消息 TTL。
    let chip = |text: &str, error: bool| -> Line<'static> {
        let (fg, bg) = if error {
            (theme.bg, theme.danger)
        } else {
            (theme.bg, theme.ok)
        };
        Line::from(vec![
            Span::styled(
                format!(" {} ", fit(text, area.width.saturating_sub(4))),
                theme.style_chip(fg, bg),
            ),
            Span::styled(" ", theme.style_canvas()),
        ])
    };
    let right = if let Some(hint) = mode_hint(app) {
        chip(hint, false)
    } else {
        match app.message.as_ref().filter(|m| m.alive()) {
            Some(msg) => chip(&msg.text, msg.error),
            None => Line::from(Span::styled(stats_line(app), theme.style_faint())),
        }
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
    } else if app.mode == Mode::Plugin {
        // 按键直通：所有键进插件，Esc 出来
        &[
            ("任意键", "转发插件"),
            ("Esc", "退出直通"),
        ]
    } else {
        keys_for(
            &app.tab,
            app.plugin_of_tab(&app.tab)
                .is_some_and(|p| app.supports_online(&p)),
        )
    };

    frame.render_widget(
        Paragraph::new(key_hints(theme, hints, left.width)),
        left,
    );
}

/// 当前是否有模式提示（按键直通 / visual）。这是状态栏的一部分，
/// 不存进 `Message`——否则退出模式后还会挂到 TTL 过期。
fn mode_hint(app: &App) -> Option<&'static str> {
    if app.mode == Mode::Plugin {
        return Some("-- 插件直通 -- Enter/空格 转发 · Esc 退出");
    }
    if app.visual.is_some() {
        return Some(if app.tab == Tab::Library {
            "-- VISUAL -- 移动扩选 · a/A 加入播放列表（选目标）· Esc 退出"
        } else {
            "-- VISUAL -- 已选中当前行 · 移动扩选 · d 剪切 / y 复制"
        });
    }
    None
}

/// 键位速览随当前视图变化（在线画布与否由内容插件能力决定）。
fn keys_for(tab: &Tab, online: bool) -> &'static [(&'static str, &'static str)] {
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
            ("A", "加入当前列表"),
            ("v", "多选"),
            ("/", "搜索"),
            (":", "命令"),
            ("?", "帮助"),
        ],
        Tab::Playlists => &[
            ("j/k", "移动"),
            ("Enter", "播放"),
            ("a", "新建"),
            ("c", "改名"),
            ("d", "删除"),
            ("w", "存队列"),
            (":", "命令"),
            ("?", "帮助"),
        ],
        // 插件画布：满足在线约定的给完整在线键位，否则给通用键位
        Tab::Plugin(_) | Tab::Plugins if online => {
            &[
                ("/", "搜索"),
                ("j/k", "移动"),
                ("d", "下载"),
                ("Enter", "下载"),
                ("x", "取消"),
                ("i", "直通"),
                ("r", "刷新"),
                ("?", "帮助"),
            ]
        }
        Tab::Plugin(_) | Tab::Plugins => &[
            ("j/k", "移动"),
            ("i", "按键直通"),
            ("Enter", "转发"),
            ("r", "刷新"),
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
        // 插件画布：底栏报插件声明的列表位置（不是画布行号）
        Tab::Plugin(_) | Tab::Plugins => match app.tab_ui(&app.tab).and_then(|ui| ui.list) {
            Some(list) if list.total > 0 => format!("{}/{}", list.index + 1, list.total),
            _ => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tui::app::VisualSel;
    use crate::client::tui::conn::Daemon;

    #[test]
    fn 模式提示跟随状态_退出即消失() {
        let mut app = App::new(Daemon::detached());
        assert_eq!(mode_hint(&app), None);

        app.mode = Mode::Plugin;
        assert!(mode_hint(&app).unwrap().contains("插件直通"));
        app.mode = Mode::Normal;
        assert_eq!(mode_hint(&app), None);

        app.visual = Some(VisualSel { anchor: 0 });
        assert!(mode_hint(&app).unwrap().contains("VISUAL"));
        app.visual = None;
        assert_eq!(mode_hint(&app), None);
    }
}
