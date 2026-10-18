//! 帮助浮层：左边键位、右边命令，窄终端退化成单列（j/k 滚动）。
//!
//! 两张表都来自 `keymap::HELP` 和 `cmd::SPECS`（单一事实来源），这里只排版。

use super::super::{cmd, keymap};
use super::theme::Theme;
use super::widgets::{POPUP_CHROME, key_hints, pad_right, popup};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// 单列布局的判定宽度（两列挤不下就改成上下排列）。
const TWO_COL_MIN: u16 = 58;
/// 键名列宽。
const KEY_W: u16 = 17;

pub fn draw(frame: &mut Frame, scroll: &mut usize) {
    let theme = Theme::get();
    // 帮助是一整页：先抹掉底下的列表，浮层才有“聚焦”的感觉
    let whole = frame.area();
    super::widgets::fill(frame.buffer_mut(), whole, theme.style_canvas());
    let keys = key_lines(theme);
    let cmds = cmd_lines(theme);

    let total = frame.area();
    let wide = total.width >= TWO_COL_MIN + 6;
    let needed = if wide {
        keys.len().max(cmds.len())
    } else {
        keys.len() + cmds.len() + 1
    } as u16;
    let height = (needed + POPUP_CHROME).min(total.height.saturating_sub(2).max(7));

    let p = popup(frame, theme, if wide { 88 } else { 84 }, height);
    if p.title.is_empty() {
        return;
    }

    // 标题 + 右下角的版本号
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "帮助",
            theme.style_accent_strong(),
        ))),
        Rect::new(p.title.x + 2, p.title.y, p.title.width.saturating_sub(4), 1),
    );
    let version = format!("rmus {}", env!("CARGO_PKG_VERSION"));
    let vw = version.chars().count() as u16;
    if p.title.width > vw + 20 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(version, theme.style_faint())))
                .alignment(ratatui::layout::Alignment::Right),
            Rect::new(p.title.right() - 2 - vw, p.title.y, vw, 1),
        );
    }

    // 滚动位置在这里夹紧并写回（app 只需要 +1/-1）
    let height_usize = p.body.height as usize;
    *scroll = (*scroll).min((needed as usize).saturating_sub(height_usize));
    let scroll = *scroll;
    if wide {
        let half = p.body.width / 2;
        let left = Rect::new(
            p.body.x + 2,
            p.body.y,
            half.saturating_sub(2),
            p.body.height,
        );
        let right = Rect::new(
            p.body.x + half + 2,
            p.body.y,
            p.body.width.saturating_sub(half + 4),
            p.body.height,
        );
        render_column(frame, left, &keys, scroll, height_usize);
        render_column(frame, right, &cmds, scroll, height_usize);
    } else {
        let mut all = keys;
        all.push(Line::default());
        all.extend(cmds);
        render_column(
            frame,
            Rect::new(
                p.body.x + 2,
                p.body.y,
                p.body.width.saturating_sub(4),
                p.body.height,
            ),
            &all,
            scroll,
            height_usize,
        );
    }

    if !p.hint.is_empty() {
        let hints = key_hints(
            theme,
            &[("j/k", "滚动"), ("Esc", "关闭")],
            p.hint.width.saturating_sub(4),
        );
        frame.render_widget(
            Paragraph::new(hints),
            Rect::new(p.hint.x + 3, p.hint.y, p.hint.width.saturating_sub(4), 1),
        );
    }
}

fn render_column(
    frame: &mut Frame,
    area: Rect,
    lines: &[Line<'static>],
    scroll: usize,
    height: usize,
) {
    if area.is_empty() {
        return;
    }
    let visible: Vec<Line<'static>> = lines.iter().skip(scroll).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn section(theme: &Theme, title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_owned(),
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    ))
}

fn key_lines(theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![section(theme, "键盘")];
    lines.extend(keymap::HELP.iter().map(|(key, desc)| {
        Line::from(vec![
            Span::styled(pad_right(key, KEY_W), theme.style_text()),
            Span::styled((*desc).to_owned(), theme.style_muted()),
        ])
    }));
    lines
}

fn cmd_lines(theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![section(theme, "命令（: 前缀）")];
    lines.extend(cmd::SPECS.iter().map(|spec| {
        let names = spec.names.join(" / ");
        Line::from(vec![
            Span::styled(pad_right(&names, KEY_W), theme.style_text()),
            Span::styled(spec.desc.to_owned(), theme.style_muted()),
        ])
    }));
    lines
}
