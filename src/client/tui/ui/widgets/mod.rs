//! 渲染层共用零件。按职责分四个文件，这里只做统一出口：
//!
//! - [`paint`]：底色 / 1px 线 / 色块进度条 / 行底
//! - [`text`]：CJK 安全的截断、补白、输入行
//! - [`popup`]：浮层骨架（投影、描边、标题行、提示行）
//! - [`hints`]：键位速览、空态
//!
//! 约定：所有函数只画，不改 [`App`](crate::client::tui::app::App) 状态；
//! 尺寸一律 saturating 运算，窄到离谱的终端也只是画得丑，不会 panic。

mod hints;
mod paint;
mod popup;
mod text;

pub use hints::{empty_state, key_hints};
pub use paint::{bar, canvas, fill, list_row_bg, list_row_bg_visual, rule};
pub use popup::{POPUP_CHROME, popup};
pub use text::{fit, input_line, pad_right};
