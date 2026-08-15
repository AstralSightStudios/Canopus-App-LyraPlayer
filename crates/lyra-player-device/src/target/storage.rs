//! Read-only adapter for the Lyra Import quick application's shared files.

use alloc::{string::String, vec::Vec};
use core::ffi::c_void;

use canopus_target_private::{O_RDONLY, nuttx_close, nuttx_lseek, nuttx_open, nuttx_read};
use lyra_player_core::persistence::Store;

pub const IMPORT_ROOT: &str = "/data/files/com.canopus.lyraimport/lyra";
const LEGACY_IMPORT_ROOT: &str = "/data/quickapp/files/com.canopus.lyraimport/lyra";
const MAX_MANIFEST_BYTES: usize = 256 * 1024;

pub struct FsStore;

fn c_path(path: &str) -> Result<Vec<u8>, i32> {
    if path.as_bytes().contains(&0) {
        return Err(-1);
    }
    let mut output = Vec::with_capacity(path.len() + 1);
    output.extend_from_slice(path.as_bytes());
    output.push(0);
    Ok(output)
}

fn path_exists(path: &str) -> bool {
    let Ok(path) = c_path(path) else {
        return false;
    };
    let fd = unsafe { nuttx_open(path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return false;
    }
    unsafe { nuttx_close(fd) >= 0 }
}

pub fn resolve_path(path: &str) -> Option<String> {
    if !path.starts_with(IMPORT_ROOT) && !path.starts_with(LEGACY_IMPORT_ROOT) {
        return None;
    }
    if path_exists(path) {
        return Some(String::from(path));
    }
    let relative = path.strip_prefix(IMPORT_ROOT)?.strip_prefix('/')?;
    let legacy = alloc::format!("{LEGACY_IMPORT_ROOT}/{relative}");
    path_exists(&legacy).then_some(legacy)
}

fn read_bounded(path: &str, limit: usize) -> Result<Option<Vec<u8>>, i32> {
    let Some(path) = resolve_path(path) else {
        return Ok(None);
    };
    let path = c_path(&path)?;
    let fd = unsafe { nuttx_open(path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return Ok(None);
    }
    let mut output = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count =
            unsafe { nuttx_read(fd, chunk.as_mut_ptr().cast::<c_void>(), chunk.len() as u32) };
        if count < 0 {
            let _ = unsafe { nuttx_close(fd) };
            return Err(count);
        }
        if count == 0 {
            break;
        }
        if output.len() + count as usize > limit {
            let _ = unsafe { nuttx_close(fd) };
            return Err(-2);
        }
        output.extend_from_slice(&chunk[..count as usize]);
    }
    let result = unsafe { nuttx_close(fd) };
    if result < 0 {
        return Err(result);
    }
    Ok(Some(output))
}

impl Store for FsStore {
    type Error = i32;

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        read_bounded(path, MAX_MANIFEST_BYTES)
    }
}

pub fn load_library() -> Result<Vec<lyra_player_core::Song>, i32> {
    lyra_player_core::persistence::load_library(&mut FsStore).map_err(map_error)
}

pub fn validate_lvgl_v9_image(path: &str, width: u16, height: u16) -> bool {
    let Some(path) = resolve_path(path) else {
        return false;
    };
    let Ok(path) = c_path(&path) else {
        return false;
    };
    let fd = unsafe { nuttx_open(path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return false;
    }
    let mut header = [0u8; 12];
    let mut read = 0usize;
    while read < header.len() {
        let count = unsafe {
            nuttx_read(
                fd,
                header[read..].as_mut_ptr().cast::<c_void>(),
                (header.len() - read) as u32,
            )
        };
        if count <= 0 {
            let _ = unsafe { nuttx_close(fd) };
            return false;
        }
        read += count as usize;
    }
    let length = unsafe { nuttx_lseek(fd, 0, 2) };
    let close = unsafe { nuttx_close(fd) };
    let stride = width.checked_mul(4);
    let expected = 12i64 + i64::from(width) * i64::from(height) * 4;
    close >= 0
        && length == expected
        && header[0..4] == [0x19, 0x10, 0, 0]
        && u16::from_le_bytes([header[4], header[5]]) == width
        && u16::from_le_bytes([header[6], header[7]]) == height
        && stride.is_some_and(|stride| u16::from_le_bytes([header[8], header[9]]) == stride)
        && header[10..12] == [0, 0]
}

fn map_error(error: lyra_player_core::persistence::PersistenceError<i32>) -> i32 {
    match error {
        lyra_player_core::persistence::PersistenceError::Storage(error) => error,
        lyra_player_core::persistence::PersistenceError::Json => -5,
        lyra_player_core::persistence::PersistenceError::Version => -6,
        lyra_player_core::persistence::PersistenceError::UnsafePath => -7,
    }
}
