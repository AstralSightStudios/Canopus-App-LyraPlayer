#![cfg_attr(feature = "device", no_std)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "device")]
extern crate alloc;

#[cfg(feature = "device")]
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};

    pub struct FirmwareAllocator;

    unsafe impl GlobalAlloc for FirmwareAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let header = core::mem::size_of::<usize>();
            let total = match layout
                .size()
                .checked_add(layout.align())
                .and_then(|size| size.checked_add(header))
            {
                Some(total) if total <= u32::MAX as usize => total,
                _ => return core::ptr::null_mut(),
            };
            let raw = unsafe { canopus_target_private::bt_alloc(total as u32) }.cast::<u8>();
            if raw.is_null() {
                return raw;
            }
            let start = unsafe { raw.add(header) } as usize;
            let aligned = (start + layout.align() - 1) & !(layout.align() - 1);
            let output = aligned as *mut u8;
            unsafe { output.sub(header).cast::<usize>().write(raw as usize) };
            output
        }

        unsafe fn dealloc(&self, pointer: *mut u8, _layout: Layout) {
            if pointer.is_null() {
                return;
            }
            let header = core::mem::size_of::<usize>();
            let raw =
                unsafe { pointer.sub(header).cast::<usize>().read() } as *mut core::ffi::c_void;
            unsafe { canopus_target_private::bt_free(raw) };
        }
    }
}

#[cfg(feature = "device")]
#[global_allocator]
static ALLOCATOR: allocator::FirmwareAllocator = allocator::FirmwareAllocator;

mod module;

#[cfg(feature = "device")]
mod target;

pub use module::*;

#[cfg(all(feature = "device", not(test)))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
