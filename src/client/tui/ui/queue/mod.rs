//! 队列视图：一列曲目，正在播放的一首带左侧强调条。
//!
//! 列宽按终端宽度分三档（完整 / 去掉专辑 / 只留曲名），
//! 所有文字都按显示宽度截断，中文不会把列挤歪。

mod columns;
mod row;

use super::super::app::App;
use super::list::window;
use super::theme::Theme;
use super::widgets::{empty_state, list_row_bg, rule};
use columns::columns;
use ratatui::Frame;
use ratatui::layout::Rect;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = Theme::get();

    // 空队列：不画表头，整块留给空态
    if app.queue.is_empty() {
        empty_state(
            frame,
            theme,
            area,
            "队列是空的",
            "按 / 搜索音乐库，或 :add <路径> 加入文件",
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

    let current = app.status.as_ref().and_then(|s| s.current);
    let height = list.height as usize;
    let selected = app.queue_selected.min(app.queue.len().saturating_sub(1));
    let first = window(&mut app.scroll_queue, selected, app.queue.len(), height);

    for (row, i) in (first..app.queue.len()).take(height).enumerate() {
        let y = list.y + row as u16;
        let track = &app.queue[i];
        let playing = current == Some(track.index);
        let is_selected = i == selected;
        let row_area = Rect::new(list.x, y, list.width, 1);
        list_row_bg(frame.buffer_mut(), theme, row_area, is_selected, playing);
        row::draw_row(frame, theme, cols, row_area, track, playing, is_selected);
    }
}
