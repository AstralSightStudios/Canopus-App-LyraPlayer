use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

use crate::Song;

pub const QUICKAPP_PACKAGE: &str = "com.canopus.lyraimport";
pub const IMPORT_ROOT: &str = "/data/files/com.canopus.lyraimport/lyra";
pub const LIBRARY_PATH: &str = "/data/files/com.canopus.lyraimport/lyra/library.json";
const LEGACY_IMPORT_ROOT: &str = "/data/quickapp/files/com.canopus.lyraimport/lyra";

pub trait Store {
    type Error;

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error>;
}

#[derive(Serialize, Deserialize)]
struct LibraryFile {
    version: u8,
    #[serde(default)]
    tracks: Vec<Song>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceError<E> {
    Storage(E),
    Json,
    Version,
    UnsafePath,
}

pub fn load_library<S: Store>(store: &mut S) -> Result<Vec<Song>, PersistenceError<S::Error>> {
    let Some(bytes) = store
        .read(LIBRARY_PATH)
        .map_err(PersistenceError::Storage)?
    else {
        return Ok(Vec::new());
    };
    let mut file: LibraryFile =
        serde_json::from_slice(&bytes).map_err(|_| PersistenceError::Json)?;
    if file.version != 1 {
        return Err(PersistenceError::Version);
    }
    for track in &mut file.tracks {
        if let Some(path) = &mut track.local_path {
            normalize_legacy_path(path);
        }
        if !track.album.cover_url.is_empty() {
            normalize_legacy_path(&mut track.album.cover_url);
        }
        if !track.album.background_url.is_empty() {
            normalize_legacy_path(&mut track.album.background_url);
        }
        if let Some(path) = &mut track.lyrics_path {
            normalize_legacy_path(path);
        }
    }
    if file.tracks.iter().any(|track| {
        !track.local_path.as_deref().is_some_and(is_safe_audio_path)
            || !track.album.cover_url.is_empty() && !is_safe_cover_path(&track.album.cover_url)
            || !track.album.background_url.is_empty()
                && !is_safe_background_path(&track.album.background_url)
            || track
                .lyrics_path
                .as_deref()
                .is_some_and(|path| !is_safe_lyrics_path(path))
    }) {
        return Err(PersistenceError::UnsafePath);
    }
    Ok(file.tracks)
}

fn normalize_legacy_path(path: &mut String) {
    let Some(relative) = path
        .strip_prefix(LEGACY_IMPORT_ROOT)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return;
    };
    *path = alloc::format!("{IMPORT_ROOT}/{relative}");
}

pub fn is_safe_audio_path(path: &str) -> bool {
    is_safe_import_path(path, &[".mp3"])
}

pub fn is_safe_cover_path(path: &str) -> bool {
    is_safe_import_path(path, &[".jpg", ".jpeg", ".png", ".bin"])
}

pub fn is_safe_background_path(path: &str) -> bool {
    is_safe_import_path(path, &[".bin"])
}

pub fn is_safe_lyrics_path(path: &str) -> bool {
    is_safe_import_path(path, &[".lrc", ".json", ".txt"])
}

fn is_safe_import_path(path: &str, extensions: &[&str]) -> bool {
    let Some(relative) = path
        .strip_prefix(IMPORT_ROOT)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return false;
    };
    !relative.is_empty()
        && relative.len() <= 240
        && !relative.starts_with('.')
        && !relative.contains("..")
        && !relative
            .bytes()
            .any(|byte| matches!(byte, b'\\' | 0) || byte.is_ascii_control())
        && extensions
            .iter()
            .any(|extension| relative.to_ascii_lowercase().ends_with(extension))
}

pub fn physical_path(relative: &str) -> Option<String> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.starts_with('.')
        || relative.contains("..")
        || relative
            .bytes()
            .any(|byte| matches!(byte, b'\\' | 0) || byte.is_ascii_control())
    {
        return None;
    }
    Some(alloc::format!("{IMPORT_ROOT}/{relative}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    #[derive(Default)]
    struct MemoryStore(BTreeMap<String, Vec<u8>>);
    impl Store for MemoryStore {
        type Error = ();
        fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok(self.0.get(path).cloned())
        }
    }

    #[test]
    fn quickapp_library_round_trips() {
        let song = Song {
            id: 7,
            name: "Local".into(),
            local_path: Some(alloc::format!("{IMPORT_ROOT}/tracks/7/audio.mp3")),
            lyrics_path: Some(alloc::format!("{IMPORT_ROOT}/tracks/7/lyrics.lrc")),
            album: crate::AlbumRef {
                background_url: alloc::format!("{IMPORT_ROOT}/tracks/7/background.bin"),
                ..crate::AlbumRef::default()
            },
            ..Song::default()
        };
        let bytes = serde_json::to_vec(&LibraryFile {
            version: 1,
            tracks: alloc::vec![song.clone()],
        })
        .unwrap();
        let mut store = MemoryStore::default();
        store.0.insert(LIBRARY_PATH.into(), bytes);
        assert_eq!(load_library(&mut store).unwrap(), alloc::vec![song]);
    }

    #[test]
    fn legacy_manifest_paths_are_normalized_to_real_storage_root() {
        let legacy_audio = alloc::format!("{LEGACY_IMPORT_ROOT}/tracks/7/audio.mp3");
        let legacy_cover = alloc::format!("{LEGACY_IMPORT_ROOT}/tracks/7/cover.jpg");
        let legacy_background = alloc::format!("{LEGACY_IMPORT_ROOT}/tracks/7/background.bin");
        let legacy_lyrics = alloc::format!("{LEGACY_IMPORT_ROOT}/tracks/7/lyrics.json");
        let song = Song {
            id: 7,
            name: "Legacy".into(),
            local_path: Some(legacy_audio),
            lyrics_path: Some(legacy_lyrics),
            album: crate::AlbumRef {
                cover_url: legacy_cover,
                background_url: legacy_background,
                ..crate::AlbumRef::default()
            },
            ..Song::default()
        };
        let bytes = serde_json::to_vec(&LibraryFile {
            version: 1,
            tracks: alloc::vec![song],
        })
        .unwrap();
        let mut store = MemoryStore::default();
        store.0.insert(LIBRARY_PATH.into(), bytes);
        let loaded = load_library(&mut store).unwrap();
        assert_eq!(
            loaded[0].local_path.as_deref(),
            Some("/data/files/com.canopus.lyraimport/lyra/tracks/7/audio.mp3")
        );
        assert_eq!(
            loaded[0].album.cover_url,
            "/data/files/com.canopus.lyraimport/lyra/tracks/7/cover.jpg"
        );
        assert_eq!(
            loaded[0].album.background_url,
            "/data/files/com.canopus.lyraimport/lyra/tracks/7/background.bin"
        );
        assert_eq!(
            loaded[0].lyrics_path.as_deref(),
            Some("/data/files/com.canopus.lyraimport/lyra/tracks/7/lyrics.json")
        );
    }

    #[test]
    fn rejects_paths_outside_quickapp_sandbox() {
        assert!(is_safe_audio_path(&alloc::format!(
            "{IMPORT_ROOT}/tracks/1/audio.mp3"
        )));
        assert!(!is_safe_audio_path("/data/canopus/audio.mp3"));
        assert!(!is_safe_audio_path(&alloc::format!(
            "{IMPORT_ROOT}/../escape.mp3"
        )));
        assert!(!is_safe_cover_path(&alloc::format!(
            "{IMPORT_ROOT}/tracks/1/audio.mp3"
        )));
        assert!(is_safe_background_path(&alloc::format!(
            "{IMPORT_ROOT}/tracks/1/background.bin"
        )));
        assert!(!is_safe_background_path(&alloc::format!(
            "{IMPORT_ROOT}/tracks/1/background.jpg"
        )));
    }
}
