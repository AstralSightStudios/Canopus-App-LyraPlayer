//! Page backend: the firmware's page manager creates, pauses, resumes and
//! destroys the two pages; each page owns custom LVGL objects drawn from the
//! app state. LVGL is only ever touched from the page-owner thread (create,
//! resume, the refresh timer and widget events) -- never from the Bluetooth
//! thread.

use core::ffi::c_void;

use canopus_target_private::*;

use super::native_app::{APP_ID, PAGE_COUNT, PAGE_LIBRARY, live_page_ptr};

pub mod assets;
pub mod library;
pub mod lv;
pub mod player;

/// The refresh timer ticks every 100 ms: the library poll, the progress bar,
/// the lyric highlight and the lyric scroll all derive from it.
const REFRESH_PERIOD_MS: u32 = 100;

struct Slot {
    timer: *mut c_void,
    active: bool,
    interactive: bool,
}

static mut SLOTS: [Slot; PAGE_COUNT] = [const {
    Slot {
        timer: core::ptr::null_mut(),
        active: false,
        interactive: false,
    }
}; PAGE_COUNT];

fn slot(index: usize) -> &'static mut Slot {
    // SAFETY: the page lifecycle is serialized on the UI thread and `index`
    // is checked by every caller.
    unsafe { &mut *core::ptr::addr_of_mut!(SLOTS).cast::<Slot>().add(index) }
}

extern "C" fn refresh_timer(timer: *mut c_void) {
    if timer.is_null() {
        return;
    }
    for index in 0..PAGE_COUNT {
        let slot = slot(index);
        if slot.timer != timer {
            continue;
        }
        if slot.active && slot.interactive {
            super::ui_maintenance_tick();
            match index {
                PAGE_LIBRARY => library::refresh(),
                _ => player::refresh(),
            }
        }
        return;
    }
}

pub fn page_create(index: usize, root: *mut c_void) -> i32 {
    if index >= PAGE_COUNT || root.is_null() {
        return -1;
    }
    assets::ensure();
    let result = match index {
        PAGE_LIBRARY => library::create(root),
        _ => player::create(root),
    };
    if result != 0 {
        return result;
    }
    let slot = slot(index);
    slot.active = true;
    slot.interactive = true;
    slot.timer = unsafe {
        lv_timer_start(refresh_timer, REFRESH_PERIOD_MS, index as *mut c_void)
    };
    if slot.timer.is_null() {
        destroy_page(index);
        return -1;
    }
    0
}

unsafe fn lv_timer_start(
    callback: extern "C" fn(*mut c_void),
    period: u32,
    data: *mut c_void,
) -> *mut c_void {
    unsafe { lvx_timer_create(callback, period, data) }
}

pub fn page_resume(index: usize) -> i32 {
    if index >= PAGE_COUNT || !slot(index).active {
        return -1;
    }
    slot(index).interactive = true;
    if super::sync_resumed_page(index) != 0 {
        return -1;
    }
    match index {
        PAGE_LIBRARY => library::resume(),
        _ => player::resume(),
    }
    0
}

pub fn page_pause(index: usize) -> i32 {
    if index >= PAGE_COUNT {
        return -1;
    }
    slot(index).interactive = false;
    0
}

pub fn page_destroy(index: usize) -> i32 {
    if index >= PAGE_COUNT {
        return -1;
    }
    destroy_page(index);
    0
}

fn destroy_page(index: usize) {
    let slot = slot(index);
    slot.active = false;
    slot.interactive = false;
    if !slot.timer.is_null() {
        unsafe { lvx_timer_delete(slot.timer) };
        slot.timer = core::ptr::null_mut();
    }
    match index {
        PAGE_LIBRARY => library::destroy(),
        _ => player::destroy(),
    }
}

pub fn navigate(page_index: usize) {
    let key = ((APP_ID as u32) << 16) | page_index as u32;
    unsafe { activity_navigate(key, 0, 0, 0) };
}

/// Finishes a page so the one below resumes. Band 11's `page_finish` takes
/// the page key (`app << 16 | page`) in the argument register; Band 10 Pro's
/// takes the page descriptor.
pub fn back(page_index: usize) {
    let argument = if lv::BAND_11 {
        let key = ((APP_ID as u32) << 16) | page_index as u32;
        key as usize as *mut firmware_page_descriptor
    } else {
        live_page_ptr(page_index)
    };
    unsafe { activity_finish(argument) };
}

pub fn page_accepts_input(index: usize) -> bool {
    index < PAGE_COUNT && slot(index).active && slot(index).interactive
}

/// Widget events carry `(page << 8) | widget` as their cookie.
pub fn cookie(page: usize, widget: usize) -> usize {
    (page << 8) | widget
}

pub fn split_cookie(cookie: usize) -> (usize, usize) {
    (cookie >> 8, cookie & 0xFF)
}
