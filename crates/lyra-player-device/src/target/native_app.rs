//! Supervisor-owned native app registration and the local player's four pages.
//! Stage 1 queues the writable page descriptors; the Supervisor installs the
//! application and launcher entry before it dispatches stage 2.

use core::sync::atomic::Ordering;

use canopus_abi::{AppEntryV1, ContextV1};
use canopus_runtime::launcher;
use canopus_target_private::*;

use super::runtime::*;
use super::ui_backend;

pub const PAGE_COUNT: usize = 4;
pub const PAGE_OVERVIEW: usize = 0;
pub const PAGE_LIBRARY: usize = 1;
pub const PAGE_PLAYER: usize = 2;

pub const DISPLAY_NAME: &[u8] = b"Lyra\0";
pub const LAUNCHER_ICON: &[u8] = b"/data/canopus/appicon_lyra.bin\0";
const PAGE_NAMES: [&[u8]; PAGE_COUNT] = [
    b"lyra_home\0",
    b"lyra_library\0",
    b"lyra_player\0",
    b"lyra_lyrics\0",
];

static mut PAGE_DESCRIPTORS: core::mem::MaybeUninit<[firmware_page_descriptor; PAGE_COUNT]> =
    core::mem::MaybeUninit::uninit();

pub fn page_descriptor_ptr(index: usize) -> *mut firmware_page_descriptor {
    // SAFETY: PAGE_DESCRIPTORS is initialized before registration queues these
    // persistent descriptors. `addr_of_mut!` avoids creating static references;
    // the serialized UI owner accesses the descriptors through raw pointers.
    unsafe {
        core::ptr::addr_of_mut!(PAGE_DESCRIPTORS)
            .cast::<firmware_page_descriptor>()
            .add(index)
    }
}

fn descriptor_init(index: usize, name: &[u8], page_id: u16) {
    let descriptor = page_descriptor_ptr(index);
    // SAFETY: descriptor points at the static array, which is initialized
    // before registration. The Supervisor later writes its assigned app ID.
    unsafe {
        core::ptr::write_bytes(
            descriptor.cast::<u8>(),
            0,
            core::mem::size_of::<firmware_page_descriptor>(),
        );
        (*descriptor).page_name = name.as_ptr() as *mut core::ffi::c_void;
        (*descriptor).page_id = page_id;
        (*descriptor).on_signal = page_on_signal as *const () as *mut core::ffi::c_void;
        (*descriptor).on_create = page_on_create as *const () as *mut core::ffi::c_void;
        (*descriptor).on_resume = page_on_resume as *const () as *mut core::ffi::c_void;
        (*descriptor).on_pause = page_on_pause as *const () as *mut core::ffi::c_void;
        #[cfg(not(any(
            feature = "target-xiaomi-band-11-4-100-139",
            feature = "target-xiaomi-band-11-4-100-155"
        )))]
        {
            (*descriptor).on_destroy = page_on_destroy as *const () as *mut core::ffi::c_void;
        }
        #[cfg(any(
            feature = "target-xiaomi-band-11-4-100-139",
            feature = "target-xiaomi-band-11-4-100-155"
        ))]
        {
            (*descriptor).on_ui_destroy = page_on_destroy as *const () as *mut core::ffi::c_void;
        }
    }
}

/// Queues registration in stage 1. The Supervisor calls stage 2 only after
/// application and launcher publication succeeds. Opening the app remains a
/// launcher action, as before.
///
/// # Safety
/// `ctx` must be the live context passed to this module's publication callback
/// on the serialized UI owner, or null (which registration rejects).
pub unsafe fn install_stage(ctx: *const ContextV1, stage: u32) -> Result<(), i32> {
    if !initialized() {
        return Err(-1);
    }
    let r = runtime();
    match stage {
        1 => {
            if r.app_id.load(Ordering::Acquire) != 0 {
                return Ok(());
            }
            for (index, name) in PAGE_NAMES.iter().enumerate() {
                descriptor_init(index, name, index as u16);
            }
            let pages: [*mut core::ffi::c_void; PAGE_COUNT] =
                core::array::from_fn(|index| page_descriptor_ptr(index).cast());
            let entry = AppEntryV1 {
                struct_size: core::mem::size_of::<AppEntryV1>() as u32,
                label: DISPLAY_NAME.as_ptr().cast(),
                icon: LAUNCHER_ICON.as_ptr().cast(),
                pages: pages.as_ptr(),
                page_count: PAGE_COUNT as u32,
            };
            // SAFETY: the callback supplies the live context and serialized
            // owner. The Supervisor copies `pages` before returning; descriptors,
            // strings and callbacks are static and remain valid until reboot.
            match unsafe { launcher::register_app(ctx.cast(), &entry) } {
                Ok(app_id) => {
                    r.app_id.store(u32::from(app_id), Ordering::Release);
                    r.app_state.store(APP_PENDING, Ordering::Release);
                    r.app_error.store(0, Ordering::Release);
                    Ok(())
                }
                Err(error) => {
                    r.app_error.store(error, Ordering::Release);
                    r.app_state.store(APP_FAILED, Ordering::Release);
                    Err(error)
                }
            }
        }
        2 => {
            if r.app_id.load(Ordering::Acquire) == 0 {
                return Err(-1);
            }
            r.app_state.store(APP_OK, Ordering::Release);
            r.app_error.store(0, Ordering::Release);
            Ok(())
        }
        _ => Err(-22),
    }
}

/// Firmware navigation is valid only after the Supervisor completed publication.
pub fn navigation_key(page_index: usize) -> Option<u32> {
    if !initialized() || page_index >= PAGE_COUNT {
        return None;
    }
    let r = runtime();
    let app_id = r.app_id.load(Ordering::Acquire);
    (r.app_state.load(Ordering::Acquire) == APP_OK && app_id != 0)
        .then_some((app_id << 16) | page_index as u32)
}

// ---------------------------------------------------------------------------
// Page lifecycle (firmware -> module). Renders happen only from the page
// owner thread; LVX is never touched from Bluetooth/timer callbacks.
// ---------------------------------------------------------------------------

fn page_id_of(page: *mut firmware_page_descriptor) -> usize {
    if page.is_null() {
        return usize::MAX;
    }
    // SAFETY: the firmware passes one of the descriptors we registered.
    usize::from(unsafe { (*page).page_id })
}

extern "C" fn page_on_signal(
    _page: *mut firmware_page_descriptor,
    _event: u32,
    _payload: *mut core::ffi::c_void,
) -> i32 {
    0
}

extern "C" fn page_on_create(
    page: *mut firmware_page_descriptor,
    root: *mut core::ffi::c_void,
    _start_data: *mut core::ffi::c_void,
) -> i32 {
    let index = page_id_of(page);
    if index >= PAGE_COUNT {
        return -1;
    }
    runtime().active_page.store(index as u32, Ordering::Release);
    ui_backend::page_create(index, root)
}

extern "C" fn page_on_resume(page: *mut firmware_page_descriptor) -> i32 {
    let index = page_id_of(page);
    if index >= PAGE_COUNT {
        return -1;
    }
    runtime().active_page.store(index as u32, Ordering::Release);
    ui_backend::page_resume(index)
}

extern "C" fn page_on_pause(page: *mut firmware_page_descriptor) -> i32 {
    let index = page_id_of(page);
    if index >= PAGE_COUNT {
        return -1;
    }
    ui_backend::page_pause(index)
}

extern "C" fn page_on_destroy(page: *mut firmware_page_descriptor) -> i32 {
    let index = page_id_of(page);
    if index >= PAGE_COUNT {
        return -1;
    }
    ui_backend::page_destroy(index)
}
