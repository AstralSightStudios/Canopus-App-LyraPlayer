use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

use alloc::string::String;

use canopus_target_private::bt_alloc;
use lyra_player_core::LyraApp;

#[derive(Clone, Debug)]
pub enum PendingAudioCommand {
    Stream(String),
    Toggle,
    Stop,
}

pub const APP_NONE: u32 = 0;
pub const APP_REGISTERED: u32 = 1;
pub const APP_OK: u32 = 2;
pub const APP_FAILED: u32 = 3;

pub struct Core {
    pub app: LyraApp,
    pub pending_audio: Option<PendingAudioCommand>,
    pub pending_volume: Option<u8>,
    pub audio_epoch: u32,
}

/// Only the Bluetooth owner accesses this state. No filesystem operation or
/// audio ioctl runs while holding the app/UI lock.
pub struct AudioOwner {
    pub audio: super::audio::AudioDevice,
    pub player: lyra_player_core::playback::Player,
    pub epoch: u32,
    pub error: Option<String>,
}

impl AudioOwner {
    unsafe fn initialize_at(pointer: *mut Self) {
        unsafe {
            core::ptr::addr_of_mut!((*pointer).audio).write(super::audio::AudioDevice::new());
            core::ptr::addr_of_mut!((*pointer).player).write(Default::default());
            core::ptr::addr_of_mut!((*pointer).epoch).write(0);
            core::ptr::addr_of_mut!((*pointer).error).write(None);
        }
    }
}

impl Core {
    pub fn toggle_audio(&mut self) {
        if matches!(
            self.pending_audio,
            Some(PendingAudioCommand::Stream(_) | PendingAudioCommand::Stop)
        ) {
            return;
        }
        self.pending_audio = if matches!(self.pending_audio, Some(PendingAudioCommand::Toggle)) {
            None
        } else {
            Some(PendingAudioCommand::Toggle)
        };
    }

    unsafe fn initialize_at(pointer: *mut Self) {
        unsafe {
            core::ptr::addr_of_mut!((*pointer).app).write(LyraApp::default());
            core::ptr::addr_of_mut!((*pointer).pending_audio).write(None);
            core::ptr::addr_of_mut!((*pointer).pending_volume).write(None);
            core::ptr::addr_of_mut!((*pointer).audio_epoch).write(0);
        }
    }
}

pub struct Runtime {
    pub app_state: AtomicU32,
    pub app_error: AtomicI32,
    pub app_install_result: AtomicI32,
    pub launcher_add_result: AtomicI32,
    pub last_error: AtomicI32,
    pub active_page: AtomicU32,
    pub timer_ticks: AtomicU32,
    pub ui_ticks: AtomicU32,
    /// What `connect::start` returned: 0 when the phone can import.
    pub connect_result: AtomicI32,
    /// Raised by the importer when it changed the library on disk.
    pub library_dirty: AtomicBool,
    pub library_loaded: AtomicBool,
    /// `storage::library_stamp()` of the file that was last loaded.
    pub library_stamp: AtomicU32,
    pub player_media_refresh_pending: AtomicBool,
}

impl Runtime {
    const fn new() -> Self {
        Self {
            app_state: AtomicU32::new(APP_NONE),
            app_error: AtomicI32::new(0),
            app_install_result: AtomicI32::new(0),
            launcher_add_result: AtomicI32::new(0),
            last_error: AtomicI32::new(0),
            active_page: AtomicU32::new(0),
            timer_ticks: AtomicU32::new(0),
            ui_ticks: AtomicU32::new(0),
            connect_result: AtomicI32::new(-38),
            library_dirty: AtomicBool::new(false),
            library_loaded: AtomicBool::new(false),
            library_stamp: AtomicU32::new(0),
            player_media_refresh_pending: AtomicBool::new(false),
        }
    }
}

static mut RUNTIME: core::mem::MaybeUninit<Runtime> = core::mem::MaybeUninit::uninit();
static CORE_PTR: AtomicUsize = AtomicUsize::new(0);
static AUDIO_PTR: AtomicUsize = AtomicUsize::new(0);
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
    let mut audio = AUDIO_PTR.load(Ordering::Acquire) as *mut AudioOwner;
    if audio.is_null() {
        audio = unsafe { bt_alloc(core::mem::size_of::<AudioOwner>() as u32) }.cast();
        if audio.is_null() {
            READY.store(false, Ordering::Release);
            return;
        }
        AUDIO_PTR.store(audio as usize, Ordering::Release);
    }
    unsafe {
        core::ptr::addr_of_mut!(RUNTIME)
            .cast::<Runtime>()
            .write(Runtime::new());
        Core::initialize_at(pointer);
        AudioOwner::initialize_at(audio);
    }
    CORE_LOCK.store(false, Ordering::Release);
    READY.store(true, Ordering::Release);
}

/// # Safety
/// Call only on the serialized Bluetooth owner after `prepare`.
pub unsafe fn audio_owner() -> &'static mut AudioOwner {
    unsafe { &mut *(AUDIO_PTR.load(Ordering::Acquire) as *mut AudioOwner) }
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
