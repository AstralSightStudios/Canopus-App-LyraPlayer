use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

use crate::{Session, Song};

pub const DATA_DIR: &str = "/data/canopus";
pub const SESSION_PATH: &str = "/data/canopus/lyra-player-session.json";
pub const LIBRARY_PATH: &str = "/data/canopus/lyra-player-library.json";
pub const IMPORT_DIR: &str = "/data/canopus";

pub trait Store {
    type Error;

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Self::Error>;
    fn write_atomic(&mut self, path: &str, bytes: &[u8]) -> Result<(), Self::Error>;
    fn remove(&mut self, path: &str) -> Result<(), Self::Error>;
}

#[derive(Serialize, Deserialize)]
struct SessionFile {
    version: u8,
    session: Session,
}

#[derive(Serialize, Deserialize)]
struct LibraryFile {
    version: u8,
    tracks: Vec<Song>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceError<E> {
    Storage(E),
    Json,
    Version,
}

pub fn load_session<S: Store>(
    store: &mut S,
) -> Result<Option<Session>, PersistenceError<S::Error>> {
    let Some(bytes) = store
        .read(SESSION_PATH)
        .map_err(PersistenceError::Storage)?
    else {
        return Ok(None);
    };
    let file: SessionFile = serde_json::from_slice(&bytes).map_err(|_| PersistenceError::Json)?;
    if file.version != 1 {
        return Err(PersistenceError::Version);
    }
    Ok(Some(file.session))
}

pub fn save_session<S: Store>(
    store: &mut S,
    session: &Session,
) -> Result<(), PersistenceError<S::Error>> {
    let bytes = serde_json::to_vec(&SessionFile {
        version: 1,
        session: session.clone(),
    })
    .map_err(|_| PersistenceError::Json)?;
    store
        .write_atomic(SESSION_PATH, &bytes)
        .map_err(PersistenceError::Storage)
}

pub fn clear_session<S: Store>(store: &mut S) -> Result<(), PersistenceError<S::Error>> {
    store
        .remove(SESSION_PATH)
        .map_err(PersistenceError::Storage)
}

pub fn load_library<S: Store>(store: &mut S) -> Result<Vec<Song>, PersistenceError<S::Error>> {
    let Some(bytes) = store
        .read(LIBRARY_PATH)
        .map_err(PersistenceError::Storage)?
    else {
        return Ok(Vec::new());
    };
    let file: LibraryFile = serde_json::from_slice(&bytes).map_err(|_| PersistenceError::Json)?;
    if file.version != 1 {
        return Err(PersistenceError::Version);
    }
    Ok(file.tracks)
}

pub fn save_library<S: Store>(
    store: &mut S,
    tracks: &[Song],
) -> Result<(), PersistenceError<S::Error>> {
    let bytes = serde_json::to_vec(&LibraryFile {
        version: 1,
        tracks: tracks.to_vec(),
    })
    .map_err(|_| PersistenceError::Json)?;
    store
        .write_atomic(LIBRARY_PATH, &bytes)
        .map_err(PersistenceError::Storage)
}

pub fn is_safe_local_path(path: &str) -> bool {
    let Some(file_name) = path.strip_prefix(&alloc::format!("{IMPORT_DIR}/")) else {
        return false;
    };
    safe_import_path(file_name).as_deref() == Some(path)
}

pub fn safe_import_path(file_name: &str) -> Option<String> {
    if file_name.is_empty()
        || file_name.len() > 96
        || file_name.starts_with('.')
        || file_name.contains("..")
        || !file_name.ends_with(".mp3")
        || file_name
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | 0) || byte.is_ascii_control())
    {
        return None;
    }
    Some(alloc::format!("{IMPORT_DIR}/{file_name}"))
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
        fn write_atomic(&mut self, path: &str, bytes: &[u8]) -> Result<(), Self::Error> {
            self.0.insert(path.into(), bytes.to_vec());
            Ok(())
        }
        fn remove(&mut self, path: &str) -> Result<(), Self::Error> {
            self.0.remove(path);
            Ok(())
        }
    }

    #[test]
    fn session_round_trips() {
        let mut store = MemoryStore::default();
        let session = Session {
            cookie: "MUSIC_U=secret".into(),
            ..Session::default()
        };
        save_session(&mut store, &session).unwrap();
        assert_eq!(load_session(&mut store).unwrap(), Some(session));
    }

    #[test]
    fn import_paths_reject_traversal() {
        assert!(safe_import_path("track.mp3").is_some());
        assert!(safe_import_path("../track.mp3").is_none());
        assert!(safe_import_path("track..backup.mp3").is_none());
        assert!(safe_import_path("cover.png").is_none());
        assert!(is_safe_local_path("/data/canopus/track.mp3"));
        assert!(!is_safe_local_path("/etc/passwd"));
    }
}
