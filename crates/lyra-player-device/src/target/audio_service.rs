use core::{
    ffi::c_void,
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use canopus_target_private::{
    bt_alloc, bt_free, bt_l2cap_owner, bt_queue_external, bt_queue_free_addr, bt_timer_add,
    bt_timer_cancel,
};

const AUDIO_TICK_MS: u32 = 50;
const AUDIO_TIMER_EVENT: u8 = 0x4c;
const AUDIO_TIMER_TAG: &[u8] = b"lyra_audio\0";

static ACTIVE: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU32 = AtomicU32::new(0);
static TIMER_HANDLE: AtomicU32 = AtomicU32::new(0);

fn fail_generation(generation: u32, error: i32) {
    if GENERATION.load(Ordering::Acquire) == generation && ACTIVE.swap(false, Ordering::AcqRel) {
        super::runtime::runtime()
            .last_error
            .store(error, Ordering::Release);
    }
}

#[repr(C)]
struct WorkToken {
    generation: u32,
}

#[repr(C)]
struct TimerToken {
    generation: u32,
    handle: AtomicU32,
}

pub fn start() -> i32 {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    let token = unsafe { bt_alloc(size_of::<WorkToken>() as u32) }.cast::<WorkToken>();
    if token.is_null() {
        ACTIVE.store(false, Ordering::Release);
        return -12;
    }
    unsafe { token.write(WorkToken { generation }) };

    let owner = unsafe { bt_l2cap_owner() };
    if owner.is_null() {
        unsafe { bt_free(token.cast()) };
        ACTIVE.store(false, Ordering::Release);
        return -19;
    }
    unsafe {
        let _ = bt_queue_external(
            owner,
            start_work,
            bt_queue_free_addr(),
            token.cast(),
            AUDIO_TIMER_EVENT,
        );
    }
    0
}

extern "C" fn start_work(owner_valid: i32, event: i32, argument: *mut c_void) -> i32 {
    if argument.is_null() {
        return 0;
    }
    let generation = unsafe { (*(argument.cast::<WorkToken>())).generation };
    unsafe { bt_free(argument) };
    if !ACTIVE.load(Ordering::Acquire) || GENERATION.load(Ordering::Acquire) != generation {
        return 0;
    }
    if owner_valid == 0 || event != i32::from(AUDIO_TIMER_EVENT) {
        fail_generation(generation, -19);
        return 0;
    }
    super::audio_service_tick();
    let result = schedule_timer(generation);
    if result != 0 {
        fail_generation(generation, result);
    }
    0
}

fn schedule_timer(generation: u32) -> i32 {
    if TIMER_HANDLE.load(Ordering::Acquire) != 0 {
        return -16;
    }
    let owner = unsafe { bt_l2cap_owner() };
    if owner.is_null() {
        return -19;
    }
    let token = unsafe { bt_alloc(size_of::<TimerToken>() as u32) }.cast::<TimerToken>();
    if token.is_null() {
        return -12;
    }
    unsafe {
        token.write(TimerToken {
            generation,
            handle: AtomicU32::new(0),
        });
    }
    let handle = unsafe {
        bt_timer_add(
            owner,
            AUDIO_TICK_MS,
            AUDIO_TIMER_EVENT,
            timer_callback as *const () as *mut c_void,
            token.cast(),
            AUDIO_TIMER_TAG.as_ptr(),
            1,
        )
    };
    if handle == 0 {
        unsafe { bt_free(token.cast()) };
        return -5;
    }
    unsafe { (*token).handle.store(handle, Ordering::Release) };
    if TIMER_HANDLE
        .compare_exchange(0, handle, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        let mut duplicate = handle;
        unsafe { bt_timer_cancel(&mut duplicate) };
        return -16;
    }
    let stale = !ACTIVE.load(Ordering::Acquire) || GENERATION.load(Ordering::Acquire) != generation;
    if stale
        && TIMER_HANDLE
            .compare_exchange(handle, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        let mut stale_handle = handle;
        unsafe { bt_timer_cancel(&mut stale_handle) };
    }
    0
}

extern "C" fn timer_callback(owner_valid: i32, event: i32, argument: *mut c_void) -> i32 {
    if argument.is_null() {
        return 0;
    }
    let token = unsafe { &*(argument.cast::<TimerToken>()) };
    let generation = token.generation;
    let handle = token.handle.load(Ordering::Acquire);
    let owned = TIMER_HANDLE
        .compare_exchange(handle, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    let current = owned
        && ACTIVE.load(Ordering::Acquire)
        && GENERATION.load(Ordering::Acquire) == generation
        && owner_valid != 0
        && event == i32::from(AUDIO_TIMER_EVENT);
    unsafe { bt_free(argument) };
    if !current {
        if owned
            && ACTIVE.load(Ordering::Acquire)
            && GENERATION.load(Ordering::Acquire) == generation
        {
            fail_generation(generation, -19);
        }
        return 0;
    }

    super::audio_service_tick();
    let result = schedule_timer(generation);
    if result != 0 {
        fail_generation(generation, result);
    }
    0
}
