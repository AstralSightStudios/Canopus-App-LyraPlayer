use alloc::{string::String, vec::Vec};

/// More lines than this are never shown; a song has a few dozen to a couple of
/// hundred, so the cap only guards against a corrupt file.
pub const MAX_LINES: usize = 512;
/// Longer lines are cut at a character boundary.
pub const MAX_LINE_CHARS: usize = 160;

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
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line being sung at `position_ms`: the last one that has started.
    /// Before the first line there is none.
    pub fn active_index(&self, position_ms: u32) -> Option<usize> {
        self.lines
            .partition_point(|line| line.at_ms <= position_ms)
            .checked_sub(1)
    }

    /// How long line `index` stays active; the last one runs to `duration_ms`.
    pub fn line_span_ms(&self, index: usize, duration_ms: u32) -> u32 {
        let start = self.lines[index].at_ms;
        let end = self
            .lines
            .get(index + 1)
            .map_or(duration_ms.max(start), |next| next.at_ms);
        end.saturating_sub(start)
    }
}

/// Parses an LRC file: any number of `[mm:ss.xx]` stamps per line, an
/// `[offset:±ms]` tag, ignored metadata tags (`[ti:..]`), inline word stamps
/// (`<mm:ss.xx>`) and bilingual files where the translation repeats the stamp
/// of the original line.
pub fn parse_lrc(input: &str) -> Lyrics {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut offset_ms: i64 = 0;
    let mut entries: Vec<(u32, String)> = Vec::new();
    for raw_line in input.lines() {
        let mut cursor = raw_line.trim_start();
        let mut stamps: Vec<u32> = Vec::new();
        while let Some(rest) = cursor.strip_prefix('[') {
            let Some(close) = rest.find(']') else { break };
            let tag = &rest[..close];
            if let Some(ms) = parse_timestamp(tag) {
                stamps.push(ms);
            } else if let Some(value) = tag.strip_prefix("offset:") {
                offset_ms = value.trim().parse().unwrap_or(0);
            }
            cursor = &rest[close + 1..];
        }
        if stamps.is_empty() {
            continue;
        }
        let text = strip_word_stamps(cursor);
        for at_ms in stamps {
            if entries.len() >= MAX_LINES * 2 {
                break;
            }
            entries.push((at_ms, text.clone()));
        }
    }
    entries.sort_by_key(|(at_ms, _)| *at_ms);

    let mut lines: Vec<LyricLine> = Vec::new();
    for (at_ms, text) in entries {
        let at_ms = (i64::from(at_ms) - offset_ms).max(0) as u32;
        if let Some(last) = lines.last_mut() {
            if last.at_ms == at_ms {
                if last.text.is_empty() {
                    last.text = text;
                } else if !text.is_empty() && last.translation.is_none() && last.text != text {
                    last.translation = Some(text);
                }
                continue;
            }
        }
        if lines.len() == MAX_LINES {
            break;
        }
        lines.push(LyricLine {
            at_ms,
            text,
            translation: None,
        });
    }
    Lyrics { lines }
}

fn strip_word_stamps(text: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for character in text.chars() {
        match character {
            '<' => skipping = true,
            '>' if skipping => skipping = false,
            _ if skipping => {}
            _ => out.push(character),
        }
        if out.chars().count() >= MAX_LINE_CHARS {
            break;
        }
    }
    String::from(out.trim())
}

fn parse_timestamp(value: &str) -> Option<u32> {
    let (minutes, seconds) = value.split_once(':')?;
    let minutes: u32 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = seconds.split_once(['.', ':']).unwrap_or((seconds, "0"));
    let seconds: u32 = seconds.trim().parse().ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_locates_lrc_lines() {
        let lyrics = parse_lrc("[00:01.20]first\n[00:03.500][00:05.00]next");
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].at_ms, 1_200);
        assert_eq!(lyrics.active_index(4_000), Some(1));
        assert_eq!(lyrics.active_index(500), None);
        assert_eq!(lyrics.active_index(99_000), Some(2));
    }

    #[test]
    fn same_stamp_lines_become_original_and_translation() {
        let lyrics = parse_lrc("[00:01.00]hello\n[00:01.00]你好\n[00:02.00]bye\n[00:02.00]bye");
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "hello");
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("你好"));
        assert_eq!(lyrics.lines[1].translation, None);
    }

    #[test]
    fn metadata_offset_and_word_stamps() {
        let lyrics = parse_lrc(
            "\u{feff}[ti:Song]\n[ar:Someone]\n[offset:500]\n[00:10.00]<00:10.00>one <00:10.50>two\n",
        );
        assert_eq!(lyrics.lines.len(), 1);
        assert_eq!(lyrics.lines[0].at_ms, 9_500);
        assert_eq!(lyrics.lines[0].text, "one two");
    }

    #[test]
    fn blank_stamped_lines_are_kept_as_gaps() {
        let lyrics = parse_lrc("[00:01.00]a\n[00:05.00]\n[00:09.00]b");
        assert_eq!(lyrics.lines.len(), 3);
        assert!(lyrics.lines[1].text.is_empty());
        assert_eq!(lyrics.line_span_ms(1, 20_000), 4_000);
        assert_eq!(lyrics.line_span_ms(2, 20_000), 11_000);
    }

    #[test]
    fn garbage_is_empty() {
        assert!(parse_lrc("no stamps here\n[x:y]").is_empty());
    }
}
