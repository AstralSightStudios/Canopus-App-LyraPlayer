//! Lyrics normalisation: the watch reads LRC only, in which a translation is a second
//! line with the same time stamp as the original. NetEase answers with JSON holding an
//! original and an optional translated LRC; this turns it (or a plain LRC file) into the
//! single file the watch gets.

use serde_json::Value;

/// The watch refuses lyric files above this size (`MAX_LYRICS_BYTES`).
const MAX_BYTES: usize = lyra_protocol::MAX_LYRICS_BYTES as usize;

/// `[mm:ss.xx]` stamps of a line and its text.
fn parse_line(line: &str) -> Option<(Vec<u32>, &str)> {
    let mut rest = line.trim_start();
    let mut stamps = Vec::new();
    while let Some(after) = rest.strip_prefix('[') {
        let close = after.find(']')?;
        let tag = &after[..close];
        if let Some(ms) = stamp_ms(tag) {
            stamps.push(ms);
        }
        rest = &after[close + 1..];
    }
    (!stamps.is_empty()).then_some((stamps, rest.trim()))
}

fn stamp_ms(tag: &str) -> Option<u32> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes: u32 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
    let seconds: u32 = seconds.trim().parse().ok()?;
    let fraction: u32 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u32>().ok()? * 100,
        2 => fraction.parse::<u32>().ok()? * 10,
        _ => fraction.get(..3)?.parse().ok()?,
    };
    Some(minutes * 60_000 + seconds * 1_000 + fraction)
}

fn stamp_text(ms: u32) -> String {
    format!("[{:02}:{:02}.{:02}]", ms / 60_000, ms / 1_000 % 60, ms % 1_000 / 10)
}

fn lines(lrc: &str) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = lrc
        .lines()
        .filter_map(parse_line)
        .flat_map(|(stamps, text)| stamps.into_iter().map(move |ms| (ms, text.to_string())))
        .collect();
    out.sort_by_key(|(ms, _)| *ms);
    out
}

/// Original and translation lines merged by time stamp, as LRC text.
pub fn merge(original: &str, translation: &str) -> String {
    let translated = lines(translation);
    let mut out = String::new();
    for (ms, text) in lines(original) {
        out.push_str(&format!("{}{}\n", stamp_text(ms), text));
        if !text.is_empty() {
            if let Some((_, other)) = translated.iter().find(|(at, other)| *at == ms && !other.is_empty()) {
                if *other != text {
                    out.push_str(&format!("{}{}\n", stamp_text(ms), other));
                }
            }
        }
    }
    out
}

/// Cuts at a line end so a long file stays within the watch's limit.
fn fit(mut text: String) -> String {
    if text.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        end = text[..end].rfind('\n').map_or(end, |at| at + 1);
        text.truncate(end);
    }
    text
}

/// `format` is what `sniff` recognised: "lrc" or "json".
pub fn to_lrc(bytes: &[u8], format: &str) -> Result<Vec<u8>, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let merged = match format {
        "json" => {
            let value: Value = serde_json::from_str(text).map_err(|error| format!("歌词不是有效的 JSON：{error}"))?;
            let part = |key: &str| {
                value
                    .get(key)
                    .and_then(|item| item.get("lyric"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            merge(&part("lrc"), &part("tlyric"))
        }
        _ => text.to_string(),
    };
    if lines(&merged).is_empty() {
        return Err("歌词里没有带时间的行".to_string());
    }
    Ok(fit(merged).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_becomes_a_second_line_with_the_same_stamp() {
        let merged = merge("[00:01.00]hello\n[00:03.50]bye", "[00:01.00]你好\n[00:09.00]orphan");
        assert_eq!(merged, "[00:01.00]hello\n[00:01.00]你好\n[00:03.50]bye\n");
    }

    #[test]
    fn netease_json_is_converted_and_garbage_is_refused() {
        let json = r#"{"code":200,"lrc":{"lyric":"[00:00.50]a\n[00:02.00]b"},"tlyric":{"lyric":"[00:00.50]甲"}}"#;
        let lrc = String::from_utf8(to_lrc(json.as_bytes(), "json").unwrap()).unwrap();
        assert_eq!(lrc, "[00:00.50]a\n[00:00.50]甲\n[00:02.00]b\n");
        assert!(to_lrc(b"{}", "json").is_err());
        assert!(to_lrc(b"plain text", "lrc").is_err());
        assert!(to_lrc(b"[00:01.00]ok", "lrc").is_ok());
    }

    #[test]
    fn multi_stamp_lines_and_blank_gaps_survive() {
        let merged = merge("[00:01.00][00:05.00]chorus\n[00:03.00]", "");
        assert_eq!(merged, "[00:01.00]chorus\n[00:03.00]\n[00:05.00]chorus\n");
    }

    #[test]
    fn oversized_lyrics_are_cut_at_a_line_end() {
        let big = (0..20_000).map(|i| format!("[{:02}:{:02}.00]line {i}\n", i / 60 % 100, i % 60)).collect::<String>();
        let out = String::from_utf8(to_lrc(big.as_bytes(), "lrc").unwrap()).unwrap();
        assert!(out.len() <= MAX_BYTES);
        assert!(out.ends_with('\n'));
    }
}
