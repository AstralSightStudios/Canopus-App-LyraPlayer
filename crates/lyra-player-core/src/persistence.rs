use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

use crate::{ROOT, Song};

pub const LIBRARY_FILE: &str = "library.json";
pub const LIBRARY_PATH: &str = "/data/canopus/m.lyra_player/library.json";
const FORMAT_VERSION: u8 = 2;

pub trait Store {
    type Error;

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error>;
}

#[derive(Serialize, Deserialize)]
struct LibraryFile {
    version: u8,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    tracks: Vec<Song>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    pub revision: u32,
    pub tracks: Vec<Song>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceError<E> {
    Storage(E),
    Json,
    Version,
}

pub fn load_library<S: Store>(store: &mut S) -> Result<Library, PersistenceError<S::Error>> {
    let Some(bytes) = store
        .read(LIBRARY_PATH)
        .map_err(PersistenceError::Storage)?
    else {
        return Ok(Library::default());
    };
    let file: LibraryFile = serde_json::from_slice(&bytes).map_err(|_| PersistenceError::Json)?;
    if file.version != FORMAT_VERSION {
        return Err(PersistenceError::Version);
    }
    Ok(Library {
        revision: file.revision,
        tracks: file.tracks,
    })
}

pub fn encode_library(library: &Library) -> Vec<u8> {
    let file = LibraryFile {
        version: FORMAT_VERSION,
        revision: library.revision,
        tracks: library.tracks.clone(),
    };
    serde_json::to_vec(&file).unwrap_or_default()
}

/// A track id is a plain decimal directory name; nothing the phone sends is
/// ever spliced into a path except through this check.
pub fn is_valid_asset_name(name: &str) -> bool {
    matches!(name, "audio.mp3" | "cover.bin" | "lyrics.lrc")
}

pub fn staging_dir() -> String {
    alloc::format!("{ROOT}/stage")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assets, Palette, Rgb};
    use alloc::collections::BTreeMap;

    #[derive(Default)]
    struct MemoryStore(BTreeMap<String, Vec<u8>>);
    impl Store for MemoryStore {
        type Error = ();
        fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok(self.0.get(path).cloned())
        }
    }

    fn song() -> Song {
        Song {
            id: 7,
            name: "Local".into(),
            artists: alloc::vec!["A".into(), "B".into()],
            album: "Album".into(),
            duration_ms: 123_000,
            assets: Assets(Assets::COVER | Assets::LYRICS),
            palette: Palette {
                top: Rgb(0xFF8800),
                bottom: Rgb(0x101010),
            },
        }
    }

    #[test]
    fn library_round_trips() {
        let library = Library {
            revision: 9,
            tracks: alloc::vec![song()],
        };
        let mut store = MemoryStore::default();
        store.0.insert(LIBRARY_PATH.into(), encode_library(&library));
        assert_eq!(load_library(&mut store).unwrap(), library);
    }

    #[test]
    fn missing_library_is_empty_and_old_versions_are_rejected() {
        let mut store = MemoryStore::default();
        assert_eq!(load_library(&mut store).unwrap(), Library::default());
        store
            .0
            .insert(LIBRARY_PATH.into(), br#"{"version":1,"tracks":[]}"#.to_vec());
        assert_eq!(load_library(&mut store), Err(PersistenceError::Version));
    }

    #[test]
    fn track_paths_stay_inside_the_module_directory() {
        let song = song();
        assert_eq!(
            song.audio_path(),
            "/data/canopus/m.lyra_player/tracks/7/audio.mp3"
        );
        assert!(song.cover_path().unwrap().ends_with("/7/cover.bin"));
        assert!(song.lyrics_path().unwrap().ends_with("/7/lyrics.lrc"));
        let bare = Song {
            assets: Assets::default(),
            ..song
        };
        assert!(bare.cover_path().is_none() && bare.lyrics_path().is_none());
        assert!(is_valid_asset_name("audio.mp3"));
        assert!(!is_valid_asset_name("../audio.mp3"));
    }
}
