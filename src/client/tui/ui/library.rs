//! 音乐库视图：~/Music 全量索引平铺，正在播的那首带左侧强调条。
//!
//! Enter 插播（加队尾并跳过去），a 只入队不打断播放；
//! 曲目多时用 j/k / C-d/C-u 翻找，精确找还是 `/` 快。

use super::super::app::App;
use super::list::window;
use super::queue::{columns::columns, row};
use super::theme::Theme;
use super::widgets::{empty_state, list_row_bg, rule};
use ratatui::Frame;
use ratatui::layout::Rect;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = Theme::get();

    // 库是空的：整块留给空态（多半还没扫描过）
    if app.library.is_empty() {
        empty_state(
            frame,
            theme,
            area,
            "库是空的",
            "把音乐放进 ~/Music，然后按 R 重建索引",
        );
        return;
    }

    let cols = columns(area);

    let head = Rect::new(area.x, area.y, area.width, 1);
    row::draw_head(frame, theme, cols, head);
    if area.height >= 2 {
        rule(frame, theme, Rect::new(area.x, area.y + 1, area.width, 1));
    }
    let list = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    if list.is_empty() {
        return;
    }

    let playing_path = app
        .status
        .as_ref()
        .and_then(|s| s.track.as_ref())
        .map(|t| t.path.as_path());
    let height = list.height as usize;
    let selected = app.library_selected.min(app.library.len().saturating_sub(1));
    let first = window(&mut app.scroll_library, selected, app.library.len(), height);

    for (row_i, i) in (first..app.library.len()).take(height).enumerate() {
        let y = list.y + row_i as u16;
        let track = &app.library[i];
        let playing = playing_path.is_some_and(|p| p == track.path.as_path());
        let is_selected = i == selected;
        let row_area = Rect::new(list.x, y, list.width, 1);
        list_row_bg(frame.buffer_mut(), theme, row_area, is_selected, playing);
        row::draw_row(frame, theme, cols, row_area, track, playing, is_selected);
    }
}
