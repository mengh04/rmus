//! 在线视图：插件的搜索结果列表，下载进度直接画在时长列。
//!
//! 顶部一行显示当前关键词与来源插件，下面是结果表（结构沿用队列的
//! 列布局，来源标签挤在时长左边）；没有任何搜索时给空态引导按 `/`。

use super::super::app::{App, DownloadStatus};
use super::super::fmt_mmss;
use super::list::window;
use super::queue::columns::columns;
use super::theme::Theme;
use super::widgets::{empty_state, fit, list_row_bg, rule};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = Theme::get();

    // 没有可用插件：给安装指引
    let Some(plugin) = app.online_plugin.clone() else {
        empty_state(
            frame,
            theme,
            area,
            "在线搜索不可用",
            "未发现插件：把 rmus-dl 装入插件目录后重启 rmusd",
        );
        return;
    };

    let Some(online) = &app.online else {
        empty_state(
            frame,
            theme,
            area,
            "按 / 搜索在线音乐",
            &format!("结果可直接下载，完成后自动进入音乐库（插件 {}）", plugin.name),
        );
        return;
    };

    if online.songs.is_empty() {
        empty_state(
            frame,
            theme,
            area,
            &format!("“{}” 没有结果", online.query),
            "换个关键词试试",
        );
        return;
    }

    let cols = columns(area);

    // 第一行：关键词 + 来源插件
    let head_text = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("搜索 ", theme.style_faint()),
            Span::styled(online.query.clone(), theme.style_strong()),
            Span::styled(format!("  ·  插件 {}", plugin.name), theme.style_faint()),
        ])),
        head_text,
    );
    // 第二行：列标题（与队列/音乐库同款暗灰），第三行分隔线
    if area.height >= 2 {
        draw_head(frame, theme, cols, Rect::new(area.x, area.y + 1, area.width, 1));
    }
    if area.height >= 3 {
        rule(frame, theme, Rect::new(area.x, area.y + 2, area.width, 1));
    }
    let list = Rect {
        x: area.x,
        y: area.y + 3,
        width: area.width,
        height: area.height.saturating_sub(3),
    };
    if list.is_empty() {
        return;
    }

    let height = list.height as usize;
    let selected = online.selected.min(online.songs.len().saturating_sub(1));
    let mut scroll = app.scroll_online;
    let first = window(&mut scroll, selected, online.songs.len(), height);
    app.scroll_online = scroll;

    for (row_i, i) in (first..online.songs.len()).take(height).enumerate() {
        let y = list.y + row_i as u16;
        let song = &online.songs[i];
        let is_selected = i == selected;
        let download = download_of(app, song);
        let row_area = Rect::new(list.x, y, list.width, 1);
        list_row_bg(frame.buffer_mut(), theme, row_area, is_selected, false);
        draw_row(
            frame,
            theme,
            cols,
            row_area,
            i,
            song,
            download,
            is_selected,
        );
    }
}

/// 列标题行（暗灰），比队列多一个「来源」列。
fn draw_head(
    frame: &mut Frame,
    theme: &Theme,
    cols: super::queue::columns::Cols,
    area: Rect,
) {
    let mut put = |x: u16, w: u16, text: &str, align: Alignment| {
        if w == 0 {
            return;
        }
        let rect = Rect::new(x, area.y, w, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(fit(text, w), theme.style_faint())))
                .alignment(align),
            rect,
        );
    };
    put(cols.index_x, cols.index_w, "#", Alignment::Left);
    put(cols.title_x, cols.title_w, "曲目", Alignment::Left);
    if cols.artist_w > 0 {
        put(cols.artist_x, cols.artist_w, "艺术家", Alignment::Left);
    }
    if cols.album_w > 0 {
        put(cols.album_x, cols.album_w.saturating_sub(10), "专辑", Alignment::Left);
        let src_w = 8;
        let src_x = cols.dur_x.saturating_sub(src_w + 2);
        if src_x > cols.album_x {
            put(src_x, src_w, "来源", Alignment::Right);
        }
    }
    put(cols.dur_x, cols.dur_w, "时长", Alignment::Right);
}

/// 找这首歌对应的下载（界面里按 来源+song_id 对上）。
fn download_of<'a>(
    app: &'a App,
    song: &super::super::app::OnlineSong,
) -> Option<&'a super::super::app::DownloadState> {
    app.downloads
        .values()
        .find(|d| d.song.source == song.source && d.song.song_id == song.song_id)
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    frame: &mut Frame,
    theme: &Theme,
    cols: super::queue::columns::Cols,
    area: Rect,
    index: usize,
    song: &super::super::app::OnlineSong,
    download: Option<&super::super::app::DownloadState>,
    selected: bool,
) {
    let title_style = if selected {
        theme.style_strong()
    } else {
        theme.style_text()
    };

    let mut put = |x: u16, w: u16, span: Span<'static>, align: Alignment| {
        if w == 0 {
            return;
        }
        let rect = Rect::new(x, area.y, w, 1);
        frame.render_widget(Paragraph::new(Line::from(span)).alignment(align), rect);
    };

    put(
        cols.index_x,
        cols.index_w,
        Span::styled(
            format!("{:>w$}", index + 1, w = cols.index_w as usize),
            theme.style_faint(),
        ),
        Alignment::Left,
    );
    put(
        cols.title_x,
        cols.title_w,
        Span::styled(fit(&song.title, cols.title_w), title_style),
        Alignment::Left,
    );
    if cols.artist_w > 0 {
        put(
            cols.artist_x,
            cols.artist_w,
            Span::styled(fit(&song.artist, cols.artist_w), theme.style_muted()),
            Alignment::Left,
        );
    }
    if cols.album_w > 0 {
        // 右侧要给来源标签留位置：专辑文字宽度让出 10 列
        let album_w = cols.album_w.saturating_sub(10);
        put(
            cols.album_x,
            album_w,
            Span::styled(fit(&song.album, album_w), theme.style_faint()),
            Alignment::Left,
        );
    }

    // 时长列：下载中换成百分比，完成 ✓，失败 ✗
    let (right, style) = match download {
        Some(d) => match &d.status {
            DownloadStatus::Running if d.received == 0 => {
                // 直链解析中 / 首块未到：还没有可报告的字节
                ("…".to_owned(), theme.style_accent())
            }
            DownloadStatus::Running => {
                let pct = match d.total {
                    Some(total) if total > 0 => {
                        format!("{}%", d.received * 100 / total)
                    }
                    _ => fit_bytes(d.received),
                };
                (pct, theme.style_accent())
            }
            DownloadStatus::Done => {
                ("✓".to_owned(), Style::new().fg(theme.ok).add_modifier(Modifier::BOLD))
            }
            DownloadStatus::Failed(_) => {
                ("✗".to_owned(), Style::new().fg(theme.danger).add_modifier(Modifier::BOLD))
            }
        },
        None => (
            if song.duration_ms > 0 {
                fmt_mmss(song.duration_ms)
            } else {
                "--:--".to_owned()
            },
            theme.style_faint(),
        ),
    };
    put(cols.dur_x, cols.dur_w, Span::styled(right, style), Alignment::Right);

    // 来源标签挤在时长左边；专辑文字相应让位，避免重叠
    if cols.album_w > 0 {
        let src_w = 8;
        let src_x = cols.dur_x.saturating_sub(src_w + 2);
        if src_x > cols.album_x {
            put(
                src_x,
                src_w,
                Span::styled(
                    fit(&song.source, src_w),
                    Style::new().fg(theme.cool).add_modifier(Modifier::DIM),
                ),
                Alignment::Right,
            );
        }
    }
}

/// 总大小未知时显示已收字节数（人类可读）。
fn fit_bytes(received: u64) -> String {
    let (value, unit) = match received {
        n if n >= 1 << 20 => (n as f64 / (1 << 20) as f64, "MiB"),
        n if n >= 1 << 10 => (n as f64 / (1 << 10) as f64, "KiB"),
        n => (n as f64, "B"),
    };
    format!("{value:.1}{unit}")
}
