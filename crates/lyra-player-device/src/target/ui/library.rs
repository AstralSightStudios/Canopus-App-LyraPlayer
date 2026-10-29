//! The home page and the song pages: a large title, rows of artwork, title and artist over black, the song count
//! at the end, and a floating now-playing bar with its own play/pause button. The row of the
//! current track has an accent title and a small equalizer over its artwork.

use alloc::{format, vec::Vec};

use canopus_target_private::*;
use lyra_player_core::{
    Collection,
    layout::{self, ACTION_GAP, ACTION_SPACING, ListMetrics, Metrics},
    playback::PlaybackMode,
    view::Transport,
};

use super::{
    artwork,
    assets::Glyph,
    cookie,
    lv::{self, Obj, align, dir, event, flag, prop},
    motion::{self, ONE, ease_out, mix, progress},
    split_cookie,
};
use crate::target::{
    LibraryFrame, LibraryTarget, NowPlaying,
    native_app::{PAGE_COLLECTION, PAGE_LIBRARY},
};

const M: Metrics = if lv::BAND_11 {
    layout::BAND_11
} else {
    layout::BAND_10_PRO
};
const L: ListMetrics = if lv::BAND_11 {
    layout::LIST_BAND_11
} else {
    layout::LIST_BAND_10_PRO
};

/// The list is not virtualised, so it is capped at what a page can sensibly hold.
const MAX_ROWS: usize = 64;
/// Row objects are built top-down, this much of one refresh tick at a time, so opening a long
/// library never freezes the page.
const ROW_BUDGET_MS: u32 = 24;
/// Artwork is looked up while the tick has spent less than this; building a missing thumbnail
/// (reading the whole cover) happens at most once per tick and once per track.
const ART_BUDGET_MS: u32 = 8;
const THUMB_EDGE: u16 = L.thumb.w as u16;

const W_MINI: usize = 0xFE;
const W_TOGGLE: usize = 0xFD;
const W_SHUFFLE: usize = 0xFC;
const W_LOOP: usize = 0xFB;

/// The two play buttons above a collection's songs.
const ACTION_H: i32 = L.action_h;
const ACTION_GLYPH: i32 = 26;
/// Band 11's 212 px panel only has room for two characters beside the icon.
const SHUFFLE_LABEL: &str = if lv::BAND_11 {
    "随机"
} else {
    "随机播放"
};
const LOOP_LABEL: &str = if lv::BAND_11 {
    "循环"
} else {
    "循环播放"
};
const LABEL_W: i32 = if lv::BAND_11 { 48 } else { 96 };

/// Lyra's coral, the first colour of the launcher icon.
const ACCENT: u32 = 0xFF6B8A;
const WHITE: u32 = 0xFF_FFFF;
const SECONDARY: u32 = 140;
const PLACEHOLDER: u32 = 0x2C2C2E;
const ROW_PRESSED: u32 = 34;
const MINI_FILL: u32 = 0x232326;
const MINI_PRESSED: u32 = 0x313136;
/// A new song glides into the bar from the right.
const MINI_ENTER_MS: u32 = 320;
const MINI_ENTER_SHIFT: i32 = 16;

/// What an artwork slot shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Art {
    /// The grey placeholder: no cover, or one that could not be read.
    Empty,
    /// The placeholder until the thumbnail of this track is found or built.
    Pending(u64),
    Shown(u64),
}

struct ArtSlot {
    thumb: Obj,
    placeholder: Obj,
    path: Vec<u8>,
    art: Art,
}

impl ArtSlot {
    unsafe fn create(parent: Obj, x: i32, y: i32) -> Self {
        let placeholder = unsafe { lv::container(parent, x, y, L.thumb.w, L.thumb.h) };
        unsafe {
            lv::style(
                placeholder,
                &[
                    (prop::RADIUS, L.thumb_radius as u32),
                    (prop::BG_COLOR, PLACEHOLDER),
                    (prop::BG_OPA, 255),
                ],
            );
            lv::clear_flag(placeholder, flag::CLICKABLE | flag::SCROLLABLE);
            let glyph = lvx_image_create(placeholder);
            if !glyph.is_null() {
                lvx_image_set_src(glyph, Glyph::NoteSmall.path().as_ptr().cast());
                lv::style(glyph, &[(prop::IMAGE_OPA, 110)]);
                lv::clear_flag(glyph, flag::CLICKABLE);
                lvx_object_align(glyph, align::CENTER, 0, 0);
            }
        }
        let thumb = unsafe { lvx_image_create(parent) };
        if !thumb.is_null() {
            unsafe {
                lv::resize(thumb, L.thumb.w, L.thumb.h);
                lv::place(thumb, x, y);
                lv::clear_flag(thumb, flag::CLICKABLE | flag::SCROLLABLE);
                lv::show(thumb, false);
            }
        }
        Self {
            thumb,
            placeholder,
            path: Vec::new(),
            art: Art::Empty,
        }
    }

    /// Points the slot at a track; an existing thumbnail shows at once.
    unsafe fn assign(&mut self, id: u64, has_cover: bool) {
        let wanted = if has_cover {
            Art::Pending(id)
        } else {
            Art::Empty
        };
        if self.art == wanted || (has_cover && self.art == Art::Shown(id)) {
            return;
        }
        self.art = wanted;
        unsafe {
            lv::show(self.thumb, false);
            lv::show(self.placeholder, true);
        }
        if has_cover && artwork::is_current(id, THUMB_EDGE) {
            unsafe { self.reveal(id) };
        }
    }

    /// Checks a shown thumbnail again (after an import); it stays on screen meanwhile.
    fn recheck(&mut self) {
        if let Art::Shown(id) = self.art {
            self.art = Art::Pending(id);
        }
    }

    unsafe fn reveal(&mut self, id: u64) {
        self.path = artwork::path(id);
        self.art = Art::Shown(id);
        unsafe {
            lvx_image_set_src(self.thumb, self.path.as_ptr().cast());
            lv::show(self.thumb, true);
            lv::show(self.placeholder, false);
        }
    }

    /// Resolves a pending slot. Returns false once this tick has no time left for artwork.
    unsafe fn settle(&mut self, built: &mut bool, budget: &lv::Budget) -> bool {
        let Art::Pending(id) = self.art else {
            return true;
        };
        if budget.spent(ART_BUDGET_MS) {
            return false;
        }
        if !artwork::is_current(id, THUMB_EDGE) {
            if *built {
                return false;
            }
            *built = true;
            if !artwork::build(id, THUMB_EDGE, L.thumb_radius as u16) {
                self.art = Art::Empty;
                return true;
            }
        }
        unsafe { self.reveal(id) };
        true
    }
}

struct Row {
    object: Obj,
    art: ArtSlot,
    title: Obj,
    artist: Obj,
    separator: Obj,
    hash: u32,
}

/// The floating now-playing bar.
struct Mini {
    bar: Obj,
    art: ArtSlot,
    title: Obj,
    /// The artist line, on panels where the bar has room for two lines.
    subtitle: Obj,
    button: Obj,
    glyph: Obj,
    shown: bool,
    hash: u32,
    pausable: Option<bool>,
    id: Option<u64>,
    /// When the current song's content started gliding in.
    entered: Option<u32>,
}

/// Three bars over the artwork of the current track's row; they move while it plays.
struct Equalizer {
    overlay: Obj,
    bars: [Obj; 3],
    row: Option<usize>,
    playing: bool,
}

const BAR_W: i32 = if lv::BAND_11 { 4 } else { 5 };
const BAR_GAP: i32 = 3;
const BAR_MAX: i32 = L.thumb.h * 2 / 5;
/// Heights in eighths of `BAR_MAX`; each bar starts at its own phase.
const BAR_STEPS: [i32; 12] = [3, 5, 8, 6, 4, 7, 8, 5, 2, 4, 6, 3];
/// The bars step every this many 100 ms ticks. Every redraw over the artwork costs the same on
/// this firmware however small it is: at 10 Hz the bars alone took 8 % of the emulated CPU.
const BAR_TICKS: usize = 2;
const BAR_PAUSED: [i32; 3] = [3, 5, 2];

/// A button above the songs: an icon and a short label on a rounded pill.
struct Pill {
    object: Obj,
}

struct LibraryPage {
    /// `PAGE_LIBRARY` (the home page) or `PAGE_COLLECTION` (the songs of one collection).
    index: usize,
    list: Obj,
    header: Obj,
    header_hash: u32,
    /// Shuffle and loop; only the collection page has them.
    actions: [Pill; 2],
    /// Height the rows start below the title: the button row on a collection page.
    top: i32,
    footer: Obj,
    spacer: Obj,
    /// The (row count, bar shown, counted items) the footer and spacer were last placed for.
    placed_for: Option<(usize, bool, usize)>,
    empty_glyph: Obj,
    empty_text: Obj,
    rows: Vec<Row>,
    targets: Vec<LibraryTarget>,
    mini: Mini,
    equalizer: Equalizer,
    clock: motion::Clock,
    tick: usize,
    revision: Option<u32>,

    /// The `library_key` last drawn; `NOT_SHOWN` forces the next refresh to draw (an empty library
    /// has key 0, so 0 cannot mean "nothing drawn yet").
    shown_key: u64,
}

const NOT_SHOWN: u64 = u64::MAX;

static mut PAGES: [Option<LibraryPage>; 2] = [None, None];

fn slot(index: usize) -> usize {
    usize::from(index == PAGE_COLLECTION)
}

fn page(index: usize) -> Option<&'static mut LibraryPage> {
    // SAFETY: UI thread only.
    unsafe {
        (*core::ptr::addr_of_mut!(PAGES))
            .get_mut(slot(index))?
            .as_mut()
    }
}

fn press_events(page_index: usize, object: Obj, widget: usize) {
    let cookie = cookie(page_index, widget);
    for code in [
        event::CLICKED,
        event::PRESSED,
        event::RELEASED,
        event::PRESS_LOST,
    ] {
        unsafe { lv::on_event(object, row_event, code, cookie) };
    }
}

pub fn create(index: usize, root: Obj) -> i32 {
    let bg = unsafe { lv::container(root, 0, 0, M.width, M.height) };
    if bg.is_null() {
        return -1;
    }
    unsafe {
        lv::style(bg, &[(prop::BG_COLOR, 0), (prop::BG_OPA, 255)]);
        lv::clear_flag(bg, flag::CLICKABLE | flag::SCROLLABLE);
    }

    // --- the scrolling column --------------------------------------------------------------
    let list = unsafe { lv::container(root, 0, 0, M.width, M.height) };
    if list.is_null() {
        return -1;
    }
    unsafe {
        lvx_object_set_scroll_dir(list, dir::VER);
        lvx_object_set_scrollbar_mode(list, lv::SCROLLBAR_OFF);
        lv::clear_flag(list, flag::CLICKABLE);
    }
    let header = unsafe {
        let header = lv::label(
            list,
            L.header.x,
            L.header.y,
            L.header.w,
            L.header.h,
            true,
            1,
            lv::LONG_DOT,
            255,
        );
        lv::set_text(
            header,
            if index == PAGE_COLLECTION {
                ""
            } else {
                "音乐"
            },
        );
        header
    };
    let collection = index == PAGE_COLLECTION;
    let actions = unsafe {
        [
            Pill::create(
                list,
                index,
                0,
                W_SHUFFLE,
                Glyph::Shuffle,
                SHUFFLE_LABEL,
                collection,
            ),
            Pill::create(
                list,
                index,
                1,
                W_LOOP,
                Glyph::Repeat,
                LOOP_LABEL,
                collection,
            ),
        ]
    };
    let footer = unsafe {
        let footer = lv::label(list, 0, 0, M.width, M.small_line(), false, 2, 0, 110);
        lv::show(footer, false);
        footer
    };
    let empty_glyph = unsafe {
        let glyph = lvx_image_create(root);
        if !glyph.is_null() {
            lvx_image_set_src(glyph, Glyph::Note.path().as_ptr().cast());
            lv::style(glyph, &[(prop::IMAGE_OPA, 90)]);
            lv::clear_flag(glyph, flag::CLICKABLE);
            lvx_object_align(glyph, align::TOP_MID, 0, M.height / 3);
            lv::show(glyph, false);
        }
        glyph
    };
    let empty_text = unsafe {
        let text = lv::label(
            root,
            16,
            M.height / 3 + 64,
            M.width - 32,
            M.small_line() * 4,
            false,
            2,
            0,
            SECONDARY,
        );
        lv::show(text, false);
        text
    };
    let Some(mini) = (unsafe { Mini::create(root, index) }) else {
        return -1;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(PAGES))[slot(index)] = Some(LibraryPage {
            index,
            list,
            header,
            header_hash: 0,
            actions,
            top: if collection { ACTION_H + ACTION_GAP } else { 0 },
            footer,
            spacer: lv::NULL,
            placed_for: None,
            empty_glyph,
            empty_text,
            rows: Vec::new(),
            targets: Vec::new(),
            mini,
            equalizer: Equalizer {
                overlay: lv::NULL,
                bars: [lv::NULL; 3],
                row: None,
                playing: false,
            },
            clock: motion::Clock::new(),
            tick: 0,
            revision: None,

            shown_key: NOT_SHOWN,
        });
    }
    refresh(index);
    0
}

pub fn destroy(index: usize) {
    if let Some(page) = page(index) {
        page.clock.stop();
    }
    unsafe { (*core::ptr::addr_of_mut!(PAGES))[slot(index)] = None };
}

/// The page whose animation clock fired: the one the user is looking at, as the other is
/// either paused or gone.
extern "C" fn motion_frame(_timer: *mut core::ffi::c_void) {
    for index in [PAGE_LIBRARY, PAGE_COLLECTION] {
        let Some(page) = page(index) else { continue };
        if page.clock.is_running() && !unsafe { page.mini.step(lv::now_ms()) } {
            page.clock.stop();
        }
    }
}

pub fn resume(index: usize) {
    if let Some(page) = page(index) {
        page.shown_key = NOT_SHOWN;
    }
    refresh(index);
}

extern "C" fn row_event(event: *mut core::ffi::c_void) {
    if event.is_null() {
        return;
    }
    let code = unsafe { lvx_event_get_code(event) };
    let (page_index, index) = split_cookie(unsafe { lvx_event_get_user_data(event) });
    if !matches!(page_index, PAGE_LIBRARY | PAGE_COLLECTION)
        || !super::page_accepts_input(page_index)
    {
        return;
    }
    let Some(page) = page(page_index) else { return };
    // (object, property, pressed value, resting value)
    let (object, property, pressed, resting) = match index {
        W_MINI => (page.mini.bar, prop::BG_COLOR, MINI_PRESSED, MINI_FILL),
        W_TOGGLE => (page.mini.button, prop::BG_OPA, 40, 0),
        W_SHUFFLE => (
            page.actions[0].object,
            prop::BG_COLOR,
            MINI_PRESSED,
            MINI_FILL,
        ),
        W_LOOP => (
            page.actions[1].object,
            prop::BG_COLOR,
            MINI_PRESSED,
            MINI_FILL,
        ),
        _ => match page.rows.get(index) {
            Some(row) => (row.object, prop::BG_OPA, ROW_PRESSED, 0),
            None => return,
        },
    };
    match code {
        event::PRESSED => unsafe { lv::set_style(object, property, pressed) },
        event::RELEASED | event::PRESS_LOST => unsafe { lv::set_style(object, property, resting) },
        event::CLICKED => {
            unsafe { lv::set_style(object, property, resting) };
            let target = match index {
                W_MINI => Some(LibraryTarget::NowPlaying),
                W_TOGGLE => Some(LibraryTarget::Toggle),
                W_SHUFFLE => Some(LibraryTarget::PlayCollection(PlaybackMode::Shuffle)),
                W_LOOP => Some(LibraryTarget::PlayCollection(PlaybackMode::RepeatAll)),
                _ => page.targets.get(index).copied(),
            };
            if let Some(target) = target {
                crate::target::handle_library_event(target);
            }
        }
        _ => {}
    }
}

pub fn refresh(index: usize) {
    let Some(page) = page(index) else { return };
    page.tick = page.tick.wrapping_add(1);
    let budget = lv::Budget::start();
    let key = crate::target::library_key(index);
    if key != page.shown_key {
        let frame = crate::target::library_frame(index);
        page.shown_key = key;
        // Rows that did not fit this tick's budget are built by the next tick.
        if !unsafe { page.show(frame, &budget) } {
            page.shown_key = NOT_SHOWN;
        }
    }
    unsafe {
        page.settle_art(&budget);
        page.animate();
    }
}

fn hash_text(mut hash: u32, text: &str) -> u32 {
    for byte in text.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    hash
}

impl Pill {
    /// A button over the first row; `enabled` false (the home page) builds nothing.
    unsafe fn create(
        list: Obj,
        page_index: usize,
        position: i32,
        widget: usize,
        glyph: Glyph,
        label: &str,
        enabled: bool,
    ) -> Self {
        if !enabled {
            return Self { object: lv::NULL };
        }
        let width = L.action_width(&M);
        let x = L.side + position * (width + ACTION_SPACING);
        let object = unsafe { lv::container(list, x, L.first_row, width, ACTION_H) };
        if object.is_null() {
            return Self { object };
        }
        unsafe {
            lv::style(
                object,
                &[
                    (prop::RADIUS, 9999),
                    (prop::BG_COLOR, MINI_FILL),
                    (prop::BG_OPA, 255),
                ],
            );
            lv::clear_flag(object, flag::SCROLLABLE);
            lv::add_flag(object, flag::CLICKABLE);
            press_events(page_index, object, widget);
            let content = ACTION_GLYPH + 6 + LABEL_W;
            let left = ((width - content) / 2).max(4);
            let icon = lvx_image_create(object);
            if !icon.is_null() {
                lvx_image_set_src(icon, glyph.path().as_ptr().cast());
                lv::clear_flag(icon, flag::CLICKABLE);
                lv::place(icon, left, (ACTION_H - ACTION_GLYPH) / 2);
            }
            let text = lv::label(
                object,
                left + ACTION_GLYPH + 6,
                (ACTION_H - M.small_line()) / 2,
                LABEL_W + 8,
                M.small_line(),
                false,
                1,
                0,
                255,
            );
            lv::set_text(text, label);
            lv::show(object, false);
        }
        Self { object }
    }
}

impl Mini {
    unsafe fn create(root: Obj, page_index: usize) -> Option<Self> {
        let bar = unsafe { lv::container(root, L.mini.x, L.mini.y, L.mini.w, L.mini.h) };
        if bar.is_null() {
            return None;
        }
        unsafe {
            lv::style(
                bar,
                &[
                    (prop::RADIUS, 20),
                    (prop::BG_COLOR, MINI_FILL),
                    (prop::BG_OPA, 255),
                    (prop::BORDER_WIDTH, 1),
                    (prop::BORDER_COLOR, WHITE),
                    (prop::BORDER_OPA, 22),
                ],
            );
            lv::clear_flag(bar, flag::SCROLLABLE);
            lv::add_flag(bar, flag::CLICKABLE);
            press_events(page_index, bar, W_MINI);
            lv::show(bar, false);
        }
        let art = unsafe { ArtSlot::create(bar, L.mini_thumb.x, L.mini_thumb.y) };
        let (text_x, text_w) = L.mini_text();
        let two_lines = L.mini.h >= 2 * M.small_line() + 8;
        let (title_y, subtitle) = if two_lines {
            let top = (L.mini.h - 2 * M.small_line() + 1) / 2;
            let subtitle = unsafe {
                lv::label(
                    bar,
                    text_x,
                    top + M.small_line() - 1,
                    text_w,
                    M.small_line(),
                    false,
                    1,
                    lv::LONG_DOT,
                    SECONDARY,
                )
            };
            (top, subtitle)
        } else {
            ((L.mini.h - M.small_line()) / 2, lv::NULL)
        };
        let title = unsafe {
            lv::label(
                bar,
                text_x,
                title_y,
                text_w,
                M.small_line(),
                false,
                1,
                lv::LONG_DOT,
                255,
            )
        };
        let b = L.mini_button;
        let button = unsafe { lv::container(bar, b.x, b.y, b.w, b.h) };
        let glyph = if button.is_null() {
            lv::NULL
        } else {
            unsafe {
                lv::style(
                    button,
                    &[
                        (prop::RADIUS, 9999),
                        (prop::BG_COLOR, WHITE),
                        (prop::BG_OPA, 0),
                    ],
                );
                lv::clear_flag(button, flag::SCROLLABLE);
                lv::add_flag(button, flag::CLICKABLE);
                press_events(page_index, button, W_TOGGLE);
                let glyph = lvx_image_create(button);
                if !glyph.is_null() {
                    lv::clear_flag(glyph, flag::CLICKABLE);
                    lvx_object_align(glyph, align::CENTER, 0, 0);
                }
                glyph
            }
        };
        for object in [art.thumb, art.placeholder, title, subtitle] {
            if !object.is_null() {
                // Set once here, after every other local property of these objects.
                unsafe { lv::style(object, &[(prop::OPA, 255), (prop::TRANSLATE_X, 0)]) };
            }
        }
        Some(Self {
            bar,
            art,
            title,
            subtitle,
            button,
            glyph,
            shown: false,
            hash: 0,
            pausable: None,
            id: None,
            entered: None,
        })
    }

    /// Advances the glide-in; false once it is over.
    unsafe fn step(&mut self, now: u32) -> bool {
        let Some(start) = self.entered else {
            return false;
        };
        let t = progress(start, now, 0, MINI_ENTER_MS);
        let e = ease_out(t);
        let (x, opa) = (mix(MINI_ENTER_SHIFT, 0, e), mix(0, 255, e));
        for object in [
            self.art.thumb,
            self.art.placeholder,
            self.title,
            self.subtitle,
        ] {
            if !object.is_null() {
                unsafe {
                    lv::set_style(object, prop::TRANSLATE_X, x as u32);
                    lv::set_style(object, prop::OPA, opa as u32);
                }
            }
        }
        if t == ONE {
            self.entered = None;
        }
        t != ONE
    }

    unsafe fn show(&mut self, playing: Option<&NowPlaying>) {
        let Some(playing) = playing else {
            if self.shown {
                self.shown = false;
                unsafe { lv::show(self.bar, false) };
            }
            return;
        };
        let subtitle = match playing.transport {
            Transport::Loading | Transport::Failed => transport_label(playing.transport),
            _ => playing.artist.as_str(),
        };
        let hash = hash_text(hash_text(playing.id as u32, &playing.title), subtitle);
        // A song change under an open bar glides in; a bar that just appears does not.
        if self.shown && self.id.is_some_and(|id| id != playing.id) {
            self.entered = Some(lv::now_ms());
        }
        self.id = Some(playing.id);
        unsafe {
            if hash != self.hash {
                self.hash = hash;
                lv::set_text(self.title, &playing.title);
                lv::set_text(self.subtitle, subtitle);
            }
            self.art.assign(playing.id, playing.has_cover);
            let pausable = matches!(playing.transport, Transport::Playing | Transport::Loading);
            if self.pausable != Some(pausable) {
                self.pausable = Some(pausable);
                let glyph = if pausable {
                    Glyph::PauseSmall
                } else {
                    Glyph::PlaySmall
                };
                lvx_image_set_src(self.glyph, glyph.path().as_ptr().cast());
            }
            if !self.shown {
                self.shown = true;
                lv::show(self.bar, true);
            }
        }
    }
}

impl LibraryPage {
    /// Returns whether every row exists yet.
    unsafe fn show(&mut self, frame: LibraryFrame, budget: &lv::Budget) -> bool {
        let current = frame.now_playing.as_ref().map(|playing| playing.id);
        if self
            .revision
            .replace(frame.revision)
            .is_some_and(|revision| revision != frame.revision)
        {
            self.mini.art.recheck();
            self.rows.iter_mut().for_each(|row| row.art.recheck());
        }
        let complete = unsafe {
            self.mini.show(frame.now_playing.as_ref());
            self.show_header(&frame.title);
            self.show_rows(&frame, current, budget)
        };
        if self.mini.entered.is_some() {
            self.clock.start(motion_frame);
        }
        unsafe {
            let collection = self.index == PAGE_COLLECTION;
            let empty = if collection {
                frame.rows.is_empty()
            } else {
                frame.songs == 0
            };
            lv::show(self.empty_glyph, empty);
            lv::show(self.empty_text, empty);
            if empty {
                lv::set_text(
                    self.empty_text,
                    if !frame.import_supported {
                        "此固件暂不支持\n无线导入音乐"
                    } else if collection && frame.songs > 0 {
                        "这个歌单\n还没有歌曲"
                    } else {
                        "还没有音乐\n用 AstroBox\n导入歌曲"
                    },
                );
            }
            for pill in &self.actions {
                lv::show(pill.object, collection && !frame.rows.is_empty());
            }
            // The home page's first row is "all songs"; the rest are playlists.
            let counted = if collection {
                frame.rows.len()
            } else {
                frame.rows.len().saturating_sub(1)
            };
            self.place_footer(self.targets.len(), counted, self.mini.shown);
            let row = current.and_then(|id| {
                self.targets
                    .iter()
                    .position(|target| matches!(target, LibraryTarget::Song(song) if *song == id))
            });
            let playing = frame
                .now_playing
                .as_ref()
                .is_some_and(|playing| playing.transport == Transport::Playing);
            self.place_equalizer(row, playing);
        }
        complete
    }

    unsafe fn show_rows(
        &mut self,
        frame: &LibraryFrame,
        current: Option<u64>,
        budget: &lv::Budget,
    ) -> bool {
        let mut complete = true;
        self.targets.clear();
        let shown = frame.rows.len().min(MAX_ROWS);
        for (index, entry) in frame.rows.iter().take(MAX_ROWS).enumerate() {
            let song = match entry.target {
                LibraryTarget::Song(id) => Some(id),
                _ => None,
            };
            let playing = song.is_some() && current == song;
            let hash = hash_text(
                hash_text(target_key(&entry.target), &entry.title),
                &entry.subtitle,
            ) ^ entry
                .art
                .map_or(0, |id| id as u32 ^ (id >> 32) as u32)
                .rotate_left(7)
                ^ u32::from(playing)
                ^ (u32::from(index + 1 == shown) << 1);
            if index == self.rows.len() {
                if index > 0 && budget.spent(ROW_BUDGET_MS) {
                    complete = false;
                    break;
                }
                let y = L.first_row + self.top + index as i32 * L.row_height;
                let Some(row) = (unsafe { make_row(self.list, self.index, index, y) }) else {
                    break;
                };
                self.rows.push(row);
            }
            self.targets.push(entry.target);
            let row = &mut self.rows[index];
            if row.hash != hash {
                row.hash = hash;
                unsafe {
                    lv::set_text(row.title, &entry.title);
                    lv::set_text(row.artist, &entry.subtitle);
                    lv::set_style(
                        row.title,
                        prop::TEXT_COLOR,
                        if playing { ACCENT } else { WHITE },
                    );
                    lv::show(row.separator, index + 1 < shown);
                }
            }
            unsafe {
                row.art.assign(entry.art.unwrap_or(0), entry.art.is_some());
                lv::show(row.object, true);
            }
        }
        for (index, row) in self.rows.iter().enumerate() {
            if index >= self.targets.len() {
                unsafe { lv::show(row.object, false) };
            }
        }
        complete
    }

    unsafe fn show_header(&mut self, title: &str) {
        let hash = hash_text(0x811c_9dc5, title);
        if hash != self.header_hash {
            self.header_hash = hash;
            unsafe { lv::set_text(self.header, title) };
        }
    }

    /// Puts the count under the last row and lets the column scroll its end clear of the
    /// floating bar.
    unsafe fn place_footer(&mut self, rows: usize, total: usize, bar: bool) {
        if self.placed_for == Some((rows, bar, total)) {
            return;
        }
        self.placed_for = Some((rows, bar, total));
        let end = L.first_row + self.top + rows as i32 * L.row_height + 10;
        let text = if self.index == PAGE_COLLECTION {
            format!("{total} 首歌曲")
        } else {
            format!("{total} 个歌单")
        };
        unsafe {
            lv::set_text(self.footer, &text);
            lv::place(self.footer, 0, end);
            lv::show(self.footer, rows > 0 && total > 0);
            if !self.spacer.is_null() {
                lvx_object_delete(self.spacer);
            }
            let clearance = if bar { M.height - L.mini.y + 8 } else { 24 };
            self.spacer = lv::container(self.list, 0, end + M.small_line() + clearance, 1, 1);
        }
    }

    unsafe fn place_equalizer(&mut self, row: Option<usize>, playing: bool) {
        let eq = &mut self.equalizer;
        eq.playing = playing;
        if eq.row == row && (row.is_none() || !eq.overlay.is_null()) {
            if !playing {
                unsafe { eq.rest() };
            }
            return;
        }
        if !eq.overlay.is_null() {
            unsafe { lvx_object_delete(eq.overlay) };
            eq.overlay = lv::NULL;
            eq.bars = [lv::NULL; 3];
        }
        eq.row = row;
        let Some(row) = row.and_then(|index| self.rows.get(index)) else {
            return;
        };
        // Created after the artwork, so it draws over it.
        let overlay =
            unsafe { lv::container(row.object, L.thumb.x, L.thumb.y, L.thumb.w, L.thumb.h) };
        if overlay.is_null() {
            return;
        }
        unsafe {
            lv::style(
                overlay,
                &[
                    (prop::RADIUS, L.thumb_radius as u32),
                    (prop::BG_COLOR, 0),
                    (prop::BG_OPA, 120),
                ],
            );
            lv::clear_flag(overlay, flag::CLICKABLE | flag::SCROLLABLE);
            for bar in eq.bars.iter_mut() {
                *bar = lv::container(overlay, 0, 0, BAR_W, BAR_MAX);
                if !bar.is_null() {
                    lv::style(
                        *bar,
                        &[
                            (prop::RADIUS, 2),
                            (prop::BG_COLOR, WHITE),
                            (prop::BG_OPA, 255),
                        ],
                    );
                    lv::clear_flag(*bar, flag::CLICKABLE | flag::SCROLLABLE);
                }
            }
        }
        eq.overlay = overlay;
        unsafe { eq.rest() };
    }

    unsafe fn settle_art(&mut self, budget: &lv::Budget) {
        let mut built = false;
        unsafe {
            if self.mini.shown && !self.mini.art.settle(&mut built, budget) {
                return;
            }
            for row in self.rows.iter_mut().take(self.targets.len()) {
                if !row.art.settle(&mut built, budget) {
                    return;
                }
            }
        }
    }

    unsafe fn animate(&mut self) {
        let eq = &self.equalizer;
        if eq.playing && !eq.overlay.is_null() && self.tick % BAR_TICKS == 0 {
            let phase = self.tick / BAR_TICKS;
            for (index, bar) in eq.bars.iter().enumerate() {
                let step = BAR_STEPS[(phase + index * 5) % BAR_STEPS.len()];
                unsafe { Equalizer::set_bar(index, *bar, step) };
            }
        }
    }
}

impl Equalizer {
    unsafe fn rest(&self) {
        for (index, bar) in self.bars.iter().enumerate() {
            unsafe { Self::set_bar(index, *bar, BAR_PAUSED[index]) };
        }
    }

    /// Bars stand on a common baseline, centred in the artwork.
    unsafe fn set_bar(index: usize, bar: Obj, eighths: i32) {
        if bar.is_null() {
            return;
        }
        let height = (BAR_MAX * eighths / 8).max(BAR_W);
        let left = (L.thumb.w - 3 * BAR_W - 2 * BAR_GAP) / 2;
        let baseline = (L.thumb.h + BAR_MAX) / 2;
        unsafe {
            lv::resize(bar, BAR_W, height);
            lv::place(
                bar,
                left + index as i32 * (BAR_W + BAR_GAP),
                baseline - height,
            );
        }
    }
}

/// A stable number for a row's target, to tell rows apart in the change check.
fn target_key(target: &LibraryTarget) -> u32 {
    match target {
        LibraryTarget::Song(id) => *id as u32 ^ (*id >> 32) as u32,
        LibraryTarget::Collection(Collection::Playlist(id)) => {
            (*id as u32 ^ (*id >> 32) as u32) | 1 << 31
        }
        LibraryTarget::Collection(_) => 0x7FFF_FFFF,
        _ => 0,
    }
}

unsafe fn make_row(list: Obj, page_index: usize, index: usize, y: i32) -> Option<Row> {
    let width = L.row_width(&M);
    let object = unsafe { lv::container(list, L.side, y, width, L.row_height) };
    if object.is_null() {
        return None;
    }
    unsafe {
        lv::style(
            object,
            &[
                (prop::RADIUS, 14),
                (prop::BG_COLOR, WHITE),
                (prop::BG_OPA, 0),
            ],
        );
        lv::clear_flag(object, flag::SCROLLABLE);
        lv::add_flag(object, flag::CLICKABLE);
        press_events(page_index, object, index);
        let art = ArtSlot::create(object, L.thumb.x, L.thumb.y);
        let text_w = width - L.text_x - 10;
        let title = lv::label(
            object,
            L.text_x,
            L.text_y,
            text_w,
            M.small_line(),
            false,
            1,
            lv::LONG_DOT,
            255,
        );
        let artist = lv::label(
            object,
            L.text_x,
            L.artist_y(&M),
            text_w,
            M.small_line(),
            false,
            1,
            lv::LONG_DOT,
            SECONDARY,
        );
        let separator = lv::container(object, L.text_x, L.row_height - 1, width - L.text_x - 8, 1);
        lv::style(separator, &[(prop::BG_COLOR, WHITE), (prop::BG_OPA, 40)]);
        lv::clear_flag(separator, flag::CLICKABLE | flag::SCROLLABLE);
        Some(Row {
            object,
            art,
            title,
            artist,
            separator,
            hash: 0,
        })
    }
}

pub fn transport_label(transport: Transport) -> &'static str {
    match transport {
        Transport::Idle => "未播放",
        Transport::Loading => "正在打开",
        Transport::Playing => "播放中",
        Transport::Paused => "已暂停",
        Transport::Failed => "播放失败",
    }
}
