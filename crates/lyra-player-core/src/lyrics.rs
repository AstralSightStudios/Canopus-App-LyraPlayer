use alloc::{string::String, vec::Vec};
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricLine {
    pub at_ms: u32,
    pub text: String,
    pub translation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
}

impl Lyrics {
    pub fn active_index(&self, position_ms: u32) -> Option<usize> {
        self.lines
            .partition_point(|line| line.at_ms <= position_ms)
            .checked_sub(1)
    }

    pub fn active_window(&self, position_ms: u32, radius: usize) -> &[LyricLine] {
        let Some(index) = self.active_index(position_ms) else {
            return &[];
        };
        let start = index.saturating_sub(radius);
        let end = (index + radius + 1).min(self.lines.len());
        &self.lines[start..end]
    }
}

#[derive(Deserialize)]
struct LyricEnvelope {
    code: i32,
    #[serde(default)]
    lrc: LyricPart,
    #[serde(default)]
    tlyric: LyricPart,
}

#[derive(Default, Deserialize)]
struct LyricPart {
    #[serde(default)]
    lyric: String,
}

pub fn parse_api_lyrics(json: &str) -> Result<Lyrics, LyricError> {
    let value: LyricEnvelope = serde_json::from_str(json).map_err(|_| LyricError::Json)?;
    if value.code != 200 {
        return Err(LyricError::Server(value.code));
    }
    let mut lines = parse_lrc(&value.lrc.lyric);
    for translated in parse_lrc(&value.tlyric.lyric).lines {
        if let Ok(index) = lines
            .lines
            .binary_search_by_key(&translated.at_ms, |line| line.at_ms)
        {
            if !translated.text.is_empty() {
                lines.lines[index].translation = Some(translated.text);
            }
        }
    }
    Ok(lines)
}

pub fn parse_lrc(input: &str) -> Lyrics {
    let mut output = Vec::new();
    for raw_line in input.lines() {
        let mut cursor = raw_line;
        let mut timestamps = Vec::new();
        while let Some(rest) = cursor.strip_prefix('[') {
            let Some(close) = rest.find(']') else { break };
            if let Some(ms) = parse_timestamp(&rest[..close]) {
                timestamps.push(ms);
            }
            cursor = &rest[close + 1..];
        }
        let text = cursor.trim();
        for at_ms in timestamps {
            output.push(LyricLine {
                at_ms,
                text: text.into(),
                translation: None,
            });
        }
    }
    output.sort_by_key(|line| line.at_ms);
    output.dedup_by(|right, left| {
        if left.at_ms == right.at_ms {
            if left.text.is_empty() {
                left.text.clone_from(&right.text);
            }
            true
        } else {
            false
        }
    });
    Lyrics { lines: output }
}

fn parse_timestamp(value: &str) -> Option<u32> {
    let (minutes, seconds) = value.split_once(':')?;
    let minutes: u32 = minutes.parse().ok()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
    let seconds: u32 = seconds.parse().ok()?;
    let fraction_ms = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u32>().ok()? * 100,
        2 => fraction.parse::<u32>().ok()? * 10,
        _ => fraction.get(..3)?.parse::<u32>().ok()?,
    };
    minutes
        .checked_mul(60_000)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(fraction_ms)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricError {
    Json,
    Server(i32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_locates_lrc_lines() {
        let lyrics = parse_lrc("[00:01.20]first\n[00:03.500][00:05.00]next");
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].at_ms, 1_200);
        assert_eq!(lyrics.active_index(4_000), Some(1));
    }

    #[test]
    fn merges_translation_by_timestamp() {
        let lyrics = parse_api_lyrics(
            r#"{"code":200,"lrc":{"lyric":"[00:01.00]hello"},"tlyric":{"lyric":"[00:01.00]你好"}}"#,
        )
        .unwrap();
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("你好"));
    }
}
