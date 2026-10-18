//! 队列各列的横坐标与宽度。
//!
//! 曲名永远保留；宽度不够时先砍专辑、再砍艺术家，列之间至少留 2 格。

use ratatui::layout::Rect;

/// 各列的横坐标与宽度。窄终端里用不到的气列宽度为 0。
#[derive(Debug, Clone, Copy)]
pub struct Cols {
    pub index_x: u16,
    pub index_w: u16,
    pub title_x: u16,
    pub title_w: u16,
    pub artist_x: u16,
    pub artist_w: u16,
    pub album_x: u16,
    pub album_w: u16,
    pub dur_x: u16,
    pub dur_w: u16,
}

/// 按可用宽度算列：曲名永远保留，艺术家/专辑逐级让位。
pub fn columns(area: Rect) -> Cols {
    let right = area.right();
    let dur_w = 7.min(area.width.saturating_sub(10)).max(4);
    let dur_x = right.saturating_sub(dur_w);
    let index_x = area.x + 2;
    let index_w = 3;
    let title_x = index_x + index_w + 2;
    let meta_end = dur_x.saturating_sub(2);
    let span = meta_end.saturating_sub(title_x);

    let (title_w, artist_w, album_w) = if span >= 64 {
        let artist_w = span * 30 / 100;
        let album_w = span * 26 / 100;
        (
            span.saturating_sub(artist_w + album_w + 4),
            artist_w,
            album_w,
        )
    } else if span >= 36 {
        let artist_w = span * 38 / 100;
        (span.saturating_sub(artist_w + 2), artist_w, 0)
    } else {
        (span, 0, 0)
    };

    let artist_x = title_x + title_w + 2;
    let album_x = artist_x + artist_w + 2;
    Cols {
        index_x,
        index_w,
        title_x,
        title_w,
        artist_x,
        artist_w,
        album_x,
        album_w,
        dur_x,
        dur_w,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 窄终端不panic且列不重叠() {
        for width in 10..=200u16 {
            let area = Rect::new(0, 0, width, 1);
            let c = columns(area);
            assert!(c.title_x + c.title_w <= area.right().max(c.title_x));
            if c.artist_w > 0 {
                assert!(c.artist_x >= c.title_x + c.title_w);
            }
            if c.album_w > 0 {
                assert!(c.album_x >= c.artist_x + c.artist_w);
            }
            assert!(c.dur_x + c.dur_w <= area.right());
        }
    }
}
