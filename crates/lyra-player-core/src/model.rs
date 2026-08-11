use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRef {
    pub id: u64,
    pub name: String,
    #[serde(default, alias = "picUrl")]
    pub cover_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Song {
    pub id: u64,
    pub name: String,
    #[serde(default, alias = "ar", alias = "artists")]
    pub artists: Vec<ArtistRef>,
    #[serde(default, alias = "al", alias = "album")]
    pub album: AlbumRef,
    #[serde(default, alias = "dt", alias = "duration")]
    pub duration_ms: u32,
    #[serde(default)]
    pub local_path: Option<String>,
}

impl Song {
    pub fn artist_line(&self) -> String {
        let mut out = String::new();
        for (index, artist) in self.artists.iter().enumerate() {
            if index != 0 {
                out.push_str(" / ");
            }
            out.push_str(&artist.name);
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    #[serde(default, alias = "coverImgUrl", alias = "picUrl")]
    pub cover_url: String,
    #[serde(default, alias = "trackCount")]
    pub track_count: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tracks: Vec<Song>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artist {
    pub id: u64,
    pub name: String,
    #[serde(default, alias = "cover", alias = "picUrl")]
    pub cover_url: String,
    #[serde(default, alias = "briefDesc")]
    pub brief_desc: String,
    #[serde(default)]
    pub songs: Vec<Song>,
    #[serde(default, alias = "hotAlbums")]
    pub albums: Vec<AlbumRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(alias = "userId")]
    pub user_id: u64,
    #[serde(default)]
    pub nickname: String,
    #[serde(default, alias = "avatarUrl")]
    pub avatar_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub cookie: String,
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub saved_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QrLogin {
    pub key: String,
    pub url: String,
    pub status: QrStatus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QrStatus {
    #[default]
    Idle,
    WaitingScan,
    WaitingConfirm,
    Authorized,
    Expired,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub query: String,
    pub songs: Vec<Song>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<Playlist>,
}
