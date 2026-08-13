use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

use canopus_target_private::bt_alloc;
use lyra_player_core::{LyraApp, app::RequestKind, bridge::FetchBridge};

#[derive(Clone, Copy)]
pub struct PendingRequest {
    pub kind: RequestKind,
    pub token: u64,
}

pub const APP_NONE: u32 = 0;
pub const APP_REGISTERED: u32 = 1;
pub const APP_OK: u32 = 2;
pub const APP_FAILED: u32 = 3;

pub struct Core {
    pub app: LyraApp,
    pub audio: super::audio::AudioDevice,
    pub bridge: FetchBridge,
    pub pending: BTreeMap<String, PendingRequest>,
    pub outbound: VecDeque<String>,
    pub sending: Option<String>,
    pub audio_request: Option<String>,
    pub deferred_stream_reply: Option<String>,
    pub audio_ending: bool,
}

impl Core {
    fn new() -> Self {
        Self {
            app: LyraApp::default(),
            audio: super::audio::AudioDevice::new(),
            bridge: FetchBridge::new(),
            pending: BTreeMap::new(),
            outbound: VecDeque::new(),
            sending: None,
            audio_request: None,
            deferred_stream_reply: None,
            audio_ending: false,
        }
    }
}

pub struct Runtime {
    pub app_state: AtomicU32,
    pub app_error: AtomicI32,
    pub app_install_result: AtomicI32,
    pub launcher_add_result: AtomicI32,
    pub last_error: AtomicI32,
    pub connection: AtomicUsize,
    pub connected: AtomicBool,
    pub active_page: AtomicU32,
    pub timer_ticks: AtomicU32,
}

impl Runtime {
    const fn new() -> Self {
        Self {
            app_state: AtomicU32::new(APP_NONE),
            app_error: AtomicI32::new(0),
            app_install_result: AtomicI32::new(0),
            launcher_add_result: AtomicI32::new(0),
            last_error: AtomicI32::new(0),
            connection: AtomicUsize::new(0),
            connected: AtomicBool::new(false),
            active_page: AtomicU32::new(0),
            timer_ticks: AtomicU32::new(0),
        }
    }
}

static mut RUNTIME: core::mem::MaybeUninit<Runtime> = core::mem::MaybeUninit::uninit();
static CORE_PTR: AtomicUsize = AtomicUsize::new(0);
static CORE_LOCK: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);

pub fn prepare() {
    let mut pointer = CORE_PTR.load(Ordering::Acquire) as *mut Core;
    if pointer.is_null() {
        pointer = unsafe { bt_alloc(core::mem::size_of::<Core>() as u32) }.cast();
        if pointer.is_null() {
            READY.store(false, Ordering::Release);
            return;
        }
        CORE_PTR.store(pointer as usize, Ordering::Release);
    }
    unsafe {
        core::ptr::addr_of_mut!(RUNTIME)
            .cast::<Runtime>()
            .write(Runtime::new());
        pointer.write(Core::new());
    }
    CORE_LOCK.store(false, Ordering::Release);
    READY.store(true, Ordering::Release);
}

pub fn runtime() -> &'static Runtime {
    unsafe { &*core::ptr::addr_of!(RUNTIME).cast::<Runtime>() }
}

pub fn initialized() -> bool {
    READY.load(Ordering::Acquire)
}

pub fn with_core<R>(function: impl FnOnce(&mut Core) -> R) -> R {
    while CORE_LOCK
        .compare_exchange_weak(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let pointer = CORE_PTR.load(Ordering::Acquire) as *mut Core;
    let result = unsafe { function(&mut *pointer) };
    CORE_LOCK.store(false, Ordering::Release);
    result
}

pub fn try_with_core<R>(function: impl FnOnce(&mut Core) -> R) -> Option<R> {
    if CORE_LOCK
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let pointer = CORE_PTR.load(Ordering::Acquire) as *mut Core;
    let result = unsafe { function(&mut *pointer) };
    CORE_LOCK.store(false, Ordering::Release);
    Some(result)
}
