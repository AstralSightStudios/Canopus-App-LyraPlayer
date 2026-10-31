//! Small, transactional playlist operations. Playlists contain references, not
//! copies of audio: deleting a playlist never deletes a song.

use alloc::vec::Vec;
use lyra_protocol::{
    FRAME_MAX, MAX_PLAYLIST_NAME, MAX_PLAYLIST_TRACKS, MAX_PLAYLISTS, PlaylistEntry, Reply,
    Request, Status, op,
};

use crate::{
    import::{Change, ImportStore, Outcome},
    persistence::{Library, Playlist},
};

pub(crate) fn handle<S: ImportStore>(
    request: Request,
    library: &mut Library,
    store: &mut S,
) -> Outcome {
    let mut out = Outcome::default();
    match request {
        Request::Playlists { req, offset, max } => {
            let mut used = 9;
            let entries = library
                .playlists
                .iter()
                .skip(usize::from(offset))
                .take(usize::from(max.max(1)))
                .take_while(|list| {
                    used += 11 + list.name.len();
                    used <= FRAME_MAX
                })
                .map(|list| PlaylistEntry {
                    id: list.id,
                    name: list.name.clone(),
                    tracks: list.tracks.len().min(u16::MAX as usize) as u16,
                })
                .collect();
            out.reply(Reply::Playlists {
                req,
                status: Status::Ok,
                total: library.playlists.len() as u16,
                offset,
                entries,
            });
        }
        Request::PlaylistTracks {
            req,
            id,
            offset,
            max,
        } => {
            let list = library.playlists.iter().find(|list| list.id == id);
            let tracks = list
                .map(|list| {
                    list.tracks
                        .iter()
                        .copied()
                        .skip(usize::from(offset))
                        .take(usize::from(max.max(1)).min((FRAME_MAX - 17) / 8))
                        .collect()
                })
                .unwrap_or_default();
            out.reply(Reply::PlaylistTracks {
                req,
                status: if list.is_some() {
                    Status::Ok
                } else {
                    Status::NotFound
                },
                id,
                total: list.map_or(0, |list| list.tracks.len() as u16),
                offset,
                tracks,
            });
        }
        Request::PlaylistUpsert { req, id, name } => {
            let mut candidate = library.clone();
            let status = if id == 0
                || name.trim().is_empty()
                || name.len() > MAX_PLAYLIST_NAME
                || name.contains('\0')
            {
                Status::BadRequest
            } else if let Some(list) = candidate.playlists.iter_mut().find(|list| list.id == id) {
                list.name = name;
                save(candidate, library, store)
            } else if candidate.playlists.len() >= MAX_PLAYLISTS {
                Status::NoSpace
            } else {
                candidate.playlists.push(Playlist {
                    id,
                    name,
                    tracks: Vec::new(),
                });
                save(candidate, library, store)
            };
            changed(&mut out, op::PLAYLIST_UPSERT, req, status, library.revision);
        }
        Request::PlaylistDelete { req, id } => {
            let mut candidate = library.clone();
            let status =
                if let Some(index) = candidate.playlists.iter().position(|list| list.id == id) {
                    candidate.playlists.remove(index);
                    save(candidate, library, store)
                } else {
                    Status::NotFound
                };
            changed(&mut out, op::PLAYLIST_DELETE, req, status, library.revision);
        }
        Request::PlaylistTrack {
            req,
            id,
            track,
            add,
        } => {
            let mut candidate = library.clone();
            let known = !add || candidate.tracks.iter().any(|song| song.id == track);
            let status = if !known {
                Status::NotFound
            } else if let Some(list) = candidate.playlists.iter_mut().find(|list| list.id == id) {
                if add && !list.tracks.contains(&track) {
                    if list.tracks.len() >= MAX_PLAYLIST_TRACKS {
                        out.ack(op::PLAYLIST_TRACK, req, Status::NoSpace, library.revision);
                        return out;
                    }
                    list.tracks.push(track);
                } else if !add {
                    list.tracks.retain(|id| *id != track);
                }
                if candidate == *library {
                    Status::Ok
                } else {
                    save(candidate, library, store)
                }
            } else {
                Status::NotFound
            };
            changed(&mut out, op::PLAYLIST_TRACK, req, status, library.revision);
        }
        _ => {}
    }
    out
}

fn save<S: ImportStore>(mut candidate: Library, library: &mut Library, store: &mut S) -> Status {
    candidate.revision = library.revision.wrapping_add(1);
    match store.save_library(&candidate) {
        Ok(()) => {
            *library = candidate;
            Status::Ok
        }
        Err(status) => status,
    }
}

fn changed(out: &mut Outcome, opcode: u8, req: u16, status: Status, revision: u32) {
    if status == Status::Ok {
        out.changes.push(Change::Library);
    }
    out.ack(opcode, req, status, revision);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Song;
    use lyra_protocol::{Asset, Incoming};

    #[derive(Default)]
    struct Store {
        fail: bool,
        saved: Option<Library>,
    }
    impl ImportStore for Store {
        fn stage_begin(&mut self, _: u64) -> Result<(), Status> {
            Err(Status::Unsupported)
        }
        fn stage_write(&mut self, _: Asset, _: u32, _: &[u8]) -> Result<(), Status> {
            Err(Status::Unsupported)
        }
        fn stage_finish(&mut self, _: Asset) -> Result<(), Status> {
            Err(Status::Unsupported)
        }
        fn stage_commit(&mut self, _: u64) -> Result<(), Status> {
            Err(Status::Unsupported)
        }
        fn stage_abort(&mut self) {}
        fn remove_track(&mut self, _: u64) -> Result<(), Status> {
            panic!("playlist operations must not remove audio")
        }
        fn save_library(&mut self, library: &Library) -> Result<(), Status> {
            if self.fail {
                Err(Status::Io)
            } else {
                self.saved = Some(library.clone());
                Ok(())
            }
        }
        fn free_kib(&mut self) -> u32 {
            u32::MAX
        }
    }
    fn status(out: Outcome) -> Status {
        match Incoming::decode(&out.frames[0]).unwrap() {
            Incoming::Reply(Reply::Ack { status, .. }) => status,
            _ => panic!("not an ack"),
        }
    }
    fn library() -> Library {
        Library {
            tracks: (1..=3)
                .map(|id| Song {
                    id,
                    ..Song::default()
                })
                .collect(),
            ..Library::default()
        }
    }

    #[test]
    fn songs_can_belong_to_multiple_or_no_playlists() {
        let mut library = library();
        let mut store = Store::default();
        for id in [7, 8] {
            assert_eq!(
                status(handle(
                    Request::PlaylistUpsert {
                        req: 1,
                        id,
                        name: "喜欢".into()
                    },
                    &mut library,
                    &mut store
                )),
                Status::Ok
            );
            let add = Request::PlaylistTrack {
                req: 2,
                id,
                track: 1,
                add: true,
            };
            assert_eq!(
                status(handle(add.clone(), &mut library, &mut store)),
                Status::Ok
            );
            assert_eq!(status(handle(add, &mut library, &mut store)), Status::Ok);
            assert_eq!(library.playlists.last().unwrap().tracks, alloc::vec![1]);
        }
        assert_eq!(library.collection(None), alloc::vec![1, 2, 3]);
        assert_eq!(
            status(handle(
                Request::PlaylistDelete { req: 3, id: 7 },
                &mut library,
                &mut store
            )),
            Status::Ok
        );
        assert_eq!(library.tracks.len(), 3);
        assert_eq!(library.playlists[0].id, 8);
        assert_eq!(store.saved.as_ref(), Some(&library));
    }

    #[test]
    fn failed_save_leaves_memory_and_revision_unchanged() {
        let mut library = library();
        let before = library.clone();
        let mut store = Store {
            fail: true,
            saved: None,
        };
        let out = handle(
            Request::PlaylistUpsert {
                req: 1,
                id: 7,
                name: "test".into(),
            },
            &mut library,
            &mut store,
        );
        assert!(out.changes.is_empty());
        assert_eq!(status(out), Status::Io);
        assert_eq!(library, before);
    }

    #[test]
    fn playlist_pages_fit_frames_and_members_keep_their_order() {
        let mut library = library();
        let mut store = Store::default();
        library.playlists = (1..=32)
            .map(|id| Playlist {
                id,
                name: "x".repeat(MAX_PLAYLIST_NAME),
                tracks: (1..=200).collect(),
            })
            .collect();
        let out = handle(
            Request::Playlists {
                req: 1,
                offset: 0,
                max: 255,
            },
            &mut library,
            &mut store,
        );
        assert!(out.frames[0].len() <= FRAME_MAX);
        let Incoming::Reply(Reply::Playlists { entries, total, .. }) =
            Incoming::decode(&out.frames[0]).unwrap()
        else {
            panic!()
        };
        assert_eq!(total, 32);
        assert!(!entries.is_empty());
        assert!(entries.len() < 32);
        let out = handle(
            Request::PlaylistTracks {
                req: 2,
                id: 1,
                offset: 50,
                max: 255,
            },
            &mut library,
            &mut store,
        );
        let Incoming::Reply(Reply::PlaylistTracks { tracks, total, .. }) =
            Incoming::decode(&out.frames[0]).unwrap()
        else {
            panic!()
        };
        assert_eq!(total, 200);
        assert_eq!(tracks[0], 51);
        assert!(out.frames[0].len() <= FRAME_MAX);
    }
}
