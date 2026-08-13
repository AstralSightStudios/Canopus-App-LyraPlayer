//! Small NuttX filesystem adapter for versioned Lyra JSON state.

use alloc::vec::Vec;
use core::ffi::c_void;

use canopus_target_private::{
    O_RDONLY, nuttx_close, nuttx_create, nuttx_open, nuttx_read, nuttx_rename, nuttx_unlink,
    nuttx_write,
};
use lyra_player_core::persistence::Store;

const O_WRONLY: i32 = 2;
const O_CREAT: i32 = 4;
const O_TRUNC: i32 = 32;
const MODE_USER_RW: u32 = 0o600;
const MAX_JSON_BYTES: usize = 128 * 1024;

pub fn write_atomic_bytes(path: &str, bytes: &[u8]) -> Result<(), i32> {
    FsStore.write_atomic(path, bytes)
}

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

impl Store for FsStore {
    type Error = i32;

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        let path = c_path(path)?;
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
            if output.len() + count as usize > MAX_JSON_BYTES {
                let _ = unsafe { nuttx_close(fd) };
                return Err(-2);
            }
            output.extend_from_slice(&chunk[..count as usize]);
        }
        let close_result = unsafe { nuttx_close(fd) };
        if close_result < 0 {
            return Err(close_result);
        }
        Ok(Some(output))
    }

    fn write_atomic(&mut self, path: &str, bytes: &[u8]) -> Result<(), Self::Error> {
        let temporary = alloc::format!("{path}.tmp");
        let temporary_c = c_path(&temporary)?;
        let destination_c = c_path(path)?;
        let fd = unsafe {
            nuttx_create(
                temporary_c.as_ptr(),
                O_WRONLY | O_CREAT | O_TRUNC,
                MODE_USER_RW,
            )
        };
        if fd < 0 {
            return Err(fd);
        }
        let mut offset = 0usize;
        while offset < bytes.len() {
            let remaining = bytes.len() - offset;
            let count = u32::try_from(remaining).unwrap_or(u32::MAX);
            let written =
                unsafe { nuttx_write(fd, bytes[offset..].as_ptr().cast::<c_void>(), count) };
            if written <= 0 {
                let _ = unsafe { nuttx_close(fd) };
                let _ = unsafe { nuttx_unlink(temporary_c.as_ptr()) };
                return Err(written);
            }
            offset += written as usize;
        }
        let close_result = unsafe { nuttx_close(fd) };
        if close_result < 0 {
            let _ = unsafe { nuttx_unlink(temporary_c.as_ptr()) };
            return Err(close_result);
        }
        let result = unsafe { nuttx_rename(temporary_c.as_ptr(), destination_c.as_ptr()) };
        if result < 0 {
            let _ = unsafe { nuttx_unlink(temporary_c.as_ptr()) };
            return Err(result);
        }
        Ok(())
    }

    fn remove(&mut self, path: &str) -> Result<(), Self::Error> {
        let path = c_path(path)?;
        let result = unsafe { nuttx_unlink(path.as_ptr()) };
        if result < 0 { Err(result) } else { Ok(()) }
    }
}

pub fn load() -> (
    Option<lyra_player_core::Session>,
    Vec<lyra_player_core::Song>,
) {
    let mut store = FsStore;
    let session = lyra_player_core::persistence::load_session(&mut store)
        .ok()
        .flatten();
    let library = lyra_player_core::persistence::load_library(&mut store).unwrap_or_default();
    (session, library)
}

pub fn save_session(session: &lyra_player_core::Session) -> Result<(), i32> {
    lyra_player_core::persistence::save_session(&mut FsStore, session).map_err(map_error)
}

pub fn clear_session() -> Result<(), i32> {
    lyra_player_core::persistence::clear_session(&mut FsStore).map_err(map_error)
}

fn map_error(error: lyra_player_core::persistence::PersistenceError<i32>) -> i32 {
    match error {
        lyra_player_core::persistence::PersistenceError::Storage(error) => error,
        lyra_player_core::persistence::PersistenceError::Json => -3,
        lyra_player_core::persistence::PersistenceError::Version => -4,
    }
}
