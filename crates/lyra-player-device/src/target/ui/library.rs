//! The song list: a title, a "now playing" card in the palette of the current track and
//! a scrolling list of rounded rows, each with an accent bar in its track's colour.

use alloc::{format, vec::Vec};

use canopus_target_private::*;
use lyra_player_core::{
    Palette, Rgb,
    layout::{self, ListMetrics, Metrics},
};

use super::{
    assets::Glyph,
    cookie,
    lv::{self, Obj, align, dir, event, flag, prop},
    split_cookie,
};
use crate::target::{LibraryFrame, LibraryTarget, native_app::PAGE_LIBRARY};

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
const W_CARD: usize = 0xFE;
const ROW_FILL: u32 = 22;
const ROW_PRESSED: u32 = 48;

struct Row {
    object: Obj,
    accent: Obj,
    title: Obj,
    artist: Obj,
    hash: u32,
}

struct LibraryPage {
    count: Obj,
    card: Obj,
    card_title: Obj,
    card_state: Obj,
    list: Obj,
    spacer: Obj,
    /// How many rows the spacer was last placed for.
    spacer_rows: usize,
    empty_glyph: Obj,
    empty_text: Obj,
    rows: Vec<Row>,
    targets: Vec<LibraryTarget>,
    card_shown: bool,
    card_hash: u32,
    /// The `library_key` last drawn; `NOT_SHOWN` forces the next refresh to draw (an empty library
    /// has key 0, so 0 cannot mean "nothing drawn yet").
    shown_key: u64,
}

const NOT_SHOWN: u64 = u64::MAX;

static mut PAGE: Option<LibraryPage> = None;

fn page() -> Option<&'static mut LibraryPage> {
    // SAFETY: UI thread only.
    unsafe { (*core::ptr::addr_of_mut!(PAGE)).as_mut() }
}

fn accent_of(palette: Palette) -> u32 {
    lv::rgb(palette.top.mix(Rgb(0xFFFFFF), 96))
}

pub fn create(root: Obj) -> i32 {
    let bg = unsafe { lv::container(root, 0, 0, M.width, M.height) };
    if bg.is_null() {
        return -1;
    }
    unsafe {
        lv::style(
            bg,
            &[
                (prop::BG_COLOR, 0x1A1D29),
                (prop::BG_OPA, 255),
                (prop::BG_GRAD_DIR, 1),
                (prop::BG_MAIN_STOP, 0),
                (prop::BG_GRAD_STOP, 255),
                (prop::BG_GRAD_COLOR, 0x08090D),
            ],
        );
        lv::clear_flag(bg, flag::CLICKABLE | flag::SCROLLABLE);
        let header = lv::label(root, L.header.x, L.header.y, L.header.w, L.header.h, true, 1, 0, 255);
        lv::set_text(header, "音乐");
    }
    let count = unsafe { lv::label(root, L.count.x, L.count.y, L.count.w, L.count.h, false, 3, 0, 140) };

    // --- now playing card -----------------------------------------------------------------
    let card = unsafe { lv::container(root, L.card.x, L.card.y, L.card.w, L.card.h) };
    let (card_title, card_state) = if card.is_null() {
        (lv::NULL, lv::NULL)
    } else {
        unsafe {
            let palette = Palette::default();
            lv::style(
                card,
                &[
                    (prop::RADIUS, 24),
                    (prop::BG_COLOR, lv::rgb(palette.top)),
                    (prop::BG_OPA, 255),
                    (prop::BG_GRAD_DIR, 1),
                    (prop::BG_MAIN_STOP, 0),
                    (prop::BG_GRAD_STOP, 255),
                    (prop::BG_GRAD_COLOR, lv::rgb(palette.bottom)),
                ],
            );
            lv::clear_flag(card, flag::SCROLLABLE);
            lv::add_flag(card, flag::CLICKABLE);
            let cookie = cookie(PAGE_LIBRARY, W_CARD);
            for code in [event::CLICKED] {
                lv::on_event(card, row_event, code, cookie);
            }
            lv::show(card, false);
            let glyph = lvx_image_create(card);
            if !glyph.is_null() {
                lvx_image_set_src(glyph, Glyph::Note.path().as_ptr().cast());
                lv::style(glyph, &[(prop::IMAGE_OPA, 230)]);
                lv::clear_flag(glyph, flag::CLICKABLE);
                lvx_object_align(glyph, align::TOP_LEFT, 12, (L.card.h - 48) / 2);
            }
            let text_x = 70;
            let text_w = L.card.w - text_x - 14;
            let title = lv::label(card, text_x, 2, text_w, M.large_line(), true, 1, lv::LONG_CIRCULAR, 255);
            let state = lv::label(card, text_x, 2 + M.large_line() - 2, text_w, M.small_line(), false, 1, 0, 170);
            (title, state)
        }
    };

    // --- list -----------------------------------------------------------------------------
    let list = unsafe { lv::container(root, 0, L.list_top, M.width, M.height - L.list_top) };
    if list.is_null() {
        return -1;
    }
    unsafe {
        lvx_object_set_scroll_dir(list, dir::VER);
        lvx_object_set_scrollbar_mode(list, lv::SCROLLBAR_OFF);
        lv::clear_flag(list, flag::CLICKABLE);
    }
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
            140,
        );
        lv::show(text, false);
        text
    };
    unsafe {
        PAGE = Some(LibraryPage {
            count,
            card,
            card_title,
            card_state,
            list,
            spacer: lv::NULL,
            spacer_rows: usize::MAX,
            empty_glyph,
            empty_text,
            rows: Vec::new(),
            targets: Vec::new(),
            card_shown: false,
            card_hash: 0,
            shown_key: NOT_SHOWN,
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
        page.shown_key = NOT_SHOWN;
    }
    refresh();
}

extern "C" fn row_event(event: *mut core::ffi::c_void) {
    if event.is_null() || !super::page_accepts_input(PAGE_LIBRARY) {
        return;
    }
    let code = unsafe { lvx_event_get_code(event) };
    let (page_index, index) = split_cookie(unsafe { lvx_event_get_user_data(event) });
    if page_index != PAGE_LIBRARY {
        return;
    }
    let Some(page) = page() else { return };
    let object = if index == W_CARD {
        page.card
    } else {
        match page.rows.get(index) {
            Some(row) => row.object,
            None => return,
        }
    };
    match code {
        event::PRESSED => unsafe { lv::set_style(object, prop::BG_OPA, ROW_PRESSED) },
        event::RELEASED | event::PRESS_LOST if index != W_CARD => unsafe {
            lv::set_style(object, prop::BG_OPA, ROW_FILL)
        },
        event::CLICKED => {
            if index != W_CARD {
                unsafe { lv::set_style(object, prop::BG_OPA, ROW_FILL) };
            }
            let target = if index == W_CARD {
                Some(LibraryTarget::NowPlaying)
            } else {
                page.targets.get(index).copied()
            };
            if let Some(target) = target {
                crate::target::handle_library_event(target);
            }
        }
        _ => {}
    }
}

pub fn refresh() {
    let Some(page) = page() else { return };
    let key = crate::target::library_key();
    if key == page.shown_key {
        return;
    }
    let frame = crate::target::library_frame();
    page.shown_key = key;
    // Rows that did not fit this tick's budget are built by the next tick.
    if !unsafe { page.show(frame) } {
        page.shown_key = NOT_SHOWN;
    }
}

fn hash_text(mut hash: u32, text: &str) -> u32 {
    for byte in text.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    hash
}

impl LibraryPage {
    /// Returns whether every row exists yet.
    unsafe fn show(&mut self, frame: LibraryFrame) -> bool {
        let complete = unsafe {
            lv::set_text(self.count, &format!("{} 首", frame.tracks.len()));
            self.show_card(&frame);
            self.show_rows(&frame)
        };
        unsafe {
            let empty = frame.tracks.is_empty();
            lv::show(self.empty_glyph, empty);
            lv::show(self.empty_text, empty);
            if empty {
                lv::set_text(
                    self.empty_text,
                    if frame.import_supported {
                        "还没有音乐\n用 AstroBox\n导入歌曲"
                    } else {
                        "此固件暂不支持\n无线导入音乐"
                    },
                );
            }
        }
        complete
    }

    unsafe fn show_card(&mut self, frame: &LibraryFrame) {
        let Some(playing) = &frame.now_playing else {
            if self.card_shown {
                self.card_shown = false;
                unsafe {
                    lv::show(self.card, false);
                    self.place_list(L.list_top);
                }
            }
            return;
        };
        let hash = hash_text(hash_text(playing.palette.top.0 ^ playing.palette.bottom.0, &playing.title), playing.state);
        unsafe {
            if hash != self.card_hash {
                self.card_hash = hash;
                lv::set_text(self.card_title, &playing.title);
                lv::set_text(self.card_state, playing.state);
                lv::set_style(self.card, prop::BG_COLOR, lv::rgb(playing.palette.top));
                lv::set_style(self.card, prop::BG_GRAD_COLOR, lv::rgb(playing.palette.bottom));
            }
            if !self.card_shown {
                self.card_shown = true;
                lv::show(self.card, true);
                self.place_list(L.card.bottom() + 8);
            }
        }
    }

    unsafe fn place_list(&mut self, top: i32) {
        unsafe {
            lv::place(self.list, 0, top);
            lv::resize(self.list, M.width, M.height - top);
        }
    }

    unsafe fn show_rows(&mut self, frame: &LibraryFrame) -> bool {
        let budget = lv::Budget::start();
        let mut complete = true;
        self.targets.clear();
        let width = M.width - 2 * L.side;
        let text_w = width - L.text_x - 12;
        for (index, track) in frame.tracks.iter().take(MAX_ROWS).enumerate() {
            self.targets.push(LibraryTarget::Song(track.id));
            let playing = frame.current == Some(track.id);
            let hash = hash_text(hash_text(track.palette.top.0, &track.name), &track.artist) ^ u32::from(playing);
            if index == self.rows.len() {
                if index > 0 && budget.spent(ROW_BUDGET_MS) {
                    self.targets.pop();
                    complete = false;
                    break;
                }
                let y = index as i32 * (L.row_height + L.row_gap);
                let Some(row) = (unsafe { self.make_row(y, width, text_w, track.palette) }) else {
                    self.targets.pop();
                    break;
                };
                self.rows.push(row);
            }
            let row = &mut self.rows[index];
            if row.hash != hash {
                row.hash = hash;
                unsafe {
                    lv::set_text(row.title, &track.name);
                    lv::set_text(row.artist, &track.artist);
                    lv::set_style(row.accent, prop::BG_COLOR, accent_of(track.palette));
                    lv::set_style(row.accent, prop::BG_OPA, if playing { 255 } else { 150 });
                    lvx_label_set_long_mode(row.title, if playing { lv::LONG_CIRCULAR } else { lv::LONG_DOT });
                }
            }
            unsafe { lv::show(row.object, true) };
        }
        for (index, row) in self.rows.iter().enumerate() {
            if index >= self.targets.len() {
                unsafe { lv::show(row.object, false) };
            }
        }
        // Extend the content a little past the last row so it can scroll clear of the
        // bottom edge.
        if self.spacer_rows != self.targets.len() {
            self.spacer_rows = self.targets.len();
            unsafe {
                if !self.spacer.is_null() {
                    lvx_object_delete(self.spacer);
                }
                let end = self.targets.len() as i32 * (L.row_height + L.row_gap) + 24;
                self.spacer = lv::container(self.list, 0, end, 1, 1);
            }
        }
        complete
    }

    unsafe fn make_row(&mut self, y: i32, width: i32, text_w: i32, palette: Palette) -> Option<Row> {
        let index = self.rows.len();
        let object = unsafe { lv::container(self.list, L.side, y, width, L.row_height) };
        if object.is_null() {
            return None;
        }
        unsafe {
            lv::style(
                object,
                &[(prop::RADIUS, 22), (prop::BG_COLOR, 0xFFFFFF), (prop::BG_OPA, ROW_FILL)],
            );
            lv::clear_flag(object, flag::SCROLLABLE);
            lv::add_flag(object, flag::CLICKABLE);
            let cookie = cookie(PAGE_LIBRARY, index);
            for code in [event::CLICKED, event::PRESSED, event::RELEASED, event::PRESS_LOST] {
                lv::on_event(object, row_event, code, cookie);
            }
            let accent = lv::container(object, L.accent.x, L.accent.y, L.accent.w, L.accent.h);
            lv::style(
                accent,
                &[(prop::RADIUS, 3), (prop::BG_COLOR, accent_of(palette)), (prop::BG_OPA, 150)],
            );
            lv::clear_flag(accent, flag::CLICKABLE | flag::SCROLLABLE);
            let title = lv::label(object, L.text_x, 4, text_w, M.large_line(), true, 1, lv::LONG_DOT, 255);
            let artist = lv::label(
                object,
                L.text_x,
                4 + M.large_line() - 2,
                text_w,
                M.small_line(),
                false,
                1,
                lv::LONG_DOT,
                140,
            );
            Some(Row {
                object,
                accent,
                title,
                artist,
                hash: 0,
            })
        }
    }
}

pub fn transport_label(transport: lyra_player_core::view::Transport) -> &'static str {
    use lyra_player_core::view::Transport;
    match transport {
        Transport::Idle => "未播放",
        Transport::Loading => "正在打开",
        Transport::Playing => "播放中",
        Transport::Paused => "已暂停",
        Transport::Failed => "播放失败",
    }
}
