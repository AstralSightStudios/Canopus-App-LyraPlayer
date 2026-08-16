//! Client for the BluetoothAudio module's `/dev/canopus_audio` character
//! device. The fd is exclusively owned by the resident Lyra core.

use alloc::{string::String, vec::Vec};
use core::ffi::c_void;

use canopus_target_private::{
    O_RDONLY, O_RDWR, canopus_fw_clock_gettime, get_errno, nuttx_close, nuttx_ioctl, nuttx_lseek,
    nuttx_open, nuttx_read, nuttx_write, stock_timespec_t,
};
use lyra_player_core::playback::{AudioSink, Player};

use super::storage;

const DEVICE_PATH: &[u8] = b"/dev/canopus_audio\0";
const FORMAT_MP3: u32 = 1;
const IOC_SET_FORMAT: u32 = 0x305;
const IOC_START: u32 = 0x306;
const IOC_PAUSE: u32 = 0x307;
const IOC_RESUME: u32 = 0x308;
const IOC_STOP: u32 = 0x309;
const IOC_DRAIN: u32 = 0x30D;
const IOC_GET_STATUS: u32 = 0x30F;
const IOC_SET_VOLUME: u32 = 0x310;
const IOC_GET_VOLUME: u32 = 0x311;
const AUDIO_STATE_CONFIGURED: u32 = 2;
const AUDIO_STATE_BUFFERING: u32 = 3;
const AUDIO_STATE_PLAYING: u32 = 4;
const AUDIO_STATE_PAUSED: u32 = 5;
const AUDIO_STATE_ERROR: u32 = 8;
const EAGAIN: i32 = -11;
const EPIPE: i32 = -32;
const LOCAL_READ_CHUNK: usize = 4096;
const LOCAL_BURST_CHUNKS: usize = 8;
const SEEK_SET: i32 = 0;
const SEEK_SCAN_WINDOW: usize = 8 * 1024;
const EINVAL: i32 = -22;
const EIO: i32 = -5;
const CLOCK_MONOTONIC: u32 = 1;

const STAGE_NONE: u8 = 0;
const STAGE_AUDIO_OPEN: u8 = 1;
const STAGE_LOCAL_OPEN: u8 = 2;
const STAGE_LOCAL_READ: u8 = 3;
const STAGE_AUDIO_WRITE: u8 = 4;
const STAGE_IOCTL_STOP: u8 = 5;
const STAGE_IOCTL_SET_FORMAT: u8 = 6;
const STAGE_IOCTL_START: u8 = 7;
const STAGE_IOCTL_OTHER: u8 = 8;

fn monotonic_ms() -> Option<u64> {
    let mut time = stock_timespec_t {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result =
        unsafe { canopus_fw_clock_gettime(CLOCK_MONOTONIC, core::ptr::addr_of_mut!(time)) };
    if result != 0 || time.tv_sec < 0 || time.tv_nsec < 0 || time.tv_nsec >= 1_000_000_000 {
        return None;
    }
    Some(
        (time.tv_sec as u64)
            .saturating_mul(1_000)
            .saturating_add(time.tv_nsec as u64 / 1_000_000),
    )
}

/// Recovers the real error from a `-1` firmware return. NuttX collapses every
/// driver error into `-1` and parks the positive errno in the task's errno
/// slot; read it back so callers retain the actual failure reason instead of
/// an undifferentiated `-1`.
fn neg_errno(raw: i32) -> i32 {
    if raw == -1 {
        let errno = unsafe { get_errno() };
        if errno > 0 {
            return -errno;
        }
    }
    raw
}

#[repr(C)]
struct AudioFormatV1 {
    struct_size: u32,
    format: u32,
    sample_rate_hint: u32,
    channels_hint: u32,
    flags: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AudioStatusV1 {
    struct_size: u32,
    abi_version: u32,
    state: u32,
    last_error: i32,
    format: u32,
    input_capacity: u32,
    input_used: u32,
    input_free: u32,
    decoded_sample_rate: u32,
    decoded_channels: u32,
    negotiated_bitpool: u32,
    bytes_accepted: u32,
    bytes_consumed: u32,
    pcm_frames: u32,
    rtp_packets: u32,
    underruns: u32,
    generation: u32,
    volume_percent: u32,
}

fn classify_generic_write_failure(status: Option<AudioStatusV1>) -> Result<usize, i32> {
    let Some(status) = status else {
        return Err(-1);
    };
    if status.state == AUDIO_STATE_ERROR {
        return Err(if status.last_error < 0 {
            status.last_error
        } else {
            EIO
        });
    }
    if matches!(
        status.state,
        AUDIO_STATE_CONFIGURED | AUDIO_STATE_BUFFERING | AUDIO_STATE_PLAYING | AUDIO_STATE_PAUSED
    ) {
        return Ok(0);
    }
    Err(if status.last_error < 0 {
        status.last_error
    } else {
        EPIPE
    })
}

#[derive(Clone, Copy)]
struct Mp3Frame {
    bytes: u32,
    samples: u32,
    sample_rate: u32,
}

fn parse_mp3_frame(header: [u8; 4]) -> Option<Mp3Frame> {
    let bits = u32::from_be_bytes(header);
    if bits & 0xffe0_0000 != 0xffe0_0000 {
        return None;
    }
    let version = (bits >> 19) & 0x3;
    let layer = (bits >> 17) & 0x3;
    let bitrate_index = ((bits >> 12) & 0xf) as usize;
    let sample_rate_index = ((bits >> 10) & 0x3) as usize;
    if version == 1
        || layer != 1
        || bitrate_index == 0
        || bitrate_index == 15
        || sample_rate_index == 3
    {
        return None;
    }
    const MPEG1_BITRATES: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const MPEG2_BITRATES: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    const SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 32_000];
    let sample_rate = SAMPLE_RATES[sample_rate_index]
        / match version {
            3 => 1,
            2 => 2,
            _ => 4,
        };
    let bitrate = if version == 3 {
        MPEG1_BITRATES[bitrate_index]
    } else {
        MPEG2_BITRATES[bitrate_index]
    } * 1_000;
    let padding = (bits >> 9) & 1;
    let (coefficient, samples) = if version == 3 {
        (144, 1_152)
    } else {
        (72, 576)
    };
    let bytes = coefficient * bitrate / sample_rate + padding;
    (bytes >= 4).then_some(Mp3Frame {
        bytes,
        samples,
        sample_rate,
    })
}

struct SeekWindow {
    fd: i32,
    start: u64,
    len: usize,
    bytes: [u8; SEEK_SCAN_WINDOW],
}

impl SeekWindow {
    fn new(fd: i32) -> Self {
        Self {
            fd,
            start: u64::MAX,
            len: 0,
            bytes: [0; SEEK_SCAN_WINDOW],
        }
    }

    fn header_at(&mut self, offset: u64) -> Result<Option<[u8; 4]>, i32> {
        let cached = self.start != u64::MAX
            && offset >= self.start
            && offset.saturating_add(4) <= self.start.saturating_add(self.len as u64);
        if !cached {
            let position = unsafe { nuttx_lseek(self.fd, offset as i64, SEEK_SET) };
            if position < 0 || position as u64 != offset {
                return Err(if position < 0 { position as i32 } else { EIO });
            }
            let count = unsafe {
                nuttx_read(
                    self.fd,
                    self.bytes.as_mut_ptr().cast::<c_void>(),
                    self.bytes.len() as u32,
                )
            };
            if count < 0 {
                return Err(count);
            }
            self.start = offset;
            self.len = count as usize;
        }
        let index = (offset - self.start) as usize;
        if index + 4 > self.len {
            return Ok(None);
        }
        Ok(Some(
            self.bytes[index..index + 4].try_into().unwrap_or([0; 4]),
        ))
    }
}

#[allow(dead_code)]
fn find_mp3_seek_point(fd: i32, target_ms: u32) -> Result<(u64, u32), i32> {
    let target_us = u64::from(target_ms).saturating_mul(1_000);
    let mut reader = SeekWindow::new(fd);
    let mut offset = 0u64;
    let mut elapsed_us = 0u64;
    let mut locked = false;
    let mut last = None;
    loop {
        let Some(header) = reader.header_at(offset)? else {
            return last.ok_or(EINVAL);
        };
        let Some(frame) = parse_mp3_frame(header) else {
            offset = offset.saturating_add(1);
            locked = false;
            continue;
        };
        if !locked {
            let next_offset = offset.saturating_add(u64::from(frame.bytes));
            let Some(next_header) = reader.header_at(next_offset)? else {
                return Ok((offset, (elapsed_us / 1_000).min(u64::from(u32::MAX)) as u32));
            };
            if parse_mp3_frame(next_header).is_none() {
                offset = offset.saturating_add(1);
                continue;
            }
            locked = true;
        }
        let position_ms = (elapsed_us / 1_000).min(u64::from(u32::MAX)) as u32;
        last = Some((offset, position_ms));
        let frame_us =
            u64::from(frame.samples).saturating_mul(1_000_000) / u64::from(frame.sample_rate);
        if elapsed_us.saturating_add(frame_us) > target_us {
            return Ok((offset, position_ms));
        }
        elapsed_us = elapsed_us.saturating_add(frame_us);
        offset = offset.saturating_add(u64::from(frame.bytes));
    }
}

pub struct AudioDevice {
    fd: i32,
    local_fd: i32,
    last_stage: u8,
    position_base_ms: u32,
    position_started_at_ms: Option<u64>,
}

impl AudioDevice {
    pub const fn new() -> Self {
        Self {
            fd: -1,
            local_fd: -1,
            last_stage: STAGE_NONE,
            position_base_ms: 0,
            position_started_at_ms: None,
        }
    }

    fn ensure_open(&mut self) -> Result<i32, i32> {
        if self.fd >= 0 {
            return Ok(self.fd);
        }
        self.last_stage = STAGE_AUDIO_OPEN;
        let fd = unsafe { nuttx_open(DEVICE_PATH.as_ptr(), O_RDWR) };
        if fd < 0 {
            return Err(neg_errno(fd));
        }
        self.fd = fd;
        Ok(fd)
    }

    fn ioctl_stage(command: u32) -> u8 {
        match command {
            IOC_STOP => STAGE_IOCTL_STOP,
            IOC_SET_FORMAT => STAGE_IOCTL_SET_FORMAT,
            IOC_START => STAGE_IOCTL_START,
            _ => STAGE_IOCTL_OTHER,
        }
    }

    fn stage_name(stage: u8) -> &'static str {
        match stage {
            STAGE_AUDIO_OPEN => "audio_open",
            STAGE_LOCAL_OPEN => "local_open",
            STAGE_LOCAL_READ => "local_read",
            STAGE_AUDIO_WRITE => "audio_write",
            STAGE_IOCTL_STOP => "ioctl_stop",
            STAGE_IOCTL_SET_FORMAT => "ioctl_set_format",
            STAGE_IOCTL_START => "ioctl_start",
            STAGE_IOCTL_OTHER => "ioctl_other",
            _ => "audio",
        }
    }

    pub fn failure_stage(&self) -> &'static str {
        Self::stage_name(self.last_stage)
    }

    fn ioctl_value(&mut self, command: u32, argument: usize) -> Result<(), i32> {
        self.last_stage = Self::ioctl_stage(command);
        let fd = self.ensure_open()?;
        let result = unsafe { nuttx_ioctl(fd, command, argument) };
        if result < 0 {
            Err(neg_errno(result))
        } else {
            Ok(())
        }
    }

    fn ioctl(&mut self, command: u32) -> Result<(), i32> {
        self.ioctl_value(command, 0)
    }

    fn status(&mut self) -> Result<AudioStatusV1, i32> {
        let mut status = AudioStatusV1 {
            struct_size: core::mem::size_of::<AudioStatusV1>() as u32,
            ..AudioStatusV1::default()
        };
        self.ioctl_value(IOC_GET_STATUS, core::ptr::addr_of_mut!(status) as usize)
            .map(|()| status)
    }

    pub fn is_open(&self) -> bool {
        self.fd >= 0
    }

    pub fn volume_percent(&mut self) -> Result<u8, i32> {
        let mut volume = 0u32;
        self.ioctl_value(IOC_GET_VOLUME, core::ptr::addr_of_mut!(volume) as usize)?;
        Ok(volume.min(100) as u8)
    }

    fn freeze_position_clock(&mut self) {
        let Some(started_at) = self.position_started_at_ms.take() else {
            return;
        };
        let Some(now) = monotonic_ms() else {
            return;
        };
        self.position_base_ms = self
            .position_base_ms
            .saturating_add(now.saturating_sub(started_at).min(u64::from(u32::MAX)) as u32);
    }

    pub fn start_local(&mut self, path: &str, player: &mut Player) -> Result<(), i32> {
        self.close_local();
        let resolved = storage::resolve_path(path).ok_or(-2)?;
        let mut c_path = Vec::with_capacity(resolved.len() + 1);
        c_path.extend_from_slice(resolved.as_bytes());
        c_path.push(0);
        self.last_stage = STAGE_LOCAL_OPEN;
        let fd = unsafe { nuttx_open(c_path.as_ptr(), O_RDONLY) };
        if fd < 0 {
            return Err(neg_errno(fd));
        }
        self.local_fd = fd;
        self.position_base_ms = 0;
        self.position_started_at_ms = None;

        // Imported metadata supplies duration when available. Do not probe the
        // file with lseek here: some Band 10 storage backends can read the
        // stream but reject lseek with ENOTTY, which must not block playback.
        if let Err(error) = player.stream_opened(String::from("local"), self) {
            self.close_local();
            return Err(error);
        }
        self.pump_local(player)
    }

    fn prepare_and_prebuffer(&mut self) -> Result<(), i32> {
        AudioSink::stop(self)?;
        AudioSink::configure_mp3(self)?;
        AudioSink::start(self)
    }

    pub fn pump_local(&mut self, player: &mut Player) -> Result<(), i32> {
        if self.local_fd < 0 {
            return Ok(());
        }
        for _ in 0..LOCAL_BURST_CHUNKS {
            if !player.flush_audio(self)? {
                return Ok(());
            }
            let mut chunk = alloc::vec![0u8; LOCAL_READ_CHUNK];
            let count = unsafe {
                nuttx_read(
                    self.local_fd,
                    chunk.as_mut_ptr().cast::<c_void>(),
                    chunk.len() as u32,
                )
            };
            if count < 0 {
                self.last_stage = STAGE_LOCAL_READ;
                self.close_local();
                return Err(neg_errno(count));
            }
            if count == 0 {
                if player.stream_ended(self)? {
                    self.close_local();
                }
                return Ok(());
            }
            chunk.truncate(count as usize);
            if !player.push_audio(chunk, self)? {
                return Ok(());
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn seek_local(&mut self, target_ms: u32, player: &mut Player) -> Result<u32, i32> {
        if self.local_fd < 0 {
            return Err(EINVAL);
        }
        let remain_paused = player.state == lyra_player_core::playback::PlaybackState::Paused;
        AudioSink::stop(self)?;
        let (offset, position_ms) = find_mp3_seek_point(self.local_fd, target_ms)?;
        let result = unsafe { nuttx_lseek(self.local_fd, offset as i64, SEEK_SET) };
        if result < 0 || result as u64 != offset {
            return Err(if result < 0 {
                neg_errno(result as i32)
            } else {
                EIO
            });
        }
        self.prepare_and_prebuffer()?;
        self.position_base_ms = position_ms;
        player.prebuffered_stream_started_at(String::from("local"), position_ms);
        self.pump_local(player)?;
        if remain_paused {
            AudioSink::pause(self)?;
            player.state = lyra_player_core::playback::PlaybackState::Paused;
        }
        Ok(position_ms)
    }

    pub fn playback_position_ms(&self) -> Option<u32> {
        if self.local_fd < 0 {
            return None;
        }
        let started_at = self.position_started_at_ms?;
        let elapsed_ms = monotonic_ms()?.saturating_sub(started_at);
        Some(
            self.position_base_ms
                .saturating_add(elapsed_ms.min(u64::from(u32::MAX)) as u32),
        )
    }

    pub fn abort_local(&mut self) {
        let _ = AudioSink::stop(self);
        self.close_local();
    }

    fn close_local(&mut self) {
        if self.local_fd >= 0 {
            let _ = unsafe { nuttx_close(self.local_fd) };
            self.local_fd = -1;
            self.position_base_ms = 0;
            self.position_started_at_ms = None;
        }
    }
}

impl AudioSink for AudioDevice {
    type Error = i32;

    fn configure_mp3(&mut self) -> Result<(), Self::Error> {
        let format = AudioFormatV1 {
            struct_size: core::mem::size_of::<AudioFormatV1>() as u32,
            format: FORMAT_MP3,
            sample_rate_hint: 0,
            channels_hint: 0,
            flags: 0,
            reserved: [0; 3],
        };
        self.ioctl_value(IOC_SET_FORMAT, core::ptr::addr_of!(format) as usize)
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_START)?;
        self.position_started_at_ms = monotonic_ms();
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error> {
        self.last_stage = STAGE_AUDIO_WRITE;
        let fd = self.ensure_open()?;
        let count = u32::try_from(bytes.len()).map_err(|_| -1)?;
        let result = unsafe { nuttx_write(fd, bytes.as_ptr().cast::<c_void>(), count) };
        if result == EAGAIN {
            Ok(0)
        } else if result == -1 {
            // NuttX reports a driver's negative errno as -1 and stores the
            // actual value in the task errno slot. Read it before issuing the
            // diagnostic GET_STATUS ioctl, which would overwrite that slot.
            let write_errno = unsafe { get_errno() };
            if write_errno == 11 {
                Ok(0)
            } else if write_errno > 0 {
                Err(-write_errno)
            } else {
                classify_generic_write_failure(self.status().ok())
            }
        } else if result < 0 {
            Err(result)
        } else {
            Ok(result as usize)
        }
    }

    fn pause(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_PAUSE)?;
        self.freeze_position_clock();
        Ok(())
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_RESUME)?;
        self.position_started_at_ms = monotonic_ms();
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_STOP)?;
        self.position_base_ms = 0;
        self.position_started_at_ms = None;
        Ok(())
    }

    fn drain(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_DRAIN)
    }

    fn set_volume(&mut self, percent: u8) -> Result<(), Self::Error> {
        let volume = u32::from(percent.min(100));
        self.ioctl_value(IOC_SET_VOLUME, core::ptr::addr_of!(volume) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mpeg1_layer3_frame_header() {
        let frame = parse_mp3_frame([0xff, 0xfb, 0x90, 0x64]).unwrap();
        assert_eq!(frame.bytes, 417);
        assert_eq!(frame.samples, 1_152);
        assert_eq!(frame.sample_rate, 44_100);
    }

    #[test]
    fn generic_write_failure_is_transient_only_for_live_input_states() {
        for state in [
            AUDIO_STATE_CONFIGURED,
            AUDIO_STATE_BUFFERING,
            AUDIO_STATE_PLAYING,
            AUDIO_STATE_PAUSED,
        ] {
            let status = AudioStatusV1 {
                state,
                input_free: 0,
                ..AudioStatusV1::default()
            };
            assert_eq!(classify_generic_write_failure(Some(status)), Ok(0));
        }

        let failed = AudioStatusV1 {
            state: AUDIO_STATE_ERROR,
            last_error: -77,
            ..AudioStatusV1::default()
        };
        assert_eq!(classify_generic_write_failure(Some(failed)), Err(-77));
        assert_eq!(classify_generic_write_failure(None), Err(-1));
    }

    #[test]
    fn rejects_non_layer3_and_invalid_bitrate() {
        assert!(parse_mp3_frame([0xff, 0xfd, 0x90, 0x64]).is_none());
        assert!(parse_mp3_frame([0xff, 0xfb, 0x00, 0x64]).is_none());
    }
}
