//! List artwork: an area-averaged copy of a track's cover with its own rounded corners.
//!
//! The watch builds it the first time a row needs it, at the row size of its own panel, so
//! covers imported before thumbnails existed get one too and the phone sends nothing extra.
//! Both files are LVGL v9 ARGB8888 BINs (12-byte header, then BGRA rows).

use alloc::vec::Vec;

pub const HEADER_BYTES: usize = 12;
const MAGIC: u8 = 0x19;
const ARGB8888: u8 = 0x10;

/// Size of a square thumbnail file.
pub const fn file_bytes(edge: u16) -> usize {
    HEADER_BYTES + edge as usize * edge as usize * 4
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u16,
    pub height: u16,
    /// Bytes per source row (at least `width * 4`).
    pub stride: u16,
}

/// Reads an ARGB8888 BIN header; anything else (other colour formats, empty images) is `None`.
pub fn parse_header(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < HEADER_BYTES || bytes[0] != MAGIC || bytes[1] != ARGB8888 {
        return None;
    }
    let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let header = Header {
        width: word(4),
        height: word(6),
        stride: word(8),
    };
    (header.width > 0
        && header.height > 0
        && usize::from(header.stride) >= usize::from(header.width) * 4)
        .then_some(header)
}

fn encode_header(edge: u16, out: &mut Vec<u8>) {
    out.extend_from_slice(&[MAGIC, ARGB8888, 0, 0]);
    out.extend_from_slice(&edge.to_le_bytes());
    out.extend_from_slice(&edge.to_le_bytes());
    out.extend_from_slice(&(edge * 4).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
}

/// Shrinks the cover described by `header` to an `edge`-pixel square with corners of `radius`
/// pixels. `read_row` fills one source row (`header.stride` bytes) per call, top to bottom, and
/// returns false when the file ends early. Only reductions are supported.
///
/// The source is streamed: only two output rows of sums are held, never the whole cover.
pub fn build(
    header: Header,
    edge: u16,
    radius: u16,
    mut read_row: impl FnMut(&mut [u8]) -> bool,
) -> Option<Vec<u8>> {
    let (width, height, n) = (
        u64::from(header.width),
        u64::from(header.height),
        u64::from(edge),
    );
    if edge == 0 || n > width || n > height {
        return None;
    }
    // In "fine" units a source pixel spans `n` and an output pixel spans the source size, so
    // every overlap is an exact integer and an output pixel's weights sum to width * height.
    let cells = usize::from(edge);
    let mut sums = [alloc::vec![[0u64; 4]; cells], alloc::vec![[0u64; 4]; cells]];
    let mut row = alloc::vec![0u8; usize::from(header.stride)];
    let mut out = Vec::with_capacity(file_bytes(edge));
    encode_header(edge, &mut out);
    let mut out_row = 0u64;
    for y in 0..height {
        if !read_row(&mut row) {
            return None;
        }
        let (top, bottom) = (y * n, y * n + n);
        let split = (out_row + 1) * height;
        let (wy_here, wy_next) = if bottom <= split {
            (n, 0)
        } else {
            (split - top, bottom - split)
        };
        for x in 0..width {
            let pixel = &row[x as usize * 4..x as usize * 4 + 4];
            let alpha = u64::from(pixel[3]);
            if alpha == 0 {
                continue;
            }
            let premultiplied = [
                u64::from(pixel[0]) * alpha,
                u64::from(pixel[1]) * alpha,
                u64::from(pixel[2]) * alpha,
                alpha,
            ];
            let (left, right) = (x * n, x * n + n);
            let column = left / width;
            let column_split = (column + 1) * width;
            let (wx_here, wx_next) = if right <= column_split {
                (n, 0)
            } else {
                (column_split - left, right - column_split)
            };
            for (slot, wy) in [(0, wy_here), (1, wy_next)] {
                for (offset, wx) in [(0, wx_here), (1, wx_next)] {
                    let weight = wx * wy;
                    if weight == 0 {
                        continue;
                    }
                    let cell = &mut sums[slot][column as usize + offset];
                    for channel in 0..4 {
                        cell[channel] += premultiplied[channel] * weight;
                    }
                }
            }
        }
        if bottom >= split {
            emit_row(
                &sums[0],
                out_row as u16,
                edge,
                radius,
                width * height,
                &mut out,
            );
            sums.swap(0, 1);
            sums[1].iter_mut().for_each(|cell| *cell = [0; 4]);
            out_row += 1;
        }
    }
    (out_row == n).then_some(out)
}

fn emit_row(sums: &[[u64; 4]], y: u16, edge: u16, radius: u16, total: u64, out: &mut Vec<u8>) {
    for (x, cell) in sums.iter().enumerate() {
        let coverage = corner_coverage(x as u16, y, edge, radius);
        let alpha = (cell[3] + total / 2) / total * coverage / 255;
        if alpha == 0 || cell[3] == 0 {
            out.extend_from_slice(&[0; 4]);
            continue;
        }
        let channel = |index: usize| ((cell[index] + cell[3] / 2) / cell[3]).min(255) as u8;
        out.extend_from_slice(&[channel(0), channel(1), channel(2), alpha.min(255) as u8]);
    }
}

/// How much of pixel (`x`, `y`) lies inside the rounded square, 0..=255, with a one-pixel
/// anti-aliased rim.
fn corner_coverage(x: u16, y: u16, edge: u16, radius: u16) -> u64 {
    // Work in 1/16 pixel, at the pixel centre.
    let (px, py) = (i64::from(x) * 16 + 8, i64::from(y) * 16 + 8);
    let (size, r) = (i64::from(edge) * 16, i64::from(radius) * 16);
    let reach = |p: i64| {
        if p < r {
            r - p
        } else if p > size - r {
            p - (size - r)
        } else {
            0
        }
    };
    let (dx, dy) = (reach(px), reach(py));
    if dx == 0 || dy == 0 {
        return 255;
    }
    let distance = isqrt((dx * dx + dy * dy) as u64) as i64;
    ((r - distance + 8).clamp(0, 16) * 255 / 16) as u64
}

fn isqrt(value: u64) -> u64 {
    let (mut low, mut high) = (0u64, 1u64 << 32);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(width: u16, height: u16, pixel: impl Fn(u16, u16) -> [u8; 4]) -> Vec<u8> {
        let mut bytes = alloc::vec![MAGIC, ARGB8888, 0, 0];
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&(width * 4).to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        for y in 0..height {
            for x in 0..width {
                bytes.extend_from_slice(&pixel(x, y));
            }
        }
        bytes
    }

    fn shrink(bytes: &[u8], edge: u16, radius: u16) -> Option<Vec<u8>> {
        let header = parse_header(bytes)?;
        let mut rows = bytes[HEADER_BYTES..].chunks(usize::from(header.stride));
        build(header, edge, radius, |row| match rows.next() {
            Some(source) if source.len() == row.len() => {
                row.copy_from_slice(source);
                true
            }
            _ => false,
        })
    }

    fn pixel(thumb: &[u8], edge: u16, x: u16, y: u16) -> [u8; 4] {
        let at = HEADER_BYTES + (usize::from(y) * usize::from(edge) + usize::from(x)) * 4;
        thumb[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_flat_cover_stays_flat_inside_and_gets_round_corners() {
        let source = cover(176, 176, |_, _| [10, 120, 250, 255]);
        let thumb = shrink(&source, 52, 8).unwrap();
        assert_eq!(thumb.len(), file_bytes(52));
        assert_eq!(
            parse_header(&thumb),
            Some(Header {
                width: 52,
                height: 52,
                stride: 208
            })
        );
        assert_eq!(pixel(&thumb, 52, 26, 26), [10, 120, 250, 255]);
        assert_eq!(pixel(&thumb, 52, 26, 0), [10, 120, 250, 255]);
        assert_eq!(pixel(&thumb, 52, 0, 0)[3], 0);
        assert_eq!(pixel(&thumb, 52, 51, 51)[3], 0);
        let rim = pixel(&thumb, 52, 1, 3)[3];
        assert!(rim > 0 && rim < 255, "anti-aliased rim, got {rim}");
    }

    #[test]
    fn halves_average_and_transparent_pixels_do_not_darken() {
        // Left half red, right half blue; a transparent column must not pull colours to black.
        let source = cover(176, 176, |x, _| match x {
            0..=87 => [0, 0, 255, 255],
            88 => [0, 0, 0, 0],
            _ => [255, 0, 0, 255],
        });
        let thumb = shrink(&source, 44, 0).unwrap();
        assert_eq!(pixel(&thumb, 44, 10, 20), [0, 0, 255, 255]);
        assert_eq!(pixel(&thumb, 44, 30, 20), [255, 0, 0, 255]);
        // Output column 22 covers source columns 88..=91: one transparent, three blue.
        let edge_pixel = pixel(&thumb, 44, 22, 20);
        assert_eq!(&edge_pixel[..3], &[255, 0, 0]);
        assert!((190..=192).contains(&edge_pixel[3]));
    }

    #[test]
    fn non_integer_ratios_cover_every_output_row() {
        for edge in [52u16, 60, 64, 176] {
            let source = cover(176, 176, |x, y| [(x % 256) as u8, (y % 256) as u8, 7, 255]);
            let thumb = shrink(&source, edge, 0).unwrap();
            assert_eq!(thumb.len(), file_bytes(edge));
            // A horizontal ramp stays monotonic.
            let first = pixel(&thumb, edge, 0, edge / 2)[0];
            let last = pixel(&thumb, edge, edge - 1, edge / 2)[0];
            assert!(first < 4 && last > 170, "{edge}: {first}..{last}");
        }
    }

    #[test]
    fn rejects_other_formats_short_files_and_enlargement() {
        let mut rgb565 = cover(4, 4, |_, _| [0; 4]);
        rgb565[1] = 0x12;
        assert!(parse_header(&rgb565).is_none());
        let small = cover(32, 32, |_, _| [1, 2, 3, 255]);
        assert!(shrink(&small, 52, 8).is_none());
        let full = cover(176, 176, |_, _| [1, 2, 3, 255]);
        assert!(shrink(&full[..full.len() - 704], 52, 8).is_none());
    }
}
