//! Motion: a frame clock for the pages' animations, with the easing curves of
//! `lyra_player_core::motion`.
//!
//! The firmware's `lv_anim` is not reachable from a module and the 100 ms page tick is too
//! coarse for movement, so a page that animates starts a short LVGL timer for as long as
//! something moves.

use core::ffi::c_void;

use canopus_target_private::*;
pub use lyra_player_core::motion::*;

use super::lv;

/// Thirty updates per second leaves CPU time for MP3 decode/SBC on a single core.
const FRAME_MS: u32 = 33;

/// The timer that drives one page's animations; it only exists while something moves.
pub struct Clock {
    timer: lv::Obj,
}

impl Clock {
    pub const fn new() -> Self {
        Self { timer: lv::NULL }
    }

    pub fn start(&mut self, frame: extern "C" fn(*mut c_void)) {
        if self.timer.is_null() {
            self.timer = unsafe { lvx_timer_create(frame, FRAME_MS, core::ptr::null_mut()) };
        }
    }

    pub fn is_running(&self) -> bool {
        !self.timer.is_null()
    }

    /// Safe from inside the timer's own callback: LVGL allows a timer to delete itself.
    pub fn stop(&mut self) {
        if !self.timer.is_null() {
            unsafe { lvx_timer_delete(self.timer) };
            self.timer = lv::NULL;
        }
    }
}
