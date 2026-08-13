//! Client for the BluetoothAudio module's `/dev/canopus_audio` character
//! device. The fd is exclusively owned by the resident Lyra core.

use alloc::{string::String, vec::Vec};
use core::ffi::c_void;

use canopus_target_private::{
    O_RDONLY, O_RDWR, nuttx_close, nuttx_ioctl, nuttx_open, nuttx_read, nuttx_write,
};
use lyra_player_core::playback::{AudioSink, Player};

const DEVICE_PATH: &[u8] = b"/dev/canopus_audio\0";
const FORMAT_MP3: u32 = 1;
const IOC_SET_FORMAT: u32 = 0x4341_0002;
const IOC_START: u32 = 0x4341_0003;
const IOC_PAUSE: u32 = 0x4341_0004;
const IOC_RESUME: u32 = 0x4341_0005;
const IOC_STOP: u32 = 0x4341_0006;
const IOC_DRAIN: u32 = 0x4341_0007;
const IOC_SET_VOLUME: u32 = 0x4341_000A;

#[repr(C)]
struct AudioFormatV1 {
    struct_size: u32,
    format: u32,
    sample_rate_hint: u32,
    channels_hint: u32,
    flags: u32,
    reserved: [u32; 3],
}

pub struct AudioDevice {
    fd: i32,
    local_fd: i32,
}

impl AudioDevice {
    pub const fn new() -> Self {
        Self {
            fd: -1,
            local_fd: -1,
        }
    }

    fn ensure_open(&mut self) -> Result<i32, i32> {
        if self.fd >= 0 {
            return Ok(self.fd);
        }
        let fd = unsafe { nuttx_open(DEVICE_PATH.as_ptr(), O_RDWR) };
        if fd < 0 {
            return Err(fd);
        }
        self.fd = fd;
        Ok(fd)
    }

    fn ioctl_value(&mut self, command: u32, argument: usize) -> Result<(), i32> {
        let fd = self.ensure_open()?;
        let result = unsafe { nuttx_ioctl(fd, command, argument) };
        if result < 0 { Err(result) } else { Ok(()) }
    }

    fn ioctl(&mut self, command: u32) -> Result<(), i32> {
        self.ioctl_value(command, 0)
    }

    pub fn stop_local(&mut self) {
        self.close_local();
    }

    pub fn start_local(&mut self, path: &str, player: &mut Player) -> Result<(), i32> {
        self.close_local();
        let mut c_path = Vec::with_capacity(path.len() + 1);
        c_path.extend_from_slice(path.as_bytes());
        c_path.push(0);
        let fd = unsafe { nuttx_open(c_path.as_ptr(), O_RDONLY) };
        if fd < 0 {
            return Err(fd);
        }
        self.local_fd = fd;
        if let Err(error) = player.stream_opened(String::from("local"), self) {
            self.close_local();
            return Err(error);
        }
        Ok(())
    }

    pub fn pump_local(&mut self, player: &mut Player) -> Result<(), i32> {
        if self.local_fd < 0 || !player.flush_audio(self)? {
            return Ok(());
        }
        let mut chunk = [0u8; 2048];
        let count = unsafe {
            nuttx_read(
                self.local_fd,
                chunk.as_mut_ptr().cast::<c_void>(),
                chunk.len() as u32,
            )
        };
        if count < 0 {
            self.close_local();
            return Err(count);
        }
        if count == 0 {
            if player.stream_ended(self)? {
                self.close_local();
            }
            return Ok(());
        }
        let _ = player.push_audio(Vec::from(&chunk[..count as usize]), self)?;
        Ok(())
    }

    fn close_local(&mut self) {
        if self.local_fd >= 0 {
            let _ = unsafe { nuttx_close(self.local_fd) };
            self.local_fd = -1;
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
        self.ioctl(IOC_START)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error> {
        let fd = self.ensure_open()?;
        let count = u32::try_from(bytes.len()).map_err(|_| -1)?;
        let result = unsafe { nuttx_write(fd, bytes.as_ptr().cast::<c_void>(), count) };
        if result < 0 {
            Err(result)
        } else {
            Ok(result as usize)
        }
    }

    fn pause(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_PAUSE)
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_RESUME)
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_STOP)
    }

    fn drain(&mut self) -> Result<(), Self::Error> {
        self.ioctl(IOC_DRAIN)
    }

    fn set_volume(&mut self, percent: u8) -> Result<(), Self::Error> {
        let volume = u32::from(percent.min(100));
        self.ioctl_value(IOC_SET_VOLUME, core::ptr::addr_of!(volume) as usize)
    }
}
