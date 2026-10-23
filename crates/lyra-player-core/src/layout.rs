//! Pixel geometry of the player for both panels. Every number is a position on
//! the panel itself, so the device backend only needs to place objects.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub const fn center_x(self) -> i32 {
        self.x + self.w / 2
    }

    pub const fn center_y(self) -> i32 {
        self.y + self.h / 2
    }

    pub const fn overlaps(self, other: Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub width: i32,
    pub height: i32,
    /// Pixel size of the font used for titles and the active lyric line.
    pub large_px: i32,
    /// Pixel size of the font used for everything else.
    pub small_px: i32,

    // Player page.
    pub back: Rect,
    pub mode: Rect,
    pub cover: Rect,
    pub title: Rect,
    pub artist: Rect,
    pub bar: Rect,
    pub elapsed: Rect,
    pub remaining: Rect,
    pub prev: Rect,
    pub play: Rect,
    pub next: Rect,
    pub volume_down: Rect,
    pub volume_up: Rect,
    /// The volume read-out: always visible on Band 11 (between the buttons),
    /// a transient label at the top of the Band 10 Pro page.
    pub volume: Rect,
    /// Centre line of the two page-indicator dots.
    pub dots_y: i32,

    // Lyrics page (relative to that page).
    pub lyrics_header: Rect,
    pub lyrics_view: Rect,
    pub lyric_pad_x: i32,
    pub lyric_gap: i32,
    /// Where the top of the active line rests inside the view.
    pub lyric_anchor: i32,
}

/// The song list page.
#[derive(Clone, Copy, Debug)]
pub struct ListMetrics {
    pub header: Rect,
    pub count: Rect,
    /// The "now playing" card under the header.
    pub card: Rect,
    /// The scrolling list below it, with the card (when shown) pushing it down.
    pub list_top: i32,
    pub row_height: i32,
    pub row_gap: i32,
    pub side: i32,
    pub accent: Rect,
    pub text_x: i32,
}

pub const LIST_BAND_11: ListMetrics = ListMetrics {
    header: Rect::new(16, 14, 120, 42),
    count: Rect::new(140, 22, 56, 34),
    card: Rect::new(10, 62, 192, 76),
    list_top: 64,
    row_height: 78,
    row_gap: 8,
    side: 10,
    accent: Rect::new(10, 20, 5, 38),
    text_x: 24,
};

pub const LIST_BAND_10_PRO: ListMetrics = ListMetrics {
    header: Rect::new(24, 12, 180, 46),
    count: Rect::new(210, 20, 100, 34),
    card: Rect::new(14, 62, 308, 80),
    list_top: 66,
    row_height: 84,
    row_gap: 8,
    side: 14,
    accent: Rect::new(12, 20, 5, 42),
    text_x: 28,
};

pub const BAND_11: Metrics = Metrics {
    width: 212,
    height: 520,
    large_px: 28,
    small_px: 24,
    back: Rect::new(14, 16, 40, 40),
    mode: Rect::new(158, 16, 40, 40),
    cover: Rect::new(18, 68, 176, 176),
    title: Rect::new(12, 252, 188, 40),
    artist: Rect::new(12, 292, 188, 34),
    bar: Rect::new(18, 336, 176, 6),
    elapsed: Rect::new(14, 346, 90, 34),
    remaining: Rect::new(108, 346, 90, 34),
    prev: Rect::new(12, 392, 56, 56),
    play: Rect::new(70, 384, 72, 72),
    next: Rect::new(144, 392, 56, 56),
    volume_down: Rect::new(14, 466, 40, 34),
    volume_up: Rect::new(158, 466, 40, 34),
    volume: Rect::new(58, 466, 96, 34),
    dots_y: 508,
    lyrics_header: Rect::new(0, 0, 212, 56),
    lyrics_view: Rect::new(0, 56, 212, 440),
    lyric_pad_x: 16,
    lyric_gap: 18,
    lyric_anchor: 56,
};

pub const BAND_10_PRO: Metrics = Metrics {
    width: 336,
    height: 480,
    large_px: 32,
    small_px: 24,
    back: Rect::new(20, 14, 44, 44),
    mode: Rect::new(272, 14, 44, 44),
    cover: Rect::new(80, 62, 176, 176),
    title: Rect::new(24, 248, 288, 44),
    artist: Rect::new(24, 292, 288, 38),
    bar: Rect::new(40, 340, 256, 6),
    elapsed: Rect::new(30, 350, 120, 34),
    remaining: Rect::new(186, 350, 120, 34),
    prev: Rect::new(58, 394, 60, 60),
    play: Rect::new(130, 386, 76, 76),
    next: Rect::new(218, 394, 60, 60),
    volume_down: Rect::new(10, 404, 40, 40),
    volume_up: Rect::new(286, 404, 40, 40),
    volume: Rect::new(24, 248, 288, 44),
    dots_y: 470,
    lyrics_header: Rect::new(0, 0, 336, 56),
    lyrics_view: Rect::new(0, 56, 336, 404),
    lyric_pad_x: 28,
    lyric_gap: 20,
    lyric_anchor: 64,
};

impl Metrics {
    /// MiSans spans 1.326 em (hhea ascender 1044, descender -282 per 1000
    /// units); one extra pixel for the rasterizer rounding each side up.
    pub const fn line_height(px: i32) -> i32 {
        (px * 1_326 + 999) / 1_000 + 1
    }

    pub const fn large_line(&self) -> i32 {
        Self::line_height(self.large_px)
    }

    pub const fn small_line(&self) -> i32 {
        Self::line_height(self.small_px)
    }

    /// The player page's own objects, which must not overlap each other.
    pub fn player_rects(&self) -> [Rect; 13] {
        [
            self.back,
            self.mode,
            self.cover,
            self.title,
            self.artist,
            self.bar,
            self.elapsed,
            self.remaining,
            self.prev,
            self.play,
            self.next,
            self.volume_down,
            self.volume_up,
        ]
    }

    pub const fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: [Metrics; 2] = [BAND_11, BAND_10_PRO];

    #[test]
    fn list_rows_fit_two_text_lines_and_the_panel() {
        for (metrics, list) in [(BAND_11, LIST_BAND_11), (BAND_10_PRO, LIST_BAND_10_PRO)] {
            assert!(list.row_height >= metrics.large_line() + metrics.small_line() + 6);
            assert!(list.card.bottom() <= metrics.height / 3);
            assert!(list.side * 2 + 160 <= metrics.width);
            assert!(list.header.h >= metrics.large_line());
            assert!(!list.header.overlaps(list.count));
            assert!(list.accent.bottom() <= list.row_height);
        }
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn every_player_object_fits_the_panel_without_overlap() {
        for metrics in BOTH {
            let rects = metrics.player_rects();
            for (index, rect) in rects.iter().enumerate() {
                assert!(inside(metrics.screen(), *rect), "{rect:?} is off the panel");
                for other in &rects[index + 1..] {
                    assert!(!rect.overlaps(*other), "{rect:?} overlaps {other:?}");
                }
            }
            assert!(!metrics.volume.overlaps(metrics.volume_down));
            assert!(!metrics.volume.overlaps(metrics.volume_up));
            assert!(inside(metrics.screen(), metrics.volume));
        }
    }

    #[test]
    fn labels_are_at_least_one_line_tall() {
        // LVGL's circular long mode scrolls *vertically* when the line is
        // taller than the box, so a short title in a short box would bounce.
        for metrics in BOTH {
            assert!(metrics.title.h >= metrics.large_line());
            assert!(metrics.artist.h >= metrics.small_line());
            assert!(metrics.elapsed.h >= metrics.small_line());
            assert!(metrics.remaining.h >= metrics.small_line());
        }
    }

    #[test]
    fn touch_targets_are_comfortable() {
        for metrics in BOTH {
            for rect in [metrics.prev, metrics.play, metrics.next] {
                assert!(rect.w >= 52 && rect.h >= 52);
            }
            for rect in [metrics.back, metrics.mode] {
                assert!(rect.w >= 40 && rect.h >= 40);
            }
            assert!(metrics.play.w > metrics.prev.w);
        }
    }

    #[test]
    fn controls_are_centred_and_the_cover_is_a_square() {
        for metrics in BOTH {
            assert_eq!(metrics.cover.w, metrics.cover.h);
            assert_eq!(metrics.cover.w, i32::from(crate::COVER_SIZE));
            assert_eq!(metrics.cover.center_x(), metrics.width / 2);
            assert_eq!(metrics.play.center_x(), metrics.width / 2);
            assert_eq!(
                metrics.play.center_x() - metrics.prev.center_x(),
                metrics.next.center_x() - metrics.play.center_x()
            );
        }
    }

    #[test]
    fn lyrics_view_fills_below_the_header() {
        for metrics in BOTH {
            assert_eq!(metrics.lyrics_header.bottom(), metrics.lyrics_view.y);
            assert!(metrics.lyrics_view.bottom() < metrics.dots_y - 3);
            assert!(metrics.lyric_anchor + metrics.large_line() < metrics.lyrics_view.h / 2);
        }
    }
}
