//! The player: a horizontal pager with two pages -- the now-playing page and
//! the lyrics page -- over a colour wash taken from the cover.
//!
//! The pager is a plain LVGL container that scrolls horizontally and snaps to
//! its two children, so the swipe is the firmware's own scroll physics. The
//! lyric page is a vertically scrolling container: the user can browse it with
//! a finger, and while nobody touches it the active line is kept at a fixed
//! height by animated `scroll_to_y` calls.

use alloc::{string::String, vec::Vec};
use core::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

use canopus_target_private::*;
use lyra_player_core::{
    Palette,
    layout::{self, Metrics, Rect},
    lyrics::Lyrics,
    playback::PlaybackMode,
    view::{PlayerView, Transport},
};

use super::{
    assets::Glyph,
    cookie,
    lv::{self, Obj, align, dir, event, flag, prop, snap},
    motion::{self, ONE, ease_in, ease_in_out, ease_out, ease_out_back, mix, mix_rgb, progress},
    split_cookie,
};
use crate::target::{PlayerFrame, native_app::PAGE_PLAYER};

const M: Metrics = if lv::BAND_11 {
    layout::BAND_11
} else {
    layout::BAND_10_PRO
};

const W_BACK: usize = 1;
const W_PREV: usize = 2;
const W_PLAY: usize = 3;
const W_NEXT: usize = 4;
const W_MODE: usize = 5;
const W_VOL_DOWN: usize = 6;
const W_VOL_UP: usize = 7;
const W_PAGER: usize = 8;
const W_VIEW: usize = 9;

/// Ticks are 100 ms. The lyric view returns to the active line this long after
/// the finger leaves it.
const BROWSE_HOLD_TICKS: u32 = 40;
/// How long the volume read-out replaces the title.
const TOAST_TICKS: u32 = 15;
/// How much of one 100 ms tick label creation may take: a little while the player page is in
/// view (the user is pressing buttons), more once the lyric page is being waited for.
const LINES_BUDGET_HIDDEN_MS: u32 = 6;
const LINES_BUDGET_VISIBLE_MS: u32 = 24;
const MAX_LYRIC_LINES: usize = 160;

const WHITE: u32 = 0xFF_FFFF;
/// A pressed button shows a soft disc behind its glyph; at rest only the glyph is drawn.
const PRESSED_OPA: u32 = 40;
/// The artwork floats over the background on a soft drop shadow. The firmware's LVGL draws a
/// shadow as a solid core with an outward falloff, so the core is shrunk by the drop: it stays
/// hidden under the artwork and only the falloff shows, strongest below it.
const COVER_RADIUS: u32 = 26;
const SHADOW_OPA: u32 = 170;
const COVER_SHADOW: [(u32, u32); 5] = [
    (prop::SHADOW_WIDTH, layout::COVER_SHADOW_WIDTH as u32),
    (prop::SHADOW_COLOR, 0),
    (prop::SHADOW_OPA, SHADOW_OPA),
    (prop::SHADOW_OFFSET_Y, layout::COVER_SHADOW_DROP as u32),
    (prop::SHADOW_SPREAD, -layout::COVER_SHADOW_DROP as u32),
];
const FADE_STEPS: [u32; 5] = [255, 150, 100, 70, 50];
/// 256 is full size; the lines that are not being sung are drawn at 7/8.
const INACTIVE_SCALE: u32 = 224;

// Motion. A new active lyric line grows on a light spring while the others dim and shrink, and
// the view glides to it on the same beat. A song change slides the cover and its titles out
// against the direction of travel and the new ones in, while the background cross-fades.
const FOCUS_SCALE_MS: u32 = 520;
const FOCUS_OPA_MS: u32 = 360;
/// Lines further than this from both the old and the new active line change at once.
const FOCUS_REACH: usize = 2;
const SCROLL_MS: u32 = 560;
const SWAP_OUT_MS: u32 = 140;
const SWAP_IN_MS: u32 = 340;
const SWAP_SHIFT: i32 = 28;
const PALETTE_MS: u32 = 520;

/// Set while the page scrolls the lyric view itself, so the scroll events it causes are not
/// mistaken for the user browsing.
static SELF_SCROLL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Button {
    circle: Obj,
    glyph: Obj,
}

impl Button {
    const NONE: Self = Self {
        circle: lv::NULL,
        glyph: lv::NULL,
    };
}

/// How a lyric line is drawn: text opacity and scale (256 = full size).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Look {
    opa: i32,
    scale: i32,
}

const HIDDEN_LINE: Look = Look {
    opa: 0,
    scale: INACTIVE_SCALE as i32,
};

struct Line {
    label: Obj,
    translation: Obj,
    /// Layout position inside the lyric view.
    y: i32,
    /// Where the line is heading (its distance from the active line decides), what it shows
    /// now, and what it showed when the current focus change began.
    target: Look,
    shown: Look,
    from: Look,
}

/// A song change in progress.
struct Swap {
    start: u32,
    /// +1: the new song enters from the right (next), -1 from the left (previous).
    direction: i32,
    /// The song to show once the old one is out; taken at the midpoint.
    next: Option<PlayerView>,
    /// Where the old content was when the swap began (a swap can interrupt another).
    from_x: i32,
    from_opa: i32,
}

struct Scroll {
    from: i32,
    to: i32,
    start: u32,
}

struct PaletteFade {
    from: Palette,
    to: Palette,
    start: u32,
}

struct PlayerPage {
    bg: Obj,
    pager: Obj,
    cover: Obj,
    cover_image: Obj,
    cover_placeholder: Obj,
    cover_path: Vec<u8>,
    title: Obj,
    artist: Obj,
    elapsed: Obj,
    remaining: Obj,
    bar_fill: Obj,
    back: Button,
    prev: Button,
    play: Button,
    next: Button,
    mode: Button,
    vol_down: Button,
    vol_up: Button,
    dots: [Obj; 2],
    // lyric page
    ly_title: Obj,
    ly_view: Obj,
    ly_empty: Obj,
    ly_spacer: Obj,
    fade_top: Obj,
    fade_bottom: Obj,
    lines: Vec<Line>,
    lines_for: Option<u64>,
    lines_total: usize,
    pending: Vec<(String, Option<String>)>,
    laid_out: bool,
    last_active: Option<usize>,
    last_scroll_target: Option<i32>,
    // interaction
    held: bool,
    hold_until: u32,
    tick: u32,
    toast_until: u32,
    // what is on screen
    shown: Option<PlayerView>,
    /// The volume read-out has taken over the title line.
    title_borrowed: bool,
    glyph_paths: [Option<Glyph>; 7],
    dot_page: usize,
    /// Pulling the first page to the right has already asked to go back.
    back_armed: bool,
    /// The current drag started on the first page.
    pull_from_first: bool,
    // motion
    clock: motion::Clock,
    swap: Option<Swap>,
    /// Which way the next song change slides; set by the previous / next buttons.
    direction: i32,
    /// The cover and title block's horizontal offset and opacity, as drawn.
    card_x: i32,
    card_opa: i32,
    palette: Palette,
    palette_fade: Option<PaletteFade>,
    focus_start: Option<u32>,
    focus_lines: Vec<usize>,
    scroll: Option<Scroll>,
}

static mut PAGE: Option<PlayerPage> = None;

fn page() -> Option<&'static mut PlayerPage> {
    // SAFETY: the page lifecycle and every widget callback run on the UI
    // thread, one at a time.
    unsafe { (*core::ptr::addr_of_mut!(PAGE)).as_mut() }
}

// ---------------------------------------------------------------------------
// Building the page
// ---------------------------------------------------------------------------

unsafe fn glyph_image(parent: Obj, glyph: Glyph, opa: u32) -> Obj {
    let image = unsafe { lvx_image_create(parent) };
    if image.is_null() {
        return image;
    }
    unsafe {
        lvx_image_set_src(image, glyph.path().as_ptr().cast());
        lv::style(image, &[(prop::IMAGE_OPA, opa)]);
        lv::clear_flag(image, flag::CLICKABLE | flag::SCROLLABLE);
        lvx_object_align(image, align::CENTER, 0, 0);
    }
    image
}

/// A bare white glyph; the round touch area behind it lights up only while pressed.
unsafe fn icon_button(
    parent: Obj,
    rect: Rect,
    glyph: Glyph,
    widget: usize,
    glyph_opa: u32,
) -> Button {
    let circle = unsafe { lv::container(parent, rect.x, rect.y, rect.w, rect.h) };
    if circle.is_null() {
        return Button::NONE;
    }
    unsafe {
        lv::style(
            circle,
            &[
                (prop::RADIUS, 9999),
                (prop::BG_COLOR, WHITE),
                (prop::BG_OPA, 0),
            ],
        );
        lv::clear_flag(circle, flag::SCROLLABLE);
        lv::add_flag(circle, flag::CLICKABLE);
        let cookie = cookie(PAGE_PLAYER, widget);
        lv::on_event(circle, button_event, event::CLICKED, cookie);
        lv::on_event(circle, button_event, event::PRESSED, cookie);
        lv::on_event(circle, button_event, event::RELEASED, cookie);
        lv::on_event(circle, button_event, event::PRESS_LOST, cookie);
    }
    let glyph = unsafe { glyph_image(circle, glyph, glyph_opa) };
    Button { circle, glyph }
}

unsafe fn text_label(
    parent: Obj,
    rect: Rect,
    large: bool,
    centered: bool,
    single_line: bool,
    opa: u32,
) -> Obj {
    let label = unsafe { lvx_label_create(parent) };
    if label.is_null() {
        return label;
    }
    unsafe {
        let font = if large {
            STYLE_MISANS_DEMIBOLD_32
        } else {
            STYLE_MISANS_REGULAR_24
        };
        lvx_style_apply(label, font as *const c_void, 255, 0);
        lv::style(
            label,
            &[
                (prop::TEXT_COLOR, WHITE),
                (prop::TEXT_OPA, opa),
                (prop::TEXT_ALIGN, if centered { 2 } else { 1 }),
            ],
        );
        if single_line {
            // Circular scrolling continuously redraws both metadata lines (and
            // even the offscreen lyrics header). Firmware rendering can starve
            // the Bluetooth owner until the page closes or the screen sleeps.
            lvx_label_set_long_mode(label, lv::LONG_DOT);
        }
        lv::resize(label, rect.w, rect.h);
        lv::place(label, rect.x, rect.y);
        lv::clear_flag(label, flag::CLICKABLE);
    }
    label
}

/// A band that melts into the page background (`from_top`: opaque at its top edge).
unsafe fn fade(parent: Obj, y: i32, height: i32, from_top: bool, palette: Palette) -> Obj {
    let color = lv::rgb(background_at(
        palette,
        if from_top { y } else { y + height },
    ));
    let band = unsafe { lv::container(parent, 0, y, M.width, height) };
    if !band.is_null() {
        unsafe {
            lv::style(
                band,
                &[
                    (prop::BG_COLOR, color),
                    (prop::BG_OPA, 255),
                    (prop::BG_GRAD_DIR, 1),
                    (prop::BG_MAIN_STOP, 0),
                    (prop::BG_GRAD_STOP, 255),
                    (prop::BG_GRAD_COLOR, color),
                    (prop::BG_MAIN_OPA, if from_top { 255 } else { 0 }),
                    (prop::BG_GRAD_OPA, if from_top { 0 } else { 255 }),
                ],
            );
            lv::clear_flag(band, flag::CLICKABLE | flag::SCROLLABLE);
        }
    }
    band
}

/// The properties a song change animates, set once at creation (they follow every other local
/// property of these objects in id order).
unsafe fn card_style(object: Obj) {
    if !object.is_null() {
        unsafe { lv::style(object, &[(prop::OPA, 255), (prop::TRANSLATE_X, 0)]) };
    }
}

/// The no-cover square: a lighter shade of the page's own colour.
fn placeholder_color(palette: Palette) -> lyra_player_core::Rgb {
    palette.top.mix(lyra_player_core::Rgb(0xFFFFFF), 40)
}

/// The page background colour at height `y`: the palette fades top to bottom.
fn background_at(palette: Palette, y: i32) -> lyra_player_core::Rgb {
    palette.top.mix(
        palette.bottom,
        (y.clamp(0, M.height) * 256 / M.height) as u32,
    )
}

pub fn create(root: Obj) -> i32 {
    unsafe { build(root) }
}

unsafe fn build(root: Obj) -> i32 {
    let palette = Palette::default();
    let bg = unsafe { lv::container(root, 0, 0, M.width, M.height) };
    let pager = unsafe { lv::container(root, 0, 0, M.width, M.height) };
    if bg.is_null() || pager.is_null() {
        return -1;
    }
    unsafe {
        lv::style(
            bg,
            &[
                (prop::BG_COLOR, lv::rgb(palette.top)),
                (prop::BG_OPA, 255),
                (prop::BG_GRAD_DIR, 1),
                (prop::BG_MAIN_STOP, 0),
                (prop::BG_GRAD_STOP, 255),
                (prop::BG_GRAD_COLOR, lv::rgb(palette.bottom)),
            ],
        );
        lv::clear_flag(bg, flag::CLICKABLE | flag::SCROLLABLE);

        lv::add_flag(pager, flag::SCROLL_ONE);
        lvx_object_set_scroll_dir(pager, dir::HOR);
        lvx_object_set_scroll_snap_x(pager, snap::CENTER);
        lvx_object_set_scrollbar_mode(pager, lv::SCROLLBAR_OFF);
        lv::on_event(
            pager,
            pager_event,
            event::SCROLL,
            cookie(PAGE_PLAYER, W_PAGER),
        );
        lv::on_event(
            pager,
            pager_event,
            event::SCROLL_BEGIN,
            cookie(PAGE_PLAYER, W_PAGER),
        );
        lv::on_event(
            pager,
            pager_event,
            event::SCROLL_END,
            cookie(PAGE_PLAYER, W_PAGER),
        );
    }
    let page_a = unsafe { lv::container(pager, 0, 0, M.width, M.height) };
    let page_b = unsafe { lv::container(pager, M.width, 0, M.width, M.height) };
    if page_a.is_null() || page_b.is_null() {
        return -1;
    }
    unsafe {
        for page in [page_a, page_b] {
            lv::add_flag(page, flag::SNAPPABLE);
            lv::clear_flag(page, flag::SCROLLABLE | flag::CLICKABLE);
        }
    }

    // --- now playing -------------------------------------------------------
    let cover_placeholder = unsafe {
        let placeholder = lv::container(page_a, M.cover.x, M.cover.y, M.cover.w, M.cover.h);
        if !placeholder.is_null() {
            lv::style(
                placeholder,
                &[
                    (prop::RADIUS, COVER_RADIUS),
                    (prop::BG_COLOR, lv::rgb(placeholder_color(palette))),
                    (prop::BG_OPA, 255),
                ],
            );
            lv::style(placeholder, &COVER_SHADOW);
            card_style(placeholder);
            lv::clear_flag(placeholder, flag::CLICKABLE | flag::SCROLLABLE);
            glyph_image(placeholder, Glyph::Note, 150);
        }
        placeholder
    };
    let (cover, cover_image) = unsafe {
        let cover = lv::container(page_a, M.cover.x, M.cover.y, M.cover.w, M.cover.h);
        let mut image = lv::NULL;
        if !cover.is_null() {
            lv::style(
                cover,
                &[(prop::RADIUS, COVER_RADIUS), (prop::CLIP_CORNER, 1)],
            );
            lv::style(cover, &COVER_SHADOW);
            card_style(cover);
            lv::clear_flag(cover, flag::CLICKABLE | flag::SCROLLABLE);
            lv::show(cover, false);
            image = lvx_image_create(cover);
            if !image.is_null() {
                lv::clear_flag(image, flag::CLICKABLE | flag::SCROLLABLE);
            }
        }
        (cover, image)
    };
    let title = unsafe { text_label(page_a, M.title, true, true, true, 255) };
    let artist = unsafe { text_label(page_a, M.artist, false, true, true, 150) };
    unsafe {
        card_style(title);
        card_style(artist);
    }
    let elapsed = unsafe { text_label(page_a, M.elapsed, false, false, false, 140) };
    let remaining = unsafe {
        let label = text_label(page_a, M.remaining, false, false, false, 140);
        lv::set_style(label, prop::TEXT_ALIGN, 3);
        label
    };
    let (bar_track, bar_fill) = unsafe {
        let track = lv::container(page_a, M.bar.x, M.bar.y, M.bar.w, M.bar.h);
        let fill = lv::container(page_a, M.bar.x, M.bar.y, 1, M.bar.h);
        for (object, opa) in [(track, 64), (fill, 240)] {
            if !object.is_null() {
                lv::style(
                    object,
                    &[
                        (prop::RADIUS, 3),
                        (prop::BG_COLOR, WHITE),
                        (prop::BG_OPA, opa),
                    ],
                );
                lv::clear_flag(object, flag::CLICKABLE | flag::SCROLLABLE);
            }
        }
        (track, fill)
    };
    let _ = bar_track;
    let back = unsafe { icon_button(page_a, M.back, Glyph::Back, W_BACK, 230) };
    let mode = unsafe { icon_button(page_a, M.mode, Glyph::Once, W_MODE, 230) };
    let prev = unsafe { icon_button(page_a, M.prev, Glyph::Prev, W_PREV, 255) };
    let play = unsafe { icon_button(page_a, M.play, Glyph::Play, W_PLAY, 255) };
    let next = unsafe { icon_button(page_a, M.next, Glyph::Next, W_NEXT, 255) };
    let vol_down = unsafe { icon_button(page_a, M.volume_down, Glyph::Minus, W_VOL_DOWN, 170) };
    let vol_up = unsafe { icon_button(page_a, M.volume_up, Glyph::Plus, W_VOL_UP, 170) };
    // --- lyrics ------------------------------------------------------------
    let ly_title = unsafe {
        let rect = Rect::new(
            M.lyric_pad_x,
            (M.lyrics_header.h - M.small_line()) / 2 + 4,
            M.width - 2 * M.lyric_pad_x,
            M.small_line(),
        );
        text_label(page_b, rect, false, true, true, 190)
    };
    let ly_view = unsafe {
        let view = lv::container(
            page_b,
            M.lyrics_view.x,
            M.lyrics_view.y,
            M.lyrics_view.w,
            M.lyrics_view.h,
        );
        if !view.is_null() {
            lvx_object_set_scroll_dir(view, dir::VER);
            lvx_object_set_scrollbar_mode(view, lv::SCROLLBAR_OFF);
            lv::quiet_scroll_bar(view);
            lv::add_flag(view, flag::CLICKABLE);
            let cookie = cookie(PAGE_PLAYER, W_VIEW);
            for code in [
                event::PRESSED,
                event::RELEASED,
                event::PRESS_LOST,
                event::SCROLL_END,
            ] {
                lv::on_event(view, view_event, code, cookie);
            }
        }
        view
    };
    // The lines fade out under the header and above the page dots.
    let (fade_top, fade_bottom) = unsafe {
        let top = fade(page_b, M.lyrics_view.y, 36, true, palette);
        let bottom = fade(page_b, M.lyrics_view.bottom() - 40, 40, false, palette);
        (top, bottom)
    };
    let ly_empty = unsafe {
        let rect = Rect::new(
            M.lyric_pad_x,
            M.lyrics_view.y + M.lyrics_view.h / 3,
            M.width - 2 * M.lyric_pad_x,
            M.small_line() * 2,
        );
        let label = text_label(page_b, rect, false, true, false, 130);
        lv::show(label, false);
        label
    };

    // --- page indicator ----------------------------------------------------
    let dots = unsafe {
        let mut dots = [lv::NULL; 2];
        for (index, dot) in dots.iter_mut().enumerate() {
            let x = M.width / 2 - 11 + 16 * index as i32;
            *dot = lv::container(root, x, M.dots_y - 3, 6, 6);
            if !dot.is_null() {
                lv::style(
                    *dot,
                    &[
                        (prop::RADIUS, 3),
                        (prop::BG_COLOR, WHITE),
                        (prop::BG_OPA, 255),
                    ],
                );
                lv::set_style(*dot, prop::BG_OPA, if index == 0 { 230 } else { 80 });
                lv::clear_flag(*dot, flag::CLICKABLE | flag::SCROLLABLE);
            }
        }
        dots
    };

    unsafe {
        PAGE = Some(PlayerPage {
            bg,
            pager,
            cover,
            cover_image,
            cover_placeholder,
            cover_path: Vec::new(),
            title,
            artist,
            elapsed,
            remaining,
            bar_fill,
            back,
            prev,
            play,
            next,
            mode,
            vol_down,
            vol_up,
            dots,
            ly_title,
            ly_view,
            ly_empty,
            ly_spacer: lv::NULL,
            fade_top,
            fade_bottom,
            lines: Vec::new(),
            lines_for: None,
            lines_total: 0,
            pending: Vec::new(),
            laid_out: false,
            last_active: None,
            last_scroll_target: None,
            held: false,
            hold_until: 0,
            tick: 0,
            toast_until: 0,
            shown: None,
            title_borrowed: false,
            glyph_paths: [None; 7],
            dot_page: 0,
            back_armed: false,
            pull_from_first: false,
            clock: motion::Clock::new(),
            swap: None,
            direction: 1,
            card_x: 0,
            card_opa: 255,
            palette,
            palette_fade: None,
            focus_start: None,
            focus_lines: Vec::new(),
            scroll: None,
        });
    }
    refresh();
    0
}

pub fn destroy() {
    if let Some(page) = page() {
        page.clock.stop();
    }
    unsafe { PAGE = None };
}

pub fn pause() {
    if let Some(page) = page() {
        page.clock.stop();
        page.swap = None;
        page.palette_fade = None;
        page.focus_start = None;
        page.scroll = None;
        page.held = false;
        page.hold_until = 0;
    }
}

pub fn resume() {
    if let Some(page) = page() {
        page.shown = None;
        page.last_active = None;
        page.last_scroll_target = None;
        unsafe { page.place_card(0, 255) };
    }
    refresh();
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

extern "C" fn button_event(event: *mut c_void) {
    if event.is_null() {
        return;
    }
    let code = unsafe { lvx_event_get_code(event) };
    let (page_index, widget) = split_cookie(unsafe { lvx_event_get_user_data(event) });
    if page_index != PAGE_PLAYER || !super::page_accepts_input(PAGE_PLAYER) {
        return;
    }
    let Some(page) = page() else { return };
    let button = match widget {
        W_BACK => page.back,
        W_PREV => page.prev,
        W_PLAY => page.play,
        W_NEXT => page.next,
        W_MODE => page.mode,
        W_VOL_DOWN => page.vol_down,
        W_VOL_UP => page.vol_up,
        _ => return,
    };
    match code {
        event::PRESSED => unsafe { lv::set_style(button.circle, prop::BG_OPA, PRESSED_OPA) },
        event::RELEASED | event::PRESS_LOST => unsafe {
            lv::set_style(button.circle, prop::BG_OPA, 0)
        },
        event::CLICKED => {
            unsafe { lv::set_style(button.circle, prop::BG_OPA, 0) };
            match widget {
                W_PREV => page.direction = -1,
                W_NEXT => page.direction = 1,
                _ => {}
            }
            crate::target::handle_player_event(match widget {
                W_BACK => crate::target::PlayerEvent::Back,
                W_PREV => crate::target::PlayerEvent::Previous,
                W_PLAY => crate::target::PlayerEvent::Toggle,
                W_NEXT => crate::target::PlayerEvent::Next,
                W_MODE => crate::target::PlayerEvent::Mode,
                W_VOL_DOWN => crate::target::PlayerEvent::VolumeDown,
                _ => crate::target::PlayerEvent::VolumeUp,
            });
            if matches!(widget, W_VOL_DOWN | W_VOL_UP) {
                page.toast_until = page.tick + TOAST_TICKS;
                page.title_borrowed = true;
            }
        }
        _ => {}
    }
}

/// Pulling the first page this far to the right (the pager stretches elastically)
/// leaves the player, like the system's swipe-back.
const BACK_PULL: i32 = 26;

extern "C" fn pager_event(event: *mut c_void) {
    if event.is_null() || !super::page_accepts_input(PAGE_PLAYER) {
        return;
    }
    let Some(page) = page() else { return };
    let code = unsafe { lvx_event_get_code(event) };
    let scrolled = unsafe { lvx_object_scroll_x(page.pager) };
    if code == event::SCROLL_BEGIN {
        page.pull_from_first = scrolled <= 2;
        return;
    }
    if code == event::SCROLL_END {
        page.back_armed = false;
        return;
    }
    if scrolled <= -BACK_PULL && page.pull_from_first && !page.back_armed {
        page.back_armed = true;
        crate::target::handle_player_event(crate::target::PlayerEvent::Back);
        return;
    }
    let index = usize::from(scrolled > M.width / 2);
    if index != page.dot_page {
        page.dot_page = index;
        unsafe {
            for (position, dot) in page.dots.iter().enumerate() {
                lv::set_style(*dot, prop::BG_OPA, if position == index { 230 } else { 80 });
            }
        }
    }
}

extern "C" fn view_event(event: *mut c_void) {
    if event.is_null() {
        return;
    }
    let code = unsafe { lvx_event_get_code(event) };
    let Some(page) = page() else { return };
    match code {
        event::PRESSED => {
            page.held = true;
            page.scroll = None;
        }
        event::RELEASED | event::PRESS_LOST => {
            page.held = false;
            page.hold_until = page.tick + BROWSE_HOLD_TICKS;
        }
        event::SCROLL_END if !SELF_SCROLL.load(Ordering::Relaxed) => {
            page.hold_until = page.tick + BROWSE_HOLD_TICKS
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

pub fn refresh() {
    let Some(page) = page() else { return };
    page.tick = page.tick.wrapping_add(1);
    let mut frame = crate::target::player_frame();
    unsafe {
        page.apply_view(frame.view.take());
        page.apply_lyrics(&frame);
    }
}

impl PlayerPage {
    unsafe fn apply_view(&mut self, view: Option<PlayerView>) {
        let Some(view) = view else {
            self.shown = None;
            return;
        };
        let previous = self.shown.take();
        let changed = |f: &dyn Fn(&PlayerView) -> bool| previous.as_ref().is_none_or(f);
        // A freshly opened or resumed page shows the song at once; a change while it is open
        // animates.
        if changed(&|old| old.id != view.id) {
            if previous.is_some() {
                self.begin_swap(&view);
            } else {
                unsafe { self.show_track(&view) };
            }
        }
        if changed(&|old| old.palette != view.palette) {
            if previous.is_some() {
                self.begin_palette(view.palette);
            } else {
                self.palette_fade = None;
                self.palette = view.palette;
                unsafe { self.paint(view.palette) };
            }
        }
        if changed(&|old| old.elapsed != view.elapsed) {
            unsafe { set_text(self.elapsed, &view.elapsed) };
        }
        if changed(&|old| old.remaining != view.remaining) {
            unsafe { set_text(self.remaining, &view.remaining) };
        }
        if changed(&|old| old.progress != view.progress) {
            let width = (M.bar.w * view.progress as i32 / 1000).max(M.bar.h);
            unsafe { lv::resize(self.bar_fill, width, M.bar.h) };
        }
        if changed(&|old| old.transport != view.transport) {
            let glyph = match view.transport {
                Transport::Playing | Transport::Loading => Glyph::Pause,
                _ => Glyph::Play,
            };
            unsafe { self.set_glyph(self.play, 0, glyph) };
        }
        if changed(&|old| old.has_previous != view.has_previous) {
            unsafe {
                lv::set_style(
                    self.prev.glyph,
                    prop::IMAGE_OPA,
                    enabled_opa(view.has_previous),
                )
            };
        }
        if changed(&|old| old.has_next != view.has_next) {
            unsafe { lv::set_style(self.next.glyph, prop::IMAGE_OPA, enabled_opa(view.has_next)) };
        }
        if changed(&|old| old.mode != view.mode) {
            let glyph = match view.mode {
                PlaybackMode::ListOnce => Glyph::Once,
                PlaybackMode::RepeatAll => Glyph::Repeat,
                PlaybackMode::RepeatOne => Glyph::RepeatOne,
                PlaybackMode::Shuffle => Glyph::Shuffle,
            };
            unsafe { self.set_glyph(self.mode, 1, glyph) };
        }
        if changed(&|old| old.volume != view.volume) || self.toast_active() || self.title_borrowed {
            unsafe { self.show_volume(&view) };
        }
        if changed(&|old| old.error != view.error || old.transport != view.transport)
            && view.transport == Transport::Failed
        {
            if let Some(error) = &view.error {
                unsafe { set_text(self.artist, error) };
            }
        }
        self.shown = Some(view);
    }

    fn toast_active(&self) -> bool {
        self.tick < self.toast_until
    }

    unsafe fn show_track(&mut self, view: &PlayerView) {
        unsafe {
            set_text(self.title, &view.title);
            set_text(self.artist, &view.artist);
            set_text(self.ly_title, &join_title(&view.title, &view.artist));
        }
        match &view.cover {
            Some(path) => {
                self.cover_path = lv::c_string(path);
                unsafe {
                    lvx_image_set_src(self.cover_image, self.cover_path.as_ptr().cast());
                    let scale = M.cover.w * 256 / i32::from(lyra_player_core::COVER_SIZE);
                    lvx_image_set_scale(self.cover_image, scale, scale);
                    lvx_object_align(self.cover_image, align::CENTER, 0, 0);
                    lv::show(self.cover, true);
                    lv::show(self.cover_placeholder, false);
                }
            }
            None => unsafe {
                lv::show(self.cover, false);
                lv::show(self.cover_placeholder, true);
            },
        }
        if view.transport == Transport::Failed {
            if let Some(error) = &view.error {
                unsafe { set_text(self.artist, error) };
            }
        }
        self.last_active = None;
    }

    unsafe fn paint(&mut self, palette: Palette) {
        unsafe {
            lv::set_style(self.bg, prop::BG_COLOR, lv::rgb(palette.top));
            lv::set_style(self.bg, prop::BG_GRAD_COLOR, lv::rgb(palette.bottom));
            lv::set_style(
                self.cover_placeholder,
                prop::BG_COLOR,
                lv::rgb(placeholder_color(palette)),
            );
            let top = lv::rgb(background_at(palette, M.lyrics_view.y));
            lv::set_style(self.fade_top, prop::BG_COLOR, top);
            lv::set_style(self.fade_top, prop::BG_GRAD_COLOR, top);
            let bottom = lv::rgb(background_at(palette, M.lyrics_view.bottom()));
            lv::set_style(self.fade_bottom, prop::BG_COLOR, bottom);
            lv::set_style(self.fade_bottom, prop::BG_GRAD_COLOR, bottom);
        }
    }

    unsafe fn set_glyph(&mut self, button: Button, slot: usize, glyph: Glyph) {
        if self.glyph_paths[slot] != Some(glyph) {
            self.glyph_paths[slot] = Some(glyph);
            unsafe { lvx_image_set_src(button.glyph, glyph.path().as_ptr().cast()) };
        }
    }

    unsafe fn show_volume(&mut self, view: &PlayerView) {
        let text = alloc::format!("{}%", view.volume);
        if self.toast_active() {
            unsafe { set_text(self.title, &alloc::format!("音量 {text}")) };
        } else if self.title_borrowed {
            // The read-out is over: the title gets its line back.
            self.title_borrowed = false;
            unsafe { set_text(self.title, &view.title) };
        }
    }
}

fn enabled_opa(enabled: bool) -> u32 {
    if enabled { 255 } else { 80 }
}

fn join_title(title: &str, artist: &str) -> String {
    if artist.is_empty() {
        String::from(title)
    } else {
        alloc::format!("{title} · {artist}")
    }
}

unsafe fn set_text(label: Obj, text: &str) {
    if label.is_null() {
        return;
    }
    let text = lv::c_string(text);
    unsafe { lvx_label_set_text(label, text.as_ptr()) };
}

// ---------------------------------------------------------------------------
// Lyrics
// ---------------------------------------------------------------------------

impl PlayerPage {
    unsafe fn apply_lyrics(&mut self, frame: &PlayerFrame) {
        if self.lines_for != frame.lyrics_id {
            unsafe { self.clear_lines() };
            self.lines_for = frame.lyrics_id;
            if frame.lyrics_id.is_some() {
                let lyrics = crate::target::lyrics_snapshot();
                unsafe { self.start_lines(lyrics) };
            }
            return;
        }
        if self.lines.len() < self.lines_total {
            unsafe { self.create_more_lines() };
            return;
        }
        if !self.laid_out {
            unsafe { self.lay_out() };
            return;
        }
        unsafe { self.follow(frame.active_lyric) };
    }

    unsafe fn clear_lines(&mut self) {
        unsafe {
            for line in self.lines.drain(..) {
                lvx_object_delete(line.label);
                if !line.translation.is_null() {
                    lvx_object_delete(line.translation);
                }
            }
            if !self.ly_spacer.is_null() {
                lvx_object_delete(self.ly_spacer);
                self.ly_spacer = lv::NULL;
            }
            lv::show(self.ly_empty, false);
        }
        self.lines_total = 0;
        self.laid_out = false;
        self.last_active = None;
        self.last_scroll_target = None;
        self.pending = Vec::new();
        self.focus_start = None;
        self.focus_lines.clear();
        self.scroll = None;
    }

    unsafe fn start_lines(&mut self, lyrics: Lyrics) {
        if lyrics.is_empty() {
            unsafe {
                set_text(self.ly_empty, "暂无歌词");
                lv::show(self.ly_empty, true);
            }
            self.laid_out = true;
            return;
        }
        self.pending = lyrics
            .lines
            .into_iter()
            .take(MAX_LYRIC_LINES)
            .map(|line| {
                let text = if line.text.is_empty() {
                    String::from("· · ·")
                } else {
                    line.text
                };
                (text, line.translation)
            })
            .collect();
        self.lines_total = self.pending.len();
        self.laid_out = false;
    }

    /// Creates the next few label objects. Creating a hundred at once would
    /// freeze the page, so they trickle in within a per-tick time budget (at
    /// least one per tick); they stay transparent until `lay_out` has measured
    /// and placed them.
    unsafe fn create_more_lines(&mut self) {
        let width = M.lyrics_view.w - 2 * M.lyric_pad_x;
        let limit = if self.dot_page == 1 {
            LINES_BUDGET_VISIBLE_MS
        } else {
            LINES_BUDGET_HIDDEN_MS
        };
        let budget = lv::Budget::start();
        let mut created = 0;
        loop {
            let index = self.lines.len();
            if index >= self.lines_total || (created > 0 && budget.spent(limit)) {
                break;
            }
            created += 1;
            let (text, translation) = core::mem::take(&mut self.pending[index]);
            let rect = Rect::new(M.lyric_pad_x, 0, width, lv::SIZE_CONTENT);
            let label = unsafe { text_label(self.ly_view, rect, true, false, false, 0) };
            if !label.is_null() {
                // Lines other than the active one are drawn smaller, scaled from their
                // top-left corner.
                unsafe {
                    lv::style(
                        label,
                        &[
                            (prop::TRANSFORM_SCALE_X, INACTIVE_SCALE),
                            (prop::TRANSFORM_SCALE_Y, INACTIVE_SCALE),
                            (prop::TRANSFORM_PIVOT_X, 0),
                            (prop::TRANSFORM_PIVOT_Y, 0),
                        ],
                    );
                }
            }
            let translation_label = match &translation {
                Some(_) => unsafe { text_label(self.ly_view, rect, false, false, false, 0) },
                None => lv::NULL,
            };
            if label.is_null() {
                self.lines_total = index;
                break;
            }
            unsafe {
                set_text(label, &text);
                if let Some(translation) = &translation {
                    set_text(translation_label, translation);
                }
            }
            self.lines.push(Line {
                label,
                translation: translation_label,
                y: 0,
                target: HIDDEN_LINE,
                shown: HIDDEN_LINE,
                from: HIDDEN_LINE,
            });
        }
    }

    /// Reads the heights LVGL gave the labels and stacks them.
    unsafe fn lay_out(&mut self) {
        let mut heights: Vec<(i32, i32)> = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            let main = unsafe { lv::measured(line.label).3 };
            let sub = if line.translation.is_null() {
                0
            } else {
                unsafe { lv::measured(line.translation).3 }
            };
            if main <= 1 || (!line.translation.is_null() && sub <= 1) {
                return;
            }
            heights.push((main, sub));
        }
        let mut y = M.lyric_anchor;
        for (line, (main, sub)) in self.lines.iter_mut().zip(heights) {
            line.y = y;
            unsafe { lv::place(line.label, M.lyric_pad_x, y) };
            y += main;
            if !line.translation.is_null() {
                unsafe { lv::place(line.translation, M.lyric_pad_x, y + 2) };
                y += sub + 2;
            }
            y += M.lyric_gap;
        }
        // Content ends at the last label; extend it so the last line can
        // still be scrolled up to the anchor.
        if let Some(last) = self.lines.last() {
            let tail = last.y - M.lyric_anchor + M.lyrics_view.h;
            unsafe {
                let spacer = lv::container(self.ly_view, 0, tail.max(y), 1, 1);
                self.ly_spacer = spacer;
            }
        }
        self.laid_out = true;
        self.last_active = None;
        self.last_scroll_target = None;
    }

    unsafe fn follow(&mut self, active: Option<usize>) {
        if self.last_active != active || self.lines.iter().any(|line| line.target.opa == 0) {
            // Only what the user can see moves; everything else takes its new look at once.
            let animate = self.dot_page == 1;
            let previous = self.last_active;
            self.focus_lines.clear();
            for (index, line) in self.lines.iter_mut().enumerate() {
                let distance = |anchor: Option<usize>| {
                    anchor.map_or(index + 1, |anchor| index.abs_diff(anchor))
                };
                let from_active = distance(active);
                line.target = Look {
                    opa: FADE_STEPS[from_active.min(FADE_STEPS.len() - 1)] as i32,
                    scale: if from_active == 0 {
                        256
                    } else {
                        INACTIVE_SCALE as i32
                    },
                };
                line.from = line.shown;
                if line.shown == line.target {
                    continue;
                }
                if animate && (from_active <= FOCUS_REACH || distance(previous) <= FOCUS_REACH) {
                    self.focus_lines.push(index);
                } else {
                    let target = line.target;
                    unsafe { show_look(line, target) };
                }
            }
            self.last_active = active;
            self.focus_start = if self.focus_lines.is_empty() {
                None
            } else {
                self.clock.start(frame);
                Some(lv::now_ms())
            };
        }
        if self.held {
            self.last_scroll_target = None;
            self.scroll = None;
            return;
        }
        if self.tick < self.hold_until {
            return;
        }
        let target = match active.and_then(|index| self.lines.get(index)) {
            Some(line) => (line.y - M.lyric_anchor).max(0),
            None => 0,
        };
        if self.last_scroll_target != Some(target) {
            self.last_scroll_target = Some(target);
            if self.dot_page == 1 {
                let from = unsafe { lvx_object_scroll_y(self.ly_view) };
                self.scroll = Some(Scroll {
                    from,
                    to: target,
                    start: lv::now_ms(),
                });
                self.clock.start(frame);
            } else {
                self.scroll = None;
                unsafe { scroll_view(self.ly_view, target) };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Motion
// ---------------------------------------------------------------------------

/// Draws a lyric line with `look`, touching only the properties that change.
unsafe fn show_look(line: &mut Line, look: Look) {
    let old = core::mem::replace(&mut line.shown, look);
    unsafe {
        if old.opa != look.opa {
            let opa = look.opa.clamp(0, 255) as u32;
            lv::set_style(line.label, prop::TEXT_OPA, opa);
            if !line.translation.is_null() {
                lv::set_style(line.translation, prop::TEXT_OPA, opa * 6 / 10);
            }
        }
        if old.scale != look.scale {
            lv::set_style(line.label, prop::TRANSFORM_SCALE_X, look.scale as u32);
            lv::set_style(line.label, prop::TRANSFORM_SCALE_Y, look.scale as u32);
        }
    }
}

unsafe fn scroll_view(view: Obj, y: i32) {
    SELF_SCROLL.store(true, Ordering::Relaxed);
    unsafe { lvx_object_scroll_to_y(view, y, 0) };
    SELF_SCROLL.store(false, Ordering::Relaxed);
}

extern "C" fn frame(_timer: *mut c_void) {
    let Some(page) = page() else { return };
    let now = lv::now_ms();
    // `|`, not `||`: every animation advances on every frame.
    let moving = unsafe {
        page.step_swap(now) | page.step_palette(now) | page.step_focus(now) | page.step_scroll(now)
    };
    if !moving {
        page.clock.stop();
    }
}

impl PlayerPage {
    fn begin_swap(&mut self, view: &PlayerView) {
        let direction = core::mem::replace(&mut self.direction, 1);
        match &mut self.swap {
            // Still on its way out: the newer song takes the waiting one's place.
            Some(swap) if swap.next.is_some() => swap.next = Some(view.clone()),
            _ => {
                self.swap = Some(Swap {
                    start: lv::now_ms(),
                    direction,
                    next: Some(view.clone()),
                    from_x: self.card_x,
                    from_opa: self.card_opa,
                })
            }
        }
        self.clock.start(frame);
    }

    unsafe fn step_swap(&mut self, now: u32) -> bool {
        let Some(swap) = &mut self.swap else {
            return false;
        };
        let direction = swap.direction;
        if swap.next.is_some() {
            let t = progress(swap.start, now, 0, SWAP_OUT_MS);
            let (x, opa) = (
                mix(swap.from_x, -direction * SWAP_SHIFT, ease_in(t)),
                mix(swap.from_opa, 0, ease_in(t)),
            );
            if t == ONE {
                let next = swap.next.take();
                swap.start = now;
                if let Some(view) = next {
                    unsafe { self.show_track(&view) };
                }
                unsafe { self.place_card(direction * SWAP_SHIFT, 0) };
            } else {
                unsafe { self.place_card(x, opa) };
            }
            return true;
        }
        let t = progress(swap.start, now, 0, SWAP_IN_MS);
        let (x, opa) = (
            mix(direction * SWAP_SHIFT, 0, ease_out(t)),
            mix(0, 255, ease_out(t)),
        );
        unsafe { self.place_card(x, opa) };
        if t == ONE {
            self.swap = None;
            return false;
        }
        true
    }

    /// Offsets and fades the cover with its title and artist.
    unsafe fn place_card(&mut self, x: i32, opa: i32) {
        let opa = opa.clamp(0, 255);
        let moved = x != self.card_x;
        let faded = opa != self.card_opa;
        self.card_x = x;
        self.card_opa = opa;
        for object in [self.cover, self.cover_placeholder, self.title, self.artist] {
            if object.is_null() {
                continue;
            }
            unsafe {
                if moved {
                    lv::set_style(object, prop::TRANSLATE_X, x as u32);
                }
                if faded {
                    lv::set_style(object, prop::OPA, opa as u32);
                }
            }
        }
        if faded {
            // The shadow's core sits under the artwork; through a half-transparent cover it would
            // show as a dark inner square, so the shadow fades out much faster than the cover
            // (OPA scales it once more on top of this).
            let shadow = SHADOW_OPA * (opa * opa) as u32 / (255 * 255);
            for object in [self.cover, self.cover_placeholder] {
                if !object.is_null() {
                    unsafe { lv::set_style(object, prop::SHADOW_OPA, shadow) };
                }
            }
        }
    }

    fn begin_palette(&mut self, to: Palette) {
        self.palette_fade = Some(PaletteFade {
            from: self.palette,
            to,
            start: lv::now_ms(),
        });
        self.clock.start(frame);
    }

    unsafe fn step_palette(&mut self, now: u32) -> bool {
        let Some(fade) = &self.palette_fade else {
            return false;
        };
        let t = progress(fade.start, now, 0, PALETTE_MS);
        let e = ease_in_out(t);
        let palette = Palette {
            top: lyra_player_core::Rgb(mix_rgb(fade.from.top.0, fade.to.top.0, e)),
            bottom: lyra_player_core::Rgb(mix_rgb(fade.from.bottom.0, fade.to.bottom.0, e)),
        };
        if t == ONE {
            self.palette_fade = None;
        }
        if palette != self.palette {
            self.palette = palette;
            unsafe { self.paint(palette) };
        }
        t != ONE
    }

    unsafe fn step_focus(&mut self, now: u32) -> bool {
        let Some(start) = self.focus_start else {
            return false;
        };
        let scale_t = progress(start, now, 0, FOCUS_SCALE_MS);
        let opa_t = progress(start, now, 0, FOCUS_OPA_MS);
        for &index in &self.focus_lines {
            let Some(line) = self.lines.get_mut(index) else {
                continue;
            };
            // The line coming into focus springs up; the others simply settle.
            let grows = line.target.scale > line.from.scale;
            let scale = if grows {
                ease_out_back(scale_t)
            } else {
                ease_out(scale_t)
            };
            let look = Look {
                opa: mix(line.from.opa, line.target.opa, ease_out(opa_t)),
                scale: mix(line.from.scale, line.target.scale, scale),
            };
            unsafe { show_look(line, look) };
        }
        if scale_t == ONE && opa_t == ONE {
            self.focus_start = None;
            return false;
        }
        true
    }

    unsafe fn step_scroll(&mut self, now: u32) -> bool {
        let Some(scroll) = &self.scroll else {
            return false;
        };
        if self.held {
            self.scroll = None;
            return false;
        }
        let t = progress(scroll.start, now, 0, SCROLL_MS);
        let y = mix(scroll.from, scroll.to, ease_in_out(t));
        unsafe { scroll_view(self.ly_view, y) };
        if t == ONE {
            self.scroll = None;
            return false;
        }
        true
    }
}
