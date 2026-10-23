//! The player: a horizontal pager with two pages -- the now-playing page and
//! the lyrics page -- over a colour wash taken from the cover.
//!
//! The pager is a plain LVGL container that scrolls horizontally and snaps to
//! its two children, so the swipe is the firmware's own scroll physics. The
//! lyric page is a vertically scrolling container: the user can browse it with
//! a finger, and while nobody touches it the active line is kept at a fixed
//! height by animated `scroll_to_y` calls.

use alloc::{string::String, vec::Vec};
use core::ffi::c_void;

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
    split_cookie,
};
use crate::target::{
    PlayerFrame,
    native_app::PAGE_PLAYER,
};

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
/// How long the volume read-out replaces the title (Band 10 Pro).
const TOAST_TICKS: u32 = 15;
/// How much of one 100 ms tick label creation may take: a little while the player page is in
/// view (the user is pressing buttons), more once the lyric page is being waited for.
const LINES_BUDGET_HIDDEN_MS: u32 = 16;
const LINES_BUDGET_VISIBLE_MS: u32 = 56;
const MAX_LYRIC_LINES: usize = 160;

const WHITE: u32 = 0xFF_FFFF;
const FADE_STEPS: [u32; 5] = [255, 150, 100, 70, 50];
/// 256 is full size; the lines that are not being sung are drawn at 7/8.
const INACTIVE_SCALE: u32 = 224;

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

struct Line {
    label: Obj,
    translation: Obj,
    /// Layout position inside the lyric view.
    y: i32,
    opa: u32,
}

struct PlayerPage {
    bg: Obj,
    pager: Obj,
    cover: Obj,
    cover_placeholder: Obj,
    cover_path: Vec<u8>,
    title: Obj,
    artist: Obj,
    elapsed: Obj,
    remaining: Obj,
    volume: Obj,
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
    shown_title: String,
    glyph_paths: [Option<Glyph>; 7],
    dot_page: usize,
    /// Pulling the first page to the right has already asked to go back.
    back_armed: bool,
    /// The current drag started on the first page.
    pull_from_first: bool,
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

/// A translucent glass disc with a white glyph, the shape of the reference
/// design's buttons.
unsafe fn glass_button(
    parent: Obj,
    rect: Rect,
    glyph: Glyph,
    widget: usize,
    fill_opa: u32,
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
                (prop::BG_OPA, fill_opa),
                (prop::BORDER_WIDTH, 1),
                (prop::BORDER_COLOR, WHITE),
                (prop::BORDER_OPA, 70),
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
    let glyph = unsafe { glyph_image(circle, glyph, 255) };
    Button { circle, glyph }
}

unsafe fn text_label(
    parent: Obj,
    rect: Rect,
    large: bool,
    centered: bool,
    scrolling: bool,
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
        if scrolling {
            lvx_label_set_long_mode(label, LV_LABEL_LONG_SCROLL_CIRCULAR);
        }
        lv::resize(label, rect.w, rect.h);
        lv::place(label, rect.x, rect.y);
        lv::clear_flag(label, flag::CLICKABLE);
    }
    label
}

/// A band that melts into the page background (`from_top`: opaque at its top edge).
unsafe fn fade(parent: Obj, y: i32, height: i32, from_top: bool, palette: Palette) -> Obj {
    let color = lv::rgb(background_at(palette, if from_top { y } else { y + height }));
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

/// The page background colour at height `y`: the palette fades top to bottom.
fn background_at(palette: Palette, y: i32) -> lyra_player_core::Rgb {
    palette
        .top
        .mix(palette.bottom, (y.clamp(0, M.height) * 256 / M.height) as u32)
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
        lv::on_event(pager, pager_event, event::SCROLL, cookie(PAGE_PLAYER, W_PAGER));
        lv::on_event(pager, pager_event, event::SCROLL_BEGIN, cookie(PAGE_PLAYER, W_PAGER));
        lv::on_event(pager, pager_event, event::SCROLL_END, cookie(PAGE_PLAYER, W_PAGER));
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
                    (prop::RADIUS, 26),
                    (prop::BG_COLOR, WHITE),
                    (prop::BG_OPA, 36),
                ],
            );
            lv::clear_flag(placeholder, flag::CLICKABLE | flag::SCROLLABLE);
            glyph_image(placeholder, Glyph::Note, 120);
        }
        placeholder
    };
    let cover = unsafe {
        let cover = lvx_image_create(page_a);
        if !cover.is_null() {
            lv::resize(cover, M.cover.w, M.cover.h);
            lv::place(cover, M.cover.x, M.cover.y);
            lv::style(cover, &[(prop::RADIUS, 26), (prop::CLIP_CORNER, 1)]);
            lv::clear_flag(cover, flag::CLICKABLE | flag::SCROLLABLE);
            lv::show(cover, false);
        }
        cover
    };
    let title = unsafe { text_label(page_a, M.title, true, true, true, 255) };
    let artist = unsafe { text_label(page_a, M.artist, false, true, true, 150) };
    let elapsed = unsafe { text_label(page_a, M.elapsed, false, false, false, 150) };
    let remaining = unsafe {
        let label = text_label(page_a, M.remaining, false, false, false, 150);
        lv::set_style(label, prop::TEXT_ALIGN, 3);
        label
    };
    let (bar_track, bar_fill) = unsafe {
        let track = lv::container(page_a, M.bar.x, M.bar.y, M.bar.w, M.bar.h);
        let fill = lv::container(page_a, M.bar.x, M.bar.y, 1, M.bar.h);
        for (object, opa) in [(track, 70), (fill, 235)] {
            if !object.is_null() {
                lv::style(
                    object,
                    &[(prop::RADIUS, 3), (prop::BG_COLOR, WHITE), (prop::BG_OPA, opa)],
                );
                lv::clear_flag(object, flag::CLICKABLE | flag::SCROLLABLE);
            }
        }
        (track, fill)
    };
    let _ = bar_track;
    let back = unsafe { glass_button(page_a, M.back, Glyph::Back, W_BACK, 46) };
    let mode = unsafe { glass_button(page_a, M.mode, Glyph::Once, W_MODE, 46) };
    let prev = unsafe { glass_button(page_a, M.prev, Glyph::Prev, W_PREV, 56) };
    let play = unsafe { glass_button(page_a, M.play, Glyph::Play, W_PLAY, 70) };
    let next = unsafe { glass_button(page_a, M.next, Glyph::Next, W_NEXT, 56) };
    let vol_down = unsafe { glass_button(page_a, M.volume_down, Glyph::Minus, W_VOL_DOWN, 40) };
    let vol_up = unsafe { glass_button(page_a, M.volume_up, Glyph::Plus, W_VOL_UP, 40) };
    let volume = if lv::BAND_11 {
        unsafe {
            let label = text_label(page_a, M.volume, false, true, false, 200);
            lv::show(label, true);
            label
        }
    } else {
        lv::NULL
    };

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
                    &[(prop::RADIUS, 3), (prop::BG_COLOR, WHITE), (prop::BG_OPA, 255)],
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
            cover_placeholder,
            cover_path: Vec::new(),
            title,
            artist,
            elapsed,
            remaining,
            volume,
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
            shown_title: String::new(),
            glyph_paths: [None; 7],
            dot_page: 0,
            back_armed: false,
            pull_from_first: false,
        });
    }
    refresh();
    0
}

pub fn destroy() {
    unsafe { PAGE = None };
}

pub fn resume() {
    if let Some(page) = page() {
        page.shown = None;
        page.last_active = None;
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
        event::PRESSED => unsafe { lv::set_style(button.circle, prop::BG_OPA, 150) },
        event::RELEASED | event::PRESS_LOST => unsafe {
            lv::set_style(button.circle, prop::BG_OPA, base_opa(widget))
        },
        event::CLICKED => {
            unsafe { lv::set_style(button.circle, prop::BG_OPA, base_opa(widget)) };
            crate::target::handle_player_event(match widget {
                W_BACK => crate::target::PlayerEvent::Back,
                W_PREV => crate::target::PlayerEvent::Previous,
                W_PLAY => crate::target::PlayerEvent::Toggle,
                W_NEXT => crate::target::PlayerEvent::Next,
                W_MODE => crate::target::PlayerEvent::Mode,
                W_VOL_DOWN => crate::target::PlayerEvent::VolumeDown,
                _ => crate::target::PlayerEvent::VolumeUp,
            });
            if matches!(widget, W_VOL_DOWN | W_VOL_UP) && !lv::BAND_11 {
                page.toast_until = page.tick + TOAST_TICKS;
                page.shown_title.clear();
            }
        }
        _ => {}
    }
}

const fn base_opa(widget: usize) -> u32 {
    match widget {
        W_PLAY => 70,
        W_PREV | W_NEXT => 56,
        W_VOL_DOWN | W_VOL_UP => 40,
        _ => 46,
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
        event::PRESSED => page.held = true,
        event::RELEASED | event::PRESS_LOST => {
            page.held = false;
            page.hold_until = page.tick + BROWSE_HOLD_TICKS;
        }
        event::SCROLL_END => page.hold_until = page.tick + BROWSE_HOLD_TICKS,
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

pub fn refresh() {
    let Some(page) = page() else { return };
    page.tick = page.tick.wrapping_add(1);
    let frame = crate::target::player_frame();
    unsafe {
        page.apply_view(&frame);
        page.apply_lyrics(&frame);
    }
}

impl PlayerPage {
    unsafe fn apply_view(&mut self, frame: &PlayerFrame) {
        let Some(view) = &frame.view else {
            if self.shown.is_some() {
                self.shown = None;
            }
            return;
        };
        let previous = self.shown.clone();
        let changed = |f: &dyn Fn(&PlayerView) -> bool| previous.as_ref().is_none_or(f);
        if changed(&|old| old.id != view.id) {
            unsafe { self.show_track(view) };
        }
        if changed(&|old| old.palette != view.palette) {
            unsafe { self.set_palette(view.palette) };
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
            unsafe { lv::set_style(self.prev.glyph, prop::IMAGE_OPA, enabled_opa(view.has_previous)) };
        }
        if changed(&|old| old.has_next != view.has_next) {
            unsafe { lv::set_style(self.next.glyph, prop::IMAGE_OPA, enabled_opa(view.has_next)) };
        }
        if changed(&|old| old.mode != view.mode) {
            let glyph = match view.mode {
                PlaybackMode::ListOnce => Glyph::Once,
                PlaybackMode::RepeatAll => Glyph::Repeat,
                PlaybackMode::Shuffle => Glyph::Shuffle,
            };
            unsafe { self.set_glyph(self.mode, 1, glyph) };
        }
        if changed(&|old| old.volume != view.volume) || self.toast_active() {
            unsafe { self.show_volume(view) };
        }
        if changed(&|old| old.error != view.error || old.transport != view.transport)
            && view.transport == Transport::Failed
        {
            if let Some(error) = &view.error {
                unsafe { set_text(self.artist, error) };
            }
        }
        self.shown = Some(view.clone());
    }

    fn toast_active(&self) -> bool {
        !lv::BAND_11 && self.tick < self.toast_until
    }

    unsafe fn show_track(&mut self, view: &PlayerView) {
        unsafe {
            set_text(self.title, &view.title);
            set_text(self.artist, &view.artist);
            set_text(self.ly_title, &join_title(&view.title, &view.artist));
        }
        self.shown_title = view.title.clone();
        match &view.cover {
            Some(path) => {
                self.cover_path = lv::c_string(path);
                unsafe {
                    lvx_image_set_src(self.cover, self.cover_path.as_ptr().cast());
                    lv::show(self.cover, true);
                    lv::show(self.cover_placeholder, false);
                }
            }
            None => unsafe {
                lv::show(self.cover, false);
                lv::show(self.cover_placeholder, true);
            },
        }
        self.last_active = None;
    }

    unsafe fn set_palette(&mut self, palette: Palette) {
        unsafe {
            lv::set_style(self.bg, prop::BG_COLOR, lv::rgb(palette.top));
            lv::set_style(self.bg, prop::BG_GRAD_COLOR, lv::rgb(palette.bottom));
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
        if lv::BAND_11 {
            unsafe { set_text(self.volume, &text) };
        } else if self.toast_active() {
            unsafe { set_text(self.title, &alloc::format!("音量 {text}")) };
        } else {
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
        let limit = if self.dot_page == 1 { LINES_BUDGET_VISIBLE_MS } else { LINES_BUDGET_HIDDEN_MS };
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
                opa: 0,
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
        if self.last_active != active || self.lines.iter().any(|line| line.opa == 0) {
            for (index, line) in self.lines.iter_mut().enumerate() {
                let distance = match active {
                    Some(active) => index.abs_diff(active),
                    None => index + 1,
                };
                let opa = FADE_STEPS[distance.min(FADE_STEPS.len() - 1)];
                if line.opa != opa {
                    line.opa = opa;
                    let scale = if distance == 0 { 256 } else { INACTIVE_SCALE };
                    unsafe {
                        lv::set_style(line.label, prop::TEXT_OPA, opa);
                        lv::set_style(line.label, prop::TRANSFORM_SCALE_X, scale);
                        lv::set_style(line.label, prop::TRANSFORM_SCALE_Y, scale);
                        if !line.translation.is_null() {
                            lv::set_style(line.translation, prop::TEXT_OPA, opa * 6 / 10);
                        }
                    }
                }
            }
            self.last_active = active;
        }
        if self.held {
            self.last_scroll_target = None;
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
            let visible = self.dot_page == 1;
            unsafe { lvx_object_scroll_to_y(self.ly_view, target, u32::from(visible)) };
        }
    }
}
