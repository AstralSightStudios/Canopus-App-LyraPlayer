use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    pub addr: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct SelectedFile {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub duration_ms: u32,
    /// Detected from the file's content: "mp3", "jpeg", "png", "json" or "lrc".
    pub format: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceLibraryTrack {
    pub id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u32,
}

/// A playlist on the watch: only references to songs.
#[derive(Clone, Debug, Default)]
pub struct DevicePlaylist {
    pub id: u64,
    pub name: String,
    pub tracks: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct PlaylistEditor {
    /// `None` creates a playlist; `Some` renames the existing one.
    pub id: Option<u64>,
    pub name: String,
}

/// The page's two tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Local,
    Library,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub root: Option<String>,
    pub tab: Tab,
    /// The song the running (or last) import carries.
    pub import_title: String,
    /// The watch playlist whose songs are listed open.
    pub expanded_playlist: Option<u64>,
    pub playlist_editor: Option<PlaylistEditor>,
    pub devices: Vec<DeviceInfo>,
    pub selected_addr: String,
    pub audio: Option<SelectedFile>,
    pub cover: Option<SelectedFile>,
    pub lyrics: Option<SelectedFile>,
    /// Whether the cover/lyrics came out of the picked MP3's ID3 tag.
    pub cover_embedded: bool,
    pub lyrics_embedded: bool,
    /// Bitrate the local import is compressed to; 0 keeps the original.
    pub local_audio_bitrate: u32,
    pub track_name: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub status: String,
    pub sent: u64,
    pub total: u64,
    pub speed_bytes_per_second: u64,
    /// Average time the watch took to take one frame, over the last speed sample.
    pub round_trip_ms: u32,
    pub active: bool,
    /// A confirmation dialog is waiting for an answer; duplicate clicks are ignored.
    pub confirm_open: bool,
    /// Songs whose deletion was confirmed and has not finished refreshing yet.
    pub deleting_tracks: Vec<u64>,
    pub device_library: Vec<DeviceLibraryTrack>,
    pub library_revision: String,
    pub library_total: usize,
    pub library_busy: bool,
    pub library_target: Option<u64>,
    /// The protocol version the module on the watch speaks; 0 until it has answered.
    pub device_version: u8,
    pub device_playlists: Vec<DevicePlaylist>,
}

static STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

pub fn with_state<R>(f: impl FnOnce(&mut UiState) -> R) -> R {
    let mut state = STATE
        .get_or_init(|| {
            Mutex::new(UiState {
                local_audio_bitrate: crate::audio::DEFAULT,
                ..UiState::default()
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut state)
}

pub fn snapshot() -> UiState {
    with_state(|state| state.clone())
}
