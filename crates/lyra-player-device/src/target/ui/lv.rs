//! Thin helpers over the firmware's LVGL v9.0 entry points.
//!
//! The numbers below are LVGL v9.0's: the style property ids were checked
//! against the firmware's own `lv_obj_set_style_*` wrappers (each is
//! `lv_obj_set_local_style_prop(obj, ID, value, selector)`), the object flags
//! against `lv_obj_class` defaults and the event codes against the stock
//! callbacks. Everything here runs on the page-owner (UI) thread only.

use alloc::vec::Vec;
use core::ffi::c_void;

use canopus_target_private::*;
use lyra_player_core::Rgb;

pub type Obj = *mut c_void;

pub const NULL: Obj = core::ptr::null_mut();

pub const BAND_11: bool = cfg!(any(
    feature = "target-xiaomi-band-11-4-100-139",
    feature = "target-xiaomi-band-11-4-100-155",
    feature = "target-xiaomi-band-11-4-100-179"
));

pub mod flag {
    pub const CLICKABLE: u32 = 1 << 1;
    pub const SCROLLABLE: u32 = 1 << 4;
    pub const SCROLL_ONE: u32 = 1 << 7;
    pub const SNAPPABLE: u32 = 1 << 12;
}

pub mod prop {
    pub const RADIUS: u32 = 12;
    pub const BG_COLOR: u32 = 28;
    pub const BG_OPA: u32 = 29;
    pub const BG_GRAD_DIR: u32 = 32;
    pub const BG_MAIN_STOP: u32 = 33;
    pub const BG_GRAD_STOP: u32 = 34;
    pub const BG_GRAD_COLOR: u32 = 35;
    pub const BG_MAIN_OPA: u32 = 36;
    pub const BG_GRAD_OPA: u32 = 37;
    pub const CLIP_CORNER: u32 = 45;
    pub const BORDER_WIDTH: u32 = 48;
    pub const BORDER_COLOR: u32 = 49;
    pub const BORDER_OPA: u32 = 50;
    pub const SHADOW_WIDTH: u32 = 60;
    pub const SHADOW_COLOR: u32 = 61;
    pub const SHADOW_OPA: u32 = 62;
    pub const SHADOW_OFFSET_Y: u32 = 65;
    /// Signed: a negative spread shrinks the shadow.
    pub const SHADOW_SPREAD: u32 = 66;
    pub const IMAGE_OPA: u32 = 68;
    pub const TEXT_COLOR: u32 = 88;
    pub const TEXT_OPA: u32 = 89;
    pub const TEXT_ALIGN: u32 = 94;
    /// Opacity of the whole object (background, shadow, image and text alike).
    pub const OPA: u32 = 95;
    pub const TRANSLATE_X: u32 = 106;
    pub const TRANSFORM_SCALE_X: u32 = 108;
    pub const TRANSFORM_SCALE_Y: u32 = 109;
    pub const TRANSFORM_PIVOT_X: u32 = 111;
    pub const TRANSFORM_PIVOT_Y: u32 = 112;
}

pub mod align {
    pub const TOP_LEFT: u32 = 1;
    pub const TOP_MID: u32 = 2;
    pub const CENTER: u32 = 9;
}

pub mod dir {
    pub const HOR: u32 = 3;
    pub const VER: u32 = 12;
}

pub mod snap {
    pub const CENTER: u32 = 3;
}

pub mod event {
    pub const PRESSED: u32 = 1;
    pub const PRESS_LOST: u32 = 3;
    pub const CLICKED: u32 = 7;
    pub const RELEASED: u32 = if super::BAND_11 { 8 } else { 2 };
    pub const SCROLL_BEGIN: u32 = 9;
    pub const SCROLL_END: u32 = 11;
    pub const SCROLL: u32 = 12;
}

/// `LV_SIZE_CONTENT`: the object sizes itself to its content.
pub const SIZE_CONTENT: i32 = (1 << 29) | 2001;
pub const SCROLLBAR_OFF: u32 = 0;

/// Milliseconds on the firmware's monotonic clock.
pub fn now_ms() -> u32 {
    let ctx = crate::module::context();
    unsafe { ((*(*ctx).time).monotonic_ms)(ctx as *const _) }
}

/// A slice of one refresh tick that page building may spend before it must hand the UI thread
/// back: touch handling, animation and the progress bar all run on it too.
pub struct Budget(u32);

impl Budget {
    pub fn start() -> Self {
        Self(now_ms())
    }

    pub fn spent(&self, limit_ms: u32) -> bool {
        now_ms().wrapping_sub(self.0) >= limit_ms
    }
}

/// A NUL-terminated copy of `text` for the firmware's C string parameters.
pub fn c_string(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend(text.bytes().filter(|byte| *byte != 0));
    bytes.push(0);
    bytes
}

/// Applies local style values to the main part of an object.
///
/// Every property of an object is set once, at creation, in ascending id order;
/// later changes only overwrite values that are already present, which is the
/// cheap path of `lv_style_set_prop` (no allocation, no array move).
pub unsafe fn style(object: Obj, props: &[(u32, u32)]) {
    let mut last = 0;
    for &(id, value) in props {
        debug_assert!(id > last, "style properties must ascend");
        last = id;
        unsafe { lvx_object_set_local_style_u32(object, id, value, 0) };
    }
}

pub unsafe fn set_style(object: Obj, id: u32, value: u32) {
    unsafe { lvx_object_set_local_style_u32(object, id, value, 0) };
}

/// Keeps Band 10 Pro's stock scroll bar from appearing on a container that the app scrolls
/// itself (the lyrics follow the song: the bar would flash at every line).
///
/// Containers there are the stock page-content widget, which draws its own bar whenever a
/// `SCROLL_BEGIN` arrives while a mode byte at +60 is 2; `LV_SCROLLBAR_MODE_OFF` and the scrollbar
/// part's styles do not reach it. The constructor stores `0x015B0102` at +60 (mode 2, bar shape 1,
/// then two angles); the 036 and 043 constructors are identical but for their calls. Band 11
/// containers are plain `lv_obj`s, far smaller than that, so it is never touched there.
pub unsafe fn quiet_scroll_bar(object: Obj) {
    if !BAND_11 && !object.is_null() {
        unsafe { object.cast::<u8>().add(60).write(0) };
    }
}

/// A bare container: no theme style, not clickable until asked.
pub unsafe fn container(parent: Obj, x: i32, y: i32, w: i32, h: i32) -> Obj {
    let object = unsafe { lvx_object_create(parent) };
    if object.is_null() {
        return object;
    }
    unsafe {
        lvx_object_remove_style_all(object);
        if !BAND_11 {
            // Band 10 Pro keeps the stock page-content look (opaque ground) since it cannot strip styles.
            lvx_object_set_local_style_u32(object, prop::BG_OPA, 0, 0);
        }
        lvx_object_set_size(object, w, h);
        lvx_object_align(object, align::TOP_LEFT, x, y);
    }
    object
}

pub unsafe fn place(object: Obj, x: i32, y: i32) {
    unsafe { lvx_object_align(object, align::TOP_LEFT, x, y) };
}

pub unsafe fn resize(object: Obj, w: i32, h: i32) {
    unsafe { lvx_object_set_size(object, w, h) };
}

pub unsafe fn show(object: Obj, visible: bool) {
    if !object.is_null() {
        unsafe { lvx_set_hidden(object, u32::from(!visible)) };
    }
}

pub unsafe fn add_flag(object: Obj, flags: u32) {
    unsafe { lvx_object_add_flag(object, flags) };
}

pub unsafe fn clear_flag(object: Obj, flags: u32) {
    unsafe { lvx_object_clear_flag(object, flags) };
}

pub unsafe fn on_event(object: Obj, callback: LvxEventCallback, code: u32, cookie: usize) {
    unsafe { lvx_event_add(object, callback, code, cookie as *mut c_void) };
}

pub fn rgb(color: Rgb) -> u32 {
    color.0 & 0x00FF_FFFF
}

/// Size of an object after its last layout pass.
pub unsafe fn measured(object: Obj) -> (i32, i32, i32, i32) {
    let [x1, y1, x2, y2] = unsafe { lvx_object_coords(object) };
    (x1, y1, x2 - x1 + 1, y2 - y1 + 1)
}

/// A text label in one of the firmware's two MiSans sizes.
///
/// `align` is `LV_TEXT_ALIGN_*` (1 left, 2 centre, 3 right); `long_mode` is
/// `LV_LABEL_LONG_*` (0 wrap, 1 dot, 3 circular scroll, 4 clip). A height of
/// [`SIZE_CONTENT`] lets a wrapping label grow with its text.
pub unsafe fn label(
    parent: Obj,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    large: bool,
    align: u32,
    long_mode: u32,
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
        style(
            label,
            &[
                (prop::TEXT_COLOR, 0xFF_FFFF),
                (prop::TEXT_OPA, opa),
                (prop::TEXT_ALIGN, align),
            ],
        );
        if long_mode != 0 {
            lvx_label_set_long_mode(label, long_mode);
        }
        lvx_object_set_size(label, w, h);
        lvx_object_align(label, align::TOP_LEFT, x, y);
        lvx_object_clear_flag(label, flag::CLICKABLE);
    }
    label
}

pub const LONG_DOT: u32 = 1;
pub const LONG_CIRCULAR: u32 = 3;

pub unsafe fn set_text(label: Obj, text: &str) {
    if label.is_null() {
        return;
    }
    let text = c_string(text);
    unsafe { lvx_label_set_text(label, text.as_ptr()) };
}
