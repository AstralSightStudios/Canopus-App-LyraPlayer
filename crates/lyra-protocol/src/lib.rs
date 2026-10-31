//! The binary protocol between the Lyra importer (an AstroBox plugin) and the
//! Lyra Player module, carried by CanopusConnect messages (ADR-CAN-009).
//!
//! A CanopusConnect message is one frame of at most 480 bytes, delivered in
//! order and exactly once while the phone listens. Everything is little
//! endian. A frame starts with an opcode:
//!
//! * phone -> watch requests: `[op][req: u16][payload]`, except `DATA`, which
//!   is `[op][asset: u8][offset: u32][bytes]` and has no reply of its own;
//! * watch -> phone replies: `[op | 0x80][req: u16][status: u8][payload]`;
//! * watch -> phone events: `[0xC0 + n][payload]`.
//!
//! Audio, cover and lyrics are streamed with `DATA` frames at increasing
//! offsets; the watch confirms progress with `PROGRESS` events (at least every
//! [`PROGRESS_STEP`] bytes, and whenever it sees an offset it did not expect,
//! in which case the phone resumes from the offset the event names).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod codec;
mod crc;

pub use codec::*;
pub use crc::{Crc32, crc32};

pub const MODULE_ID: &str = "lyra_player";
/// The newest protocol this crate speaks; the module answers `HELLO` with its own.
/// Version 2 added playlists.
pub const VERSION: u8 = 2;
/// What a client announces in `HELLO` when it only needs the importer: a module that
/// speaks anything from here on accepts it, and its reply says what else it can do.
pub const MIN_VERSION: u8 = 1;
/// `HELLO` reply capability bits.
/// The module accepts frames the Supervisor delivers without a reply per frame, so the phone
/// may send `DATA` in bursts instead of one answered message at a time.
pub const CAP_STREAM: u8 = 1;
pub const MAX_PLAYLISTS: usize = 32;
pub const MAX_PLAYLIST_NAME: usize = 64;
pub const MAX_PLAYLIST_TRACKS: usize = 1024;

/// CanopusConnect carries at most this many bytes per message.
pub const FRAME_MAX: usize = 480;
/// Audio, cover and lyrics bytes per `DATA` frame.
pub const DATA_MAX: usize = 464;
/// The watch confirms at least every this many bytes; the phone never has more
/// than [`WINDOW`] unconfirmed bytes in flight.
pub const PROGRESS_STEP: u32 = 4 * 1024;
pub const WINDOW: u32 = 16 * 1024;

pub const MAX_NAME: usize = 96;
pub const MAX_ARTISTS: usize = 4;
pub const MAX_ARTIST: usize = 48;
pub const MAX_ALBUM: usize = 64;
/// Hard caps the watch enforces on the files of one track.
pub const MAX_AUDIO_BYTES: u32 = 64 * 1024 * 1024;
pub const MAX_COVER_BYTES: u32 = 256 * 1024;
pub const MAX_LYRICS_BYTES: u32 = 64 * 1024;
