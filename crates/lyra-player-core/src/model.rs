use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    #[serde(default)]
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRef {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default, alias = "coverUrl")]
    pub cover_url: String,
    #[serde(default, alias = "backgroundUrl")]
    pub background_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Song {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album: AlbumRef,
    #[serde(default, alias = "durationMs")]
    pub duration_ms: u32,
    #[serde(default, alias = "audioPath")]
    pub local_path: Option<String>,
    #[serde(default, alias = "lyricsPath")]
    pub lyrics_path: Option<String>,
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
