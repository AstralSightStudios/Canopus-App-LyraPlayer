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

/// The artwork's drop shadow: blur width and how far it drops. The device shrinks the shadow by
/// the drop so its solid core stays under the artwork.
pub const COVER_SHADOW_WIDTH: i32 = 36;
pub const COVER_SHADOW_DROP: i32 = 10;

/// How far below the artwork LVGL draws for its shadow (`lv_obj_calculate_ext_draw_size`: half
/// the width plus one, plus the spread and the drop, which cancel). Anything that redraws inside
/// that band redraws the shadow too, and the firmware computes a shadow from scratch every time,
/// so the scrolling title must start below it.
pub const fn cover_shadow_reach() -> i32 {
    COVER_SHADOW_WIDTH / 2 + 1
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
    /// The volume read-out: on Band 11 a level bar centred in this slot between
    /// the buttons, on Band 10 Pro a transient label over the title.
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

/// The song list page: the title, the rows and the song count scroll as one column; the
/// now-playing bar floats over its bottom edge.
#[derive(Clone, Copy, Debug)]
pub struct ListMetrics {
    /// The large title, at the top of the column.
    pub header: Rect,
    /// Top of the first row inside the column.
    pub first_row: i32,
    pub row_height: i32,
    /// Horizontal inset of a row (and of its pressed highlight) from the panel edge.
    pub side: i32,
    /// The artwork inside a row; the watch builds list thumbnails at exactly this edge.
    pub thumb: Rect,
    pub thumb_radius: i32,
    /// Where the title line starts inside a row; the artist line follows it.
    pub text_x: i32,
    pub text_y: i32,
    /// The floating now-playing bar, on the panel.
    pub mini: Rect,
    /// Its artwork and play/pause button, inside the bar.
    pub mini_thumb: Rect,
    pub mini_button: Rect,
    /// Height of the shuffle and loop buttons above a collection's songs, which then start
    /// `action_h + ACTION_GAP` lower than the home page's rows.
    pub action_h: i32,
}

/// Space between the play buttons and the first song of a collection.
pub const ACTION_GAP: i32 = 10;
/// Space between the two play buttons.
pub const ACTION_SPACING: i32 = 8;

pub const LIST_BAND_11: ListMetrics = ListMetrics {
    header: Rect::new(16, 14, 180, 42),
    first_row: 62,
    row_height: 72,
    side: 6,
    thumb: Rect::new(8, 10, 52, 52),
    thumb_radius: 8,
    text_x: 70,
    text_y: 3,
    mini: Rect::new(8, 436, 196, 68),
    mini_thumb: Rect::new(8, 8, 52, 52),
    mini_button: Rect::new(152, 14, 40, 40),
    action_h: 44,
};

pub const LIST_BAND_10_PRO: ListMetrics = ListMetrics {
    header: Rect::new(24, 12, 200, 46),
    first_row: 64,
    row_height: 80,
    side: 10,
    thumb: Rect::new(10, 10, 60, 60),
    thumb_radius: 9,
    text_x: 84,
    text_y: 7,
    mini: Rect::new(12, 392, 312, 76),
    mini_thumb: Rect::new(8, 8, 60, 60),
    mini_button: Rect::new(252, 12, 52, 52),
    action_h: 48,
};

impl ListMetrics {
    pub const fn row_width(&self, metrics: &Metrics) -> i32 {
        metrics.width - 2 * self.side
    }

    /// Where the artist line starts inside a row: the lines share one pixel of leading.
    pub const fn artist_y(&self, metrics: &Metrics) -> i32 {
        self.text_y + metrics.small_line() - 1
    }

    /// Width of one of the two play buttons.
    pub const fn action_width(&self, metrics: &Metrics) -> i32 {
        (self.row_width(metrics) - ACTION_SPACING) / 2
    }

    /// Top of the first song of a collection page.
    pub const fn songs_top(&self) -> i32 {
        self.first_row + self.action_h + ACTION_GAP
    }

    /// The bar's text column: between the artwork and the button.
    pub const fn mini_text(&self) -> (i32, i32) {
        let x = self.mini_thumb.right() + 8;
        (x, self.mini_button.x - x)
    }
}

pub const BAND_11: Metrics = Metrics {
    width: 212,
    height: 520,
    large_px: 28,
    small_px: 24,
    back: Rect::new(14, 16, 40, 40),
    mode: Rect::new(158, 16, 40, 40),
    cover: Rect::new(18, 68, 176, 176),
    title: Rect::new(12, 263, 188, 39),
    artist: Rect::new(12, 302, 188, 34),
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
    lyric_anchor: 144,
};

pub const BAND_10_PRO: Metrics = Metrics {
    width: 336,
    height: 480,
    large_px: 32,
    small_px: 24,
    back: Rect::new(20, 14, 44, 44),
    mode: Rect::new(272, 14, 44, 44),
    cover: Rect::new(80, 62, 176, 176),
    title: Rect::new(24, 257, 288, 44),
    artist: Rect::new(24, 301, 288, 38),
    bar: Rect::new(40, 340, 256, 6),
    elapsed: Rect::new(30, 350, 120, 34),
    remaining: Rect::new(186, 350, 120, 34),
    prev: Rect::new(58, 394, 60, 60),
    play: Rect::new(130, 386, 76, 76),
    next: Rect::new(218, 394, 60, 60),
    volume_down: Rect::new(10, 404, 40, 40),
    volume_up: Rect::new(286, 404, 40, 40),
    volume: Rect::new(24, 257, 288, 44),
    dots_y: 470,
    lyrics_header: Rect::new(0, 0, 336, 56),
    lyrics_view: Rect::new(0, 56, 336, 404),
    lyric_pad_x: 28,
    lyric_gap: 20,
    lyric_anchor: 128,
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
    fn list_rows_hold_artwork_and_two_text_lines() {
        for (metrics, list) in [(BAND_11, LIST_BAND_11), (BAND_10_PRO, LIST_BAND_10_PRO)] {
            let row = Rect::new(0, 0, list.row_width(&metrics), list.row_height);
            assert!(inside(row, list.thumb));
            assert_eq!(list.thumb.w, list.thumb.h);
            assert_eq!(
                list.thumb.y * 2 + list.thumb.h,
                list.row_height,
                "artwork centred"
            );
            assert!(list.thumb.right() < list.text_x);
            assert!(list.artist_y(&metrics) + metrics.small_line() <= list.row_height);
            assert!(row.w - list.text_x >= 120, "room for the title");
            assert!(list.header.h >= metrics.large_line());
            assert!(list.header.bottom() <= list.first_row);
            assert!(list.thumb_radius * 4 < list.thumb.w);
        }
    }

    #[test]
    fn the_now_playing_bar_floats_inside_the_panel() {
        for (metrics, list) in [(BAND_11, LIST_BAND_11), (BAND_10_PRO, LIST_BAND_10_PRO)] {
            assert!(inside(metrics.screen(), list.mini));
            // Both panels have strongly rounded corners: keep the bar's own corners inside them.
            assert!(metrics.height - list.mini.bottom() >= 12 && list.mini.x >= 8);
            assert_eq!(list.mini.center_x(), metrics.width / 2);
            let bar = Rect::new(0, 0, list.mini.w, list.mini.h);
            assert!(inside(bar, list.mini_thumb) && inside(bar, list.mini_button));
            assert!(!list.mini_thumb.overlaps(list.mini_button));
            // One thumbnail file serves the rows and the bar.
            assert_eq!(
                list.mini_thumb,
                Rect::new(
                    list.mini_thumb.x,
                    list.mini_thumb.y,
                    list.thumb.w,
                    list.thumb.h
                )
            );
            assert!(list.mini.h >= metrics.small_line() + 16);
            assert!(list.mini_text().1 >= 72);
            assert!(list.mini_button.w >= 40 && list.mini_button.h >= 40);
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
    fn the_scrolling_title_stays_clear_of_the_cover_shadow() {
        for metrics in BOTH {
            assert!(metrics.title.y >= metrics.cover.bottom() + cover_shadow_reach());
            assert!(metrics.volume.y >= metrics.cover.bottom() + cover_shadow_reach());
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
    fn play_buttons_fit_their_icon_and_label_and_leave_songs_in_view() {
        // An icon, a gap and the label (two characters on the narrow panel, four on the wide one).
        for (metrics, list, label_chars) in [
            (BAND_11, LIST_BAND_11, 2),
            (BAND_10_PRO, LIST_BAND_10_PRO, 4),
        ] {
            let content = 26 + 6 + label_chars * metrics.small_px;
            assert!(
                list.action_width(&metrics) >= content + 8,
                "{}",
                metrics.width
            );
            assert!(list.action_h >= 40, "touch target");
            // At least one whole song row shows above the floating now-playing bar.
            assert!(
                list.songs_top() + list.row_height <= list.mini.y,
                "{}",
                metrics.width
            );
            // The two buttons together stay inside the row column.
            assert!(2 * list.action_width(&metrics) + ACTION_SPACING <= list.row_width(&metrics));
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
