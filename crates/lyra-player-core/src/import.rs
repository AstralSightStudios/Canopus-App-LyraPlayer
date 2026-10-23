//! The watch side of the importer protocol: turns the requests the phone sends
//! over CanopusConnect into stored tracks, library changes and replies.
//!
//! Everything that touches a file sits behind [`ImportStore`], so the whole
//! protocol -- ordering, resume, CRC, commit, delete -- is exercised on the
//! host.

use alloc::vec::Vec;

use lyra_protocol::{
    Asset, AssetSpec, Control, Crc32, DATA_MAX, Event, FRAME_MAX, ListEntry, PROGRESS_STEP, Reply,
    Request, Status, TrackMeta, VERSION, op,
};

use crate::{Assets, Palette, Rgb, Song, persistence::Library};

/// Where the bytes go. Implemented on the device with the module's file
/// system service.
pub trait ImportStore {
    /// Starts a clean staging area for track `id`.
    fn stage_begin(&mut self, id: u64) -> Result<(), Status>;
    /// Appends `bytes` of `asset` at `offset` (0 creates the file).
    fn stage_write(&mut self, asset: Asset, offset: u32, bytes: &[u8]) -> Result<(), Status>;
    /// The asset is complete; close its file.
    fn stage_finish(&mut self, asset: Asset) -> Result<(), Status>;
    /// Replaces track `id`'s directory by the staged one.
    fn stage_commit(&mut self, id: u64) -> Result<(), Status>;
    fn stage_abort(&mut self);
    fn remove_track(&mut self, id: u64) -> Result<(), Status>;
    fn save_library(&mut self, library: &Library) -> Result<(), Status>;
    /// Free space in KiB, `u32::MAX` when unknown.
    fn free_kib(&mut self) -> u32;
}

/// What the app has to do besides replying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The library on disk changed; reload it.
    Library,
    Play(u64),
    Control(Control),
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// Encoded frames, in the order they must be sent.
    pub frames: Vec<Vec<u8>>,
    pub changes: Vec<Change>,
}

impl Outcome {
    fn reply(&mut self, reply: Reply) {
        if let Ok(frame) = reply.encode() {
            self.frames.push(frame);
        }
    }

    fn event(&mut self, event: Event) {
        if let Ok(frame) = event.encode() {
            self.frames.push(frame);
        }
    }

    fn ack(&mut self, op: u8, req: u16, status: Status, value: u32) {
        self.reply(Reply::Ack {
            op,
            req,
            status,
            value,
        });
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Platform {
    pub width: u16,
    pub height: u16,
}

struct Slot {
    spec: AssetSpec,
    next: u32,
    crc: Crc32,
    confirmed: u32,
    done: bool,
}

struct Stage {
    meta: TrackMeta,
    slots: Vec<Slot>,
}

impl Stage {
    fn slot(&mut self, asset: Asset) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|slot| slot.spec.kind == asset)
    }
}

pub struct Importer {
    platform: Platform,
    stage: Option<Stage>,
}

impl Importer {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            stage: None,
        }
    }

    pub fn is_importing(&self) -> bool {
        self.stage.is_some()
    }

    /// Drops a half-finished transfer, e.g. because the phone went away.
    pub fn reset<S: ImportStore>(&mut self, store: &mut S) {
        if self.stage.take().is_some() {
            store.stage_abort();
        }
    }

    pub fn handle<S: ImportStore>(
        &mut self,
        request: Request,
        library: &mut Library,
        store: &mut S,
    ) -> Outcome {
        let mut out = Outcome::default();
        match request {
            Request::Hello { req, version } => {
                let status = if version == 0 || version > VERSION {
                    Status::Unsupported
                } else {
                    Status::Ok
                };
                out.reply(Reply::Hello {
                    req,
                    status,
                    version: VERSION,
                    max_data: DATA_MAX as u16,
                    free_kib: store.free_kib(),
                    tracks: library.tracks.len().min(usize::from(u16::MAX)) as u16,
                    revision: library.revision,
                    width: self.platform.width,
                    height: self.platform.height,
                });
            }
            Request::List { req, offset, max } => {
                out.reply(list_page(req, offset, max, library));
            }
            Request::Begin { req, meta } => {
                let status = self.begin(meta, library, store);
                out.ack(op::BEGIN, req, status, 0);
            }
            Request::Data {
                asset,
                offset,
                bytes,
            } => self.data(asset, offset, &bytes, store, &mut out),
            Request::AssetEnd { req, asset } => {
                let status = self.asset_end(asset, store);
                out.ack(op::ASSET_END, req, status, 0);
            }
            Request::Commit { req, id } => {
                let status = self.commit(id, library, store);
                if status == Status::Ok {
                    out.changes.push(Change::Library);
                }
                out.ack(op::COMMIT, req, status, library.revision);
            }
            Request::Abort { req } => {
                self.reset(store);
                out.ack(op::ABORT, req, Status::Ok, 0);
            }
            Request::Delete { req, id } => {
                let status = delete(id, library, store);
                if status == Status::Ok {
                    out.changes.push(Change::Library);
                }
                out.ack(op::DELETE, req, status, library.revision);
            }
            Request::Move { req, id, up } => {
                let status = reorder(id, up, library, store);
                if status == Status::Ok {
                    out.changes.push(Change::Library);
                }
                out.ack(op::MOVE, req, status, library.revision);
            }
            Request::Play { req, id } => {
                let known = library.tracks.iter().any(|song| song.id == id);
                if known {
                    out.changes.push(Change::Play(id));
                }
                let status = if known { Status::Ok } else { Status::NotFound };
                out.ack(op::PLAY, req, status, 0);
            }
            Request::Control { req, command } => {
                out.changes.push(Change::Control(command));
                out.ack(op::CONTROL, req, Status::Ok, 0);
            }
        }
        out
    }

    fn begin<S: ImportStore>(
        &mut self,
        meta: TrackMeta,
        library: &Library,
        store: &mut S,
    ) -> Status {
        // A new transfer replaces an unfinished one: the phone only ever has
        // one, and a stale stage would otherwise wedge the next import.
        self.reset(store);
        let audio = meta.assets.iter().filter(|s| s.kind == Asset::Audio).count();
        let distinct = meta
            .assets
            .iter()
            .enumerate()
            .all(|(i, a)| meta.assets[..i].iter().all(|b| b.kind != a.kind));
        let sized = meta
            .assets
            .iter()
            .all(|spec| spec.size > 0 && spec.size <= spec.kind.max_bytes());
        if meta.id == 0 || meta.name.is_empty() || audio != 1 || !distinct || !sized {
            return Status::BadRequest;
        }
        let needed: u64 = meta.assets.iter().map(|spec| u64::from(spec.size)).sum();
        let replaced = library
            .tracks
            .iter()
            .any(|song| song.id == meta.id);
        let free = store.free_kib();
        if free != u32::MAX && !replaced && needed / 1024 + 64 > u64::from(free) {
            return Status::NoSpace;
        }
        if let Err(status) = store.stage_begin(meta.id) {
            return status;
        }
        let slots = meta
            .assets
            .iter()
            .map(|spec| Slot {
                spec: *spec,
                next: 0,
                crc: Crc32::new(),
                confirmed: 0,
                done: false,
            })
            .collect();
        self.stage = Some(Stage { meta, slots });
        Status::Ok
    }

    fn data<S: ImportStore>(
        &mut self,
        asset: Asset,
        offset: u32,
        bytes: &[u8],
        store: &mut S,
        out: &mut Outcome,
    ) {
        let Some(stage) = &mut self.stage else {
            // Nothing is being imported; tell the phone to stop sending.
            out.event(Event::Progress {
                asset,
                next_offset: 0,
                status: Status::Sequence,
            });
            return;
        };
        let Some(slot) = stage.slot(asset) else {
            out.event(Event::Progress {
                asset,
                next_offset: 0,
                status: Status::BadRequest,
            });
            return;
        };
        if slot.done || offset != slot.next {
            // Lost or repeated data: say where the watch is so the phone resumes.
            out.event(Event::Progress {
                asset,
                next_offset: slot.next,
                status: Status::Sequence,
            });
            return;
        }
        let end = u64::from(offset) + bytes.len() as u64;
        if end > u64::from(slot.spec.size) {
            out.event(Event::Progress {
                asset,
                next_offset: slot.next,
                status: Status::BadRequest,
            });
            return;
        }
        if let Err(status) = store.stage_write(asset, offset, bytes) {
            out.event(Event::Progress {
                asset,
                next_offset: slot.next,
                status,
            });
            return;
        }
        slot.crc.update(bytes);
        slot.next = end as u32;
        if slot.next - slot.confirmed >= PROGRESS_STEP || slot.next == slot.spec.size {
            slot.confirmed = slot.next;
            out.event(Event::Progress {
                asset,
                next_offset: slot.next,
                status: Status::Ok,
            });
        }
    }

    fn asset_end<S: ImportStore>(&mut self, asset: Asset, store: &mut S) -> Status {
        let Some(slot) = self.stage.as_mut().and_then(|stage| stage.slot(asset)) else {
            return Status::Sequence;
        };
        if slot.next != slot.spec.size {
            return Status::Sequence;
        }
        if slot.crc.finish() != slot.spec.crc32 {
            return Status::Crc;
        }
        if let Err(status) = store.stage_finish(asset) {
            return status;
        }
        slot.done = true;
        Status::Ok
    }

    fn commit<S: ImportStore>(&mut self, id: u64, library: &mut Library, store: &mut S) -> Status {
        let Some(stage) = &self.stage else {
            return Status::Sequence;
        };
        if stage.meta.id != id || stage.slots.iter().any(|slot| !slot.done) {
            return Status::Sequence;
        }
        if let Err(status) = store.stage_commit(id) {
            return status;
        }
        let stage = self.stage.take().expect("checked above");
        let meta = stage.meta;
        let mut assets = 0;
        for slot in &stage.slots {
            match slot.spec.kind {
                Asset::Cover => assets |= Assets::COVER,
                Asset::Lyrics => assets |= Assets::LYRICS,
                Asset::Audio => {}
            }
        }
        let song = Song {
            id,
            name: meta.name,
            artists: meta.artists,
            album: meta.album,
            duration_ms: meta.duration_ms,
            assets: Assets(assets),
            palette: Palette {
                top: Rgb(meta.palette_top),
                bottom: Rgb(meta.palette_bottom),
            },
        };
        match library.tracks.iter_mut().find(|existing| existing.id == id) {
            Some(existing) => *existing = song,
            None => library.tracks.push(song),
        }
        library.revision = library.revision.wrapping_add(1);
        match store.save_library(library) {
            Ok(()) => Status::Ok,
            Err(status) => status,
        }
    }
}

fn delete<S: ImportStore>(id: u64, library: &mut Library, store: &mut S) -> Status {
    let Some(index) = library.tracks.iter().position(|song| song.id == id) else {
        return Status::NotFound;
    };
    library.tracks.remove(index);
    library.revision = library.revision.wrapping_add(1);
    // The library goes first: a crash between the two steps leaves orphan
    // files, never a listed track without audio.
    if let Err(status) = store.save_library(library) {
        return status;
    }
    let _ = store.remove_track(id);
    Status::Ok
}

fn reorder<S: ImportStore>(id: u64, up: bool, library: &mut Library, store: &mut S) -> Status {
    let Some(index) = library.tracks.iter().position(|song| song.id == id) else {
        return Status::NotFound;
    };
    let other = if up {
        index.checked_sub(1)
    } else {
        Some(index + 1).filter(|next| *next < library.tracks.len())
    };
    let Some(other) = other else {
        return Status::Ok;
    };
    library.tracks.swap(index, other);
    library.revision = library.revision.wrapping_add(1);
    match store.save_library(library) {
        Ok(()) => Status::Ok,
        Err(status) => status,
    }
}

fn list_page(req: u16, offset: u16, max: u8, library: &Library) -> Reply {
    let mut entries: Vec<ListEntry> = Vec::new();
    // reply header: op, req, status, total, offset, count
    let mut used = 1 + 2 + 1 + 2 + 2 + 1;
    for song in library
        .tracks
        .iter()
        .skip(usize::from(offset))
        .take(usize::from(max.max(1)))
    {
        let entry = ListEntry {
            id: song.id,
            duration_ms: song.duration_ms,
            name: song.name.clone(),
            artist: song.artist_line(),
        };
        if used + entry.encoded_len() > FRAME_MAX {
            break;
        }
        used += entry.encoded_len();
        entries.push(entry);
    }
    Reply::List {
        req,
        status: Status::Ok,
        total: library.tracks.len().min(usize::from(u16::MAX)) as u16,
        offset,
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{collections::BTreeMap, vec};
    use lyra_protocol::crc32;

    #[derive(Default)]
    struct Memory {
        staged: BTreeMap<u8, Vec<u8>>,
        committed: BTreeMap<u64, BTreeMap<u8, Vec<u8>>>,
        stage_id: Option<u64>,
        library: Option<Library>,
        fail_write: Option<Status>,
        free: Option<u32>,
    }

    impl ImportStore for Memory {
        fn stage_begin(&mut self, id: u64) -> Result<(), Status> {
            self.staged.clear();
            self.stage_id = Some(id);
            Ok(())
        }
        fn stage_write(&mut self, asset: Asset, offset: u32, bytes: &[u8]) -> Result<(), Status> {
            if let Some(status) = self.fail_write {
                return Err(status);
            }
            let file = self.staged.entry(asset as u8).or_default();
            assert_eq!(file.len() as u32, offset, "the store only ever appends");
            file.extend_from_slice(bytes);
            Ok(())
        }
        fn stage_finish(&mut self, _asset: Asset) -> Result<(), Status> {
            Ok(())
        }
        fn stage_commit(&mut self, id: u64) -> Result<(), Status> {
            assert_eq!(self.stage_id, Some(id));
            self.committed.insert(id, core::mem::take(&mut self.staged));
            Ok(())
        }
        fn stage_abort(&mut self) {
            self.staged.clear();
            self.stage_id = None;
        }
        fn remove_track(&mut self, id: u64) -> Result<(), Status> {
            self.committed.remove(&id);
            Ok(())
        }
        fn save_library(&mut self, library: &Library) -> Result<(), Status> {
            self.library = Some(library.clone());
            Ok(())
        }
        fn free_kib(&mut self) -> u32 {
            self.free.unwrap_or(u32::MAX)
        }
    }

    fn importer() -> Importer {
        Importer::new(Platform {
            width: 212,
            height: 520,
        })
    }

    fn payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    fn meta(id: u64, audio: &[u8], cover: Option<&[u8]>, lyrics: Option<&[u8]>) -> TrackMeta {
        let mut assets = vec![AssetSpec {
            kind: Asset::Audio,
            size: audio.len() as u32,
            crc32: crc32(audio),
        }];
        if let Some(cover) = cover {
            assets.push(AssetSpec {
                kind: Asset::Cover,
                size: cover.len() as u32,
                crc32: crc32(cover),
            });
        }
        if let Some(lyrics) = lyrics {
            assets.push(AssetSpec {
                kind: Asset::Lyrics,
                size: lyrics.len() as u32,
                crc32: crc32(lyrics),
            });
        }
        TrackMeta {
            id,
            duration_ms: 123_000,
            name: "Name".into(),
            artists: vec!["A".into()],
            album: "Al".into(),
            palette_top: 0x112233,
            palette_bottom: 0x000000,
            assets,
        }
    }

    fn acks(out: &Outcome) -> Vec<(u8, Status)> {
        out.frames
            .iter()
            .filter_map(|frame| match lyra_protocol::Incoming::decode(frame).unwrap() {
                lyra_protocol::Incoming::Reply(Reply::Ack { op, status, .. }) => Some((op, status)),
                _ => None,
            })
            .collect()
    }

    fn progress(out: &Outcome) -> Vec<(u32, Status)> {
        out.frames
            .iter()
            .filter_map(|frame| match lyra_protocol::Incoming::decode(frame).unwrap() {
                lyra_protocol::Incoming::Event(Event::Progress {
                    next_offset,
                    status,
                    ..
                }) => Some((next_offset, status)),
                _ => None,
            })
            .collect()
    }

    /// Sends one asset the way the phone does and returns the events seen.
    fn send_asset(
        importer: &mut Importer,
        library: &mut Library,
        store: &mut Memory,
        asset: Asset,
        bytes: &[u8],
    ) -> Vec<(u32, Status)> {
        let mut seen = Vec::new();
        for (index, chunk) in bytes.chunks(DATA_MAX).enumerate() {
            let out = importer.handle(
                Request::Data {
                    asset,
                    offset: (index * DATA_MAX) as u32,
                    bytes: chunk.to_vec(),
                },
                library,
                store,
            );
            seen.extend(progress(&out));
        }
        let out = importer.handle(Request::AssetEnd { req: 9, asset }, library, store);
        assert_eq!(acks(&out), vec![(op::ASSET_END, Status::Ok)]);
        seen
    }

    #[test]
    fn a_whole_track_is_imported_and_listed() {
        let (audio, cover, lyrics) = (payload(20_000, 1), payload(3_000, 2), b"[00:01.00]hi".to_vec());
        let mut importer = importer();
        let mut library = Library::default();
        let mut store = Memory::default();

        let out = importer.handle(
            Request::Begin {
                req: 1,
                meta: meta(77, &audio, Some(&cover), Some(&lyrics)),
            },
            &mut library,
            &mut store,
        );
        assert_eq!(acks(&out), vec![(op::BEGIN, Status::Ok)]);

        let events = send_asset(&mut importer, &mut library, &mut store, Asset::Audio, &audio);
        // Progress at least every PROGRESS_STEP bytes and once at the end.
        assert!(events.windows(2).all(|w| w[1].0 - w[0].0 <= PROGRESS_STEP + DATA_MAX as u32));
        assert_eq!(events.last(), Some(&(audio.len() as u32, Status::Ok)));
        send_asset(&mut importer, &mut library, &mut store, Asset::Cover, &cover);
        send_asset(&mut importer, &mut library, &mut store, Asset::Lyrics, &lyrics);

        let out = importer.handle(Request::Commit { req: 2, id: 77 }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::COMMIT, Status::Ok)]);
        assert_eq!(out.changes, vec![Change::Library]);
        assert!(!importer.is_importing());

        assert_eq!(store.committed[&77][&(Asset::Audio as u8)], audio);
        let song = &library.tracks[0];
        assert_eq!(song.id, 77);
        assert!(song.assets.has(Assets::COVER) && song.assets.has(Assets::LYRICS));
        assert_eq!(song.palette.top, Rgb(0x112233));
        assert_eq!(store.library.as_ref().unwrap(), &library);
        assert_eq!(library.revision, 1);
    }

    #[test]
    fn data_out_of_order_is_answered_with_the_expected_offset() {
        let audio = payload(2_000, 3);
        let mut importer = importer();
        let mut library = Library::default();
        let mut store = Memory::default();
        importer.handle(
            Request::Begin {
                req: 1,
                meta: meta(5, &audio, None, None),
            },
            &mut library,
            &mut store,
        );
        let send = |importer: &mut Importer, library: &mut Library, store: &mut Memory, offset: u32| {
            importer.handle(
                Request::Data {
                    asset: Asset::Audio,
                    offset,
                    bytes: audio[offset as usize..(offset as usize + 100)].to_vec(),
                },
                library,
                store,
            )
        };
        assert!(progress(&send(&mut importer, &mut library, &mut store, 0)).is_empty());
        // 100 was skipped: the watch asks for it.
        let skipped = send(&mut importer, &mut library, &mut store, 200);
        assert_eq!(progress(&skipped), vec![(100, Status::Sequence)]);
        // A repeat of old data is also refused, and nothing was appended twice.
        let repeat = send(&mut importer, &mut library, &mut store, 0);
        assert_eq!(progress(&repeat), vec![(100, Status::Sequence)]);
        assert_eq!(store.staged[&(Asset::Audio as u8)].len(), 100);
        // Resuming at the named offset works.
        assert!(progress(&send(&mut importer, &mut library, &mut store, 100)).is_empty());
        assert_eq!(store.staged[&(Asset::Audio as u8)], audio[..200]);
    }

    #[test]
    fn a_corrupt_or_short_asset_is_not_committed() {
        let audio = payload(1_000, 4);
        let mut importer = importer();
        let mut library = Library::default();
        let mut store = Memory::default();
        importer.handle(
            Request::Begin {
                req: 1,
                meta: meta(5, &audio, None, None),
            },
            &mut library,
            &mut store,
        );
        let mut bad = audio.clone();
        bad[10] ^= 0xFF;
        importer.handle(
            Request::Data {
                asset: Asset::Audio,
                offset: 0,
                bytes: bad[..464].to_vec(),
            },
            &mut library,
            &mut store,
        );
        // Not all bytes there yet.
        let early = importer.handle(
            Request::AssetEnd {
                req: 2,
                asset: Asset::Audio,
            },
            &mut library,
            &mut store,
        );
        assert_eq!(acks(&early), vec![(op::ASSET_END, Status::Sequence)]);
        importer.handle(
            Request::Data {
                asset: Asset::Audio,
                offset: 464,
                bytes: bad[464..].to_vec(),
            },
            &mut library,
            &mut store,
        );
        let end = importer.handle(
            Request::AssetEnd {
                req: 3,
                asset: Asset::Audio,
            },
            &mut library,
            &mut store,
        );
        assert_eq!(acks(&end), vec![(op::ASSET_END, Status::Crc)]);
        let commit = importer.handle(Request::Commit { req: 4, id: 5 }, &mut library, &mut store);
        assert_eq!(acks(&commit), vec![(op::COMMIT, Status::Sequence)]);
        assert!(library.tracks.is_empty() && store.committed.is_empty());
    }

    #[test]
    fn bad_begin_requests_are_refused_without_touching_the_store() {
        let audio = payload(100, 5);
        let mut library = Library::default();
        let mut store = Memory::default();
        let mut importer = importer();
        let mut no_audio = meta(1, &audio, None, None);
        no_audio.assets.clear();
        let mut zero_id = meta(0, &audio, None, None);
        zero_id.name = "x".into();
        let mut huge = meta(2, &audio, None, None);
        huge.assets[0].size = lyra_protocol::MAX_AUDIO_BYTES + 1;
        let mut twice = meta(3, &audio, None, None);
        twice.assets.push(twice.assets[0]);
        for bad in [no_audio, zero_id, huge, twice] {
            let out = importer.handle(Request::Begin { req: 1, meta: bad }, &mut library, &mut store);
            assert_eq!(acks(&out), vec![(op::BEGIN, Status::BadRequest)]);
        }
        assert!(store.stage_id.is_none());
        store.free = Some(10);
        let big = meta(4, &payload(50_000, 1), None, None);
        let out = importer.handle(Request::Begin { req: 1, meta: big }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::BEGIN, Status::NoSpace)]);
    }

    #[test]
    fn a_new_begin_replaces_an_unfinished_transfer_and_write_errors_surface() {
        let audio = payload(2_000, 6);
        let mut importer = importer();
        let mut library = Library::default();
        let mut store = Memory::default();
        for id in [1, 2] {
            let out = importer.handle(
                Request::Begin {
                    req: 1,
                    meta: meta(id, &audio, None, None),
                },
                &mut library,
                &mut store,
            );
            assert_eq!(acks(&out), vec![(op::BEGIN, Status::Ok)]);
        }
        assert_eq!(store.stage_id, Some(2));
        store.fail_write = Some(Status::NoSpace);
        let out = importer.handle(
            Request::Data {
                asset: Asset::Audio,
                offset: 0,
                bytes: audio[..100].to_vec(),
            },
            &mut library,
            &mut store,
        );
        assert_eq!(progress(&out), vec![(0, Status::NoSpace)]);
        // Data without a transfer is refused.
        importer.reset(&mut store);
        let out = importer.handle(
            Request::Data {
                asset: Asset::Audio,
                offset: 0,
                bytes: vec![1],
            },
            &mut library,
            &mut store,
        );
        assert_eq!(progress(&out), vec![(0, Status::Sequence)]);
    }

    fn with_tracks(ids: &[u64]) -> (Importer, Library, Memory) {
        let mut importer = importer();
        let mut library = Library::default();
        let mut store = Memory::default();
        for id in ids {
            let audio = payload(300, *id as u8);
            importer.handle(
                Request::Begin {
                    req: 1,
                    meta: meta(*id, &audio, None, None),
                },
                &mut library,
                &mut store,
            );
            send_asset(&mut importer, &mut library, &mut store, Asset::Audio, &audio);
            importer.handle(Request::Commit { req: 2, id: *id }, &mut library, &mut store);
        }
        (importer, library, store)
    }

    #[test]
    fn reimporting_a_track_replaces_it_in_place() {
        let (mut importer, mut library, mut store) = with_tracks(&[1, 2, 3]);
        let audio = payload(500, 9);
        let mut changed = meta(2, &audio, None, None);
        changed.name = "Renamed".into();
        importer.handle(Request::Begin { req: 1, meta: changed }, &mut library, &mut store);
        send_asset(&mut importer, &mut library, &mut store, Asset::Audio, &audio);
        importer.handle(Request::Commit { req: 2, id: 2 }, &mut library, &mut store);
        let names: Vec<_> = library.tracks.iter().map(|s| (s.id, s.name.as_str())).collect();
        assert_eq!(names, vec![(1, "Name"), (2, "Renamed"), (3, "Name")]);
        assert_eq!(store.committed[&2][&(Asset::Audio as u8)], audio);
    }

    #[test]
    fn delete_and_move_update_the_library_and_the_files() {
        let (mut importer, mut library, mut store) = with_tracks(&[1, 2, 3]);
        let out = importer.handle(Request::Move { req: 1, id: 3, up: true }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::MOVE, Status::Ok)]);
        assert_eq!(library.tracks.iter().map(|s| s.id).collect::<Vec<_>>(), vec![1, 3, 2]);
        // Moving past either end is a quiet no-op.
        let revision = library.revision;
        let out = importer.handle(Request::Move { req: 2, id: 1, up: true }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::MOVE, Status::Ok)]);
        assert_eq!(library.revision, revision);
        assert!(out.changes.is_empty() || out.changes == vec![Change::Library]);

        let out = importer.handle(Request::Delete { req: 3, id: 3 }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::DELETE, Status::Ok)]);
        assert!(!store.committed.contains_key(&3));
        assert_eq!(library.tracks.len(), 2);
        let out = importer.handle(Request::Delete { req: 4, id: 3 }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::DELETE, Status::NotFound)]);
    }

    #[test]
    fn hello_list_play_and_control() {
        let (mut importer, mut library, mut store) = with_tracks(&[10, 20, 30]);
        let out = importer.handle(Request::Hello { req: 1, version: 1 }, &mut library, &mut store);
        let Ok(lyra_protocol::Incoming::Reply(Reply::Hello { tracks, width, height, status, .. })) =
            lyra_protocol::Incoming::decode(&out.frames[0])
        else {
            panic!("no hello")
        };
        assert_eq!((tracks, width, height, status), (3, 212, 520, Status::Ok));
        let out = importer.handle(Request::Hello { req: 1, version: 9 }, &mut library, &mut store);
        let Ok(lyra_protocol::Incoming::Reply(Reply::Hello { status, .. })) =
            lyra_protocol::Incoming::decode(&out.frames[0])
        else {
            panic!("no hello")
        };
        assert_eq!(status, Status::Unsupported);

        let out = importer.handle(Request::List { req: 2, offset: 1, max: 8 }, &mut library, &mut store);
        let Ok(lyra_protocol::Incoming::Reply(Reply::List { entries, total, offset, .. })) =
            lyra_protocol::Incoming::decode(&out.frames[0])
        else {
            panic!("no list")
        };
        assert_eq!((total, offset), (3, 1));
        assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), vec![20, 30]);

        let out = importer.handle(Request::Play { req: 3, id: 20 }, &mut library, &mut store);
        assert_eq!(out.changes, vec![Change::Play(20)]);
        let out = importer.handle(Request::Play { req: 3, id: 99 }, &mut library, &mut store);
        assert_eq!(acks(&out), vec![(op::PLAY, Status::NotFound)]);
        let out = importer.handle(
            Request::Control { req: 4, command: Control::Next },
            &mut library,
            &mut store,
        );
        assert_eq!(out.changes, vec![Change::Control(Control::Next)]);
    }

    #[test]
    fn a_long_library_pages_by_frame_size() {
        let mut library = Library::default();
        for id in 0..50 {
            library.tracks.push(Song {
                id,
                name: "x".repeat(90),
                artists: vec!["y".repeat(40)],
                ..Song::default()
            });
        }
        let mut offset = 0u16;
        let mut seen = 0;
        loop {
            let Reply::List { entries, .. } = list_page(1, offset, 255, &library) else {
                panic!()
            };
            assert!(Reply::List { req: 1, status: Status::Ok, total: 50, offset, entries: entries.clone() }
                .encode()
                .unwrap()
                .len()
                <= FRAME_MAX);
            if entries.is_empty() {
                break;
            }
            seen += entries.len();
            offset += entries.len() as u16;
        }
        assert_eq!(seen, 50);
    }
}
