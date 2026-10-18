//! 搜索浮层：输入即过滤（daemon 侧模糊匹配），Enter 插播。
//!
//! 输入行就是浮层标题：`/ 关键词`，光标交给终端画。

use super::super::{app::SearchState, fmt_mmss};
use super::theme::Theme;
use super::widgets::{POPUP_CHROME, empty_state, fit, input_line, key_hints, list_row_bg, popup};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// 结果列表最多展示的行数（超出靠滚动跟随选中项）。
const MAX_ROWS: u16 = 12;

pub fn draw(frame: &mut Frame, search: &SearchState, playing: Option<&std::path::Path>) {
    let theme = Theme::get();
    let rows = (search.results.len() as u16).clamp(1, MAX_ROWS);
    let p = popup(frame, theme, 62, rows + POPUP_CHROME);
    if p.title.is_empty() {
        return;
    }

    // ---- 标题行 = 输入行 --------------------------------------------------
    let meta = if search.query.trim().is_empty() {
        "输入关键词".to_owned()
    } else {
        format!("{} 个结果", search.results.len())
    };
    let meta_w = meta.width() as u16;
    let room_for_meta = p.title.width > meta_w + 24;
    let input_w = p
        .title
        .width
        .saturating_sub(6)
        .saturating_sub(if room_for_meta { meta_w + 2 } else { 0 });
    let input = Rect::new(p.title.x + 2, p.title.y, input_w, 1);
    let (cx, cy) = input_line(frame, theme, input, "/", &search.query);
    frame.set_cursor_position((cx, cy));
    if room_for_meta {
        let meta_area = Rect::new(p.title.right() - 2 - meta_w, p.title.y, meta_w, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(meta, theme.style_faint())))
                .alignment(Alignment::Right),
            meta_area,
        );
    }

    // ---- 结果 -------------------------------------------------------------
    if search.results.is_empty() {
        if search.query.trim().is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "歌名 / 歌手 / 专辑，支持拼音与首字母",
                    theme.style_muted(),
                )))
                .alignment(Alignment::Center),
                Rect::new(p.body.x, p.body.y + p.body.height / 2, p.body.width, 1),
            );
        } else {
            empty_state(
                frame,
                theme,
                p.body,
                "没有匹配的曲目",
                "换个关键词试试，Esc 关闭",
            );
        }
    } else {
        let height = p.body.height as usize;
        let selected = search.selected.min(search.results.len().saturating_sub(1));
        let first = if selected >= height {
            selected + 1 - height
        } else {
            0
        };
        for (row, i) in (first..search.results.len()).take(height).enumerate() {
            let y = p.body.y + row as u16;
            let track = &search.results[i];
            let row_area = Rect::new(p.body.x, y, p.body.width, 1);
            let is_selected = i == selected;
            let is_playing = playing.is_some_and(|path| path == track.path.as_path());
            list_row_bg(frame.buffer_mut(), theme, row_area, is_selected, is_playing);
            draw_result_row(frame, theme, row_area, track, is_selected);
        }
    }

    // ---- 底部键位 ---------------------------------------------------------
    if !p.hint.is_empty() {
        let hints = key_hints(
            theme,
            &[("↑↓", "选择"), ("Enter", "插播"), ("Esc", "关闭")],
            p.hint.width.saturating_sub(4),
        );
        frame.render_widget(
            Paragraph::new(hints),
            Rect::new(p.hint.x + 3, p.hint.y, p.hint.width.saturating_sub(4), 1),
        );
    }
}

fn draw_result_row(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    track: &crate::proto::TrackInfo,
    selected: bool,
) {
    let x = area.x + 2;
    let right = area.right().saturating_sub(2);
    let dur_w = 7u16.min(area.width.saturating_sub(12)).max(4);
    let dur_x = right.saturating_sub(dur_w);
    // 宽浮层里补上专辑列，定位同一首歌的不同版本更省事
    let album_w = if area.width >= 84 {
        (area.width as u32 * 26 / 100) as u16
    } else {
        0
    };
    let album_x = dur_x.saturating_sub(2 + album_w);
    let artist_w = if area.width >= 60 {
        (area.width as u32 * 24 / 100) as u16
    } else {
        0
    };
    let artist_x = album_x.saturating_sub(2 + artist_w);
    let title_w = artist_x.saturating_sub(2 + x);

    let title_style = if selected {
        theme.style_strong()
    } else {
        theme.style_text()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            fit(&track.title, title_w),
            title_style,
        ))),
        Rect::new(x, area.y, title_w, 1),
    );
    if artist_w > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit(&track.artist, artist_w),
                theme.style_muted(),
            ))),
            Rect::new(artist_x, area.y, artist_w, 1),
        );
    }
    if album_w > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit(&track.album, album_w),
                theme.style_faint(),
            ))),
            Rect::new(album_x, area.y, album_w, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            fmt_mmss(track.duration_ms),
            Style::new().fg(theme.faint),
        )))
        .alignment(Alignment::Right),
        Rect::new(dur_x, area.y, dur_w, 1),
    );
}
