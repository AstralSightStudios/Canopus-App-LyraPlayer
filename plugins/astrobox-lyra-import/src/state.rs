use std::{
    fs,
    sync::{Mutex, OnceLock},
};

const NETEASE_SESSION_DIR: &str = "data";
const NETEASE_SESSION_PATH: &str = "data/netease-session.cookie";
const NETEASE_SESSION_TMP: &str = "data/netease-session.cookie.tmp";
const MAX_SESSION_BYTES: usize = 64 * 1024;

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
pub struct CloudPlaylist {
    pub id: u64,
    pub name: String,
    pub creator: String,
    pub track_count: u32,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceLibraryTrack {
    pub id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NeteaseView {
    #[default]
    Home,
    SearchResults,
    Playlists,
    PlaylistTracks,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferMode {
    #[default]
    Crc32,
    Fast,
    Ultra,
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
    pub speed_bytes_per_second: u64,
    pub transfer_mode: TransferMode,
    pub active: bool,
    pub device_library: Vec<DeviceLibraryTrack>,
    pub library_request_id: String,
    pub library_revision: String,
    pub library_total: usize,
    pub library_busy: bool,
    pub library_target: Option<u64>,
    pub library_nonce: u64,
    pub netease_cookie: String,
    pub netease_view: NeteaseView,
    pub netease_audio_bitrate: u32,
    pub netease_query: String,
    pub netease_results: Vec<CloudSong>,
    pub netease_playlists: Vec<CloudPlaylist>,
    pub netease_playlist_name: String,
    pub netease_playlist_tracks: Vec<CloudSong>,
    pub netease_playlist_page: usize,
    pub qr_key: String,
    pub qr_url: String,
}

static STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

pub fn with_state<R>(f: impl FnOnce(&mut UiState) -> R) -> R {
    let mut state = STATE
        .get_or_init(|| {
            Mutex::new(UiState {
                transfer_mode: TransferMode::Crc32,
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

pub fn load_netease_session() -> Result<Option<String>, String> {
    let bytes = match fs::read(NETEASE_SESSION_PATH) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取网易云登录状态：{error}")),
    };
    if bytes.is_empty() || bytes.len() > MAX_SESSION_BYTES {
        return Err("网易云登录状态文件为空或过大".to_string());
    }
    let cookie =
        String::from_utf8(bytes).map_err(|_| "网易云登录状态文件不是有效 UTF-8".to_string())?;
    let cookie = cookie.trim().to_string();
    if cookie.is_empty() {
        Ok(None)
    } else {
        Ok(Some(cookie))
    }
}

pub fn save_netease_session(cookie: &str) -> Result<(), String> {
    let cookie = cookie.trim();
    if cookie.is_empty() || cookie.len() > MAX_SESSION_BYTES {
        return Err("网易云登录数据为空或过大".to_string());
    }
    fs::create_dir_all(NETEASE_SESSION_DIR)
        .map_err(|error| format!("无法创建登录状态目录：{error}"))?;
    fs::write(NETEASE_SESSION_TMP, cookie.as_bytes())
        .map_err(|error| format!("无法写入网易云登录状态：{error}"))?;
    if let Err(first_error) = fs::rename(NETEASE_SESSION_TMP, NETEASE_SESSION_PATH) {
        if first_error.kind() != std::io::ErrorKind::AlreadyExists {
            let _ = fs::remove_file(NETEASE_SESSION_TMP);
            return Err(format!("无法发布网易云登录状态：{first_error}"));
        }
        fs::remove_file(NETEASE_SESSION_PATH)
            .map_err(|error| format!("无法替换网易云登录状态：{error}"))?;
        fs::rename(NETEASE_SESSION_TMP, NETEASE_SESSION_PATH)
            .map_err(|error| format!("无法发布网易云登录状态：{error}"))?;
    }
    Ok(())
}

pub fn clear_netease_session() -> Result<(), String> {
    match fs::remove_file(NETEASE_SESSION_PATH) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法删除网易云登录状态：{error}")),
    }
}
