//! 具体色值与两套降级：量化到 xterm-256、退化到单色。

use super::Theme;
use super::color::ansi256;
use ratatui::style::Color;

/// 默认（也是唯一）深色主题：墨蓝三层底色 + 暖琥珀强调 + 冷钢蓝次要信息。
pub const DARK: Theme = Theme {
    bg: Color::Rgb(0x10, 0x11, 0x18),
    sunken: Color::Rgb(0x07, 0x08, 0x0c),
    surface: Color::Rgb(0x1a, 0x1d, 0x27),
    raised: Color::Rgb(0x2b, 0x2f, 0x3e),
    line: Color::Rgb(0x30, 0x34, 0x44),
    text: Color::Rgb(0xe8, 0xe7, 0xef),
    muted: Color::Rgb(0x9a, 0x9c, 0xae),
    faint: Color::Rgb(0x6b, 0x6f, 0x85),
    accent: Color::Rgb(0xf0, 0xb8, 0x60),
    accent_dim: Color::Rgb(0x7d, 0x5e, 0x30),
    cool: Color::Rgb(0x8f, 0xb0, 0xce),
    danger: Color::Rgb(0xe8, 0x78, 0x8a),
    ok: Color::Rgb(0x7c, 0xc9, 0xa2),
    mono: false,
};

impl Theme {
    /// 把 24 位色量化到 xterm-256（取色立方/灰阶里更近的一个）。
    pub(super) fn quantized(self) -> Theme {
        let q = |c: Color| match c {
            Color::Rgb(r, g, b) => ansi256(r, g, b),
            other => other,
        };
        Theme {
            bg: q(self.bg),
            sunken: q(self.sunken),
            surface: q(self.surface),
            raised: q(self.raised),
            line: q(self.line),
            text: q(self.text),
            muted: q(self.muted),
            faint: q(self.faint),
            accent: q(self.accent),
            accent_dim: q(self.accent_dim),
            cool: q(self.cool),
            danger: q(self.danger),
            ok: q(self.ok),
            mono: self.mono,
        }
    }

    /// 退化到"无色"：所有颜色归零，层级全靠加粗/反显/字形表达。
    ///
    /// 关键点：必须真的把颜色设成 `Reset`。crossterm 在 NO_COLOR 下会把颜色
    /// 序列写成空的 `ESC[;m`（终端按 SGR 0 解析），反而会把加粗/反显一起清掉；
    /// 颜色全是 Reset 时它干脆不发颜色序列，属性和字形才能活下来。
    pub(super) fn monoized(self) -> Theme {
        Theme {
            bg: Color::Reset,
            sunken: Color::Reset,
            surface: Color::Reset,
            raised: Color::Reset,
            line: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            faint: Color::Reset,
            accent: Color::Reset,
            accent_dim: Color::Reset,
            cool: Color::Reset,
            danger: Color::Reset,
            ok: Color::Reset,
            mono: true,
        }
    }
}
