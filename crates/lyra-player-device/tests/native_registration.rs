//! Host regression of the real registration code with modeled firmware pages.
//! The stand-in descriptor only supplies fields used by the module; exact
//! firmware layout and firmware publication are covered by device builds.
#![cfg(not(feature = "device"))]
#![allow(non_camel_case_types, dead_code)]

extern crate self as canopus_target_private;

use canopus_abi::*;
use core::ffi::c_void;
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::ffi::CStr;
use std::sync::Mutex;

#[derive(Default)]
pub struct firmware_page_descriptor {
    page_name: *mut c_void,
    page_id: u16,
    app_id: u16,
    on_signal: *mut c_void,
    on_create: *mut c_void,
    on_resume: *mut c_void,
    on_pause: *mut c_void,
    on_destroy: *mut c_void,
    on_ui_destroy: *mut c_void,
    extension_callback_100: *mut c_void,
}

mod runtime {
    use core::sync::atomic::{AtomicI32, AtomicU32};

    pub const APP_PENDING: u32 = 1;
    pub const APP_OK: u32 = 2;
    pub const APP_FAILED: u32 = 3;

    pub struct Runtime {
        pub app_id: AtomicU32,
        pub app_state: AtomicU32,
        pub app_error: AtomicI32,
        pub active_page: AtomicU32,
    }

    static RUNTIME: Runtime = Runtime {
        app_id: AtomicU32::new(0),
        app_state: AtomicU32::new(0),
        app_error: AtomicI32::new(0),
        active_page: AtomicU32::new(0),
    };

    pub fn initialized() -> bool {
        true
    }

    pub fn runtime() -> &'static Runtime {
        &RUNTIME
    }
}

mod ui {
    use core::ffi::c_void;

    pub mod assets {
        pub fn ensure_launcher_icon() {}
    }

    pub fn page_create(_: usize, _: *mut c_void) -> i32 {
        0
    }
    pub fn page_resume(_: usize) -> i32 {
        0
    }
    pub fn page_pause(_: usize) -> i32 {
        0
    }
    pub fn page_destroy(_: usize) -> i32 {
        0
    }
}

#[path = "../src/target/native_app.rs"]
mod native_app;

static TEST_LOCK: Mutex<()> = Mutex::new(());
static RESULT: AtomicI32 = AtomicI32::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static PAGES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

unsafe extern "C" fn register(_: *const ContextV1, entry: *const AppEntryV1) -> i32 {
    let entry = unsafe { &*entry };
    assert_eq!(entry.struct_size as usize, size_of::<AppEntryV1>());
    assert_eq!(unsafe { CStr::from_ptr(entry.label) }.to_bytes(), b"Lyra");
    assert_eq!(
        unsafe { CStr::from_ptr(entry.icon) }.to_bytes(),
        b"/data/canopus/m.lyra_player/appicon.bin"
    );
    assert_eq!(entry.page_count as usize, native_app::PAGE_COUNT);
    let pages = unsafe { std::slice::from_raw_parts(entry.pages, entry.page_count as usize) };
    *PAGES.lock().unwrap() = pages.iter().map(|page| *page as usize).collect();
    for (index, page) in pages.iter().enumerate() {
        assert_eq!(*page, native_app::page_descriptor_ptr(index).cast());
        let page = unsafe { &*page.cast::<firmware_page_descriptor>() };
        assert_eq!(
            page.app_id, 0,
            "the Supervisor assigns the page app ID later"
        );
        assert_eq!(page.page_id as usize, index);
        assert!(!page.on_create.is_null() && !page.on_destroy.is_null());
    }
    CALLS.fetch_add(1, Ordering::Relaxed);
    RESULT.load(Ordering::Relaxed)
}

static LAUNCHER: CanopusLauncherObj = CanopusLauncherObj {
    register_app: register,
};

fn services() -> ServicesV1Head {
    let mut services = ServicesV1Head {
        struct_size: size_of::<ServicesV1Head>() as u32,
        group_version: [0; SVC_GROUP_COUNT as usize],
    };
    services.group_version[SVC_LAUNCHER as usize] = 1;
    services
}

fn context(services: &ServicesV1Head) -> CanopusContext {
    let mut ctx: CanopusContext = unsafe { core::mem::zeroed() };
    ctx.struct_size = size_of::<CanopusContext>() as u32;
    ctx.services = core::ptr::from_ref(services).cast();
    ctx.launcher = &LAUNCHER;
    ctx
}

fn reset(result: i32) {
    let r = runtime::runtime();
    r.app_id.store(0, Ordering::Relaxed);
    r.app_state.store(0, Ordering::Relaxed);
    r.app_error.store(0, Ordering::Relaxed);
    RESULT.store(result, Ordering::Relaxed);
    CALLS.store(0, Ordering::Relaxed);
    PAGES.lock().unwrap().clear();
}

#[test]
fn assigned_identity_drives_navigation_after_publication() {
    let _guard = TEST_LOCK.lock().unwrap();
    let services = services();
    let ctx = context(&services);
    let ctx = core::ptr::from_ref(&ctx).cast();
    for assigned in [0xD1, 0xE4] {
        reset(assigned);
        assert_eq!(native_app::navigation_key(0), None);
        assert_eq!(unsafe { native_app::install_stage(ctx, 2) }, Err(-1));
        assert_eq!(unsafe { native_app::install_stage(ctx, 1) }, Ok(()));
        assert_eq!(
            runtime::runtime().app_id.load(Ordering::Acquire),
            assigned as u32
        );
        assert_eq!(native_app::navigation_key(0), None, "queued is not ready");

        // The Supervisor copied the stack array and fills the persistent pages
        // on a later UI tick, after install_stage has returned.
        for (index, address) in PAGES.lock().unwrap().iter().copied().enumerate() {
            let page = unsafe { &mut *(address as *mut firmware_page_descriptor) };
            assert_eq!(page.page_id as usize, index);
            page.app_id = assigned as u16;
        }
        assert_eq!(unsafe { native_app::install_stage(ctx, 1) }, Ok(()));
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(unsafe { native_app::install_stage(ctx, 2) }, Ok(()));
        for index in 0..native_app::PAGE_COUNT {
            assert_eq!(
                native_app::navigation_key(index),
                Some(((assigned as u32) << 16) | index as u32)
            );
            assert_eq!(
                unsafe { (*native_app::page_descriptor_ptr(index)).app_id },
                assigned as u16
            );
        }
        assert_eq!(native_app::navigation_key(native_app::PAGE_COUNT), None);
        assert_eq!(unsafe { native_app::install_stage(ctx, 2) }, Ok(()));
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn registration_failure_cannot_enable_navigation_and_can_be_retried() {
    let _guard = TEST_LOCK.lock().unwrap();
    let services = services();
    let ctx = context(&services);
    let ctx = core::ptr::from_ref(&ctx).cast();
    reset(-12);
    assert_eq!(unsafe { native_app::install_stage(ctx, 1) }, Err(-12));
    assert_eq!(runtime::runtime().app_error.load(Ordering::Acquire), -12);
    assert_eq!(
        runtime::runtime().app_state.load(Ordering::Acquire),
        runtime::APP_FAILED
    );
    assert_eq!(unsafe { native_app::install_stage(ctx, 2) }, Err(-1));
    assert_eq!(native_app::navigation_key(0), None);
    RESULT.store(0xD3, Ordering::Relaxed);
    assert_eq!(unsafe { native_app::install_stage(ctx, 1) }, Ok(()));
    assert_eq!(runtime::runtime().app_error.load(Ordering::Acquire), 0);
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(native_app::navigation_key(0), None);
}

#[test]
fn missing_service_and_invalid_stage_fail_without_registration() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset(0xD2);
    assert_eq!(
        unsafe { native_app::install_stage(core::ptr::null(), 1) },
        Err(-22)
    );
    let mut services = services();
    services.group_version[SVC_LAUNCHER as usize] = 0;
    let ctx = context(&services);
    let ctx = core::ptr::from_ref(&ctx).cast();
    assert_eq!(unsafe { native_app::install_stage(ctx, 1) }, Err(-38));
    assert_eq!(unsafe { native_app::install_stage(ctx, 0) }, Err(-22));
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert_eq!(native_app::navigation_key(0), None);
}
