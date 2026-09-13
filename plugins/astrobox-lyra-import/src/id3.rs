//! Reads the title, artist and album an MP3 carries in its ID3 tags, so a
//! picked file can prefill the import form even when the host supplies no
//! usable file name (Android content URIs).

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// Text frames sit near the start of a tag; anything past this is almost
/// always embedded artwork, which is not worth holding in memory.
const MAX_TAG_BYTES: u64 = 4 * 1024 * 1024;
const ID3V1_BYTES: i64 = 128;
/// How ID3v2.4 separates multiple values inside one text frame, joined the way
/// the watch player joins artists.
const VALUE_SEPARATOR: &str = " / ";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl TrackTags {
    fn merge_missing(&mut self, other: TrackTags) {
        self.title = self.title.take().or(other.title);
        self.artist = self.artist.take().or(other.artist);
        self.album = self.album.take().or(other.album);
    }

    fn is_complete(&self) -> bool {
        self.title.is_some() && self.artist.is_some() && self.album.is_some()
    }
}

/// Tags found in the file; unreadable or untagged files yield empty tags.
pub fn read_tags(path: &Path) -> TrackTags {
    let Ok(mut file) = File::open(path) else {
        return TrackTags::default();
    };
    let mut tags = read_id3v2(&mut file).unwrap_or_default();
    if !tags.is_complete()
        && let Some(v1) = read_id3v1(&mut file)
    {
        tags.merge_missing(v1);
    }
    tags
}

fn read_id3v2(file: &mut File) -> Option<TrackTags> {
    let mut header = [0u8; 10];
    file.read_exact(&mut header).ok()?;
    if &header[..3] != b"ID3" || header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    let size = syncsafe(&header[6..10]);
    let mut tag = Vec::new();
    file.take(size.min(MAX_TAG_BYTES))
        .read_to_end(&mut tag)
        .ok()?;
    Some(parse_id3v2(header[3], header[5], tag))
}

/// Parses the body of an ID3v2 tag (everything after the 10-byte header).
fn parse_id3v2(version: u8, flags: u8, mut tag: Vec<u8>) -> TrackTags {
    let mut tags = TrackTags::default();
    if !(2..=4).contains(&version) {
        return tags;
    }
    // Before v2.4, unsynchronisation applies to the whole tag at once.
    if flags & 0x80 != 0 && version < 4 {
        tag = resynchronise(&tag);
    }
    let mut offset = 0usize;
    if flags & 0x40 != 0 && version >= 3 {
        let Some(size) = tag.get(..4) else {
            return tags;
        };
        // v2.3 counts only the bytes after its size field; v2.4 counts all.
        offset = if version == 3 {
            4 + u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize
        } else {
            syncsafe(size) as usize
        };
    }

    let (id_len, header_len) = if version == 2 { (3, 6) } else { (4, 10) };
    while let Some(header) = tag.get(offset..offset + header_len) {
        if header[0] == 0 {
            break; // padding
        }
        let id = &header[..id_len];
        let size = match version {
            2 => u32::from_be_bytes([0, header[3], header[4], header[5]]) as usize,
            3 => u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize,
            // Some v2.4 writers store plain big-endian sizes; a byte with its
            // high bit set cannot be syncsafe, so read those as big-endian.
            _ if header[4..8].iter().any(|byte| byte & 0x80 != 0) => {
                u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize
            }
            _ => syncsafe(&header[4..8]) as usize,
        };
        let start = offset + header_len;
        let Some(body) = tag.get(start..start.saturating_add(size)) else {
            break; // truncated by MAX_TAG_BYTES or malformed
        };
        offset = start + size;

        let field = match id {
            b"TIT2" | b"TT2" => &mut tags.title,
            b"TPE1" | b"TP1" => &mut tags.artist,
            b"TALB" | b"TAL" => &mut tags.album,
            _ => continue,
        };
        if field.is_some() {
            continue;
        }
        let Some(body) = frame_body(version, header, body) else {
            continue;
        };
        *field = decode_text_frame(&body);
    }
    tags
}

/// The frame payload with its per-frame transforms undone, or `None` for
/// compressed or encrypted frames.
fn frame_body(version: u8, header: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    match version {
        3 => {
            let format = header[9];
            if format & 0xC0 != 0 {
                return None;
            }
            // A grouping identity byte precedes the text.
            let skip = usize::from(format & 0x20 != 0);
            body.get(skip..).map(<[u8]>::to_vec)
        }
        4 => {
            let format = header[9];
            if format & 0x0C != 0 {
                return None;
            }
            // Grouping identity (1 byte), then data length indicator (4 bytes).
            let skip = usize::from(format & 0x40 != 0) + if format & 0x01 != 0 { 4 } else { 0 };
            let body = body.get(skip..)?;
            Some(if format & 0x02 != 0 {
                resynchronise(body)
            } else {
                body.to_vec()
            })
        }
        _ => Some(body.to_vec()),
    }
}

fn decode_text_frame(body: &[u8]) -> Option<String> {
    let (&encoding, text) = body.split_first()?;
    let values: Vec<String> = match encoding {
        0 => split_terminated(text, 1)
            .into_iter()
            .map(latin1_or_utf8)
            .collect::<Option<_>>()?,
        1 | 2 => split_terminated(text, 2)
            .into_iter()
            .map(|value| utf16(value, encoding == 2))
            .collect::<Option<_>>()?,
        3 => split_terminated(text, 1)
            .into_iter()
            .map(|value| String::from_utf8(value.to_vec()).ok())
            .collect::<Option<_>>()?,
        _ => return None,
    };
    let values: Vec<&str> = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    (!values.is_empty()).then(|| values.join(VALUE_SEPARATOR))
}

/// Splits on NUL terminators of `unit` bytes, aligned to the unit size.
fn split_terminated(text: &[u8], unit: usize) -> Vec<&[u8]> {
    let mut values = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index + unit <= text.len() {
        if text[index..index + unit].iter().all(|byte| *byte == 0) {
            values.push(&text[start..index]);
            start = index + unit;
        }
        index += unit;
    }
    values.push(&text[start..]);
    values
}

/// ISO-8859-1 frames written by Chinese taggers often hold UTF-8 or GBK. UTF-8
/// is decoded as such; other non-ASCII bytes cannot be told apart from GBK, so
/// the value is dropped rather than shown as mojibake.
fn latin1_or_utf8(bytes: &[u8]) -> Option<String> {
    if bytes.is_ascii() {
        return Some(bytes.iter().map(|byte| char::from(*byte)).collect());
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn utf16(bytes: &[u8], default_big_endian: bool) -> Option<String> {
    let (big_endian, bytes) = match bytes {
        [0xFE, 0xFF, rest @ ..] => (true, rest),
        [0xFF, 0xFE, rest @ ..] => (false, rest),
        _ => (default_big_endian, bytes),
    };
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16(&units).ok()
}

fn read_id3v1(file: &mut File) -> Option<TrackTags> {
    file.seek(SeekFrom::End(-ID3V1_BYTES)).ok()?;
    let mut block = [0u8; ID3V1_BYTES as usize];
    file.read_exact(&mut block).ok()?;
    parse_id3v1(&block)
}

fn parse_id3v1(block: &[u8; ID3V1_BYTES as usize]) -> Option<TrackTags> {
    if &block[..3] != b"TAG" {
        return None;
    }
    let field = |bytes: &[u8]| {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        latin1_or_utf8(&bytes[..end])
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Some(TrackTags {
        title: field(&block[3..33]),
        artist: field(&block[33..63]),
        album: field(&block[63..93]),
    })
}

fn syncsafe(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |size, byte| (size << 7) | u64::from(byte & 0x7F))
}

/// Undoes ID3 unsynchronisation: every `FF 00` pair was written for a lone `FF`.
fn resynchronise(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        output.push(bytes[index]);
        if bytes[index] == 0xFF && bytes.get(index + 1) == Some(&0) {
            index += 1;
        }
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn syncsafe_bytes(size: usize) -> [u8; 4] {
        let size = size as u32;
        [3, 2, 1, 0].map(|shift| ((size >> (shift * 7)) & 0x7F) as u8)
    }

    fn frame(version: u8, id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        match version {
            2 => out.extend(&(body.len() as u32).to_be_bytes()[1..]),
            3 => out.extend((body.len() as u32).to_be_bytes()),
            _ => out.extend(syncsafe_bytes(body.len())),
        }
        if version != 2 {
            out.extend([0, 0]);
        }
        out.extend(body);
        out
    }

    fn text(encoding: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![encoding];
        out.extend(bytes);
        out
    }

    fn utf16le_with_bom(value: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        out.extend(value.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    fn tag(version: u8, frames: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = frames.concat();
        let mut out = vec![b'I', b'D', b'3', version, 0, 0];
        out.extend(syncsafe_bytes(body.len() + 32));
        out.extend(body);
        out.extend([0u8; 32]); // padding
        out
    }

    fn read_bytes(bytes: &[u8]) -> TrackTags {
        let directory = crate::artwork::unique_output_directory("id3-test");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("audio%3A1000012345");
        fs::write(&path, bytes).unwrap();
        let tags = read_tags(&path);
        let _ = fs::remove_dir_all(directory);
        tags
    }

    #[test]
    fn reads_v24_utf8_with_multiple_artists() {
        let tags = read_bytes(&tag(
            4,
            &[
                frame(4, b"TIT2", &text(3, "晴天".as_bytes())),
                frame(4, b"TPE1", &text(3, "周杰伦\0方文山".as_bytes())),
                frame(4, b"TALB", &text(3, "叶惠美\0".as_bytes())),
            ],
        ));
        assert_eq!(tags.title.as_deref(), Some("晴天"));
        assert_eq!(tags.artist.as_deref(), Some("周杰伦 / 方文山"));
        assert_eq!(tags.album.as_deref(), Some("叶惠美"));
    }

    #[test]
    fn reads_v23_utf16_and_skips_artwork_frames() {
        let artwork = frame(3, b"APIC", &vec![0xFF; 70_000]);
        let tags = read_bytes(&tag(
            3,
            &[
                artwork,
                frame(3, b"TIT2", &text(1, &utf16le_with_bom("七里香"))),
                frame(3, b"TPE1", &text(1, &utf16le_with_bom("周杰伦"))),
            ],
        ));
        assert_eq!(tags.title.as_deref(), Some("七里香"));
        assert_eq!(tags.artist.as_deref(), Some("周杰伦"));
        assert_eq!(tags.album, None);
    }

    #[test]
    fn reads_v22_three_letter_frames() {
        let tags = read_bytes(&tag(
            2,
            &[
                frame(2, b"TT2", &text(0, b"Yesterday")),
                frame(2, b"TP1", &text(0, b"The Beatles")),
            ],
        ));
        assert_eq!(tags.title.as_deref(), Some("Yesterday"));
        assert_eq!(tags.artist.as_deref(), Some("The Beatles"));
    }

    #[test]
    fn latin1_frames_accept_utf8_but_drop_unknown_codepages() {
        assert_eq!(latin1_or_utf8("稻香".as_bytes()).as_deref(), Some("稻香"));
        // "稻香" in GBK: not UTF-8, and not trustworthy as Latin-1 either.
        assert_eq!(latin1_or_utf8(&[0xB5, 0xBE, 0xCF, 0xE3]), None);
    }

    #[test]
    fn falls_back_to_id3v1_for_missing_fields() {
        let mut bytes = tag(3, &[frame(3, b"TIT2", &text(0, b"Title v2"))]);
        bytes.extend(vec![0u8; 4096]);
        let mut v1 = [0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..11].copy_from_slice(b"Title v1");
        v1[33..42].copy_from_slice(b"Artist v1");
        v1[63..71].copy_from_slice(b"Album v1");
        bytes.extend(v1);
        let tags = read_bytes(&bytes);
        assert_eq!(tags.title.as_deref(), Some("Title v2"));
        assert_eq!(tags.artist.as_deref(), Some("Artist v1"));
        assert_eq!(tags.album.as_deref(), Some("Album v1"));
    }

    #[test]
    fn untagged_and_malformed_files_yield_nothing() {
        assert_eq!(read_bytes(&[0xFF, 0xFB, 0x90, 0x64]), TrackTags::default());
        // A frame claiming more bytes than the tag holds.
        let mut truncated = tag(3, &[]);
        truncated.splice(10..10, *b"TIT2\x00\x10\x00\x00\x00\x00\x03");
        assert_eq!(read_bytes(&truncated), TrackTags::default());
        assert_eq!(
            parse_id3v2(4, 0, frame(4, b"TIT2", &text(9, b"bad encoding"))),
            TrackTags::default()
        );
    }

    #[test]
    fn unsynchronised_v23_tag_is_restored() {
        let body = [
            frame(3, b"TIT2", &text(1, &utf16le_with_bom("ÿ标题"))),
            frame(3, b"TPE1", &text(0, b"Artist")),
        ]
        .concat();
        let mut unsynced = Vec::new();
        for byte in &body {
            unsynced.push(*byte);
            if *byte == 0xFF {
                unsynced.push(0);
            }
        }
        // Without resynchronisation the inserted bytes would shift the second
        // frame out of place.
        let tags = parse_id3v2(3, 0x80, unsynced);
        assert_eq!(tags.title.as_deref(), Some("ÿ标题"));
        assert_eq!(tags.artist.as_deref(), Some("Artist"));
        assert_eq!(
            resynchronise(&[0xFF, 0x00, 0xE0, 0xFF, 0x00]),
            [0xFF, 0xE0, 0xFF]
        );
    }

    #[test]
    fn v24_frame_flags_are_honoured() {
        // Data length indicator plus grouping byte ahead of the text.
        let mut body = vec![0x07, 0, 0, 0, 5];
        body.extend(text(3, b"Song"));
        let mut grouped = frame(4, b"TIT2", &body);
        grouped[9] = 0x41;
        // A compressed frame is skipped, not misread.
        let mut compressed = frame(4, b"TPE1", &text(3, b"zlib"));
        compressed[9] = 0x09;
        let tags = parse_id3v2(4, 0, [grouped, compressed].concat());
        assert_eq!(tags.title.as_deref(), Some("Song"));
        assert_eq!(tags.artist, None);
    }
}
