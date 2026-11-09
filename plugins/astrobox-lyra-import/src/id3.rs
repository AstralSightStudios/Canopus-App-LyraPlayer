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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedCover {
    pub bytes: Vec<u8>,
    pub kind: crate::sniff::ImageFormat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedLyrics {
    pub text: String,
    /// Whether the text already carries LRC timestamps.
    pub timed: bool,
}

/// Everything useful an MP3 carries in its ID3v2 tag: text metadata plus the
/// embedded front cover and lyrics, when present.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmbeddedMedia {
    pub tags: TrackTags,
    pub cover: Option<EmbeddedCover>,
    pub lyrics: Option<EmbeddedLyrics>,
}

/// Tags found in the file; unreadable or untagged files yield empty tags.
pub fn read_media(path: &Path) -> EmbeddedMedia {
    let Ok(mut file) = File::open(path) else {
        return EmbeddedMedia::default();
    };
    let mut media = read_id3v2(&mut file).unwrap_or_default();
    if !media.tags.is_complete()
        && let Some(v1) = read_id3v1(&mut file)
    {
        media.tags.merge_missing(v1);
    }
    media
}

#[cfg(test)]
fn read_tags(path: &Path) -> TrackTags {
    read_media(path).tags
}

fn read_id3v2(file: &mut File) -> Option<EmbeddedMedia> {
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
fn parse_id3v2(version: u8, flags: u8, mut tag: Vec<u8>) -> EmbeddedMedia {
    let mut media = EmbeddedMedia::default();
    if !(2..=4).contains(&version) {
        return media;
    }
    // Before v2.4, unsynchronisation applies to the whole tag at once.
    if flags & 0x80 != 0 && version < 4 {
        tag = resynchronise(&tag);
    }
    let mut offset = 0usize;
    if flags & 0x40 != 0 && version >= 3 {
        let Some(size) = tag.get(..4) else {
            return media;
        };
        // v2.3 counts only the bytes after its size field; v2.4 counts all.
        offset = if version == 3 {
            4 + u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize
        } else {
            syncsafe(size) as usize
        };
    }

    let (id_len, header_len) = if version == 2 { (3, 6) } else { (4, 10) };
    let mut cover_is_front = false;
    let mut lyrics_rank = 0u8;
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
            b"TIT2" | b"TT2" => Some(&mut media.tags.title),
            b"TPE1" | b"TP1" => Some(&mut media.tags.artist),
            b"TALB" | b"TAL" => Some(&mut media.tags.album),
            _ => None,
        };
        if let Some(field) = field {
            if field.is_some() {
                continue;
            }
            if let Some(body) = frame_body(version, header, body) {
                *field = decode_text_frame(&body);
            }
            continue;
        }

        let Some(body) = frame_body(version, header, body) else {
            continue;
        };
        match id {
            b"APIC" | b"PIC" => {
                let Some((picture_type, bytes)) = parse_apic(version, &body) else {
                    continue;
                };
                let Some(kind) = crate::sniff::image_format_of(&bytes) else {
                    continue;
                };
                let is_front = picture_type == 3;
                if media.cover.is_none() || (is_front && !cover_is_front) {
                    media.cover = Some(EmbeddedCover { bytes, kind });
                    cover_is_front = is_front;
                }
            }
            b"USLT" | b"ULT" => {
                if lyrics_rank < 1
                    && let Some(lyrics) = parse_uslt(&body)
                {
                    media.lyrics = Some(lyrics);
                    lyrics_rank = 1;
                }
            }
            b"SYLT" => {
                if let Some(lyrics) = parse_sylt(&body) {
                    media.lyrics = Some(lyrics);
                    lyrics_rank = 2;
                }
            }
            _ => {}
        }
    }
    media
}

/// An attached picture frame: `(picture type, image bytes)`.
fn parse_apic(version: u8, body: &[u8]) -> Option<(u8, Vec<u8>)> {
    let (&encoding, rest) = body.split_first()?;
    let (picture_type, rest) = if version == 2 {
        // v2.2 PIC: a fixed three-byte "JPG"/"PNG" format instead of a MIME type.
        let rest = rest.get(3..)?;
        let (&picture_type, rest) = rest.split_first()?;
        (picture_type, rest)
    } else {
        let (mime, rest) = split_once_terminated(rest, 1)?;
        // A picture that is only a link to another file is not embedded art.
        if mime.starts_with(b"-->") {
            return None;
        }
        let (&picture_type, rest) = rest.split_first()?;
        (picture_type, rest)
    };
    let (_, data) = split_once_terminated(rest, text_unit(encoding))?;
    (!data.is_empty()).then(|| (picture_type, data.to_vec()))
}

/// An unsynchronised lyrics frame; timestamps are kept when the text has them.
fn parse_uslt(body: &[u8]) -> Option<EmbeddedLyrics> {
    let (&encoding, rest) = body.split_first()?;
    let rest = rest.get(3..)?; // language
    let (_, text) = split_once_terminated(rest, text_unit(encoding))?;
    let text = decode_text(encoding, text)?;
    let text = text
        .trim_start_matches('\u{feff}')
        .trim_matches('\0')
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    let timed = has_timestamps(&text);
    Some(EmbeddedLyrics { text, timed })
}

/// A synchronised lyrics frame, converted to timed LRC. Only millisecond
/// timestamps are understood; the rarer MPEG-frame variant is skipped.
fn parse_sylt(body: &[u8]) -> Option<EmbeddedLyrics> {
    let (&encoding, rest) = body.split_first()?;
    let rest = rest.get(3..)?; // language
    let (&timestamp_format, rest) = rest.split_first()?;
    let (&_content_type, rest) = rest.split_first()?;
    let (_, mut rest) = split_once_terminated(rest, text_unit(encoding))?;
    if timestamp_format != 2 {
        return None;
    }
    let mut text = String::new();
    while !rest.is_empty() {
        let (line, after) = split_once_terminated(rest, text_unit(encoding))?;
        let stamp = after.get(..4)?;
        rest = after.get(4..)?;
        let millis = u32::from_be_bytes([stamp[0], stamp[1], stamp[2], stamp[3]]);
        let line = decode_text(encoding, line)?;
        let line = line.trim_matches('\0').trim();
        if !line.is_empty() {
            text.push_str(&format!(
                "[{:02}:{:02}.{:02}]{line}\n",
                millis / 60_000,
                millis / 1_000 % 60,
                millis % 1_000 / 10
            ));
        }
    }
    (!text.is_empty()).then_some(EmbeddedLyrics { text, timed: true })
}

/// The terminator width of an encoded string: UTF-16 uses two bytes.
fn text_unit(encoding: u8) -> usize {
    if matches!(encoding, 1 | 2) { 2 } else { 1 }
}

/// Splits at the first NUL terminator of `unit` bytes, aligned to the unit.
fn split_once_terminated(text: &[u8], unit: usize) -> Option<(&[u8], &[u8])> {
    let mut index = 0;
    while index + unit <= text.len() {
        if text[index..index + unit].iter().all(|byte| *byte == 0) {
            return Some((&text[..index], &text[index + unit..]));
        }
        index += unit;
    }
    None
}

fn decode_text(encoding: u8, bytes: &[u8]) -> Option<String> {
    match encoding {
        0 => latin1_or_utf8(trim_nuls(bytes, 1)),
        1 | 2 => utf16(trim_nuls(bytes, 2), encoding == 2),
        3 => String::from_utf8(trim_nuls(bytes, 1).to_vec()).ok(),
        _ => None,
    }
}

fn trim_nuls(bytes: &[u8], unit: usize) -> &[u8] {
    let mut end = bytes.len();
    while end >= unit && bytes[end - unit..end].iter().all(|byte| *byte == 0) {
        end -= unit;
    }
    &bytes[..end]
}

fn has_timestamps(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim_start()
            .strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .is_some_and(|(tag, _)| is_stamp_tag(tag))
    })
}

/// `[mm:ss.xx]` rather than a metadata tag such as `[ti:歌名]`.
fn is_stamp_tag(tag: &str) -> bool {
    let Some((minutes, rest)) = tag.split_once(':') else {
        return false;
    };
    let seconds = rest.split_once('.').map_or(rest, |(seconds, _)| seconds);
    minutes.trim().parse::<u32>().is_ok() && seconds.trim().parse::<u32>().is_ok()
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

/// The part of an MP3 file that holds its audio: leading ID3v2 tags and trailing APEv2 and ID3v1
/// tags are left out. The watch only plays the frames, and a tag with embedded artwork is often
/// larger than a minute of audio: it took that much longer to send, and seconds at the start of
/// every playback to read past.
pub fn audio_range<R: Read + Seek>(file: &mut R) -> std::io::Result<(u64, u64)> {
    let len = file.seek(SeekFrom::End(0))?;
    let mut start = 0u64;
    // Some writers stack several tags.
    loop {
        let mut header = [0u8; 10];
        file.seek(SeekFrom::Start(start))?;
        if read_up_to(file, &mut header)? < header.len()
            || &header[..3] != b"ID3"
            || header[3] == 0xFF
            || header[4] == 0xFF
            || header[6..10].iter().any(|byte| byte & 0x80 != 0)
        {
            break;
        }
        let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
        let next = start + 10 + syncsafe(&header[6..10]) + footer;
        if next >= len {
            // A tag that claims the rest of the file is damaged; send the file as it is.
            return Ok((0, len));
        }
        start = next;
    }
    let mut end = len;
    if end - start >= ID3V1_BYTES as u64 {
        let mut marker = [0u8; 3];
        file.seek(SeekFrom::Start(end - ID3V1_BYTES as u64))?;
        if read_up_to(file, &mut marker)? == marker.len() && &marker == b"TAG" {
            end -= ID3V1_BYTES as u64;
        }
    }
    // An APEv2 footer: "APETAGEX", version, size (footer and items), item count, flags.
    if end - start >= 32 {
        let mut footer = [0u8; 32];
        file.seek(SeekFrom::Start(end - 32))?;
        if read_up_to(file, &mut footer)? == footer.len() && &footer[..8] == b"APETAGEX" {
            let size = u64::from(u32::from_le_bytes([footer[12], footer[13], footer[14], footer[15]]));
            let flags = u32::from_le_bytes([footer[20], footer[21], footer[22], footer[23]]);
            let header = if flags & (1 << 31) != 0 { 32 } else { 0 };
            if size >= 32 && size + header <= end - start {
                end -= size + header;
            }
        }
    }
    Ok((start, end))
}

fn read_up_to<R: Read>(file: &mut R, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    Ok(filled)
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

    fn ape_tag(items: &[u8], with_header: bool) -> Vec<u8> {
        let size = (items.len() + 32) as u32;
        let flags: u32 = if with_header { 1 << 31 } else { 0 };
        let block = |marker_flags: u32| {
            let mut out = b"APETAGEX".to_vec();
            out.extend(2000u32.to_le_bytes());
            out.extend(size.to_le_bytes());
            out.extend(1u32.to_le_bytes());
            out.extend((flags | marker_flags).to_le_bytes());
            out.extend([0u8; 8]);
            out
        };
        let mut out = Vec::new();
        if with_header {
            out.extend(block(1 << 29));
        }
        out.extend(items);
        out.extend(block(0));
        out
    }

    fn range_of(bytes: &[u8]) -> (u64, u64) {
        audio_range(&mut std::io::Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn audio_range_leaves_out_every_tag() {
        let audio = [0xFFu8, 0xFB, 0x90, 0x64, 1, 2, 3, 4, 5, 6];
        let first = tag(4, &[frame(4, b"APIC", &[7u8; 5000])]);
        let second = tag(3, &[frame(3, b"TIT2", &text(0, b"Title"))]);
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        let ape = ape_tag(b"some items", true);
        let file = [first.clone(), second.clone(), audio.to_vec(), ape, v1].concat();
        let start = (first.len() + second.len()) as u64;
        assert_eq!(range_of(&file), (start, start + audio.len() as u64));
    }

    #[test]
    fn audio_range_of_an_untagged_file_is_the_file() {
        let audio = vec![0xFFu8; 4096];
        assert_eq!(range_of(&audio), (0, 4096));
        let ape_without_header = [audio.clone(), ape_tag(b"x", false)].concat();
        assert_eq!(range_of(&ape_without_header), (0, 4096));
    }

    #[test]
    fn audio_range_keeps_a_file_whose_tag_claims_too_much() {
        let mut file = tag(4, &[frame(4, b"TIT2", &text(3, b"x"))]);
        // The size now runs past the end of the file.
        file[6..10].copy_from_slice(&syncsafe_bytes(1 << 20));
        file.extend([0xFF, 0xFB, 0x90, 0x64]);
        assert_eq!(range_of(&file), (0, file.len() as u64));
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
            parse_id3v2(4, 0, frame(4, b"TIT2", &text(9, b"bad encoding"))).tags,
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
        let tags = parse_id3v2(3, 0x80, unsynced).tags;
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
        let tags = parse_id3v2(4, 0, [grouped, compressed].concat()).tags;
        assert_eq!(tags.title.as_deref(), Some("Song"));
        assert_eq!(tags.artist, None);
    }

    fn apic(picture_type: u8, image: &[u8]) -> Vec<u8> {
        let mut body = vec![0]; // Latin-1
        body.extend(b"image/jpeg\0");
        body.push(picture_type);
        body.push(0); // empty description
        body.extend(image);
        body
    }

    #[test]
    fn extracts_the_front_cover_and_prefers_it_over_other_pictures() {
        let media = parse_id3v2(
            3,
            0,
            [
                frame(3, b"APIC", &apic(4, b"\xFF\xD8\xFF\xE0back")),
                frame(3, b"APIC", &apic(3, b"\xFF\xD8\xFF\xE0front")),
            ]
            .concat(),
        );
        let cover = media.cover.unwrap();
        assert_eq!(cover.kind, crate::sniff::ImageFormat::Jpeg);
        assert_eq!(cover.bytes, b"\xFF\xD8\xFF\xE0front");
    }

    #[test]
    fn extracts_embedded_lyrics_with_and_without_timestamps() {
        let mut timed = vec![3u8]; // UTF-8
        timed.extend(b"eng");
        timed.push(0); // empty descriptor
        timed.extend("[00:01.00]第一句\n[00:02.00]第二句".as_bytes());
        let media = parse_id3v2(3, 0, frame(3, b"USLT", &timed));
        let lyrics = media.lyrics.unwrap();
        assert!(lyrics.timed);
        assert!(lyrics.text.contains("第二句"));

        let mut plain = vec![3u8];
        plain.extend(b"eng");
        plain.push(0);
        plain.extend("第一句\n第二句".as_bytes());
        let media = parse_id3v2(3, 0, frame(3, b"USLT", &plain));
        let lyrics = media.lyrics.unwrap();
        assert!(!lyrics.timed);
        assert_eq!(lyrics.text, "第一句\n第二句");
    }

    #[test]
    fn converts_synchronised_lyrics_to_timed_lrc() {
        let mut body = vec![3u8]; // UTF-8
        body.extend(b"eng");
        body.push(2); // millisecond timestamps
        body.push(1); // lyrics
        body.push(0); // empty descriptor
        for (millis, line) in [(1_000u32, "one"), (2_500, "two")] {
            body.extend(line.as_bytes());
            body.push(0);
            body.extend(millis.to_be_bytes());
        }
        let media = parse_id3v2(4, 0, frame(4, b"SYLT", &body));
        let lyrics = media.lyrics.unwrap();
        assert!(lyrics.timed);
        assert_eq!(lyrics.text, "[00:01.00]one\n[00:02.50]two\n");
    }
}
