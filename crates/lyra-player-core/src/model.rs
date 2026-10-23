use alloc::{format, string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// The module-private directory on the device (`ctx->fs` resolves relative
/// names below it; raw firmware calls and LVGL need the absolute path).
pub const ROOT: &str = "/data/canopus/m.lyra_player";

/// Which optional files a track carries next to `audio.mp3`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Assets(pub u8);

impl Assets {
    pub const COVER: u8 = 1;
    pub const LYRICS: u8 = 2;

    pub const fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// An sRGB colour, `0xRRGGBB`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rgb(pub u32);

impl Rgb {
    pub const fn r(self) -> u32 {
        (self.0 >> 16) & 0xFF
    }
    pub const fn g(self) -> u32 {
        (self.0 >> 8) & 0xFF
    }
    pub const fn b(self) -> u32 {
        self.0 & 0xFF
    }

    /// Linear blend towards `other`; `amount` is 0..=256.
    pub const fn mix(self, other: Rgb, amount: u32) -> Rgb {
        let keep = 256 - amount;
        let r = (self.r() * keep + other.r() * amount) >> 8;
        let g = (self.g() * keep + other.g() * amount) >> 8;
        let b = (self.b() * keep + other.b() * amount) >> 8;
        Rgb((r << 16) | (g << 8) | b)
    }
}

/// The two colours the importer picked from the cover; the player fades from
/// `top` to `bottom` behind everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    pub top: Rgb,
    pub bottom: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            top: Rgb(0x3A3F58),
            bottom: Rgb(0x14161F),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Song {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: u32,
    #[serde(default)]
    pub assets: Assets,
    #[serde(default)]
    pub palette: Palette,
}

impl Song {
    pub fn artist_line(&self) -> String {
        let mut out = String::new();
        for (index, artist) in self.artists.iter().enumerate() {
            if index != 0 {
                out.push_str(" / ");
            }
            out.push_str(artist);
        }
        out
    }

    pub fn dir(&self) -> String {
        track_dir(self.id)
    }

    pub fn audio_path(&self) -> String {
        format!("{ROOT}/tracks/{}/audio.mp3", self.id)
    }

    pub fn cover_path(&self) -> Option<String> {
        self.assets
            .has(Assets::COVER)
            .then(|| format!("{ROOT}/tracks/{}/cover.bin", self.id))
    }

    /// The list artwork the watch derives from the cover (see [`crate::thumbnail`]), relative to
    /// the module directory.
    pub fn thumb_file(&self) -> Option<String> {
        self.assets
            .has(Assets::COVER)
            .then(|| format!("tracks/{}/{THUMB_FILE}", self.id))
    }

    pub fn lyrics_path(&self) -> Option<String> {
        self.assets
            .has(Assets::LYRICS)
            .then(|| format!("{ROOT}/tracks/{}/lyrics.lrc", self.id))
    }
}

pub fn track_dir(id: u64) -> String {
    format!("{ROOT}/tracks/{id}")
}

/// Cover edge length in pixels; the importer produces exactly this size.
pub const COVER_SIZE: u16 = 176;

/// The watch-made list artwork next to `cover.bin`; never sent by the phone.
pub const THUMB_FILE: &str = "thumb.bin";
