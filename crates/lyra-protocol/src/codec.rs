use alloc::{string::String, vec::Vec};

use crate::{
    DATA_MAX, FRAME_MAX, MAX_ALBUM, MAX_ARTIST, MAX_ARTISTS, MAX_AUDIO_BYTES, MAX_COVER_BYTES,
    MAX_LYRICS_BYTES, MAX_NAME,
};

pub mod op {
    pub const HELLO: u8 = 0x01;
    pub const LIST: u8 = 0x02;
    pub const BEGIN: u8 = 0x10;
    pub const DATA: u8 = 0x11;
    pub const ASSET_END: u8 = 0x12;
    pub const COMMIT: u8 = 0x13;
    pub const ABORT: u8 = 0x14;
    pub const DELETE: u8 = 0x20;
    pub const MOVE: u8 = 0x21;
    pub const PLAY: u8 = 0x30;
    pub const CONTROL: u8 = 0x31;
    pub const REPLY: u8 = 0x80;
    pub const PROGRESS: u8 = 0xC0;
    pub const NOW_PLAYING: u8 = 0xC1;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Asset {
    Audio = 1,
    Cover = 2,
    Lyrics = 3,
}

impl Asset {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Audio),
            2 => Some(Self::Cover),
            3 => Some(Self::Lyrics),
            _ => None,
        }
    }

    pub const fn max_bytes(self) -> u32 {
        match self {
            Self::Audio => MAX_AUDIO_BYTES,
            Self::Cover => MAX_COVER_BYTES,
            Self::Lyrics => MAX_LYRICS_BYTES,
        }
    }

    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Audio => "audio.mp3",
            Self::Cover => "cover.bin",
            Self::Lyrics => "lyrics.lrc",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    BadRequest = 1,
    NoSpace = 2,
    Busy = 3,
    Io = 4,
    Crc = 5,
    NotFound = 6,
    Unsupported = 7,
    Exists = 8,
    /// The request does not fit the transfer in progress.
    Sequence = 9,
}

impl Status {
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Ok,
            2 => Self::NoSpace,
            3 => Self::Busy,
            4 => Self::Io,
            5 => Self::Crc,
            6 => Self::NotFound,
            7 => Self::Unsupported,
            8 => Self::Exists,
            9 => Self::Sequence,
            _ => Self::BadRequest,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Control {
    Toggle = 1,
    Next = 2,
    Previous = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetSpec {
    pub kind: Asset,
    pub size: u32,
    pub crc32: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackMeta {
    pub id: u64,
    pub duration_ms: u32,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    /// `0xRRGGBB` colours the player fades between behind the cover.
    pub palette_top: u32,
    pub palette_bottom: u32,
    pub assets: Vec<AssetSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello { req: u16, version: u8 },
    List { req: u16, offset: u16, max: u8 },
    Begin { req: u16, meta: TrackMeta },
    Data { asset: Asset, offset: u32, bytes: Vec<u8> },
    AssetEnd { req: u16, asset: Asset },
    Commit { req: u16, id: u64 },
    Abort { req: u16 },
    Delete { req: u16, id: u64 },
    Move { req: u16, id: u64, up: bool },
    Play { req: u16, id: u64 },
    Control { req: u16, command: Control },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    pub id: u64,
    pub duration_ms: u32,
    pub name: String,
    pub artist: String,
}

impl ListEntry {
    pub fn encoded_len(&self) -> usize {
        8 + 4
            + 1
            + clip(&self.name, MAX_NAME).len()
            + 1
            + clip(&self.artist, MAX_ARTISTS * MAX_ARTIST).len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Hello {
        req: u16,
        status: Status,
        version: u8,
        max_data: u16,
        free_kib: u32,
        tracks: u16,
        revision: u32,
        width: u16,
        height: u16,
    },
    List {
        req: u16,
        status: Status,
        total: u16,
        offset: u16,
        entries: Vec<ListEntry>,
    },
    /// The answer to every other request; `value` is the library revision
    /// after `Commit`, `Delete` and `Move`, 0 otherwise.
    Ack {
        op: u8,
        req: u16,
        status: Status,
        value: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `next_offset` bytes of `asset` are safely stored; the phone continues
    /// from there.
    Progress {
        asset: Asset,
        next_offset: u32,
        status: Status,
    },
    NowPlaying {
        id: u64,
        state: u8,
        position_ms: u32,
        duration_ms: u32,
        volume: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Short,
    UnknownOp(u8),
    BadValue,
    BadText,
    TooLong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong;

/// The longest prefix of `text` of at most `max` bytes that ends on a
/// character boundary.
pub fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct Writer(Vec<u8>);

impl Writer {
    fn new(op: u8) -> Self {
        let mut bytes = Vec::with_capacity(64);
        bytes.push(op);
        Self(bytes)
    }
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn text(&mut self, text: &str, max: usize) {
        let text = clip(text, max.min(255));
        self.0.push(text.len() as u8);
        self.0.extend_from_slice(text.as_bytes());
    }
    fn finish(self) -> Result<Vec<u8>, TooLong> {
        if self.0.len() > FRAME_MAX {
            Err(TooLong)
        } else {
            Ok(self.0)
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < count {
            return Err(DecodeError::Short);
        }
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn text(&mut self, max: usize) -> Result<String, DecodeError> {
        let length = usize::from(self.u8()?);
        if length > max {
            return Err(DecodeError::TooLong);
        }
        core::str::from_utf8(self.take(length)?)
            .map(String::from)
            .map_err(|_| DecodeError::BadText)
    }
    fn rest(self) -> &'a [u8] {
        self.0
    }
}

impl Request {
    pub fn encode(&self) -> Result<Vec<u8>, TooLong> {
        match self {
            Self::Hello { req, version } => {
                let mut w = request(op::HELLO, *req);
                w.u8(*version);
                w.finish()
            }
            Self::List { req, offset, max } => {
                let mut w = request(op::LIST, *req);
                w.u16(*offset);
                w.u8(*max);
                w.finish()
            }
            Self::Begin { req, meta } => {
                let mut w = request(op::BEGIN, *req);
                w.u64(meta.id);
                w.u32(meta.duration_ms);
                w.u32(meta.palette_top & 0xFF_FFFF);
                w.u32(meta.palette_bottom & 0xFF_FFFF);
                w.text(&meta.name, MAX_NAME);
                let artists = &meta.artists[..meta.artists.len().min(MAX_ARTISTS)];
                w.u8(artists.len() as u8);
                for artist in artists {
                    w.text(artist, MAX_ARTIST);
                }
                w.text(&meta.album, MAX_ALBUM);
                w.u8(meta.assets.len().min(3) as u8);
                for spec in meta.assets.iter().take(3) {
                    w.u8(spec.kind as u8);
                    w.u32(spec.size);
                    w.u32(spec.crc32);
                }
                w.finish()
            }
            Self::Data {
                asset,
                offset,
                bytes,
            } => {
                if bytes.len() > DATA_MAX {
                    return Err(TooLong);
                }
                let mut w = Writer::new(op::DATA);
                w.u8(*asset as u8);
                w.u32(*offset);
                w.0.extend_from_slice(bytes);
                w.finish()
            }
            Self::AssetEnd { req, asset } => {
                let mut w = request(op::ASSET_END, *req);
                w.u8(*asset as u8);
                w.finish()
            }
            Self::Commit { req, id } => id_request(op::COMMIT, *req, *id),
            Self::Abort { req } => request(op::ABORT, *req).finish(),
            Self::Delete { req, id } => id_request(op::DELETE, *req, *id),
            Self::Move { req, id, up } => {
                let mut w = request(op::MOVE, *req);
                w.u64(*id);
                w.u8(u8::from(*up));
                w.finish()
            }
            Self::Play { req, id } => id_request(op::PLAY, *req, *id),
            Self::Control { req, command } => {
                let mut w = request(op::CONTROL, *req);
                w.u8(*command as u8);
                w.finish()
            }
        }
    }

    pub fn decode(frame: &[u8]) -> Result<Self, DecodeError> {
        if frame.len() > FRAME_MAX {
            return Err(DecodeError::TooLong);
        }
        let mut r = Reader(frame);
        let opcode = r.u8()?;
        if opcode == op::DATA {
            let asset = Asset::from_u8(r.u8()?).ok_or(DecodeError::BadValue)?;
            let offset = r.u32()?;
            let bytes = r.rest();
            if bytes.is_empty() || bytes.len() > DATA_MAX {
                return Err(DecodeError::BadValue);
            }
            return Ok(Self::Data {
                asset,
                offset,
                bytes: bytes.to_vec(),
            });
        }
        let req = r.u16()?;
        Ok(match opcode {
            op::HELLO => Self::Hello {
                req,
                version: r.u8()?,
            },
            op::LIST => Self::List {
                req,
                offset: r.u16()?,
                max: r.u8()?,
            },
            op::BEGIN => {
                let id = r.u64()?;
                let duration_ms = r.u32()?;
                let palette_top = r.u32()? & 0xFF_FFFF;
                let palette_bottom = r.u32()? & 0xFF_FFFF;
                let name = r.text(MAX_NAME)?;
                let count = usize::from(r.u8()?);
                if count > MAX_ARTISTS {
                    return Err(DecodeError::BadValue);
                }
                let mut artists = Vec::with_capacity(count);
                for _ in 0..count {
                    artists.push(r.text(MAX_ARTIST)?);
                }
                let album = r.text(MAX_ALBUM)?;
                let specs = usize::from(r.u8()?);
                if specs > 3 {
                    return Err(DecodeError::BadValue);
                }
                let mut assets = Vec::with_capacity(specs);
                for _ in 0..specs {
                    let kind = Asset::from_u8(r.u8()?).ok_or(DecodeError::BadValue)?;
                    assets.push(AssetSpec {
                        kind,
                        size: r.u32()?,
                        crc32: r.u32()?,
                    });
                }
                Self::Begin {
                    req,
                    meta: TrackMeta {
                        id,
                        duration_ms,
                        name,
                        artists,
                        album,
                        palette_top,
                        palette_bottom,
                        assets,
                    },
                }
            }
            op::ASSET_END => Self::AssetEnd {
                req,
                asset: Asset::from_u8(r.u8()?).ok_or(DecodeError::BadValue)?,
            },
            op::COMMIT => Self::Commit { req, id: r.u64()? },
            op::ABORT => Self::Abort { req },
            op::DELETE => Self::Delete { req, id: r.u64()? },
            op::MOVE => Self::Move {
                req,
                id: r.u64()?,
                up: r.u8()? != 0,
            },
            op::PLAY => Self::Play { req, id: r.u64()? },
            op::CONTROL => Self::Control {
                req,
                command: match r.u8()? {
                    1 => Control::Toggle,
                    2 => Control::Next,
                    3 => Control::Previous,
                    _ => return Err(DecodeError::BadValue),
                },
            },
            other => return Err(DecodeError::UnknownOp(other)),
        })
    }
}

fn request(opcode: u8, req: u16) -> Writer {
    let mut w = Writer::new(opcode);
    w.u16(req);
    w
}

fn id_request(opcode: u8, req: u16, id: u64) -> Result<Vec<u8>, TooLong> {
    let mut w = request(opcode, req);
    w.u64(id);
    w.finish()
}

impl Reply {
    pub fn encode(&self) -> Result<Vec<u8>, TooLong> {
        match self {
            Self::Hello {
                req,
                status,
                version,
                max_data,
                free_kib,
                tracks,
                revision,
                width,
                height,
            } => {
                let mut w = Writer::new(op::HELLO | op::REPLY);
                w.u16(*req);
                w.u8(*status as u8);
                w.u8(*version);
                w.u16(*max_data);
                w.u32(*free_kib);
                w.u16(*tracks);
                w.u32(*revision);
                w.u16(*width);
                w.u16(*height);
                w.finish()
            }
            Self::List {
                req,
                status,
                total,
                offset,
                entries,
            } => {
                let mut w = Writer::new(op::LIST | op::REPLY);
                w.u16(*req);
                w.u8(*status as u8);
                w.u16(*total);
                w.u16(*offset);
                w.u8(entries.len().min(255) as u8);
                for entry in entries {
                    w.u64(entry.id);
                    w.u32(entry.duration_ms);
                    w.text(&entry.name, MAX_NAME);
                    w.text(&entry.artist, MAX_ARTISTS * MAX_ARTIST);
                }
                w.finish()
            }
            Self::Ack {
                op: request_op,
                req,
                status,
                value,
            } => {
                let mut w = Writer::new(request_op | op::REPLY);
                w.u16(*req);
                w.u8(*status as u8);
                w.u32(*value);
                w.finish()
            }
        }
    }

    pub fn decode(frame: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader(frame);
        let opcode = r.u8()?;
        if opcode & op::REPLY == 0 || opcode >= op::PROGRESS {
            return Err(DecodeError::UnknownOp(opcode));
        }
        let request_op = opcode & !op::REPLY;
        let req = r.u16()?;
        let status = Status::from_u8(r.u8()?);
        Ok(match request_op {
            op::HELLO => Self::Hello {
                req,
                status,
                version: r.u8()?,
                max_data: r.u16()?,
                free_kib: r.u32()?,
                tracks: r.u16()?,
                revision: r.u32()?,
                width: r.u16()?,
                height: r.u16()?,
            },
            op::LIST => {
                let total = r.u16()?;
                let offset = r.u16()?;
                let count = r.u8()?;
                let mut entries = Vec::with_capacity(usize::from(count));
                for _ in 0..count {
                    entries.push(ListEntry {
                        id: r.u64()?,
                        duration_ms: r.u32()?,
                        name: r.text(MAX_NAME)?,
                        artist: r.text(MAX_ARTISTS * MAX_ARTIST)?,
                    });
                }
                Self::List {
                    req,
                    status,
                    total,
                    offset,
                    entries,
                }
            }
            _ => Self::Ack {
                op: request_op,
                req,
                status,
                value: r.u32()?,
            },
        })
    }
}

impl Event {
    pub fn encode(&self) -> Result<Vec<u8>, TooLong> {
        match self {
            Self::Progress {
                asset,
                next_offset,
                status,
            } => {
                let mut w = Writer::new(op::PROGRESS);
                w.u8(*asset as u8);
                w.u32(*next_offset);
                w.u8(*status as u8);
                w.finish()
            }
            Self::NowPlaying {
                id,
                state,
                position_ms,
                duration_ms,
                volume,
            } => {
                let mut w = Writer::new(op::NOW_PLAYING);
                w.u64(*id);
                w.u8(*state);
                w.u32(*position_ms);
                w.u32(*duration_ms);
                w.u8(*volume);
                w.finish()
            }
        }
    }

    pub fn decode(frame: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader(frame);
        match r.u8()? {
            op::PROGRESS => Ok(Self::Progress {
                asset: Asset::from_u8(r.u8()?).ok_or(DecodeError::BadValue)?,
                next_offset: r.u32()?,
                status: Status::from_u8(r.u8()?),
            }),
            op::NOW_PLAYING => Ok(Self::NowPlaying {
                id: r.u64()?,
                state: r.u8()?,
                position_ms: r.u32()?,
                duration_ms: r.u32()?,
                volume: r.u8()?,
            }),
            other => Err(DecodeError::UnknownOp(other)),
        }
    }
}

/// What a frame from the watch is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Reply(Reply),
    Event(Event),
}

impl Incoming {
    pub fn decode(frame: &[u8]) -> Result<Self, DecodeError> {
        match frame.first() {
            Some(op) if *op >= op::PROGRESS => Event::decode(frame).map(Self::Event),
            Some(_) => Reply::decode(frame).map(Self::Reply),
            None => Err(DecodeError::Short),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn meta() -> TrackMeta {
        TrackMeta {
            id: 0x0102_0304_0506_0708,
            duration_ms: 241_000,
            name: "夜もすがら君想ふ".into(),
            artists: vec!["西沢さんP".into(), "GUMI".into()],
            album: "Album".into(),
            palette_top: 0x336699,
            palette_bottom: 0x101018,
            assets: vec![
                AssetSpec {
                    kind: Asset::Audio,
                    size: 5_000_000,
                    crc32: 0xDEAD_BEEF,
                },
                AssetSpec {
                    kind: Asset::Cover,
                    size: 124_000,
                    crc32: 1,
                },
                AssetSpec {
                    kind: Asset::Lyrics,
                    size: 2_400,
                    crc32: 2,
                },
            ],
        }
    }

    #[test]
    fn every_request_round_trips() {
        let requests = vec![
            Request::Hello { req: 1, version: 1 },
            Request::List {
                req: 2,
                offset: 40,
                max: 8,
            },
            Request::Begin {
                req: 3,
                meta: meta(),
            },
            Request::Data {
                asset: Asset::Audio,
                offset: 464,
                bytes: vec![7; 464],
            },
            Request::AssetEnd {
                req: 4,
                asset: Asset::Cover,
            },
            Request::Commit { req: 5, id: 99 },
            Request::Abort { req: 6 },
            Request::Delete { req: 7, id: 1 },
            Request::Move {
                req: 8,
                id: 2,
                up: true,
            },
            Request::Play { req: 9, id: 3 },
            Request::Control {
                req: 10,
                command: Control::Next,
            },
        ];
        for request in requests {
            let frame = request.encode().unwrap();
            assert!(frame.len() <= FRAME_MAX);
            assert_eq!(Request::decode(&frame).unwrap(), request);
        }
    }

    #[test]
    fn replies_and_events_round_trip() {
        let replies = vec![
            Reply::Hello {
                req: 1,
                status: Status::Ok,
                version: 1,
                max_data: 464,
                free_kib: 300_000,
                tracks: 12,
                revision: 5,
                width: 212,
                height: 520,
            },
            Reply::List {
                req: 2,
                status: Status::Ok,
                total: 3,
                offset: 0,
                entries: vec![
                    ListEntry {
                        id: 1,
                        duration_ms: 1000,
                        name: "a".into(),
                        artist: "b / c".into(),
                    },
                    ListEntry {
                        id: 2,
                        duration_ms: 2000,
                        name: "名前".into(),
                        artist: String::new(),
                    },
                ],
            },
            Reply::Ack {
                op: op::COMMIT,
                req: 3,
                status: Status::Exists,
                value: 8,
            },
            Reply::Ack {
                op: op::BEGIN,
                req: 4,
                status: Status::NoSpace,
                value: 0,
            },
        ];
        for reply in replies {
            let frame = reply.encode().unwrap();
            assert_eq!(Incoming::decode(&frame).unwrap(), Incoming::Reply(reply));
        }
        let events = vec![
            Event::Progress {
                asset: Asset::Audio,
                next_offset: 8192,
                status: Status::Ok,
            },
            Event::NowPlaying {
                id: 5,
                state: 3,
                position_ms: 12_000,
                duration_ms: 200_000,
                volume: 80,
            },
        ];
        for event in events {
            let frame = event.encode().unwrap();
            assert_eq!(Incoming::decode(&frame).unwrap(), Incoming::Event(event));
        }
    }

    #[test]
    fn worst_case_begin_fits_one_frame_and_long_text_is_clipped_on_a_char_boundary() {
        let mut long = meta();
        long.name = "あ".repeat(100);
        long.artists = vec!["アーティスト".repeat(20); 6];
        long.album = "x".repeat(300);
        let frame = Request::Begin { req: 1, meta: long }.encode().unwrap();
        assert!(frame.len() <= FRAME_MAX);
        let Request::Begin { meta, .. } = Request::decode(&frame).unwrap() else {
            panic!()
        };
        assert!(meta.name.len() <= MAX_NAME && meta.name.chars().all(|c| c == 'あ'));
        assert_eq!(meta.artists.len(), MAX_ARTISTS);
        assert!(meta.artists.iter().all(|a| a.len() <= MAX_ARTIST));
        assert_eq!(meta.album.len(), MAX_ALBUM);
    }

    #[test]
    fn malformed_frames_are_rejected_not_misread() {
        assert_eq!(Request::decode(&[]), Err(DecodeError::Short));
        assert_eq!(
            Request::decode(&[0x7F, 0, 0]),
            Err(DecodeError::UnknownOp(0x7F))
        );
        assert_eq!(
            Request::decode(&[op::COMMIT, 1, 0, 1, 2]),
            Err(DecodeError::Short)
        );
        assert_eq!(
            Request::decode(&[op::DATA, 9, 0, 0, 0, 0, 1]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            Request::decode(&[op::DATA, 1, 0, 0, 0, 0]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            Request::decode(&[op::CONTROL, 1, 0, 9]),
            Err(DecodeError::BadValue)
        );
        let mut bad_text = request(op::BEGIN, 1);
        bad_text.u64(1);
        bad_text.u32(0);
        bad_text.u32(0);
        bad_text.u32(0);
        bad_text.u8(2);
        bad_text.0.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(Request::decode(&bad_text.0), Err(DecodeError::BadText));
        assert_eq!(
            Request::decode(&vec![0; FRAME_MAX + 1]),
            Err(DecodeError::TooLong)
        );
    }

    #[test]
    fn oversized_data_is_refused_when_encoding() {
        let request = Request::Data {
            asset: Asset::Audio,
            offset: 0,
            bytes: vec![0; DATA_MAX + 1],
        };
        assert_eq!(request.encode(), Err(TooLong));
    }
}
