//! Re-encodes an imported MP3 at a lower bitrate before it is sent to the watch.
//!
//! The watch buffers every frame it receives and keeps the whole file on its small
//! flash, so a 320 kbps rip is needlessly expensive there. The plugin decodes and
//! re-encodes the file on the host instead, keeping only one frame in memory.

use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::Path,
};

use rusty_mp3::{Error, Mp3Decoder, Mp3Encoder, Mp3EncoderConfig};

/// Import at the file's original bitrate.
pub const ORIGINAL: u32 = 0;
pub const HIGH: u32 = 128;
pub const STANDARD: u32 = 96;
pub const SMALL: u32 = 64;

pub const DEFAULT: u32 = ORIGINAL;

/// Snaps a host-provided value to one of the choices the page offers.
pub fn normalized(value: u32) -> u32 {
    match value {
        ORIGINAL | SMALL | STANDARD | HIGH => value,
        _ => DEFAULT,
    }
}

/// Reads `source` (an MP3) and writes a CBR MP3 at `bitrate_kbps` to `output`.
/// Tags are not copied: the watch only plays the frames, and the caller has
/// already read the embedded cover and lyrics.
pub fn compress(source: &Path, output: &Path, bitrate_kbps: u32) -> Result<(), String> {
    if bitrate_kbps == ORIGINAL {
        return Err("没有选择压缩音质".to_string());
    }
    let mut input = File::open(source).map_err(|error| format!("无法打开音频：{error}"))?;
    let mut writer =
        BufWriter::new(File::create(output).map_err(|error| format!("无法创建压缩音频：{error}"))?);
    let mut decoder = Mp3Decoder::new();
    let mut encoder: Option<Mp3Encoder> = None;
    let mut chunk = vec![0u8; 64 * 1024];

    loop {
        let read = input
            .read(&mut chunk)
            .map_err(|error| format!("无法读取音频：{error}"))?;
        if read == 0 {
            break;
        }
        decoder.push(&chunk[..read]);
        drain(&mut decoder, &mut encoder, &mut writer, bitrate_kbps)?;
    }
    decoder.flush();
    drain(&mut decoder, &mut encoder, &mut writer, bitrate_kbps)?;

    let Some(mut encoder) = encoder else {
        return Err("音频里没有可解码的 MP3 帧".to_string());
    };
    encoder.finish();
    while let Ok(packet) = encoder.next_packet() {
        writer
            .write_all(&packet)
            .map_err(|error| format!("无法写入压缩音频：{error}"))?;
    }
    writer
        .flush()
        .map_err(|error| format!("无法写入压缩音频：{error}"))
}

fn drain(
    decoder: &mut Mp3Decoder,
    encoder: &mut Option<Mp3Encoder>,
    writer: &mut BufWriter<File>,
    bitrate_kbps: u32,
) -> Result<(), String> {
    loop {
        match decoder.next_frame() {
            Ok(frame) => {
                let encoder = encoder.get_or_insert_with(|| {
                    Mp3Encoder::new(Mp3EncoderConfig {
                        bitrate_kbps,
                        vbr_quality: None,
                    })
                });
                encoder
                    .push_pcm_f32(&frame.samples, frame.channels, frame.sample_rate)
                    .map_err(|error| format!("无法压缩音频：{error}"))?;
                while let Ok(packet) = encoder.next_packet() {
                    writer
                        .write_all(&packet)
                        .map_err(|error| format!("无法写入压缩音频：{error}"))?;
                }
            }
            Err(Error::Again | Error::Eof) => return Ok(()),
            Err(error) => return Err(format!("无法解码音频：{error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_mp3(path: &Path) {
        let rate = 44_100;
        let samples = 2 * rate;
        let pcm: Vec<f32> = (0..samples)
            .flat_map(|index| {
                let value =
                    0.4 * (2.0 * std::f32::consts::PI * 440.0 * index as f32 / rate as f32).sin();
                [value, value * 0.5]
            })
            .collect();
        let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
            bitrate_kbps: 256,
            vbr_quality: None,
        });
        encoder.push_pcm_f32(&pcm, 2, rate).unwrap();
        encoder.finish();
        let mut bytes = Vec::new();
        while let Ok(packet) = encoder.next_packet() {
            bytes.extend_from_slice(&packet);
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn compressing_lowers_the_bitrate_and_keeps_playable_frames() {
        let directory = crate::artwork::unique_output_directory("audio-compress-test");
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.mp3");
        let output = directory.join("small.mp3");
        source_mp3(&source);

        compress(&source, &output, SMALL).unwrap();

        let before = std::fs::metadata(&source).unwrap().len();
        let after = std::fs::metadata(&output).unwrap().len();
        assert!(after < before, "{after} should be smaller than {before}");
        assert!(crate::sniff::is_mp3(&output).unwrap());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn choices_are_normalized() {
        assert_eq!(normalized(ORIGINAL), ORIGINAL);
        assert_eq!(normalized(SMALL), SMALL);
        assert_eq!(normalized(STANDARD), STANDARD);
        assert_eq!(normalized(HIGH), HIGH);
        assert_eq!(normalized(123), DEFAULT);
    }
}
