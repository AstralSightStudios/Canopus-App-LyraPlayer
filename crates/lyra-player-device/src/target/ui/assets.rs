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
    Shuffle = "shuffle",
    Once = "once",
    Minus = "minus",
    Plus = "plus",
    Note = "note",
}

const PREFIX: &str = "/data/canopus/m.lyra_player/";

impl Glyph {
    fn entry(self) -> &'static (Glyph, &'static str, &'static [u8]) {
        &ALL[self as usize]
    }

    /// Absolute NUL-terminated path for `lv_image_set_src`.
    pub fn path(self) -> &'static [u8] {
        static mut PATHS: [[u8; 64]; 11] = [[0; 64]; 11];
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

/// The launcher draws `appicon.bin` as is: a 117x117 LVGL ARGB8888 image.
const ICON_SIZE: usize = 117;
const ICON_FILE: &str = "appicon.bin";

fn mix(a: u32, b: u32, t: u32) -> u32 {
    (a * (255 - t) + b * t) / 255
}

/// A coral-to-violet disc with the white music note, drawn per pixel so the module needs
/// no bitmap for it (the note is the 48 px glyph, scaled up bilinearly).
fn icon_pixel(x: usize, y: usize, note: &[u8]) -> [u8; 4] {
    let centre = (ICON_SIZE as i32 - 1) * 8 / 2; // 1/16 pixel units
    let (px, py) = (x as i32 * 16 - centre, y as i32 * 16 - centre);
    let radius = (ICON_SIZE as i32 / 2) * 16 - 8;
    // Squared distance in 1/16 px; the anti-aliased rim is two pixels wide.
    let distance = isqrt((px * px + py * py) as u32) as i32;
    let coverage = ((radius - distance + 16).clamp(0, 32) * 255 / 32) as u32;
    if coverage == 0 {
        return [0; 4];
    }
    let t = ((x + y) * 255 / (2 * ICON_SIZE)) as u32;
    let (r, g, b) = (mix(0xFF, 0x7C, t), mix(0x6B, 0x5C, t), mix(0x8A, 0xFF, t));
    // The note, 72 px square at the centre, sampled from the 48 px glyph.
    let size = 72usize;
    let origin = (ICON_SIZE - size) / 2;
    let mut ink = 0u32;
    if (origin..origin + size).contains(&x) && (origin..origin + size).contains(&y) {
        let (u, v) = ((x - origin) * 47 * 16 / (size - 1), (y - origin) * 47 * 16 / (size - 1));
        let (u0, v0, fu, fv) = (u / 16, v / 16, (u % 16) as u32, (v % 16) as u32);
        let alpha = |col: usize, row: usize| u32::from(note[12 + (row.min(47) * 48 + col.min(47)) * 4 + 3]);
        let top = alpha(u0, v0) * (16 - fu) + alpha(u0 + 1, v0) * fu;
        let bottom = alpha(u0, v0 + 1) * (16 - fu) + alpha(u0 + 1, v0 + 1) * fu;
        ink = (top * (16 - fv) + bottom * fv) / 256;
    }
    let (r, g, b) = (mix(r, 255, ink), mix(g, 255, ink), mix(b, 255, ink));
    [b as u8, g as u8, r as u8, coverage as u8]
}

fn isqrt(value: u32) -> u32 {
    let (mut low, mut high) = (0u32, 65_536u32);
    while low + 1 < high {
        let middle = (low + high) / 2;
        if middle * middle <= value {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

/// Writes `appicon.bin` once; the launcher reads it by path when the entry is published.
pub fn ensure_launcher_icon() {
    let ctx = context();
    if ctx.is_null() {
        return;
    }
    let expected = 12 + ICON_SIZE * ICON_SIZE * 4;
    if fs::stat(ctx, ICON_FILE).ok().map(|stat| stat.size as usize) == Some(expected) {
        return;
    }
    let Ok(mut file) = fs::File::open(ctx, ICON_FILE, canopus_abi::FS_WRITE | canopus_abi::FS_CREATE | canopus_abi::FS_TRUNC)
    else {
        return;
    };
    let note = ALL[Glyph::Note as usize].2;
    let mut header = [0u8; 12];
    header[..4].copy_from_slice(&[0x19, 0x10, 0, 0]);
    header[4..6].copy_from_slice(&(ICON_SIZE as u16).to_le_bytes());
    header[6..8].copy_from_slice(&(ICON_SIZE as u16).to_le_bytes());
    header[8..10].copy_from_slice(&((ICON_SIZE * 4) as u16).to_le_bytes());
    if file.write(&header).is_err() {
        return;
    }
    let mut row = [0u8; ICON_SIZE * 4];
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            row[x * 4..x * 4 + 4].copy_from_slice(&icon_pixel(x, y, note));
        }
        if file.write(&row).is_err() {
            return;
        }
    }
    let _ = file.sync();
}
