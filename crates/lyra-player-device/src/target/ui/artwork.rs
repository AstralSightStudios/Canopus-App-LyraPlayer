//! List thumbnails: built on the UI thread from a track's `cover.bin`, at most one per refresh
//! tick, and kept next to it as `thumb.bin` (see `lyra_player_core::thumbnail`). Reads go
//! through the raw firmware calls like the rest of the library code; the module context is
//! only used for `stat` and the write.

use alloc::{format, vec::Vec};
use core::ffi::c_void;

use canopus_runtime::fs;
use canopus_target_private::{O_RDONLY, nuttx_close, nuttx_open, nuttx_read};
use lyra_player_core::{ROOT, THUMB_FILE, thumbnail};

use crate::module::context;

fn relative(id: u64, name: &str) -> alloc::string::String {
    format!("tracks/{id}/{name}")
}

/// The NUL-terminated absolute path LVGL opens.
pub fn path(id: u64) -> Vec<u8> {
    super::lv::c_string(&format!("{ROOT}/tracks/{id}/{THUMB_FILE}"))
}

/// Whether the thumbnail exists at this edge and is not older than its cover.
pub fn is_current(id: u64, edge: u16) -> bool {
    let ctx = context();
    if ctx.is_null() {
        return false;
    }
    let Ok(thumb) = fs::stat(ctx, &relative(id, THUMB_FILE)) else {
        return false;
    };
    let cover_mtime = fs::stat(ctx, &relative(id, "cover.bin")).map_or(0, |stat| stat.mtime);
    thumb.size as usize == thumbnail::file_bytes(edge) && thumb.mtime >= cover_mtime
}

struct Reader(i32);

impl Reader {
    fn fill(&mut self, buffer: &mut [u8]) -> bool {
        let mut filled = 0;
        while filled < buffer.len() {
            let rest = &mut buffer[filled..];
            let count = unsafe {
                nuttx_read(
                    self.0,
                    rest.as_mut_ptr().cast::<c_void>(),
                    rest.len() as u32,
                )
            };
            if count <= 0 {
                return false;
            }
            filled += count as usize;
        }
        true
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = unsafe { nuttx_close(self.0) };
    }
}

/// Builds and stores the thumbnail of track `id`; false when there is no usable cover.
pub fn build(id: u64, edge: u16, radius: u16) -> bool {
    let ctx = context();
    if ctx.is_null() {
        return false;
    }
    let cover = super::lv::c_string(&format!("{ROOT}/tracks/{id}/cover.bin"));
    let fd = unsafe { nuttx_open(cover.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return false;
    }
    let mut reader = Reader(fd);
    let mut header = [0u8; thumbnail::HEADER_BYTES];
    if !reader.fill(&mut header) {
        return false;
    }
    let Some(header) = thumbnail::parse_header(&header) else {
        return false;
    };
    let Some(bytes) = thumbnail::build(header, edge, radius, |row| reader.fill(row)) else {
        return false;
    };
    drop(reader);
    fs::write_all(ctx, &relative(id, THUMB_FILE), &bytes)
        .is_ok_and(|written| written == bytes.len())
}
