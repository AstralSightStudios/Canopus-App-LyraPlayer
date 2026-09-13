//! Identifies picked files by their bytes. On Android the host hands the
//! plugin a `content://` URI whose last segment carries no extension, so the
//! file name cannot say what a file is.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

use serde_json::Value;

/// How far past any ID3v2 tags to look for the first MPEG frames. Encoders may
/// pad the tag or prepend junk; real audio starts well within this.
const MP3_SCAN_BYTES: usize = 64 * 1024;
/// Some files carry more than one ID3v2 tag back to back.
const MAX_ID3_TAGS: usize = 4;
const LYRICS_BINARY_PROBE_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricsFormat {
    /// A NetEase lyric envelope, as exported by the cloud import.
    Json,
    Lrc,
}

impl LyricsFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Lrc => "lrc",
        }
    }
}

/// Whether the file holds MPEG Layer III audio, the only codec the watch's
/// decoder accepts.
pub fn is_mp3(path: &Path) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut start = 0u64;
    for _ in 0..MAX_ID3_TAGS {
        file.seek(SeekFrom::Start(start))?;
        let mut header = [0u8; 10];
        if read_up_to(&mut file, &mut header)? < header.len() {
            break;
        }
        match id3v2_tag_len(&header) {
            Some(len) => start += len,
            None => break,
        }
    }
    file.seek(SeekFrom::Start(start))?;
    let mut window = vec![0u8; MP3_SCAN_BYTES];
    let len = read_up_to(&mut file, &mut window)?;
    Ok(contains_mp3_frames(&window[..len]))
}

pub fn image_format(path: &Path) -> io::Result<Option<ImageFormat>> {
    let mut header = [0u8; 8];
    let len = read_up_to(&mut File::open(path)?, &mut header)?;
    Ok(image_format_of(&header[..len]))
}

/// The lyric format, or `None` for a file that is not text. Anything that is
/// not a JSON object is treated as LRC, which also covers plain `.txt` lyrics.
pub fn lyrics_format(bytes: &[u8]) -> Option<LyricsFormat> {
    let probe = &bytes[..bytes.len().min(LYRICS_BINARY_PROBE_BYTES)];
    if probe.contains(&0) || image_format_of(probe).is_some() || contains_mp3_frames(probe) {
        return None;
    }
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match serde_json::from_slice::<Value>(text) {
        Ok(Value::Object(_)) => Some(LyricsFormat::Json),
        _ => Some(LyricsFormat::Lrc),
    }
}

fn image_format_of(header: &[u8]) -> Option<ImageFormat> {
    if header.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if header.starts_with(b"\x89PNG\r\n\x1A\n") {
        Some(ImageFormat::Png)
    } else {
        None
    }
}

/// Total length of an ID3v2 tag starting at `header`, footer included.
fn id3v2_tag_len(header: &[u8; 10]) -> Option<u64> {
    if &header[..3] != b"ID3" || header[3] == 0xFF || header[4] == 0xFF {
        return None;
    }
    // The size is four 7-bit "syncsafe" bytes and excludes the 10-byte header.
    if header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    let size = header[6..10]
        .iter()
        .fold(0u64, |size, byte| (size << 7) | u64::from(*byte));
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// Requires two consecutive frame headers, one frame length apart, so a stray
/// sync pattern in unrelated data is not mistaken for audio.
fn contains_mp3_frames(bytes: &[u8]) -> bool {
    (0..bytes.len()).any(|offset| {
        mp3_frame_len(&bytes[offset..]).is_some_and(|len| {
            bytes
                .get(offset + len..)
                .is_some_and(|next| mp3_frame_len(next).is_some())
        })
    })
}

/// Byte length of the MPEG-1/2/2.5 Layer III frame whose header starts
/// `bytes`, mirroring the watch player's own frame parser.
fn mp3_frame_len(bytes: &[u8]) -> Option<usize> {
    let &[sync, flags, rates, _] = bytes.get(..4)? else {
        return None;
    };
    if sync != 0xFF || flags & 0xE0 != 0xE0 {
        return None;
    }
    let version = (flags >> 3) & 0x3;
    let layer = (flags >> 1) & 0x3;
    let bitrate_index = usize::from(rates >> 4);
    let sample_rate_index = usize::from((rates >> 2) & 0x3);
    let padding = usize::from((rates >> 1) & 0x1);
    // Version 1 is reserved; layer bits 01 mean Layer III.
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 {
        return None;
    }
    const MPEG1_KBPS: [usize; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const MPEG2_KBPS: [usize; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    const SAMPLE_RATES: [usize; 3] = [44_100, 48_000, 32_000];
    let sample_rate = *SAMPLE_RATES.get(sample_rate_index)?;
    let (kbps, sample_rate, coefficient) = match version {
        3 => (MPEG1_KBPS[bitrate_index], sample_rate, 144),
        2 => (MPEG2_KBPS[bitrate_index], sample_rate / 2, 72),
        _ => (MPEG2_KBPS[bitrate_index], sample_rate / 4, 72),
    };
    Some(coefficient * kbps * 1_000 / sample_rate + padding)
}

fn read_up_to(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// MPEG-1 Layer III, 128 kbps, 44.1 kHz, no padding: 417-byte frames.
    const FRAME_HEADER: [u8; 4] = [0xFF, 0xFB, 0x90, 0x64];

    fn frames(count: usize) -> Vec<u8> {
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&FRAME_HEADER);
        frame.repeat(count)
    }

    fn id3_tag(body_len: usize) -> Vec<u8> {
        let size = body_len as u32;
        let mut tag = b"ID3\x04\x00\x00".to_vec();
        tag.extend(
            (0..4)
                .rev()
                .map(|shift| ((size >> (shift * 7)) & 0x7F) as u8),
        );
        tag.extend(vec![0u8; body_len]);
        tag
    }

    fn scratch_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let directory = crate::artwork::unique_output_directory("sniff-test");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn frame_length_matches_the_header() {
        assert_eq!(mp3_frame_len(&FRAME_HEADER), Some(417));
        // Layer II and the reserved bitrate are not playable MP3.
        assert_eq!(mp3_frame_len(&[0xFF, 0xFD, 0x90, 0x64]), None);
        assert_eq!(mp3_frame_len(&[0xFF, 0xFB, 0xF0, 0x64]), None);
    }

    #[test]
    fn mp3_is_recognized_without_an_extension() {
        // An Android content URI segment: no extension at all.
        let bare = scratch_file("audio%3A1000012345", &frames(4));
        assert!(is_mp3(&bare).unwrap());

        // A large ID3v2 tag (embedded artwork) is skipped by its declared size,
        // even past the scan window, and a second tag after it is skipped too.
        let mut tagged = id3_tag(MP3_SCAN_BYTES * 2);
        tagged.extend(id3_tag(128));
        tagged.extend(frames(4));
        let tagged = scratch_file("1000012346", &tagged);
        assert!(is_mp3(&tagged).unwrap());

        let _ = fs::remove_dir_all(bare.parent().unwrap());
        let _ = fs::remove_dir_all(tagged.parent().unwrap());
    }

    #[test]
    fn non_mp3_files_are_rejected() {
        let mut png = b"\x89PNG\r\n\x1A\n".to_vec();
        png.extend(vec![0x42; 4096]);
        let mut lone_sync = vec![0u8; 2048];
        lone_sync[100..104].copy_from_slice(&FRAME_HEADER);
        for (name, bytes) in [
            ("png", png),
            ("wav", b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec()),
            ("m4a", b"\x00\x00\x00\x20ftypM4A \x00\x00\x00\x00".to_vec()),
            ("lone-sync", lone_sync),
            ("id3-only", id3_tag(64)),
        ] {
            let path = scratch_file(name, &bytes);
            assert!(!is_mp3(&path).unwrap(), "{name}");
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[test]
    fn images_are_recognized_by_signature() {
        assert_eq!(
            image_format_of(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            image_format_of(b"\x89PNG\r\n\x1A\n"),
            Some(ImageFormat::Png)
        );
        assert_eq!(image_format_of(b"GIF89a"), None);
        assert_eq!(image_format_of(&[]), None);
    }

    #[test]
    fn lyrics_format_comes_from_content() {
        assert_eq!(
            lyrics_format(br#"{"code":200,"lrc":{"lyric":"[00:01.00]hi"}}"#),
            Some(LyricsFormat::Json)
        );
        assert_eq!(
            lyrics_format(b"\xEF\xBB\xBF{\"code\":200}"),
            Some(LyricsFormat::Json)
        );
        assert_eq!(
            lyrics_format("[ti:歌名]\n[00:01.00]第一句".as_bytes()),
            Some(LyricsFormat::Lrc)
        );
        assert_eq!(lyrics_format(b"plain text lyrics"), Some(LyricsFormat::Lrc));
        assert_eq!(lyrics_format(&frames(2)), None);
        assert_eq!(lyrics_format(b"\x89PNG\r\n\x1A\n"), None);
        assert_eq!(lyrics_format(b"text\0with nul"), None);
    }
}
