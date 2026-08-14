//! Read-only adapter for the Lyra Import quick application's shared files.

use alloc::vec::Vec;
use core::ffi::c_void;

use canopus_target_private::{O_RDONLY, nuttx_close, nuttx_open, nuttx_read};
use lyra_player_core::persistence::Store;

const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_LYRICS_BYTES: usize = 512 * 1024;

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

fn read_bounded(path: &str, limit: usize) -> Result<Option<Vec<u8>>, i32> {
    let path = c_path(path)?;
    let fd = unsafe { nuttx_open(path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return Ok(None);
    }
    let mut output = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = unsafe { nuttx_read(fd, chunk.as_mut_ptr().cast::<c_void>(), chunk.len() as u32) };
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

pub fn load_lyrics(path: &str) -> Result<Option<alloc::string::String>, i32> {
    if !lyra_player_core::persistence::is_safe_lyrics_path(path) {
        return Err(-3);
    }
    let Some(bytes) = read_bounded(path, MAX_LYRICS_BYTES)? else {
        return Ok(None);
    };
    alloc::string::String::from_utf8(bytes).map(Some).map_err(|_| -4)
}

fn map_error(error: lyra_player_core::persistence::PersistenceError<i32>) -> i32 {
    match error {
        lyra_player_core::persistence::PersistenceError::Storage(error) => error,
        lyra_player_core::persistence::PersistenceError::Json => -5,
        lyra_player_core::persistence::PersistenceError::Version => -6,
        lyra_player_core::persistence::PersistenceError::UnsafePath => -7,
    }
}
