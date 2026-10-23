//! The phone's half of an import: turns a track (metadata plus the bytes of its
//! files) into the frames the watch expects, and follows the watch's replies
//! and progress events. It does no I/O and no timing; the host sends the
//! frames, feeds back what arrives and decides when to give up.

use std::collections::VecDeque;

use lyra_protocol::{
    Asset, AssetSpec, Crc32, DATA_MAX, Event, Incoming, Reply, Request, Status, TrackMeta, WINDOW,
    op,
};

/// Random-access bytes of one file; implemented for in-memory data and by the
/// plugin for its media files.
pub trait Source: Send {
    fn len(&self) -> u32;
    /// Reads up to `buf.len()` bytes at `offset`; 0 only at the end.
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Source for Vec<u8> {
    fn len(&self) -> u32 {
        self.as_slice().len() as u32
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String> {
        let start = (offset as usize).min(self.len());
        let count = buf.len().min(self.len() - start);
        buf[..count].copy_from_slice(&self[start..start + count]);
        Ok(count)
    }
}

/// The CRC-32 of a whole source, read in chunks.
pub fn crc_of(source: &mut dyn Source) -> Result<u32, String> {
    let mut crc = Crc32::new();
    let mut offset = 0;
    let mut buffer = [0u8; 4096];
    loop {
        let count = source.read(offset, &mut buffer)?;
        if count == 0 {
            return Ok(crc.finish());
        }
        crc.update(&buffer[..count]);
        offset += count as u32;
    }
}

/// Builds the asset table of a track from its sources.
pub fn specs(sources: &mut [(Asset, Box<dyn Source>)]) -> Result<Vec<AssetSpec>, String> {
    sources
        .iter_mut()
        .map(|(kind, source)| {
            Ok(AssetSpec {
                kind: *kind,
                size: source.len(),
                crc32: crc_of(source.as_mut())?,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// `BEGIN` is next.
    Start,
    AwaitBegin,
    /// Streaming asset `index`.
    Streaming(usize),
    AwaitAssetEnd(usize),
    AwaitCommit,
    Done,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The watch refused with this status.
    Refused(Status),
    /// A source could not be read.
    Source,
    /// The watch answered something that does not fit the transfer.
    Protocol,
}

pub struct Sender {
    meta: TrackMeta,
    sources: Vec<Box<dyn Source>>,
    state: State,
    /// Next byte to send of the asset being streamed.
    cursor: u32,
    /// Bytes of that asset the watch has confirmed.
    confirmed: u32,
    req: u16,
    expected_req: u16,
    queue: VecDeque<Vec<u8>>,
    resumes: u32,
    /// After a rewind, repeats of the same complaint (frames already in
    /// flight) are ignored until the watch accepts data again.
    rewound_to: Option<u32>,
}

impl Sender {
    /// `sources` are in the order of `meta.assets`.
    pub fn new(meta: TrackMeta, sources: Vec<Box<dyn Source>>) -> Self {
        assert_eq!(meta.assets.len(), sources.len());
        Self {
            meta,
            sources,
            state: State::Start,
            cursor: 0,
            confirmed: 0,
            req: 0,
            expected_req: 0,
            queue: VecDeque::new(),
            resumes: 0,
            rewound_to: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done | State::Failed(_))
    }

    /// How often the watch asked the phone to resume from an earlier offset.
    pub fn resumes(&self) -> u32 {
        self.resumes
    }

    pub fn total_bytes(&self) -> u64 {
        self.meta
            .assets
            .iter()
            .map(|spec| u64::from(spec.size))
            .sum()
    }

    /// Bytes the watch has confirmed so far, over all files.
    pub fn confirmed_bytes(&self) -> u64 {
        let current = match self.state {
            State::Streaming(index) | State::AwaitAssetEnd(index) => index,
            State::AwaitCommit | State::Done => self.meta.assets.len(),
            _ => 0,
        };
        let before: u64 = self.meta.assets[..current.min(self.meta.assets.len())]
            .iter()
            .map(|spec| u64::from(spec.size))
            .sum();
        before + u64::from(self.confirmed)
    }

    fn next_req(&mut self) -> u16 {
        self.req = self.req.wrapping_add(1).max(1);
        self.expected_req = self.req;
        self.req
    }

    fn fail(&mut self, failure: Failure) {
        self.queue.clear();
        self.state = State::Failed(failure);
    }

    fn push(&mut self, request: Request) {
        match request.encode() {
            Ok(frame) => self.queue.push_back(frame),
            Err(_) => self.fail(Failure::Protocol),
        }
    }

    /// The next frame to send, or `None` while the sender waits for the watch.
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        if let Some(frame) = self.queue.pop_front() {
            return Some(frame);
        }
        match self.state {
            State::Start => {
                let req = self.next_req();
                self.state = State::AwaitBegin;
                self.push(Request::Begin {
                    req,
                    meta: self.meta.clone(),
                });
            }
            State::Streaming(index) => self.stream(index),
            _ => {}
        }
        self.queue.pop_front()
    }

    fn stream(&mut self, index: usize) {
        let spec = self.meta.assets[index];
        let mut buffer = [0u8; DATA_MAX];
        while self.cursor < spec.size && self.cursor.saturating_sub(self.confirmed) < WINDOW {
            let want = (spec.size - self.cursor).min(DATA_MAX as u32) as usize;
            let read = match self.sources[index].read(self.cursor, &mut buffer[..want]) {
                Ok(read) if read > 0 => read,
                _ => return self.fail(Failure::Source),
            };
            self.push(Request::Data {
                asset: spec.kind,
                offset: self.cursor,
                bytes: buffer[..read].to_vec(),
            });
            self.cursor += read as u32;
            if self.queue.len() >= 8 {
                return;
            }
        }
        // Everything sent and confirmed: close the asset.
        if self.cursor == spec.size && self.confirmed == spec.size && self.queue.is_empty() {
            let req = self.next_req();
            self.state = State::AwaitAssetEnd(index);
            self.push(Request::AssetEnd {
                req,
                asset: spec.kind,
            });
        }
    }

    /// Feeds one frame the watch sent.
    pub fn on_frame(&mut self, frame: &[u8]) {
        match Incoming::decode(frame) {
            Ok(Incoming::Reply(reply)) => self.on_reply(reply),
            Ok(Incoming::Event(event)) => self.on_event(event),
            Err(_) => {}
        }
    }

    fn on_event(&mut self, event: Event) {
        let Event::Progress {
            asset,
            next_offset,
            status,
        } = event
        else {
            return;
        };
        let State::Streaming(index) = self.state else {
            return;
        };
        let spec = self.meta.assets[index];
        if asset != spec.kind {
            return;
        }
        match status {
            Status::Ok => {
                self.confirmed = self.confirmed.max(next_offset.min(spec.size));
                // With several frames on their way, a confirmation can outrun the cursor after a
                // rewind (it describes what the watch stored before the phone stepped back). What the
                // watch has stored is stored: never send it again.
                if self.cursor < self.confirmed {
                    self.cursor = self.confirmed;
                    self.queue.clear();
                }
                if self.rewound_to.is_some_and(|at| next_offset > at) {
                    self.rewound_to = None;
                }
            }
            Status::Sequence if self.rewound_to == Some(next_offset) => {}
            // The watch is somewhere else than the phone thinks: resume there.
            Status::Sequence if next_offset <= spec.size => {
                self.rewound_to = Some(next_offset);
                self.queue.clear();
                self.cursor = next_offset;
                self.confirmed = next_offset;
                self.resumes += 1;
            }
            other => self.fail(Failure::Refused(other)),
        }
    }

    fn on_reply(&mut self, reply: Reply) {
        let Reply::Ack {
            op: reply_op,
            req,
            status,
            ..
        } = reply
        else {
            return;
        };
        if req != self.expected_req {
            return;
        }
        if status != Status::Ok {
            return self.fail(Failure::Refused(status));
        }
        match (self.state, reply_op) {
            (State::AwaitBegin, op::BEGIN) => self.start_asset(0),
            (State::AwaitAssetEnd(index), op::ASSET_END) => self.start_asset(index + 1),
            (State::AwaitCommit, op::COMMIT) => self.state = State::Done,
            _ => self.fail(Failure::Protocol),
        }
    }

    fn start_asset(&mut self, index: usize) {
        self.cursor = 0;
        self.confirmed = 0;
        self.rewound_to = None;
        if index < self.meta.assets.len() {
            self.state = State::Streaming(index);
        } else {
            let req = self.next_req();
            self.state = State::AwaitCommit;
            self.push(Request::Commit {
                req,
                id: self.meta.id,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyra_player_core::{
        import::{ImportStore, Importer, Platform},
        persistence::Library,
    };
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Memory {
        staged: BTreeMap<u8, Vec<u8>>,
        committed: BTreeMap<u64, BTreeMap<u8, Vec<u8>>>,
    }

    impl ImportStore for Memory {
        fn stage_begin(&mut self, _: u64) -> Result<(), Status> {
            self.staged.clear();
            Ok(())
        }
        fn stage_write(&mut self, asset: Asset, _: u32, bytes: &[u8]) -> Result<(), Status> {
            self.staged
                .entry(asset as u8)
                .or_default()
                .extend_from_slice(bytes);
            Ok(())
        }
        fn stage_finish(&mut self, _: Asset) -> Result<(), Status> {
            Ok(())
        }
        fn stage_commit(&mut self, id: u64) -> Result<(), Status> {
            self.committed.insert(id, std::mem::take(&mut self.staged));
            Ok(())
        }
        fn stage_abort(&mut self) {
            self.staged.clear();
        }
        fn remove_track(&mut self, _: u64) -> Result<(), Status> {
            Ok(())
        }
        fn save_library(&mut self, _: &Library) -> Result<(), Status> {
            Ok(())
        }
        fn free_kib(&mut self) -> u32 {
            u32::MAX
        }
    }

    fn data(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed))
            .collect()
    }

    fn sender(audio: Vec<u8>, cover: Vec<u8>, lyrics: Vec<u8>) -> Sender {
        let mut sources: Vec<(Asset, Box<dyn Source>)> = vec![
            (Asset::Audio, Box::new(audio)),
            (Asset::Cover, Box::new(cover)),
            (Asset::Lyrics, Box::new(lyrics)),
        ];
        let assets = specs(&mut sources).unwrap();
        let meta = TrackMeta {
            id: 4242,
            duration_ms: 90_000,
            name: "Track".into(),
            artists: vec!["Artist".into()],
            album: String::new(),
            palette_top: 0x123456,
            palette_bottom: 0,
            assets,
        };
        Sender::new(meta, sources.into_iter().map(|(_, s)| s).collect())
    }

    /// Runs the sender against the real watch-side importer; `drop` decides
    /// which frames (by index) the "link" loses before they reach the watch.
    fn run(mut sender: Sender, drop: impl Fn(usize) -> bool) -> (Sender, Memory, Library) {
        let mut importer = Importer::new(Platform {
            width: 212,
            height: 520,
            caps: 0,
        });
        let mut library = Library::default();
        let mut store = Memory::default();
        let mut index = 0;
        for _ in 0..100_000 {
            if sender.is_finished() {
                break;
            }
            let Some(frame) = sender.poll() else {
                panic!("sender stalled in {:?}", sender.state());
            };
            index += 1;
            if drop(index) {
                continue;
            }
            let request = Request::decode(&frame).unwrap();
            let out = importer.handle(request, &mut library, &mut store);
            for reply in out.frames {
                sender.on_frame(&reply);
            }
        }
        (sender, store, library)
    }

    /// Like `run`, but the answers come late: `depth` frames are on their way (as when the plugin
    /// keeps several sends in flight) before the sender hears about the first of them.
    fn run_pipelined(
        mut sender: Sender,
        depth: usize,
        swap: bool,
        drop: impl Fn(usize) -> bool,
    ) -> (Sender, Memory, Library) {
        let mut importer = Importer::new(Platform {
            width: 212,
            height: 520,
            caps: 0,
        });
        let mut library = Library::default();
        let mut store = Memory::default();
        let mut on_the_way = std::collections::VecDeque::new();
        let mut index = 0;
        for _ in 0..200_000 {
            if sender.is_finished() {
                break;
            }
            while on_the_way.len() < depth {
                match sender.poll() {
                    Some(frame) => on_the_way.push_back(frame),
                    None => break,
                }
            }
            // With `swap`, every other delivery lets the frame behind the oldest overtake it.
            let which = usize::from(swap && index % 2 == 0 && on_the_way.len() > 1);
            let Some(frame) = on_the_way.remove(which) else {
                panic!("sender stalled in {:?}", sender.state());
            };
            index += 1;
            if drop(index) {
                continue;
            }
            let request = Request::decode(&frame).unwrap();
            for reply in importer.handle(request, &mut library, &mut store).frames {
                sender.on_frame(&reply);
            }
        }
        (sender, store, library)
    }

    #[test]
    fn frames_in_flight_and_late_answers_still_deliver_the_track_intact() {
        for depth in [2, 3, 6] {
            for (swap, lost) in [
                (false, None),
                (false, Some(9)),
                (true, None),
                (true, Some(40)),
            ] {
                let (audio, cover, lyrics) =
                    (data(90_000, 7), data(9_000, 8), b"[00:02.00]z".to_vec());
                let (sender, store, library) = run_pipelined(
                    sender(audio.clone(), cover.clone(), lyrics.clone()),
                    depth,
                    swap,
                    |i| Some(i) == lost,
                );
                if lost.is_none() && depth <= 5 {
                    // A frame that overtakes its neighbours is held back by the watch: no rewind.
                    assert_eq!(sender.resumes(), 0, "depth {depth}, swap {swap}");
                }
                assert_eq!(sender.state(), State::Done, "depth {depth}, lost {lost:?}");
                let files = &store.committed[&4242];
                assert_eq!(
                    files[&(Asset::Audio as u8)],
                    audio,
                    "depth {depth}, lost {lost:?}"
                );
                assert_eq!(files[&(Asset::Cover as u8)], cover);
                assert_eq!(library.tracks.len(), 1);
            }
        }
    }

    #[test]
    fn a_track_travels_from_sender_to_importer() {
        let (audio, cover, lyrics) = (data(70_000, 1), data(5_000, 2), b"[00:01.00]x".to_vec());
        let (sender, store, library) =
            run(sender(audio.clone(), cover.clone(), lyrics.clone()), |_| {
                false
            });
        assert_eq!(sender.state(), State::Done);
        assert_eq!(sender.resumes(), 0);
        assert_eq!(sender.confirmed_bytes(), sender.total_bytes());
        let files = &store.committed[&4242];
        assert_eq!(files[&(Asset::Audio as u8)], audio);
        assert_eq!(files[&(Asset::Cover as u8)], cover);
        assert_eq!(files[&(Asset::Lyrics as u8)], lyrics);
        assert_eq!(library.tracks.len(), 1);
    }

    #[test]
    fn lost_frames_make_the_sender_resume_where_the_watch_is() {
        let (audio, cover, lyrics) = (data(60_000, 3), data(2_000, 4), b"[00:01.00]y".to_vec());
        // Lose a data frame early and another in the middle.
        let (sender, store, _) = run(sender(audio.clone(), cover, lyrics), |i| i == 7 || i == 61);
        assert_eq!(sender.state(), State::Done);
        assert!(sender.resumes() >= 1);
        assert_eq!(store.committed[&4242][&(Asset::Audio as u8)], audio);
    }

    #[test]
    fn the_window_bounds_unconfirmed_bytes() {
        let mut sender = sender(data(200_000, 5), data(10, 6), b"x".to_vec());
        let mut importer = Importer::new(Platform {
            width: 1,
            height: 1,
            caps: 0,
        });
        let (mut library, mut store) = (Library::default(), Memory::default());
        // BEGIN first; answer it so streaming starts.
        let begin = sender.poll().unwrap();
        for reply in importer
            .handle(Request::decode(&begin).unwrap(), &mut library, &mut store)
            .frames
        {
            sender.on_frame(&reply);
        }
        // Without any confirmation the sender stops at the window.
        let mut sent = 0u32;
        while let Some(frame) = sender.poll() {
            if let Ok(Request::Data { bytes, .. }) = Request::decode(&frame) {
                sent += bytes.len() as u32;
            }
        }
        assert!(
            sent >= WINDOW && sent < WINDOW + DATA_MAX as u32,
            "sent {sent}"
        );
    }

    #[test]
    fn a_refusal_or_an_unreadable_source_fails_the_transfer() {
        let mut sender = sender(data(100, 7), data(10, 8), b"z".to_vec());
        let begin = sender.poll().unwrap();
        let Request::Begin { req, .. } = Request::decode(&begin).unwrap() else {
            panic!()
        };
        let refusal = Reply::Ack {
            op: op::BEGIN,
            req,
            status: Status::NoSpace,
            value: 0,
        }
        .encode()
        .unwrap();
        sender.on_frame(&refusal);
        assert_eq!(
            sender.state(),
            State::Failed(Failure::Refused(Status::NoSpace))
        );
        assert_eq!(sender.poll(), None);

        struct Broken;
        impl Source for Broken {
            fn len(&self) -> u32 {
                10
            }
            fn read(&mut self, _: u32, _: &mut [u8]) -> Result<usize, String> {
                Err("io".into())
            }
        }
        let meta = TrackMeta {
            id: 1,
            duration_ms: 0,
            name: "n".into(),
            artists: vec![],
            album: String::new(),
            palette_top: 0,
            palette_bottom: 0,
            assets: vec![AssetSpec {
                kind: Asset::Audio,
                size: 10,
                crc32: 0,
            }],
        };
        let mut sender = Sender::new(meta, vec![Box::new(Broken)]);
        let begin = sender.poll().unwrap();
        let Request::Begin { req, .. } = Request::decode(&begin).unwrap() else {
            panic!()
        };
        sender.on_frame(
            &Reply::Ack {
                op: op::BEGIN,
                req,
                status: Status::Ok,
                value: 0,
            }
            .encode()
            .unwrap(),
        );
        assert_eq!(sender.poll(), None);
        assert_eq!(sender.state(), State::Failed(Failure::Source));
    }
}
