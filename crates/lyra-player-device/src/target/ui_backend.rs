//! Stock LVX renderer: maps committed semantic snapshots onto firmware list
//! rows, labels, page titles and content, and dispatches row/back events with
//! generation-checked bindings. Firmware widget pointers live only here and are
//! cleared on page destruction. LVX is never touched from Bluetooth or timer
//! callbacks — only from the page owner thread (create/resume/row events).

use alloc::vec::Vec;

use canopus_target_private::*;
use canopus_ui_core::{NodeKind, Snapshot};
use lyra_player_core::ui::{PLAYER_BACKGROUND_KEY, PLAYER_COVER_KEY};

use super::native_app::{APP_ID, PAGE_COUNT, PAGE_OVERVIEW, PAGE_PLAYER, page_descriptor_ptr};
use super::storage;

static EMPTY_TEXT: [u8; 1] = [0];
const REFRESH_PERIOD_MS: u32 = 100;
const PLAYER_CONTENT_HEIGHT: i32 = 480;
const PLAYER_MEDIA_TOP: i32 = 72;
const PLAYER_TITLE_TOP: i32 = 270;
const PLAYER_AUTHOR_TOP: i32 = 318;
const PLAYER_CONTROL_TOP: i32 = 382;
#[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
const PLAYER_CONTROL_SIZE: i32 = 192;
#[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
const PLAYER_CONTROL_PRESSED_SIZE: i32 = 100;
#[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
const PLAYER_CONTROL_SIZE: i32 = 44;
#[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
const PLAYER_CONTROL_PRESSED_SIZE: i32 = 40;
const CONTROL_ANIMATION_PERIOD_MS: u32 = 16;
const CONTROL_ANIMATION_STEPS: u32 = 8;
const ROW_IMAGE_BUTTON: u8 = 4;
const EVENT_ALL: u32 = 0;
const EVENT_PRESSED: u32 = 1;
const EVENT_RELEASED: u32 = 2;
const EVENT_PRESS_LOST: u32 = 3;
const CONTROL_PREVIOUS_ICON: &[u8] = b"/data/canopus/lyra-previous.bin\0";
const CONTROL_PLAY_ICON: &[u8] = b"/data/canopus/lyra-play.bin\0";
const CONTROL_PAUSE_ICON: &[u8] = b"/data/canopus/lyra-pause.bin\0";
const CONTROL_NEXT_ICON: &[u8] = b"/data/canopus/lyra-next.bin\0";

#[derive(Copy, Clone)]
#[repr(C)]
struct Binding {
    generation: u32,
    key: u32,
    event_id: u32,
    enabled: bool,
}

struct PageBackend {
    root: *mut core::ffi::c_void,
    content_root: *mut core::ffi::c_void,
    page_title: *mut core::ffi::c_void,
    refresh_timer: *mut core::ffi::c_void,
    rows: [*mut core::ffi::c_void; UI_MAX_ROWS],
    #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
    control_images: [*mut core::ffi::c_void; UI_MAX_ROWS],
    labels: [*mut core::ffi::c_void; UI_MAX_LABELS],
    images: [*mut core::ffi::c_void; 2],
    image_paths: [Option<Vec<u8>>; 2],
    background: *mut core::ffi::c_void,
    background_path: Option<Vec<u8>>,
    bars: [*mut core::ffi::c_void; 4],
    image_hashes: [u32; 2],
    background_hash: u32,
    bar_hashes: [u32; 4],
    row_kinds: [u8; UI_MAX_ROWS],
    row_keys: [u32; UI_MAX_ROWS],
    row_hashes: [u32; UI_MAX_ROWS],
    anim_timers: [*mut core::ffi::c_void; UI_MAX_ROWS],
    anim_sizes: [i32; UI_MAX_ROWS],
    anim_from: [i32; UI_MAX_ROWS],
    anim_to: [i32; UI_MAX_ROWS],
    anim_progress: [u32; UI_MAX_ROWS],
    animating: [bool; UI_MAX_ROWS],
    label_hashes: [u32; UI_MAX_LABELS],
    bindings: [Binding; UI_MAX_ROWS],
    row_count: u32,
    label_count: u32,
    rendered_generation: u32,
    layout_hash: u32,
    layout_count: u32,
    page_index: u8,
    layout_valid: bool,
    active: bool,
    interactive: bool,
    refresh_failed: bool,
}

const fn empty_backend() -> PageBackend {
    PageBackend {
        root: core::ptr::null_mut(),
        content_root: core::ptr::null_mut(),
        page_title: core::ptr::null_mut(),
        refresh_timer: core::ptr::null_mut(),
        rows: [core::ptr::null_mut(); UI_MAX_ROWS],
        #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
        control_images: [core::ptr::null_mut(); UI_MAX_ROWS],
        labels: [core::ptr::null_mut(); UI_MAX_LABELS],
        images: [core::ptr::null_mut(); 2],
        image_paths: [None, None],
        background: core::ptr::null_mut(),
        background_path: None,
        bars: [core::ptr::null_mut(); 4],
        image_hashes: [0; 2],
        background_hash: 0,
        bar_hashes: [0; 4],
        row_kinds: [0; UI_MAX_ROWS],
        row_keys: [0; UI_MAX_ROWS],
        row_hashes: [0; UI_MAX_ROWS],
        anim_timers: [core::ptr::null_mut(); UI_MAX_ROWS],
        anim_sizes: [PLAYER_CONTROL_SIZE; UI_MAX_ROWS],
        anim_from: [PLAYER_CONTROL_SIZE; UI_MAX_ROWS],
        anim_to: [PLAYER_CONTROL_SIZE; UI_MAX_ROWS],
        anim_progress: [0; UI_MAX_ROWS],
        animating: [false; UI_MAX_ROWS],
        label_hashes: [0; UI_MAX_LABELS],
        bindings: [Binding {
            generation: 0,
            key: 0,
            event_id: 0,
            enabled: false,
        }; UI_MAX_ROWS],
        row_count: 0,
        label_count: 0,
        rendered_generation: 0,
        layout_hash: 0,
        layout_count: 0,
        page_index: 0,
        layout_valid: false,
        active: false,
        interactive: false,
        refresh_failed: false,
    }
}

static mut PAGES: [PageBackend; PAGE_COUNT] = [const { empty_backend() }; PAGE_COUNT];

unsafe fn apply_misans(object: *mut core::ffi::c_void) {
    if !object.is_null() {
        let _ = unsafe {
            lvx_style_apply(
                object,
                STYLE_MISANS_DEMIBOLD_32 as *const core::ffi::c_void,
                255,
                0,
            )
        };
    }
}

unsafe fn apply_author_misans(object: *mut core::ffi::c_void) {
    if !object.is_null() {
        #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
        let _ = unsafe {
            lvx_style_apply(
                object,
                STYLE_MISANS_REGULAR_24 as *const core::ffi::c_void,
                255,
                0,
            )
        };
        #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
        unsafe {
            apply_misans(object);
        }
    }
}

fn wrapped_label_height(text: &str) -> i32 {
    const HALF_WIDTH_UNITS_PER_LINE: u32 = 20;
    const LINE_HEIGHT: i32 = 44;

    let mut lines = 1u32;
    let mut units = 0u32;
    for character in text.chars() {
        if character == '\n' {
            lines = lines.saturating_add(1);
            units = 0;
            continue;
        }
        let width = if character.is_ascii() { 1 } else { 2 };
        if units != 0 && units.saturating_add(width) > HALF_WIDTH_UNITS_PER_LINE {
            lines = lines.saturating_add(1);
            units = 0;
        }
        units = units.saturating_add(width);
    }
    (lines.min(6) as i32).saturating_mul(LINE_HEIGHT)
}

fn page_backend(index: usize) -> &'static mut PageBackend {
    // SAFETY: page indices are validated by every caller against PAGE_COUNT;
    // the firmware serializes page lifecycle callbacks on the UI thread.
    // `addr_of_mut!` avoids the `static_mut_refs` deny lint.
    unsafe {
        &mut *core::ptr::addr_of_mut!(PAGES)
            .cast::<PageBackend>()
            .add(index)
    }
}

extern "C" fn refresh_timer(timer: *mut core::ffi::c_void) {
    if timer.is_null() {
        return;
    }
    for page_index in 0..PAGE_COUNT {
        let backend = page_backend(page_index);
        if backend.refresh_timer != timer {
            continue;
        }
        if backend.active && backend.interactive {
            super::ui_maintenance_tick();
            let rendered_generation = backend.rendered_generation;
            if super::rebuild_if_changed(page_index, rendered_generation) != 0 {
                backend.refresh_failed = true;
            }
        }
        return;
    }
}

// ---------------------------------------------------------------------------
// Page lifecycle (delegated from native_app)
// ---------------------------------------------------------------------------

pub fn page_create(page_index: usize, root: *mut core::ffi::c_void) -> i32 {
    if page_index >= PAGE_COUNT || root.is_null() {
        return -1;
    }
    let backend = page_backend(page_index);
    // A prior destroy zeroed the backend; adopt the fresh firmware root.
    backend.root = root;
    backend.page_index = page_index as u8;
    backend.active = true;
    backend.interactive = true;
    let result = super::rebuild(page_index);
    if result != 0 {
        *page_backend(page_index) = empty_backend();
        return result;
    }
    let timer = unsafe {
        lvx_timer_create(
            refresh_timer,
            REFRESH_PERIOD_MS,
            page_index as *mut core::ffi::c_void,
        )
    };
    if timer.is_null() {
        *page_backend(page_index) = empty_backend();
        return -1;
    }
    page_backend(page_index).refresh_timer = timer;
    0
}

pub fn page_resume(page_index: usize) -> i32 {
    if page_index >= PAGE_COUNT {
        return -1;
    }
    let backend = page_backend(page_index);
    if !backend.active {
        return -1;
    }
    backend.interactive = true;
    backend.refresh_failed = false;
    if super::sync_resumed_page(page_index) != 0 {
        return -1;
    }
    super::rebuild(page_index)
}

pub fn page_pause(page_index: usize) -> i32 {
    if page_index >= PAGE_COUNT {
        return -1;
    }
    page_backend(page_index).interactive = false;
    0
}

pub fn page_destroy(page_index: usize) -> i32 {
    if page_index >= PAGE_COUNT {
        return -1;
    }
    let backend = page_backend(page_index);
    backend.active = false;
    backend.interactive = false;
    if !backend.refresh_timer.is_null() {
        unsafe { lvx_timer_delete(backend.refresh_timer) };
        backend.refresh_timer = core::ptr::null_mut();
    }
    for timer in backend.anim_timers {
        if !timer.is_null() {
            unsafe { lvx_timer_delete(timer) };
        }
    }
    // Drop every firmware widget pointer; the next create starts empty.
    *backend = empty_backend();
    0
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

pub fn navigate(page_index: usize) {
    let key = ((APP_ID as u32) << 16) | page_index as u32;
    unsafe { activity_navigate(key, 0, 0, 0) };
}

pub fn back(page_index: usize) {
    unsafe { activity_finish(page_descriptor_ptr(page_index)) };
}

// ---------------------------------------------------------------------------
// Event dispatch (firmware LVX events -> module actions)
// ---------------------------------------------------------------------------

fn encoded_cookie(page_index: usize, slot: usize) -> usize {
    (page_index << 8) | slot
}

extern "C" fn row_event(event: *mut core::ffi::c_void) {
    if event.is_null() {
        return;
    }
    // SAFETY: `event` is a firmware LVX event object from the page owner thread.
    let code = unsafe { lvx_event_get_code(event) };
    let encoded = unsafe { lvx_event_get_user_data(event) };
    let page_index = encoded >> 8;
    let row_index = encoded & 0xFF;
    if page_index >= PAGE_COUNT || row_index >= UI_MAX_ROWS {
        return;
    }
    let backend = page_backend(page_index);
    if !backend.active || !backend.interactive {
        return;
    }
    // Switch rows register on the trailing object for LV_EVENT_ALL and act only
    // on VALUE_CHANGED; rows register for CLICKED.
    if backend.row_kinds[row_index] == ROW_SWITCH {
        if code != EVENT_VALUE_CHANGED {
            return;
        }
    } else if code != EVENT_CLICKED {
        return;
    }
    let binding = backend.bindings[row_index];
    if binding.event_id == 0 || !binding.enabled {
        return;
    }
    super::handle_ui_event(
        page_index,
        binding.generation,
        binding.key,
        binding.event_id,
    );
}

fn control_x_offset(key: u32) -> i32 {
    match key {
        7 => -72,
        6 => 0,
        8 => 72,
        _ => 0,
    }
}

fn control_visual(backend: &PageBackend, slot: usize) -> *mut core::ffi::c_void {
    #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
    {
        backend.control_images[slot]
    }
    #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
    {
        backend.rows[slot]
    }
}

#[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
unsafe fn set_control_image_scale(image: *mut core::ffi::c_void, key: u32, scale: i32) {
    unsafe {
        lvx_object_set_size(image, 64, 64);
        lvx_image_set_scale(image, scale, scale);
        lvx_object_align(
            image,
            ALIGN_TOP_MID,
            control_x_offset(key),
            PLAYER_CONTROL_TOP,
        );
    }
}

#[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
unsafe fn set_control_hitbox_geometry(object: *mut core::ffi::c_void, key: u32) {
    unsafe {
        lvx_object_set_size(object, 64, 64);
        lvx_object_align(
            object,
            ALIGN_TOP_MID,
            control_x_offset(key),
            PLAYER_CONTROL_TOP,
        );
    }
}

unsafe fn set_control_geometry_size(object: *mut core::ffi::c_void, key: u32, size: i32) {
    unsafe {
        #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
        {
            set_control_image_scale(object, key, size);
        }
        #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
        lvx_object_set_size(object, size, size);
        lvx_object_align(
            object,
            ALIGN_TOP_MID,
            control_x_offset(key),
            PLAYER_CONTROL_TOP,
        );
    }
}

unsafe fn set_control_geometry(object: *mut core::ffi::c_void, key: u32, pressed: bool) {
    let size = if pressed {
        PLAYER_CONTROL_PRESSED_SIZE
    } else {
        PLAYER_CONTROL_SIZE
    };
    unsafe { set_control_geometry_size(object, key, size) };
}

fn smoothstep(progress: u32) -> u32 {
    let steps = CONTROL_ANIMATION_STEPS;
    let progress = progress.min(steps);
    let squared = progress * progress;
    3 * squared * steps - 2 * squared * progress
}

unsafe fn advance_control_animation(backend: &mut PageBackend, slot: usize) {
    if !backend.animating[slot] {
        return;
    }
    let progress = backend.anim_progress[slot].saturating_add(1);
    let eased = smoothstep(progress);
    let denominator = CONTROL_ANIMATION_STEPS * CONTROL_ANIMATION_STEPS * CONTROL_ANIMATION_STEPS;
    let from = backend.anim_from[slot];
    let to = backend.anim_to[slot];
    let size = from + ((to - from) * eased as i32) / denominator as i32;
    backend.anim_sizes[slot] = size;
    let visual = control_visual(backend, slot);
    if visual.is_null() {
        return;
    }
    unsafe {
        set_control_geometry_size(visual, backend.row_keys[slot], size);
    }
    if progress >= CONTROL_ANIMATION_STEPS {
        backend.anim_sizes[slot] = to;
        backend.anim_progress[slot] = 0;
        backend.animating[slot] = false;
    } else {
        backend.anim_progress[slot] = progress;
    }
}

extern "C" fn control_animation_timer(timer: *mut core::ffi::c_void) {
    if timer.is_null() {
        return;
    }
    for page_index in 0..PAGE_COUNT {
        let backend = page_backend(page_index);
        for slot in 0..UI_MAX_ROWS {
            if backend.anim_timers[slot] == timer {
                if backend.active && backend.row_kinds[slot] == ROW_IMAGE_BUTTON {
                    unsafe { advance_control_animation(backend, slot) };
                }
                return;
            }
        }
    }
}

fn start_control_animation(page_index: usize, slot: usize, pressed: bool) {
    if page_index >= PAGE_COUNT || slot >= UI_MAX_ROWS {
        return;
    }
    let backend = page_backend(page_index);
    if backend.rows[slot].is_null() {
        return;
    }
    if backend.anim_timers[slot].is_null() {
        unsafe { set_control_geometry(backend.rows[slot], backend.row_keys[slot], pressed) };
        return;
    }
    let target = if pressed {
        PLAYER_CONTROL_PRESSED_SIZE
    } else {
        PLAYER_CONTROL_SIZE
    };
    if backend.anim_sizes[slot] == target && !backend.animating[slot] {
        return;
    }
    backend.anim_from[slot] = backend.anim_sizes[slot];
    backend.anim_to[slot] = target;
    backend.anim_progress[slot] = 0;
    backend.animating[slot] = true;
}

extern "C" fn image_button_event(event: *mut core::ffi::c_void) {
    if event.is_null() {
        return;
    }
    let code = unsafe { lvx_event_get_code(event) };
    let encoded = unsafe { lvx_event_get_user_data(event) };
    let page_index = encoded >> 8;
    let slot = encoded & 0xFF;
    if page_index >= PAGE_COUNT || slot >= UI_MAX_ROWS {
        return;
    }
    let backend = page_backend(page_index);
    if !backend.active || !backend.interactive || backend.row_kinds[slot] != ROW_IMAGE_BUTTON {
        return;
    }
    match code {
        EVENT_PRESSED => start_control_animation(page_index, slot, true),
        EVENT_RELEASED | EVENT_PRESS_LOST | EVENT_CLICKED => {
            start_control_animation(page_index, slot, false)
        }
        _ => return,
    }
    if code != EVENT_CLICKED {
        return;
    }
    #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
    {
        let binding = backend.bindings[slot];
        if binding.event_id != 0 && binding.enabled {
            super::handle_ui_event(
                page_index,
                binding.generation,
                binding.key,
                binding.event_id,
            );
        }
    }
}

fn control_icon(key: u32, label: &str) -> Option<&'static [u8]> {
    match key {
        7 => Some(CONTROL_PREVIOUS_ICON),
        6 if label == "暂停" => Some(CONTROL_PAUSE_ICON),
        6 => Some(CONTROL_PLAY_ICON),
        8 => Some(CONTROL_NEXT_ICON),
        _ => None,
    }
}

extern "C" fn page_title_back(event: *mut core::ffi::c_void) {
    if event.is_null() {
        return;
    }
    // SAFETY: the title back callback is registered with the page context
    // cookie (page_index << 8).
    let encoded = unsafe { lvx_event_get_user_data(event) };
    let page_index = encoded >> 8;
    if page_index >= PAGE_COUNT || page_index == PAGE_OVERVIEW {
        return;
    }
    let backend = page_backend(page_index);
    if !backend.active || !backend.interactive {
        return;
    }
    backend.interactive = false;
    super::handle_back(page_index);
}

// ---------------------------------------------------------------------------
// Snapshot render
// ---------------------------------------------------------------------------

fn hash_word(mut hash: u32, value: u32) -> u32 {
    for byte in value.to_le_bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    hash
}

fn hash_text(mut hash: u32, text: &str) -> u32 {
    for byte in text.as_bytes() {
        hash = (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
    }
    hash_word(hash, text.len() as u32)
}

#[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
fn skip_image(_key: u32) -> bool {
    true
}

#[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
fn skip_image(key: u32) -> bool {
    key == PLAYER_BACKGROUND_KEY
}

#[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
fn sync_background(backend: &mut PageBackend, page_index: usize, snapshot: &Snapshot) -> bool {
    if page_index != PAGE_PLAYER {
        return false;
    }
    let mut source = None;
    for index in 0..snapshot.node_count as usize {
        let node = &snapshot.nodes[index];
        if node.kind() == Some(NodeKind::Image) && node.key == PLAYER_BACKGROUND_KEY {
            source = Some((snapshot.primary(node), snapshot.values[index].resource_id));
            break;
        }
    }
    let Some((path, resource_id)) = source else {
        if !backend.background.is_null() {
            unsafe { lvx_set_hidden(backend.background, 1) };
        }
        backend.background_hash = 0;
        return false;
    };
    let Some(resolved_path) = storage::resolve_path(path) else {
        if !backend.background.is_null() {
            unsafe { lvx_set_hidden(backend.background, 1) };
        }
        backend.background_hash = 0;
        return false;
    };
    if !storage::validate_lvgl_v9_image(&resolved_path, 336, 520) {
        if !backend.background.is_null() {
            unsafe { lvx_set_hidden(backend.background, 1) };
        }
        backend.background_hash = 0;
        return false;
    }
    let image_hash = hash_word(hash_text(0x811C_9DC5, &resolved_path), resource_id);
    let mut source_path = resolved_path.into_bytes();
    source_path.push(0);
    let created_now = backend.background.is_null();
    if created_now {
        backend.background = unsafe { lvx_image_create(backend.content_root) };
        if backend.background.is_null() {
            return false;
        }
    }
    let path_changed = backend.background_path.as_deref() != Some(source_path.as_slice());
    if created_now || backend.background_hash != image_hash || path_changed {
        backend.background_path = Some(source_path);
        let source = backend
            .background_path
            .as_ref()
            .map_or(core::ptr::null(), |path| path.as_ptr());
        unsafe { lvx_image_set_src(backend.background, source.cast()) };
        backend.background_hash = image_hash;
    }
    unsafe {
        lvx_object_set_size(backend.background, 336, 520);
        lvx_object_align(backend.background, ALIGN_TOP_MID, 0, 0);
        lvx_object_move_to_index(backend.background, 0);
        lvx_set_hidden(backend.background, 0);
    }
    true
}

#[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
fn sync_background(_backend: &mut PageBackend, _page_index: usize, _snapshot: &Snapshot) -> bool {
    false
}

fn layout_fingerprint(snapshot: &Snapshot) -> (u32, u32) {
    let mut hash = 0x811C_9DC5u32;
    let mut count = 0u32;
    for index in 0..snapshot.node_count as usize {
        let node = &snapshot.nodes[index];
        let marker = match node.kind() {
            Some(NodeKind::Text) => 1,
            Some(NodeKind::StatusRow) => 2,
            Some(NodeKind::Button) => 3,
            Some(NodeKind::ActionRow) => 4,
            Some(NodeKind::SwitchRow) => 5,
            Some(NodeKind::Image) if skip_image(node.key) => continue,
            Some(NodeKind::Image) => 6,
            Some(NodeKind::Progress) => 7,
            _ => continue,
        };
        hash = hash_word(hash, marker);
        hash = hash_word(hash, node.key);
        if node.kind() == Some(NodeKind::Text) {
            hash = hash_text(hash, snapshot.primary(node));
        }
        if matches!(node.kind(), Some(NodeKind::Image | NodeKind::Progress)) {
            let layout = &snapshot.layouts[index];
            hash = hash_word(hash, layout.width as u16 as u32);
            hash = hash_word(hash, layout.height as u16 as u32);
        }
        count += 1;
    }
    (hash, count)
}

fn row_content_hash(primary: &str, secondary: &str, selected: u32) -> u32 {
    let hash = hash_text(0x811C_9DC5, primary);
    let hash = hash_text(hash, secondary);
    hash_word(hash, selected)
}

fn target_row_kind(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::SwitchRow => ROW_SWITCH,
        NodeKind::Button | NodeKind::ActionRow => ROW_ACTION,
        _ => ROW_STATUS,
    }
}

unsafe fn set_row_hidden(row: *mut core::ffi::c_void, hidden: u32) {
    unsafe {
        lvx_set_hidden(row, hidden);
        let trailing = lvx_list_row_trailing(row);
        if !trailing.is_null() {
            lvx_set_hidden(trailing, hidden);
        }
    }
}

/// Band-9 (LVGL v8) trailing-kind for a row kind. Band-9 numbers differ from
/// band-10: 1 = switch, 12 = forward arrow, 0 = none.
#[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
fn b9_row_trailing(row_kind: u8) -> u8 {
    match row_kind {
        ROW_SWITCH => TRAILING_B9_SWITCH,
        ROW_ACTION => TRAILING_B9_FORWARD,
        _ => 0,
    }
}

fn snapshot_uses_row(snapshot: &Snapshot, kind: u8, key: u32) -> bool {
    for index in 0..snapshot.node_count as usize {
        let node = &snapshot.nodes[index];
        let is_row = node.kind().is_some_and(|node_kind| {
            matches!(
                node_kind,
                NodeKind::StatusRow | NodeKind::Button | NodeKind::ActionRow | NodeKind::SwitchRow
            ) && target_row_kind(node_kind) == kind
        });
        if node.key == key && is_row {
            return true;
        }
    }
    false
}

fn find_row(
    backend: &PageBackend,
    snapshot: &Snapshot,
    kind: u8,
    key: u32,
    used_mask: u32,
) -> Option<usize> {
    let mut reusable = None;
    let mut empty = None;
    for i in 0..UI_MAX_ROWS {
        if backend.rows[i].is_null() {
            if empty.is_none() {
                empty = Some(i);
            }
        } else if backend.row_kinds[i] == kind && (used_mask & (1 << i)) == 0 {
            if backend.row_keys[i] == key {
                return Some(i);
            }
            if reusable.is_none() && !snapshot_uses_row(snapshot, kind, backend.row_keys[i]) {
                reusable = Some(i);
            }
        }
    }
    reusable.or(empty)
}

/// Applies a committed snapshot to the stock LVX page. Returns 0 on success.
pub fn apply_snapshot(page_index: usize, snapshot: &Snapshot) -> i32 {
    if page_index >= PAGE_COUNT || snapshot.node_count == 0 {
        return -1;
    }
    let backend = page_backend(page_index);
    if backend.root.is_null() {
        return -1;
    }
    if backend.content_root.is_null() {
        #[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
        {
            // Band-9 has no `lvx_content_create`; the firmware page root is
            // the content parent and its size/placement is fixed by the stock
            // page shell, so no size/align is applied here.
            backend.content_root = backend.root;
        }
        #[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
        {
            backend.content_root = unsafe { lvx_content_create(backend.root) };
            if backend.content_root.is_null() {
                return -1;
            }
            unsafe {
                let content_height = if page_index == PAGE_PLAYER {
                    PLAYER_CONTENT_HEIGHT
                } else {
                    CONTENT_HEIGHT
                };
                let content_top = if page_index == PAGE_PLAYER {
                    0
                } else {
                    CONTENT_TOP_OFFSET
                };
                lvx_object_set_size(backend.content_root, CONTENT_WIDTH, content_height);
                lvx_object_align(backend.content_root, ALIGN_TOP_MID, 0, content_top);
            }
        }
        if backend.content_root.is_null() {
            return -1;
        }
    }

    let _ = sync_background(backend, page_index, snapshot);

    // Capacity check mirrors the C backend: sections/pages are free, labels
    // and rows are bounded.
    let mut visible_rows = 0u32;
    let mut visible_labels = 0u32;
    let mut visible_images = 0u32;
    let mut visible_bars = 0u32;
    for index in 0..snapshot.node_count as usize {
        let node = &snapshot.nodes[index];
        match node.kind() {
            Some(NodeKind::Section) | Some(NodeKind::NavigationPage) => {}
            Some(NodeKind::Text) => visible_labels += 1,
            Some(NodeKind::StatusRow)
            | Some(NodeKind::Button)
            | Some(NodeKind::ActionRow)
            | Some(NodeKind::SwitchRow) => visible_rows += 1,
            Some(NodeKind::Image) if skip_image(node.key) => {}
            Some(NodeKind::Image) => visible_images += 1,
            Some(NodeKind::Progress) => visible_bars += 1,
            _ => return -1,
        }
    }
    if visible_labels > UI_MAX_LABELS as u32
        || visible_rows > UI_MAX_ROWS as u32
        || visible_images > 2
        || visible_bars > 4
    {
        return -1;
    }

    let (next_layout_hash, next_layout_count) = layout_fingerprint(snapshot);
    let layout_changed = !backend.layout_valid
        || backend.layout_hash != next_layout_hash
        || backend.layout_count != next_layout_count;
    let mut used_mask = 0u32;
    let mut label_used = 0u32;
    let mut image_used = 0usize;
    let mut bar_used = 0usize;
    let mut previous: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut player_row_count = 0u32;

    for index in 0..snapshot.node_count as usize {
        let node = &snapshot.nodes[index];
        let kind = match node.kind() {
            Some(kind) => kind,
            None => return -1,
        };
        let primary = snapshot.primary(node);

        if kind == NodeKind::Section {
            continue;
        }
        if kind == NodeKind::NavigationPage {
            if page_index == PAGE_PLAYER {
                continue;
            }
            let title_mode = if page_index == PAGE_OVERVIEW {
                0u32
            } else {
                1u32
            };
            if backend.page_title.is_null() {
                // Mode 1 draws the stock back affordance wired to
                // `page_title_back`; mode 0 passes a NULL back callback exactly
                // like the C backend, so no back button is drawn on overview.
                // Keep this nullable at the ABI boundary: a Rust function
                // pointer cannot validly contain NULL, even if it is never
                // called, and constructing one lets the compiler eliminate the
                // overview title creation path as unreachable.
                let back_callback = if title_mode != 0 {
                    page_title_back as *const ()
                } else {
                    core::ptr::null()
                };
                let back_context = (page_index << 8) as *mut core::ffi::c_void;
                backend.page_title = unsafe {
                    lvx_page_title_create(
                        backend.root,
                        primary.as_ptr(),
                        title_mode,
                        back_callback,
                        back_context,
                    )
                };
                if backend.page_title.is_null() {
                    return -1;
                }
                unsafe { apply_misans(backend.page_title) };
            }
            unsafe { lvx_set_hidden(backend.page_title, 0) };
            previous = backend.page_title;
            continue;
        }
        if kind == NodeKind::Text {
            let mut object = backend.labels[label_used as usize];
            let created_now = object.is_null();
            if created_now {
                let created = unsafe { lvx_label_create(backend.content_root) };
                if created.is_null() {
                    return -1;
                }
                backend.labels[label_used as usize] = created;
                backend.label_count += 1;
                object = created;
            }
            let is_player_title = page_index == PAGE_PLAYER && label_used == 0;
            let is_player_author = page_index == PAGE_PLAYER && label_used == 1;
            let is_player_label = page_index == PAGE_PLAYER;
            let label_width = CONTENT_WIDTH;
            let label_hash = hash_word(
                hash_word(
                    hash_text(0x811C_9DC5, primary),
                    snapshot.styles[index].text_style as u32,
                ),
                label_width as u32,
            );
            if created_now || backend.label_hashes[label_used as usize] != label_hash {
                unsafe {
                    if created_now && is_player_label {
                        lvx_label_set_text_align_center(object);
                    }
                    lvx_label_set_text(object, primary.as_ptr());
                    if is_player_author {
                        apply_author_misans(object);
                    } else {
                        apply_misans(object);
                    }
                    lvx_object_set_size(object, label_width, wrapped_label_height(primary));
                }
                backend.label_hashes[label_used as usize] = label_hash;
            }
            unsafe { lvx_set_hidden(object, 0) };
            if layout_changed {
                if is_player_title {
                    unsafe { lvx_object_align(object, ALIGN_TOP_MID, 0, PLAYER_TITLE_TOP) };
                } else if is_player_author {
                    unsafe { lvx_object_align(object, ALIGN_TOP_MID, 0, PLAYER_AUTHOR_TOP) };
                } else if previous.is_null() {
                    unsafe { lvx_align_to(object, backend.content_root, ALIGN_TOP_MID, 0, 0) };
                } else {
                    let gap = if page_index == PAGE_PLAYER && label_used == 0 {
                        8
                    } else {
                        4
                    };
                    unsafe { lvx_align_to(object, previous, ALIGN_OUT_BOTTOM_MID, 0, gap) };
                }
            }
            previous = object;
            label_used += 1;
            continue;
        }
        if kind == NodeKind::Image {
            #[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
            {
                continue;
            }
            #[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
            {
                if skip_image(node.key) {
                    continue;
                }
                let layout = &snapshot.layouts[index];
                if primary.is_empty() || layout.width <= 0 || layout.height <= 0 {
                    return -1;
                }
                let Some(resolved_path) = storage::resolve_path(primary) else {
                    if !backend.images[image_used].is_null() {
                        unsafe { lvx_set_hidden(backend.images[image_used], 1) };
                    }
                    image_used += 1;
                    continue;
                };
                if resolved_path.ends_with(".bin")
                    && !storage::validate_lvgl_v9_image(&resolved_path, 180, 180)
                {
                    if !backend.images[image_used].is_null() {
                        unsafe { lvx_set_hidden(backend.images[image_used], 1) };
                    }
                    image_used += 1;
                    continue;
                }
                let mut object = backend.images[image_used];
                let created_now = object.is_null();
                if created_now {
                    object = unsafe { lvx_image_create(backend.content_root) };
                    if object.is_null() {
                        return -1;
                    }
                    backend.images[image_used] = object;
                }
                let image_hash = hash_word(
                    hash_text(0x811C_9DC5, &resolved_path),
                    snapshot.values[index].resource_id,
                );
                let mut source_path = resolved_path.into_bytes();
                source_path.push(0);
                let path_changed =
                    backend.image_paths[image_used].as_deref() != Some(source_path.as_slice());
                if created_now || backend.image_hashes[image_used] != image_hash || path_changed {
                    backend.image_paths[image_used] = Some(source_path);
                    let source = backend.image_paths[image_used]
                        .as_ref()
                        .map_or(core::ptr::null(), |path| path.as_ptr());
                    unsafe { lvx_image_set_src(object, source.cast()) };
                    backend.image_hashes[image_used] = image_hash;
                }
                unsafe {
                    lvx_object_set_size(object, i32::from(layout.width), i32::from(layout.height));
                    lvx_set_hidden(object, 0);
                }
                if layout_changed {
                    if page_index == PAGE_PLAYER && node.key == PLAYER_COVER_KEY {
                        unsafe { lvx_object_align(object, ALIGN_TOP_MID, 0, PLAYER_MEDIA_TOP) };
                    } else if previous.is_null() {
                        unsafe { lvx_align_to(object, backend.content_root, ALIGN_TOP_MID, 0, 0) };
                    } else {
                        unsafe { lvx_align_to(object, previous, ALIGN_OUT_BOTTOM_MID, 0, 8) };
                    }
                }
                previous = object;
                image_used += 1;
            }
            continue;
        }
        if kind == NodeKind::Progress {
            let layout = &snapshot.layouts[index];
            let value = &snapshot.values[index];
            if layout.width <= 0 || layout.height <= 0 || value.minimum >= value.maximum {
                return -1;
            }
            let mut object = backend.bars[bar_used];
            let created_now = object.is_null();
            if created_now {
                object = unsafe { lvx_bar_create(backend.content_root) };
                if object.is_null() {
                    return -1;
                }
                backend.bars[bar_used] = object;
            }
            let bar_hash = hash_word(
                hash_word(
                    hash_word(0x811C_9DC5, value.minimum as u32),
                    value.maximum as u32,
                ),
                value.value as u32,
            );
            if created_now || backend.bar_hashes[bar_used] != bar_hash {
                unsafe {
                    lvx_bar_set_range(object, value.minimum, value.maximum);
                    lvx_bar_set_value(object, value.value);
                }
                backend.bar_hashes[bar_used] = bar_hash;
            }
            unsafe {
                lvx_object_set_size(object, i32::from(layout.width), i32::from(layout.height));
                lvx_set_hidden(object, 0);
            }
            if layout_changed {
                if previous.is_null() {
                    unsafe { lvx_align_to(object, backend.content_root, ALIGN_TOP_MID, 0, 0) };
                } else {
                    unsafe { lvx_align_to(object, previous, ALIGN_OUT_BOTTOM_MID, 0, 8) };
                }
            }
            previous = object;
            bar_used += 1;
            continue;
        }

        if page_index == PAGE_PLAYER
            && kind == NodeKind::ActionRow
            && control_icon(node.key, primary).is_some()
        {
            let slot = match find_row(backend, snapshot, ROW_IMAGE_BUTTON, node.key, used_mask) {
                Some(slot) => slot,
                None => return -1,
            };
            let mut object = backend.rows[slot];
            #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
            let mut image = backend.control_images[slot];
            let created_now = object.is_null();
            if created_now {
                let cookie = encoded_cookie(page_index, slot) as *mut core::ffi::c_void;
                #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
                {
                    // Use the stock list-row class as the hitbox. Its event
                    // path is the only one confirmed to receive touch input
                    // on this firmware.
                    object = unsafe {
                        lvx_list_row_create(
                            backend.content_root,
                            EMPTY_TEXT.as_ptr(),
                            EMPTY_TEXT.as_ptr(),
                            TRAILING_NONE,
                        )
                    };
                    if object.is_null() {
                        return -1;
                    }
                    image = unsafe { lvx_image_create(backend.content_root) };
                    if image.is_null() {
                        return -1;
                    }
                    backend.control_images[slot] = image;
                    unsafe {
                        lvx_object_move_to_index(object, 0);
                        lvx_event_add(object, row_event, EVENT_CLICKED, cookie);
                        lvx_event_add(object, image_button_event, EVENT_ALL, cookie);
                    }
                }
                #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
                {
                    object = unsafe { lvx_image_create(backend.content_root) };
                    if object.is_null() {
                        return -1;
                    }
                    let _ = cookie;
                    unsafe {
                        lvx_object_add_flag(object, LV_OBJ_FLAG_CLICKABLE);
                        lvx_event_add(object, image_button_event, EVENT_ALL, cookie);
                    }
                }
                backend.rows[slot] = object;
                backend.row_kinds[slot] = ROW_IMAGE_BUTTON;
                backend.row_keys[slot] = node.key;
                backend.row_count += 1;
                unsafe {
                    backend.anim_timers[slot] = lvx_timer_create(
                        control_animation_timer,
                        CONTROL_ANIMATION_PERIOD_MS,
                        core::ptr::null_mut(),
                    );
                }
                backend.anim_sizes[slot] = PLAYER_CONTROL_SIZE;
                backend.anim_from[slot] = PLAYER_CONTROL_SIZE;
                backend.anim_to[slot] = PLAYER_CONTROL_SIZE;
                backend.anim_progress[slot] = 0;
                backend.animating[slot] = false;
            }
            let content_hash = row_content_hash(primary, "", u32::from(node.enabled()));
            if created_now || backend.row_hashes[slot] != content_hash {
                let icon = match control_icon(node.key, primary) {
                    Some(icon) => icon,
                    None => return -1,
                };
                #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
                unsafe {
                    lvx_image_set_src(image, icon.as_ptr().cast());
                }
                #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
                unsafe {
                    lvx_image_set_src(object, icon.as_ptr().cast());
                }
                backend.row_hashes[slot] = content_hash;
            }
            unsafe {
                #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
                {
                    set_control_hitbox_geometry(object, node.key);
                    set_control_geometry_size(image, node.key, backend.anim_sizes[slot]);
                    // Keep the proven stock-row hitbox behind the page background.
                    // This avoids mutating the row's local style while preserving its
                    // firmware-backed touch path.
                    lvx_object_move_to_index(object, 0);
                    lvx_set_hidden(object, 0);
                    lvx_set_hidden(image, 0);
                }
                #[cfg(not(feature = "target-xiaomi-band-10-pro-3-101-036"))]
                {
                    set_control_geometry_size(object, node.key, backend.anim_sizes[slot]);
                    lvx_set_hidden(object, 0);
                }
            }
            backend.bindings[slot] = Binding {
                generation: snapshot.generation,
                key: node.key,
                event_id: node.event_id,
                enabled: node.enabled(),
            };
            used_mask |= 1 << slot;
            continue;
        }

        if !matches!(
            kind,
            NodeKind::StatusRow | NodeKind::Button | NodeKind::ActionRow | NodeKind::SwitchRow
        ) {
            return -1;
        }

        // The firmware tests only whether the secondary pointer is NULL. Rust's
        // empty `str` may use the non-dereferenceable dangling sentinel address
        // 1, so pass an actual resident NUL byte for rows without detail text.
        let secondary_text = if node.secondary_len != 0 {
            snapshot.secondary(node)
        } else {
            ""
        };
        let secondary = if node.secondary_len != 0 {
            secondary_text.as_ptr()
        } else {
            EMPTY_TEXT.as_ptr()
        };
        let row_kind = target_row_kind(kind);
        #[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
        let trailing = match row_kind {
            ROW_ACTION => TRAILING_FORWARD,
            ROW_SWITCH => TRAILING_SWITCH,
            _ => TRAILING_NONE,
        };
        let slot = match find_row(backend, snapshot, row_kind, node.key, used_mask) {
            Some(slot) => slot,
            None => return -1,
        };
        let mut object = backend.rows[slot];
        let created_now = object.is_null();
        if created_now {
            #[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
            let created = unsafe {
                lvx_list_row_create(backend.content_root, primary.as_ptr(), secondary, trailing)
            };
            #[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
            let created = unsafe {
                // Band-9 row factory is (parent, primary); the trailing control
                // is attached afterwards through the kind-specific setter.
                let row = lvx_list_row_create(backend.content_root, primary.as_ptr());
                if !row.is_null() {
                    lvx_list_row_set_trailing(row, b9_row_trailing(row_kind), 0);
                }
                row
            };
            if created.is_null() {
                return -1;
            }
            backend.rows[slot] = created;
            backend.row_kinds[slot] = row_kind;
            object = created;
            unsafe { apply_misans(object) };
            let event_object = if row_kind == ROW_SWITCH {
                unsafe { lvx_list_row_trailing(created) }
            } else {
                created
            };
            if event_object.is_null() {
                return -1;
            }
            let event_code = if row_kind == ROW_SWITCH {
                EVENT_ALL
            } else {
                EVENT_CLICKED
            };
            unsafe {
                lvx_event_add(
                    event_object,
                    row_event,
                    event_code,
                    encoded_cookie(page_index, slot) as *mut core::ffi::c_void,
                );
            }
            backend.row_count += 1;
        }
        let selected: u8 = if row_kind == ROW_SWITCH {
            if node.checked() { 1 } else { 0 }
        } else {
            1
        };
        let content_hash = row_content_hash(primary, secondary_text, u32::from(selected));
        if created_now || backend.row_hashes[slot] != content_hash {
            unsafe {
                lvx_list_row_update(
                    object,
                    core::ptr::null(),
                    primary.as_ptr(),
                    secondary,
                    0,
                    selected,
                );
                apply_misans(object);
            }
            backend.row_hashes[slot] = content_hash;
        }
        unsafe { set_row_hidden(object, 0) };
        if layout_changed {
            if page_index == PAGE_PLAYER && player_row_count == 0 {
                unsafe {
                    lvx_align_to(
                        object,
                        backend.content_root,
                        ALIGN_TOP_MID,
                        0,
                        PLAYER_CONTENT_HEIGHT + ROW_GAP,
                    )
                };
            } else if previous.is_null() {
                unsafe { lvx_align_to(object, backend.content_root, ALIGN_TOP_MID, 0, 0) };
            } else {
                unsafe { lvx_align_to(object, previous, ALIGN_OUT_BOTTOM_MID, 0, ROW_GAP) };
            }
        }
        if page_index == PAGE_PLAYER {
            player_row_count = player_row_count.saturating_add(1);
        }
        previous = object;
        backend.row_keys[slot] = node.key;
        backend.bindings[slot] = Binding {
            generation: snapshot.generation,
            key: node.key,
            event_id: node.event_id,
            enabled: node.enabled(),
        };
        used_mask |= 1 << slot;
    }

    for i in 0..UI_MAX_ROWS {
        if !backend.rows[i].is_null() && (used_mask & (1 << i)) == 0 {
            if backend.row_kinds[i] == ROW_IMAGE_BUTTON {
                unsafe {
                    lvx_set_hidden(backend.rows[i], 1);
                    #[cfg(feature = "target-xiaomi-band-10-pro-3-101-036")]
                    if !backend.control_images[i].is_null() {
                        lvx_set_hidden(backend.control_images[i], 1);
                    }
                }
            } else {
                unsafe { set_row_hidden(backend.rows[i], 1) };
            }
            backend.bindings[i] = Binding {
                generation: 0,
                key: 0,
                event_id: 0,
                enabled: false,
            };
        }
    }
    for i in label_used as usize..UI_MAX_LABELS {
        if !backend.labels[i].is_null() {
            unsafe { lvx_set_hidden(backend.labels[i], 1) };
        }
    }
    for i in image_used..backend.images.len() {
        if !backend.images[i].is_null() {
            unsafe { lvx_set_hidden(backend.images[i], 1) };
        }
    }
    for i in bar_used..backend.bars.len() {
        if !backend.bars[i].is_null() {
            unsafe { lvx_set_hidden(backend.bars[i], 1) };
        }
    }
    backend.layout_hash = next_layout_hash;
    backend.layout_count = next_layout_count;
    backend.layout_valid = true;
    backend.rendered_generation = snapshot.generation;
    backend.refresh_failed = false;
    0
}
