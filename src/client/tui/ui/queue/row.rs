//! 表头与单行曲目的绘制。

use super::super::super::fmt_mmss;
use super::super::theme::Theme;
use super::super::widgets::fit;
use super::columns::Cols;
use crate::proto::TrackInfo;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn draw_head(frame: &mut Frame, theme: &Theme, cols: Cols, area: Rect) {
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
        put(cols.album_x, cols.album_w, "专辑", Alignment::Left);
    }
    put(cols.dur_x, cols.dur_w, "时长", Alignment::Right);
}

pub fn draw_row(
    frame: &mut Frame,
    theme: &Theme,
    cols: Cols,
    area: Rect,
    track: &TrackInfo,
    playing: bool,
    selected: bool,
) {
    let title_style = if playing {
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else if selected {
        theme.style_strong()
    } else {
        theme.style_text()
    };
    let index_style = if playing {
        Style::new().fg(theme.accent_dim)
    } else {
        theme.style_faint()
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
            format!("{:>w$}", track.index + 1, w = cols.index_w as usize),
            index_style,
        ),
        Alignment::Left,
    );
    put(
        cols.title_x,
        cols.title_w,
        Span::styled(fit(&track.title, cols.title_w), title_style),
        Alignment::Left,
    );
    if cols.artist_w > 0 {
        put(
            cols.artist_x,
            cols.artist_w,
            Span::styled(fit(&track.artist, cols.artist_w), theme.style_muted()),
            Alignment::Left,
        );
    }
    if cols.album_w > 0 {
        put(
            cols.album_x,
            cols.album_w,
            Span::styled(fit(&track.album, cols.album_w), theme.style_faint()),
            Alignment::Left,
        );
    }
    put(
        cols.dur_x,
        cols.dur_w,
        Span::styled(
            if track.duration_ms > 0 {
                fmt_mmss(track.duration_ms)
            } else {
                "--:--".to_owned()
            },
            theme.style_faint(),
        ),
        Alignment::Right,
    );
}
