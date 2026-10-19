//! 插件视图：UI 插件推送的渲染行序列（在线 tab = rmus-dl 画布；插件 tab = 演示）。
//!
//! 数据来自 `Event::PluginView`（daemon 拉插件 render 的输出）；样式
//! 令牌映射到宿主主题，插件 UI 自动跟随主题配色。`Mode::Plugin`（按键
//! 直通）时底部给一条提示线。

use super::super::app::{App, Tab};
use super::theme::Theme;
use super::widgets::{empty_state, fit, list_row_bg};
use crate::proto::PluginSpan;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// 语义样式令牌 -> 宿主主题。未认识的令牌一律落回正文色。
fn token_style(theme: &Theme, token: &str) -> Style {
    match token {
        "strong" => theme.style_strong(),
        "faint" | "muted" => theme.style_faint(),
        "accent" => theme.style_accent(),
        "danger" => theme.style_danger(),
        "ok" => Style::new().fg(theme.ok),
        "ok_bold" => Style::new().fg(theme.ok).add_modifier(Modifier::BOLD),
        "danger_bold" => theme.style_danger().add_modifier(Modifier::BOLD),
        "cool" => Style::new().fg(theme.cool).add_modifier(Modifier::DIM),
        "rule" => theme.style_rule(),
        "ok_bg" => {
            if theme.mono {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new().fg(theme.bg).bg(theme.ok)
            }
        }
        _ => theme.style_text(),
    }
}

fn span_line(theme: &Theme, spans: &[PluginSpan], width: u16) -> Line<'static> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    let mut used: u16 = 0;
    for span in spans {
        if used >= width {
            break;
        }
        let text = fit(&span.text, width - used);
        used += text.chars().count() as u16;
        out.push(Span::styled(text, token_style(theme, &span.style)));
    }
    Line::from(out)
}

fn view_of(app: &App) -> Option<(String, Vec<crate::proto::PluginLine>, usize)> {
    // 插件画布（注册 tab / 「插件」面板）统一从分槽缓存取当前插件的视图
    let plugin = app.plugin_of_tab(&app.tab)?;
    let ui = app.plugin_uis.get(&plugin)?;
    Some((plugin, ui.view.clone(), ui.selected))
}

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = Theme::get();

    let Some((_plugin, view, tab_selected)) = view_of(app) else {
        let (title, hint) = match &app.tab {
            Tab::Plugin(pt) => (
                format!("{} 不可用", pt.title),
                "插件缺失或未加载（把编译好的 .wasm 放入插件目录后重启 rmusd）",
            ),
            _ => (
                "没有 UI 插件".into(),
                "把编译好的 .wasm 插件放入插件目录后重启 rmusd",
            ),
        };
        empty_state(frame, theme, area, &title, hint);
        return;
    };

    // 视图区：整个内容区给插件画布（键位提示在底部状态栏，不占画布）
    let body = area;
    if body.height == 0 || view.is_empty() {
        return;
    }
    let height = body.height as usize;
    let width = body.width;
    // 画布行数由插件按 rows 控制（滚动/窗口在插件内完成），TUI 全量投影
    for (row, index) in (0..view.len()).take(height).enumerate() {
        let rect = Rect::new(body.x, body.y + row as u16, body.width, 1);
        if index == tab_selected {
            list_row_bg(frame.buffer_mut(), theme, rect, true, false);
        }
        let line = span_line(theme, &view[index].spans, width);
        frame.render_widget(Paragraph::new(line), rect);
    }
}

