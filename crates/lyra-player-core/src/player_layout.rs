//! Geometry of the custom player page; native list rows keep their own theme.

#[derive(Clone, Copy, Debug)]
pub struct PlayerLayout {
    pub width: i32,
    pub height: i32,
    pub text_width: i32,
    pub cover_top: i32,
    pub title_top: i32,
    /// Scrolling labels must be at least one font line tall. LVGL's circular
    /// long mode scrolls horizontally when the text is too wide, and otherwise
    /// scrolls *vertically* when the line is taller than the box, so a short
    /// title in a too-short box bounces up and down.
    pub title_height: i32,
    pub author_top: i32,
    pub author_height: i32,
    pub controls_top: i32,
    pub controls_spacing: i32,
    pub hitbox_size: i32,
    /// Width of the imported player background; its height is always
    /// [`BACKGROUND_HEIGHT`].
    pub background_width: i32,
}

/// Height of every imported player background asset.
pub const BACKGROUND_HEIGHT: i32 = 520;
/// Background width produced by importers that predate per-device artwork.
/// Band 11 still accepts it and center-crops it.
pub const LEGACY_BACKGROUND_WIDTH: i32 = 336;

impl PlayerLayout {
    pub const BAND_10_PRO: Self = Self {
        width: 336,
        height: 480,
        text_width: 336,
        cover_top: 72,
        title_top: 270,
        title_height: 44,
        author_top: 318,
        author_height: 44,
        controls_top: 382,
        controls_spacing: 72,
        hitbox_size: 64,
        background_width: 336,
    };
    pub const BAND_11: Self = Self {
        width: 212,
        height: 520,
        text_width: 188,
        cover_top: 92,
        title_top: 292,
        title_height: 44,
        author_top: 338,
        author_height: 40,
        controls_top: 398,
        controls_spacing: 64,
        hitbox_size: 56,
        background_width: 212,
    };

    pub const fn control_x(self, key: u32) -> i32 {
        match key {
            7 => -self.controls_spacing,
            8 => self.controls_spacing,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_regions_and_touch_targets_fit_both_displays() {
        for layout in [PlayerLayout::BAND_10_PRO, PlayerLayout::BAND_11] {
            assert!(layout.cover_top >= 64);
            assert!(layout.cover_top + 180 <= layout.title_top);
            assert!(layout.title_top + layout.title_height <= layout.author_top);
            assert!(layout.author_top + layout.author_height <= layout.controls_top);
            assert!(layout.controls_top + 64 <= layout.height - 24);
            assert!(layout.text_width <= layout.width);
            assert!(layout.controls_spacing >= layout.hitbox_size);
            for key in [7, 6, 8] {
                let center = layout.width / 2 + layout.control_x(key);
                assert!(center - layout.hitbox_size / 2 >= 8);
                assert!(center + layout.hitbox_size / 2 <= layout.width - 8);
            }
        }
    }

    /// MiSans spans 1.326 em (hhea ascender 1044, descender -282 per 1000
    /// units); allow one extra pixel for the rasterizer rounding each side up.
    fn misans_line_height(px: i32) -> i32 {
        (px * 1_326 + 999) / 1_000 + 1
    }

    #[test]
    fn scrolling_labels_are_at_least_one_line_tall() {
        // Font sizes applied by the device backend: 10 Pro uses MiSans 32 for
        // both lines (036 uses 24 for the author); Band 11 uses medium 28/24.
        for (layout, title_px, author_px) in [
            (PlayerLayout::BAND_10_PRO, 32, 32),
            (PlayerLayout::BAND_11, 28, 24),
        ] {
            assert!(layout.title_height >= misans_line_height(title_px));
            assert!(layout.author_height >= misans_line_height(author_px));
        }
    }

    #[test]
    fn background_fills_the_player_viewport() {
        for layout in [PlayerLayout::BAND_10_PRO, PlayerLayout::BAND_11] {
            assert_eq!(layout.background_width, layout.width);
            assert!(BACKGROUND_HEIGHT >= layout.height);
            assert!(LEGACY_BACKGROUND_WIDTH >= layout.width);
        }
    }
}
