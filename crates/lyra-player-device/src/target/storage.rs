//! Reads the library the importer maintains under the module directory.
//! Raw firmware file calls are used for reads because the Bluetooth thread
//! streams the same files; the module context is only needed for `stat`.

use alloc::{string::String, vec::Vec};
use core::ffi::c_void;

use canopus_runtime::fs;
use canopus_target_private::{O_RDONLY, nuttx_close, nuttx_open, nuttx_read};
use lyra_player_core::{
    ROOT,
    lyrics::{Lyrics, parse_lrc},
    persistence::{LIBRARY_FILE, Library, PersistenceError, Store},
};

use crate::module::context;

const MAX_LIBRARY_BYTES: usize = 256 * 1024;
const MAX_LYRICS_BYTES: usize = 64 * 1024;

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

/// Whether `path` names something inside the module directory.
pub fn is_module_path(path: &str) -> bool {
    path.strip_prefix(ROOT)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|rest| {
            !rest.is_empty()
                && rest.len() <= 200
                && !rest.contains("..")
                && !rest.bytes().any(|byte| byte == 0 || byte.is_ascii_control())
        })
}

fn read_bounded(path: &str, limit: usize) -> Result<Option<Vec<u8>>, i32> {
    if !is_module_path(path) {
        return Err(-7);
    }
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
        read_bounded(path, MAX_LIBRARY_BYTES)
    }
}

pub fn load_library() -> Result<Library, i32> {
    lyra_player_core::persistence::load_library(&mut FsStore).map_err(|error| match error {
        PersistenceError::Storage(error) => error,
        PersistenceError::Json => -5,
        PersistenceError::Version => -6,
    })
}

/// A cheap fingerprint of the library file (size and modification time), 0
/// when there is none. A changed stamp is what makes the page reload it.
pub fn library_stamp() -> u32 {
    let ctx = context();
    if ctx.is_null() {
        return 0;
    }
    fs::stat(ctx, LIBRARY_FILE)
        .map(|stat| stat.size.rotate_left(16) ^ stat.mtime ^ 0x4C59_0000)
        .unwrap_or(0)
}

pub fn read_lyrics(path: &str) -> Option<Lyrics> {
    let bytes = read_bounded(path, MAX_LYRICS_BYTES).ok()??;
    let text: String = String::from_utf8(bytes).ok()?;
    let lyrics = parse_lrc(&text);
    (!lyrics.is_empty()).then_some(lyrics)
}

/// Emulator aid: a `dry-run` file in the module directory makes the player
/// run without an audio sink.
pub fn dry_run_enabled() -> bool {
    let ctx = context();
    !ctx.is_null() && fs::exists(ctx, "dry-run")
}
