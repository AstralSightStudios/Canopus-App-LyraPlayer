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
}

#[derive(Clone, Debug, Default)]
pub struct CloudSong {
    pub id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_id: u64,
    pub duration_ms: u32,
    pub cover_url: String,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub root: Option<String>,
    pub devices: Vec<DeviceInfo>,
    pub selected_addr: String,
    pub audio: Option<SelectedFile>,
    pub cover: Option<SelectedFile>,
    pub lyrics: Option<SelectedFile>,
    pub track_name: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub status: String,
    pub sent: u64,
    pub total: u64,
    pub active: bool,
    pub netease_cookie: String,
    pub netease_query: String,
    pub netease_results: Vec<CloudSong>,
    pub netease_selected: usize,
    pub qr_key: String,
    pub qr_url: String,
}

static STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

pub fn with_state<R>(f: impl FnOnce(&mut UiState) -> R) -> R {
    let mut state = STATE
        .get_or_init(|| Mutex::new(UiState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut state)
}

pub fn snapshot() -> UiState {
    with_state(|state| state.clone())
}
