use canopus_abi::*;
use canopus_runtime::{status_put_u32, status_writer_publish};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static RESIDENT: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: AtomicU32 = AtomicU32::new(0);

const MAGIC: u32 = 0x4C59_5241;

const fn pack<const N: usize>(value: &[u8]) -> [u8; N] {
    let mut output = [0; N];
    let mut index = 0;
    while index < value.len() && index < N {
        output[index] = value[index];
        index += 1;
    }
    output
}

#[cfg(feature = "device")]
const TARGET_ID: &[u8] = canopus_target_private::TARGET_ID.as_bytes();
#[cfg(not(feature = "device"))]
const TARGET_ID: &[u8] = b"host-test";

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_prepare(_ctx: *const ContextV1) -> i32 {
    ACTIVE.store(false, Ordering::Release);
    RESIDENT.store(false, Ordering::Release);
    LAST_ERROR.store(0, Ordering::Release);
    #[cfg(feature = "device")]
    crate::target::prepare();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_activate(_ctx: *const ContextV1) -> i32 {
    #[cfg(feature = "device")]
    let result = crate::target::activate();
    #[cfg(not(feature = "device"))]
    let result = 0;
    if result == 0 {
        ACTIVE.store(true, Ordering::Release);
        RESIDENT.store(true, Ordering::Release);
    } else {
        LAST_ERROR.store(result as u32, Ordering::Release);
    }
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_deactivate(_ctx: *const ContextV1) -> i32 {
    if RESIDENT.load(Ordering::Acquire) {
        RESULT_REBOOT_REQUIRED as i32
    } else {
        ACTIVE.store(false, Ordering::Release);
        0
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_stop(ctx: *const ContextV1) -> i32 {
    canopus_mod_deactivate(ctx)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_query(writer: *mut StatusWriterV1) -> i32 {
    if writer.is_null() {
        return -1;
    }
    let writer = unsafe { &mut *writer };
    unsafe {
        if !status_put_u32(writer, MAGIC)
            || !status_put_u32(writer, ACTIVE.load(Ordering::Acquire) as u32)
            || !status_put_u32(writer, RESIDENT.load(Ordering::Acquire) as u32)
            || !status_put_u32(writer, LAST_ERROR.load(Ordering::Acquire))
        {
            return -1;
        }
    }
    #[cfg(feature = "device")]
    for value in crate::target::query_status() {
        if !unsafe { status_put_u32(writer, value) } {
            return -1;
        }
    }
    status_writer_publish(writer);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_publish_native_app(_ctx: *const ContextV1) -> i32 {
    -103
}

#[unsafe(no_mangle)]
pub extern "C" fn canopus_mod_publish_native_app_stage(_ctx: *const ContextV1, stage: u32) -> i32 {
    #[cfg(feature = "device")]
    let result = crate::target::native_app::install_stage(stage)
        .map(|()| 0)
        .unwrap_or_else(|error| error);
    #[cfg(not(feature = "device"))]
    let result = if matches!(stage, 1 | 2) { 0 } else { -103 };
    if result != 0 {
        LAST_ERROR.store(result as u32, Ordering::Release);
    }
    result
}

#[unsafe(no_mangle)]
pub static canopus_module_descriptor: ModuleDescriptorV1 = ModuleDescriptorV1 {
    struct_size: core::mem::size_of::<ModuleDescriptorV1>() as u32,
    abi_major: ABI_MAJOR,
    abi_minor: ABI_MINOR,
    flags: FLAG_HAS_NATIVE_APP
        | FLAG_NATIVE_APP_STANDALONE
        | FLAG_REGISTERS_LAUNCHER_ENTRY
        | FLAG_REQUIRES_UI_DISPATCHER
        | FLAG_APP_UNREGISTER_REBOOT_REQUIRED,
    module_id: pack(b"lyra_player"),
    module_version: pack(b"0.1.0"),
    build_id: pack(b"lyra-player-0.1.0"),
    target_id: pack(TARGET_ID),
    prepare: Some(canopus_mod_prepare),
    activate: Some(canopus_mod_activate),
    deactivate: Some(canopus_mod_deactivate),
    stop: Some(canopus_mod_stop),
    query: Some(canopus_mod_query),
    publish_native_app: Some(canopus_mod_publish_native_app),
    publish_native_app_stage: Some(canopus_mod_publish_native_app_stage),
};

#[unsafe(no_mangle)]
pub extern "C" fn canopus_module_descriptor_ptr() -> *const ModuleDescriptorV1 {
    &canopus_module_descriptor
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopus_runtime::status_writer_init;

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn query_publishes_host_lifecycle_status() {
        let mut bytes = [0u8; 16];
        let mut writer = StatusWriterV1 {
            buf: core::ptr::null_mut(),
            capacity: 0,
            used: 0,
            dropped: 0,
            snap: SnapshotV1 { sequence: 0 },
        };
        assert!(unsafe { status_writer_init(&mut writer, bytes.as_mut_ptr(), bytes.len() as u32) });

        assert_eq!(canopus_mod_prepare(core::ptr::null()), 0);
        assert_eq!(canopus_mod_activate(core::ptr::null()), 0);
        assert_eq!(canopus_mod_query(&mut writer), 0);
        assert_eq!(writer.used, 16);
        assert_eq!(writer.dropped, 0);
        assert_eq!(writer.snap.sequence & 1, 0);
        assert_eq!(read_u32(&bytes, 0), MAGIC);
        assert_eq!(read_u32(&bytes, 4), 1);
        assert_eq!(read_u32(&bytes, 8), 1);
        assert_eq!(read_u32(&bytes, 12), 0);
    }

    #[test]
    fn query_rejects_null_and_short_writers() {
        assert_eq!(canopus_mod_query(core::ptr::null_mut()), -1);

        let mut bytes = [0u8; 12];
        let mut writer = StatusWriterV1 {
            buf: core::ptr::null_mut(),
            capacity: 0,
            used: 0,
            dropped: 0,
            snap: SnapshotV1 { sequence: 0 },
        };
        assert!(unsafe { status_writer_init(&mut writer, bytes.as_mut_ptr(), bytes.len() as u32) });
        assert_eq!(canopus_mod_query(&mut writer), -1);
        assert_eq!(writer.dropped, 1);
    }
}
