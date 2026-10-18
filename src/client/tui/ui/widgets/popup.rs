//! 浮层骨架：投影 → 1px 描边 → 抬起的底 → 标题行 + 细分隔线 →
//! 主体 → 细分隔线 + 提示行。
//!
//! 不用 box-drawing 边框字符：描边和分隔线都是“背景色铺出来的 1px 线”，
//! 窄终端、CJK 终端都不会错位；投影让浮层从画布里浮起来。

use super::super::theme::Theme;
use super::paint::{fill, fill_glyph, rule};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// 浮层各块区域。
pub struct Popup {
    /// 标题/输入行（1 行）
    pub title: Rect,
    /// 主体（列表）区域
    pub body: Rect,
    /// 底部提示行（1 行）
    pub hint: Rect,
}

/// 浮层的固定开销：描边 2 + 上下留白 2 + 标题 1 + 两条线 2 + 提示 1。
pub const POPUP_CHROME: u16 = 8;

/// 画浮层骨架，返回各块区域。`height` 是含描边与留白的总高度。
pub fn popup(frame: &mut Frame, theme: &Theme, width_pct: u16, height: u16) -> Popup {
    let total = frame.area();
    let area = centered(total, width_pct, height);

    // 投影：右下各偏移 1 格铺更深一层的底色，浮层就有了“厚度”
    let shadow = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width,
        height: area.height,
    };
    fill(frame.buffer_mut(), shadow, Style::new().bg(theme.sunken));

    // 1px 描边：整块先铺线色，再内缩一格铺面
    if theme.mono {
        // 颜色被抹掉时，用点阵描边把浮层从背景里分出来
        fill_glyph(frame.buffer_mut(), area, "░", Style::new().fg(theme.faint));
    } else {
        fill(frame.buffer_mut(), area, Style::new().bg(theme.line));
    }
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.is_empty() {
        return Popup {
            title: Rect::default(),
            body: Rect::default(),
            hint: Rect::default(),
        };
    }
    fill(frame.buffer_mut(), inner, theme.style_surface());

    let padded = inner.height >= 8;
    let pad = u16::from(padded);

    let title = Rect::new(inner.x, inner.y + pad, inner.width, 1);
    let top_rule = Rect::new(inner.x, title.y + 1, inner.width, 1);
    let hint = Rect::new(inner.x, inner.bottom() - 1 - pad, inner.width, 1);
    let bottom_rule = Rect::new(inner.x, hint.y.saturating_sub(1), inner.width, 1);

    let body_top = top_rule.y + 1;
    let body_bottom = bottom_rule.y;
    let body = Rect::new(
        inner.x,
        body_top,
        inner.width,
        body_bottom.saturating_sub(body_top),
    );

    rule(frame, theme, top_rule);
    if !body.is_empty() {
        rule(frame, theme, bottom_rule);
    }
    Popup { title, body, hint }
}

/// 居中、带上限的浮层区域。终端太小时至少留出 1 行 1 列。
pub fn centered(total: Rect, width_pct: u16, height: u16) -> Rect {
    let want = ((total.width as u32 * width_pct as u32 / 100) as u16).max(40);
    let w = want
        .min(total.width.saturating_sub(2).max(1))
        .min(total.width.max(1));
    let h = height
        .min(total.height.saturating_sub(2).max(1))
        .min(total.height.max(1));
    Rect {
        x: total.x + (total.width.saturating_sub(w)) / 2,
        y: total.y + (total.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
