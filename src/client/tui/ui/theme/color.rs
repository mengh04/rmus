//! 终端色彩能力探测 + xterm-256 量化。

use ratatui::style::Color;

/// 终端是否支持 24 位色。
pub(super) fn want_truecolor() -> bool {
    if let Ok(raw) = std::env::var("RMUS_COLOR") {
        return matches!(raw.to_lowercase().as_str(), "truecolor" | "24bit" | "rgb");
    }
    if let Ok(v) = std::env::var("COLORTERM") {
        let v = v.to_lowercase();
        if v.contains("truecolor") || v.contains("24bit") {
            return true;
        }
    }
    // tmux / screen / Linux 控制台按 256 处理更稳
    !limited_term()
}

/// 终端明确要求不上色（NO_COLOR 规范）或根本不支持时，走单色表现。
pub(super) fn no_color() -> bool {
    if std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var("TERM").is_ok_and(|t| t == "dumb")
}

/// 256 色更稳的老终端（Linux 控制台、screen/tmux 透传）。
fn limited_term() -> bool {
    std::env::var("TERM")
        .is_ok_and(|t| t == "linux" || t.starts_with("screen") || t.starts_with("tmux"))
}

/// xterm-256 里最接近 (r,g,b) 的索引色。
pub(super) fn ansi256(r: u8, g: u8, b: u8) -> Color {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| -> usize {
        let v = i32::from(v);
        let mut best = 0;
        let mut best_d = i32::MAX;
        for (i, level) in LEVELS.iter().enumerate() {
            let d = (level - v).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    };
    let dist2 = |a: (i32, i32, i32), b: (i32, i32, i32)| -> i32 {
        let (dr, dg, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
        dr * dr + dg * dg + db * db
    };

    let (ri, gi, bi) = (near(r), near(g), near(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let d_cube = dist2(cube, (i32::from(r), i32::from(g), i32::from(b)));

    // 灰阶 232..=255：8 + 10*i
    let avg = (i32::from(r) + i32::from(g) + i32::from(b)) / 3;
    let gray_i = ((avg - 8).clamp(0, 230) / 10) as u8;
    let gray_v = 8 + 10 * i32::from(gray_i);
    let d_gray = dist2(
        (gray_v, gray_v, gray_v),
        (i32::from(r), i32::from(g), i32::from(b)),
    );

    if d_gray < d_cube {
        Color::Indexed(232 + gray_i)
    } else {
        Color::Indexed(16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tui::ui::theme::DARK;

    #[test]
    fn 量化落在合法范围() {
        let t = DARK.quantized();
        for c in [t.bg, t.surface, t.accent, t.cool, t.text] {
            match c {
                Color::Indexed(i) => assert!(i >= 16, "索引色应在 16..=255：{i}"),
                other => panic!("量化后不应再是 {other:?}"),
            }
        }
    }

    #[test]
    fn 深色与亮色区分得开() {
        let t = DARK.quantized();
        assert_ne!(t.bg, t.text);
        assert_ne!(t.accent, t.bg);
    }

    #[test]
    fn 纯黑纯白落在立方端点() {
        assert_eq!(ansi256(0, 0, 0), Color::Indexed(16));
        assert_eq!(ansi256(255, 255, 255), Color::Indexed(231));
    }
}
