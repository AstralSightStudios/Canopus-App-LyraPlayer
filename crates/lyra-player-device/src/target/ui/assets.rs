//! The player's icon glyphs: white-on-transparent LVGL ARGB8888 bitmaps built
//! from `assets/glyphs/*.svg` by `scripts/build-glyphs.py`. The package
//! installer does not deliver module resources, so the module carries them
//! and writes them into its own directory the first time a page is built.

use canopus_runtime::fs;

use crate::module::context;

/// Keeps the embedded bitmaps word aligned, so the 28 px headers cannot straddle two words and
/// read as ROM addresses to the module verifier.
#[repr(C)]
struct Aligned<Align, Bytes: ?Sized> {
    _align: [Align; 0],
    bytes: Bytes,
}

macro_rules! glyphs {
    ($($name:ident = $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Glyph { $($name),* }

        const ALL: &[(Glyph, &str, &[u8])] = &[
            $((Glyph::$name, concat!("glyph-", $file, ".bin"),
               {
                   const BYTES: &Aligned<u32, [u8]> = &Aligned {
                       _align: [],
                       bytes: *include_bytes!(concat!("../../../assets/glyph-", $file, ".bin")),
                   };
                   &BYTES.bytes
               })),*
        ];
    };
}

glyphs! {
    Play = "play",
    Pause = "pause",
    Prev = "prev",
    Next = "next",
    Back = "back",
    Repeat = "repeat",
    RepeatOne = "repeat-one",
    Shuffle = "shuffle",
    Once = "once",
    Minus = "minus",
    Plus = "plus",
    Note = "note",
    PlaySmall = "play-s",
    PauseSmall = "pause-s",
    NoteSmall = "note-s",
}

const PREFIX: &str = "/data/canopus/m.lyra_player/";

impl Glyph {
    fn entry(self) -> &'static (Glyph, &'static str, &'static [u8]) {
        &ALL[self as usize]
    }

    /// Absolute NUL-terminated path for `lv_image_set_src`.
    pub fn path(self) -> &'static [u8] {
        static mut PATHS: [[u8; 64]; ALL.len()] = [[0; 64]; ALL.len()];
        let index = self as usize;
        // SAFETY: filled once on the UI thread before any page can use it.
        let slot = unsafe { &mut (*core::ptr::addr_of_mut!(PATHS))[index] };
        if slot[0] == 0 {
            let name = self.entry().1;
            slot[..PREFIX.len()].copy_from_slice(PREFIX.as_bytes());
            slot[PREFIX.len()..PREFIX.len() + name.len()].copy_from_slice(name.as_bytes());
        }
        &slot[..PREFIX.len() + self.entry().1.len() + 1]
    }
}

/// Writes every glyph that is missing or has the wrong size.
pub fn ensure() {
    let ctx = context();
    if ctx.is_null() {
        return;
    }
    for (_, name, data) in ALL {
        let current = fs::stat(ctx, name).ok().map(|stat| stat.size as usize);
        if current != Some(data.len()) {
            let _ = fs::write_all(ctx, name, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Launcher icon
// ---------------------------------------------------------------------------

/// The launcher draws `appicon.bin` as is: a 117x117 LVGL ARGB8888 image. This one is the artwork
/// (`assets/appicon.bin`, drawn by hand), embedded like the glyphs.
static ICON: &Aligned<u32, [u8]> = &Aligned {
    _align: [],
    bytes: *include_bytes!("../../../assets/appicon.bin"),
};
const ICON_FILE: &str = "appicon.bin";

/// Whether the file at `name` already holds exactly `data`. The icon artwork has the size of the
/// one an earlier version drew in code, so the size alone cannot tell the two apart.
fn file_holds(ctx: *const canopus_abi::CanopusContext, name: &str, data: &[u8]) -> bool {
    let Ok(mut file) = fs::File::open(ctx, name, canopus_abi::FS_READ) else {
        return false;
    };
    let mut chunk = [0u8; 512];
    let mut offset = 0;
    while offset < data.len() {
        let want = chunk.len().min(data.len() - offset);
        match file.read(&mut chunk[..want]) {
            Ok(count) if count > 0 && chunk[..count] == data[offset..offset + count] => offset += count,
            _ => return false,
        }
    }
    // Nothing may follow the icon.
    matches!(file.read(&mut chunk[..1]), Ok(0))
}

/// Writes `appicon.bin` unless the file already is the current artwork; the launcher reads it by
/// path when the entry is published.
pub fn ensure_launcher_icon() {
    let ctx = context();
    if ctx.is_null() {
        return;
    }
    let icon = &ICON.bytes;
    if file_holds(ctx, ICON_FILE, icon) {
        return;
    }
    let _ = fs::write_all(ctx, ICON_FILE, icon);
}
