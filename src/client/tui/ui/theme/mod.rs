//! 视觉主题：调色板 + 统一的样式取用口。
//!
//! 设计取向 —— 深夜听音室：墨蓝的底、暖琥珀的光（电子管/唱机氛围灯），
//! 冷钢蓝留给次要信息（时长、专辑）。画面自己铺底色，所以不管终端原本
//! 是深色还是浅色主题，成品都长一个样。
//!
//! 子模块分工：
//! - [`palette`]：具体色值，以及“量化到 256 色”“退化成单色”两种降级
//! - [`color`]：终端能力探测（24 位色 / NO_COLOR / 256 色）
//!
//! 环境变量：`RMUS_COLOR=truecolor|256` 强制色彩精度；`NO_COLOR=1` 走单色。

mod color;
mod palette;

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

use palette::DARK;

/// 一套调色板。字段名即语义，不要按具体颜色理解（浅色主题会换值）。
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// 画布底色
    pub bg: Color,
    /// 比画布更深：浮层投影、凹陷区
    pub sunken: Color,
    /// 抬起的表面：浮层、选中行
    pub surface: Color,
    /// 再抬一层：当前行、输入框
    pub raised: Color,
    /// 细分隔线（用背景色铺出来的 1px 线）
    pub line: Color,
    /// 正文
    pub text: Color,
    /// 次要信息
    pub muted: Color,
    /// 装饰、占位、禁用
    pub faint: Color,
    /// 主强调色：正在播放、选中、焦点
    pub accent: Color,
    /// 强调色的暗部：徽标底、装饰线
    pub accent_dim: Color,
    /// 冷色：时长、专辑等并置信息
    pub cool: Color,
    pub danger: Color,
    pub ok: Color,
    /// NO_COLOR / TERM=dumb：颜色会被终端层抹掉，改用反显和字形表达层级
    pub mono: bool,
}

impl Theme {
    /// 全局唯一实例（首次调用时读环境变量决定色彩精度）。
    pub fn get() -> &'static Theme {
        static THEME: OnceLock<Theme> = OnceLock::new();
        THEME.get_or_init(|| {
            let base = if color::want_truecolor() {
                DARK
            } else {
                DARK.quantized()
            };
            if color::no_color() {
                base.monoized()
            } else {
                base
            }
        })
    }

    // ---- 前景样式 ---------------------------------------------------------

    pub fn style_text(&self) -> Style {
        Style::new().fg(self.text)
    }

    pub fn style_strong(&self) -> Style {
        Style::new().fg(self.text).bold()
    }

    pub fn style_muted(&self) -> Style {
        Style::new().fg(self.muted)
    }

    pub fn style_faint(&self) -> Style {
        Style::new().fg(self.faint)
    }

    pub fn style_accent(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn style_accent_strong(&self) -> Style {
        Style::new().fg(self.accent).bold()
    }

    pub fn style_cool(&self) -> Style {
        Style::new().fg(self.cool)
    }

    pub fn style_danger(&self) -> Style {
        Style::new().fg(self.danger)
    }

    // ---- 背景样式 ---------------------------------------------------------

    /// 整块画布的底色。
    pub fn style_canvas(&self) -> Style {
        Style::new().bg(self.bg)
    }

    /// 浮层主体。
    pub fn style_surface(&self) -> Style {
        Style::new().bg(self.surface)
    }

    /// 浮层里再抬一层：选中行、输入区。
    pub fn style_raised(&self) -> Style {
        Style::new().bg(self.raised)
    }

    /// 细分隔线：铺一行背景色当 1px 线用（无颜色时退化成字形线）。
    pub fn style_rule(&self) -> Style {
        Style::new().bg(self.line)
    }

    /// 小块徽标/键帽：有颜色用底色，无颜色用反显。
    pub fn style_chip(&self, fg: Color, bg: Color) -> Style {
        let style = Style::new().fg(fg).bg(bg);
        if self.mono {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    }

    /// 选中/正在播放的整行底色；无颜色时靠反显区分。
    pub fn style_selected(&self) -> Style {
        if self.mono {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            self.style_raised()
        }
    }
}
