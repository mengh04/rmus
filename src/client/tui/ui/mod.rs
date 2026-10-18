//! 渲染层：把 `App` 的状态画成一帧。这里不做任何 IO / 请求。
//!
//! 页面骨架（`draw` 只负责编排，具体绘制都在子模块）：
//! ```text
//!  rmus    1 队列 23   2 播放列表 4   · 深夜循环           播放中  循环 列表  音量 ▬▬▬ 70%
//!
//!  #  曲目                        艺术家          专辑          时长
//!  ───────────────────────────────────────────────────────────────────────
//!    1  晚风                        王菲            执迷不悔       4:12
//!  ▌ 2  红豆                        王菲            唱游           4:36
//!    3  流年                        王菲            王菲           4:40
//!  ───────────────────────────────────────────────────────────────────────
//!  红豆  王菲  唱游                                          2/23
//!  01:23  ████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░      04:36
//!   j/k 移动   Enter 播放   / 搜索   : 命令   ? 帮助       共 23 首 · 1:12:03
//! ```
//!
//! 子模块：
//! - [`theme`] / [`widgets`]：调色板与共用零件（两者都有自己的子模块）
//! - [`header`] / [`queue`] / [`library`] / [`playlists`] / [`now_playing`] / [`status`]：页面各区块
//! - [`search`] / [`palette`] / [`help`]：浮层
//! - [`list`]：列表视口的滚动窗口
//!
//! 缩放策略：窄了先丢次要列（专辑 → 艺术家），矮了先丢留白。

mod header;
mod help;
mod library;
mod list;
mod now_playing;
mod online;
mod palette;
mod playlists;
mod queue;
mod search;
mod status;
mod theme;
mod widgets;

#[cfg(test)]
mod tests;

use super::app::{App, Tab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use theme::Theme;
use widgets::{canvas, rule};

/// 页面左右留白：顶栏、内容、底栏共用，视觉上对齐成一条竖线。
pub(super) const PAGE_MARGIN: u16 = 2;

/// 一帧的纵向分区。
struct Areas {
    top_bar: Rect,
    content: Rect,
    divider: Rect,
    now: Rect,
    hints: Rect,
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.tick(); // 顺手清掉过期的底部消息

    let theme = Theme::get();
    canvas(frame, theme);
    let area = frame.area();
    if area.width < 16 || area.height < 8 {
        return;
    }

    // 没在放东西时，底部整条让给列表（只留一行键位提示）
    let has_track = app.status.as_ref().and_then(|s| s.track.as_ref()).is_some();
    let areas = layout(area, has_track);

    header::draw(frame, app, areas.top_bar);
    match app.tab {
        Tab::Queue => queue::draw(frame, app, page_inset(areas.content)),
        Tab::Library => library::draw(frame, app, page_inset(areas.content)),
        Tab::Playlists => playlists::draw(frame, app, page_inset(areas.content)),
        Tab::Online => online::draw(frame, app, page_inset(areas.content)),
    }

    rule(frame, theme, page_inset(areas.divider));
    now_playing::draw(frame, app, page_inset(areas.now));
    status::draw(frame, app, theme, page_inset(areas.hints));

    draw_overlays(frame, app);
}

/// 竖向分区：矮终端先砍顶栏上下留白，没歌时再砍底部留白。
fn layout(area: Rect, has_track: bool) -> Areas {
    let compact = area.height < 17;
    let [_top_pad, top_bar, _gap, content, divider, now, hints] = Layout::vertical([
        Constraint::Length(if compact { 0 } else { 1 }),
        Constraint::Length(1),
        Constraint::Length(if compact { 0 } else { 1 }),
        Constraint::Min(3),
        Constraint::Length(u16::from(has_track)),
        Constraint::Length(if has_track {
            if compact { 1 } else { 2 }
        } else {
            0
        }),
        Constraint::Length(1),
    ])
    .areas(area);
    Areas {
        top_bar,
        content,
        divider,
        now,
        hints,
    }
}

/// 页面左右留白，所有内容都对齐到这条竖线。
fn page_inset(area: Rect) -> Rect {
    let margin = PAGE_MARGIN.min(area.width / 2);
    Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin * 2),
        height: area.height,
    }
}

/// 浮层最后画（盖在主体上）。
fn draw_overlays(frame: &mut Frame, app: &mut App) {
    let playing = app
        .status
        .as_ref()
        .and_then(|s| s.track.as_ref())
        .map(|t| t.path.as_path());
    if let Some(search) = &app.search {
        search::draw(frame, search, playing);
    }
    if let Some(palette) = &app.palette {
        palette::draw(frame, palette);
    }
    if app.help_open {
        help::draw(frame, &mut app.help_scroll);
    }
}
