//! Reads the playing track ahead of the decoder, on the UI owner thread.
//!
//! The Bluetooth owner that writes the MP3 into `/dev/canopus_audio` is the thread the
//! BluetoothAudio module paces the headset's packets on. A file read there waits for whatever else
//! holds the flash and the file system -- covers and glyphs the UI is drawing, fonts, an import
//! writing a track -- and every such wait held back the headset's audio as well. Here a
//! Supervisor background timer (it fires on the UI owner every 10 ms, also with the screen off)
//! keeps a ring of chunks filled, and the Bluetooth owner only copies bytes out of RAM.
//!
//! The Bluetooth owner names the file with [`request`]; every chunk carries the epoch of the
//! request it was read for, so a track change only bumps the epoch and the consumer drops what is
//! left of the old file. A Supervisor without background timers (older builds on .036 / .139) could
//! not keep reading with the screen off: [`start`] gives up there and the audio code reads the file
//! itself.

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::atomic::{AtomicPtr, AtomicU8, AtomicU32, Ordering, fence},
};

use canopus_abi::ContextV1;
use canopus_target_private::{
    O_RDONLY, get_errno, nuttx_close, nuttx_lseek, nuttx_open, nuttx_read,
};
use lyra_player_core::playback::id3v2_tag_len;

use crate::module::context;

const CHUNK: usize = 4 * 1024;
/// 32 KiB ahead of the decoder (which holds another 16 KiB): 1.2 s at 320 kbit/s.
const CHUNKS: u32 = 8;
const TICK_MS: u32 = 10;
/// Reads one tick may do, so that a tick never holds the UI thread for long.
const READS_PER_TICK: usize = 2;
/// Tag bytes one tick may read past when the file system cannot seek.
const SKIP_PER_TICK: u32 = 32 * 1024;
const PATH_MAX: usize = 256;
const SEEK_SET: i32 = 0;

const STATUS_DATA: i32 = 0;
const STATUS_END: i32 = 1;

const STATE_IDLE: u32 = 0;
const STATE_RUNNING: u32 = 1;
const STATE_UNSUPPORTED: u32 = 2;

#[repr(C)]
struct Chunk {
    epoch: u32,
    len: u32,
    /// [`STATUS_DATA`], or [`STATUS_END`] / a negative error (both without bytes).
    status: i32,
    bytes: [u8; CHUNK],
}

/// The reader's state; only the timer on the UI owner touches it.
#[repr(C)]
struct Reader {
    epoch: u32,
    /// The file descriptor plus one: 0 means none, so that all-zero is a valid state.
    fd_plus_one: i32,
    /// The request is served: its end or its failure has been published.
    finished: bool,
    /// End or failure still to publish once the ring has room (0: none).
    pending_status: i32,
    /// Tag bytes still to read past.
    skip: u32,
    /// Start of the file read while looking for a tag, which turned out to be audio.
    prefix: [u8; 10],
    prefix_len: u32,
}

/// All-zero is a valid initial state of every field.
#[repr(C)]
struct Shared {
    // The request, written by the Bluetooth owner under a sequence count (odd while writing).
    request_seq: AtomicU32,
    request_epoch: AtomicU32,
    request_len: AtomicU32,
    request_path: [AtomicU8; PATH_MAX],
    // Chunks `head..tail` are published; the consumer owns `head`, the reader owns `tail`.
    head: AtomicU32,
    tail: AtomicU32,
    /// The consumer's position inside the chunk at `head`.
    offset: AtomicU32,
    chunks: [UnsafeCell<Chunk>; CHUNKS as usize],
    reader: UnsafeCell<Reader>,
}

static SHARED: AtomicPtr<Shared> = AtomicPtr::new(core::ptr::null_mut());
static STATE: AtomicU32 = AtomicU32::new(STATE_IDLE);

fn shared() -> Option<&'static Shared> {
    let pointer = SHARED.load(Ordering::Acquire);
    // SAFETY: published once by `start` and never freed (the module is resident).
    (!pointer.is_null()).then(|| unsafe { &*pointer })
}

fn neg_errno(result: i32) -> i32 {
    if result == -1 {
        let errno = unsafe { get_errno() };
        if errno > 0 {
            return -errno;
        }
    }
    result
}

/// Starts the reader. Call on the UI owner: the Supervisor sets up its background pump there.
/// Idempotent; on firmware without background timers it gives up once and for all.
pub fn start() {
    if STATE.load(Ordering::Acquire) != STATE_IDLE {
        return;
    }
    let ctx = context();
    if ctx.is_null() {
        return;
    }
    let time = unsafe { (*ctx).time };
    if time.is_null() {
        STATE.store(STATE_UNSUPPORTED, Ordering::Release);
        return;
    }
    let layout = Layout::new::<Shared>();
    // SAFETY: `Shared` is valid all-zero.
    let pointer = unsafe { alloc_zeroed(layout) }.cast::<Shared>();
    if pointer.is_null() {
        return;
    }
    let ctx = ctx as *const ContextV1;
    let started = unsafe {
        let id = ((*time).timer_create)(ctx, TICK_MS, 1, tick, pointer.cast());
        // Without the background pump the reader would stop with the screen and starve the
        // headset; the audio code then keeps reading by itself.
        if id < 0 {
            false
        } else if ((*time).timer_set_background)(ctx, id, 1) < 0
            || ((*time).timer_start)(ctx, id) < 0
        {
            let _ = ((*time).timer_destroy)(ctx, id);
            false
        } else {
            true
        }
    };
    if !started {
        unsafe { dealloc(pointer.cast(), layout) };
        STATE.store(STATE_UNSUPPORTED, Ordering::Release);
        return;
    }
    SHARED.store(pointer, Ordering::Release);
    STATE.store(STATE_RUNNING, Ordering::Release);
}

// ---------------------------------------------------------------------------
// The Bluetooth owner's side
// ---------------------------------------------------------------------------

/// Asks for `path` to be read from the start; the chunks of this request carry the returned
/// epoch. `None` when the reader does not run: the caller reads the file itself.
pub fn request(path: &str) -> Option<u32> {
    if STATE.load(Ordering::Acquire) != STATE_RUNNING {
        return None;
    }
    let shared = shared()?;
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.len() >= PATH_MAX {
        return None;
    }
    Some(shared.publish(bytes))
}

/// Stops reading; the file is closed on the reader's next tick.
pub fn stop() {
    if let Some(shared) = shared() {
        shared.publish(&[]);
    }
}

pub enum Front {
    Data(&'static [u8]),
    /// Nothing read yet; try again on the next tick.
    Pending,
    End,
    Failed(i32),
}

/// The unread bytes at the front of the ring for request `epoch`. Chunks of older requests are
/// dropped on the way.
pub fn front(epoch: u32) -> Front {
    let Some(shared) = shared() else {
        return Front::Failed(-22);
    };
    loop {
        let head = shared.head.load(Ordering::Relaxed);
        if head == shared.tail.load(Ordering::Acquire) {
            return Front::Pending;
        }
        // SAFETY: the reader filled this chunk before it published `tail` past it and reuses it
        // only after `head` has moved on, so until then it is the consumer's to read.
        let chunk = unsafe { &*shared.chunks[(head % CHUNKS) as usize].get() };
        let offset = shared.offset.load(Ordering::Relaxed) as usize;
        if chunk.epoch != epoch || (chunk.status == STATUS_DATA && offset >= chunk.len as usize) {
            shared.advance(head);
            continue;
        }
        return match chunk.status {
            STATUS_DATA => Front::Data(&chunk.bytes[offset..chunk.len as usize]),
            STATUS_END => Front::End,
            error => Front::Failed(error),
        };
    }
}

/// Marks `count` bytes of the last [`Front::Data`] as taken.
pub fn consume(count: usize) {
    let Some(shared) = shared() else { return };
    let head = shared.head.load(Ordering::Relaxed);
    if head == shared.tail.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: as in `front`.
    let chunk = unsafe { &*shared.chunks[(head % CHUNKS) as usize].get() };
    let offset = shared.offset.load(Ordering::Relaxed) as usize + count;
    if offset >= chunk.len as usize {
        shared.advance(head);
    } else {
        shared.offset.store(offset as u32, Ordering::Relaxed);
    }
}

impl Shared {
    fn advance(&self, head: u32) {
        self.offset.store(0, Ordering::Relaxed);
        self.head.store(head.wrapping_add(1), Ordering::Release);
    }

    /// Writes a new request (an empty path stops reading) and returns its epoch.
    fn publish(&self, path: &[u8]) -> u32 {
        let sequence = self.request_seq.load(Ordering::Relaxed);
        self.request_seq
            .store(sequence.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, byte) in self.request_path.iter().zip(path) {
            slot.store(*byte, Ordering::Relaxed);
        }
        self.request_len.store(path.len() as u32, Ordering::Relaxed);
        let epoch = match self.request_epoch.load(Ordering::Relaxed).wrapping_add(1) {
            0 => 1,
            epoch => epoch,
        };
        self.request_epoch.store(epoch, Ordering::Relaxed);
        self.request_seq
            .store(sequence.wrapping_add(2), Ordering::Release);
        epoch
    }

    /// A new request, if one was published completely since the reader last looked.
    fn take_request(&self, current: u32) -> Option<(u32, [u8; PATH_MAX], usize)> {
        let sequence = self.request_seq.load(Ordering::Acquire);
        if sequence & 1 != 0 {
            return None;
        }
        let epoch = self.request_epoch.load(Ordering::Relaxed);
        if epoch == current {
            return None;
        }
        let len = (self.request_len.load(Ordering::Relaxed) as usize).min(PATH_MAX - 1);
        let mut path = [0u8; PATH_MAX];
        for (byte, slot) in path.iter_mut().zip(&self.request_path[..len]) {
            *byte = slot.load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        // Torn by a request written meanwhile: the next tick reads that one.
        (self.request_seq.load(Ordering::Relaxed) == sequence).then_some((epoch, path, len))
    }
}

// ---------------------------------------------------------------------------
// The reader, on the UI owner
// ---------------------------------------------------------------------------

unsafe extern "C" fn tick(_ctx: *const ContextV1, _timer: i32, user: *mut c_void) {
    if user.is_null() {
        return;
    }
    // SAFETY: `user` is the `Shared` this timer was created with; it is never freed once the timer
    // runs. Only this timer touches `reader` and the chunk at `tail`.
    let shared = unsafe { &*user.cast::<Shared>() };
    let reader = unsafe { &mut *shared.reader.get() };
    read_ahead(shared, reader);
}

fn read_ahead(shared: &Shared, reader: &mut Reader) {
    if let Some((epoch, path, len)) = shared.take_request(reader.epoch) {
        reader.close();
        reader.epoch = epoch;
        reader.finished = len == 0;
        reader.pending_status = 0;
        reader.skip = 0;
        reader.prefix_len = 0;
        if len != 0 {
            let fd = unsafe { nuttx_open(path.as_ptr(), O_RDONLY) };
            if fd < 0 {
                reader.pending_status = match neg_errno(fd) {
                    0 | STATUS_END => -2,
                    error => error,
                };
            } else {
                reader.fd_plus_one = fd + 1;
                reader.find_tag();
            }
        }
    }
    if reader.finished || (reader.skip != 0 && !reader.skip_tag()) {
        return;
    }
    for _ in 0..READS_PER_TICK {
        let tail = shared.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(shared.head.load(Ordering::Acquire)) >= CHUNKS {
            return;
        }
        // SAFETY: the chunk at `tail` is not published, so the consumer does not read it.
        let chunk = unsafe { &mut *shared.chunks[(tail % CHUNKS) as usize].get() };
        chunk.epoch = reader.epoch;
        chunk.len = 0;
        chunk.status = STATUS_DATA;
        if reader.pending_status != 0 {
            chunk.status = reader.pending_status;
            reader.finished = true;
        } else {
            let prefix = reader.prefix_len as usize;
            chunk.bytes[..prefix].copy_from_slice(&reader.prefix[..prefix]);
            reader.prefix_len = 0;
            let count = unsafe {
                nuttx_read(
                    reader.fd(),
                    chunk.bytes[prefix..].as_mut_ptr().cast::<c_void>(),
                    (CHUNK - prefix) as u32,
                )
            };
            if count > 0 {
                chunk.len = (prefix + count as usize) as u32;
            } else {
                // End of file or a failure: whatever came before it goes out first.
                reader.pending_status = if count == 0 {
                    STATUS_END
                } else {
                    match neg_errno(count) {
                        0 | STATUS_END => -5,
                        error => error,
                    }
                };
                reader.close();
                if prefix == 0 {
                    chunk.status = reader.pending_status;
                    reader.finished = true;
                } else {
                    chunk.len = prefix as u32;
                }
            }
        }
        shared.tail.store(tail.wrapping_add(1), Ordering::Release);
        if reader.finished {
            return;
        }
    }
}

impl Reader {
    fn fd(&self) -> i32 {
        self.fd_plus_one - 1
    }

    fn close(&mut self) {
        if self.fd_plus_one > 0 {
            let _ = unsafe { nuttx_close(self.fd()) };
            self.fd_plus_one = 0;
        }
    }

    /// Reads the first ten bytes: a tag is stepped over, audio is kept for the first chunk.
    fn find_tag(&mut self) {
        let mut header = [0u8; 10];
        let filled = read_full(self.fd(), &mut header);
        match id3v2_tag_len(&header[..filled]) {
            Some(tag) => {
                // Some storage backends cannot seek; reading past the tag works everywhere.
                let position = unsafe { nuttx_lseek(self.fd(), i64::from(tag), SEEK_SET) };
                if position != i64::from(tag) {
                    self.skip = tag - filled as u32;
                }
            }
            None => {
                self.prefix[..filled].copy_from_slice(&header[..filled]);
                self.prefix_len = filled as u32;
            }
        }
    }

    /// Reads past (part of) the tag; `true` once it is behind.
    fn skip_tag(&mut self) -> bool {
        let mut scratch = [0u8; 1024];
        let mut budget = SKIP_PER_TICK;
        while self.skip != 0 && budget != 0 {
            let want = (scratch.len() as u32).min(self.skip).min(budget);
            let count = unsafe { nuttx_read(self.fd(), scratch.as_mut_ptr().cast::<c_void>(), want) };
            if count <= 0 {
                // The file ends inside its tag: the next read reports the end.
                self.skip = 0;
                break;
            }
            self.skip -= count as u32;
            budget -= count as u32;
        }
        self.skip == 0
    }
}

/// Reads until `buffer` is full or the file ends; the count read.
pub fn read_full(fd: i32, buffer: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buffer.len() {
        let count = unsafe {
            nuttx_read(
                fd,
                buffer[filled..].as_mut_ptr().cast::<c_void>(),
                (buffer.len() - filled) as u32,
            )
        };
        if count <= 0 {
            break;
        }
        filled += count as usize;
    }
    filled
}
