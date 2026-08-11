use alloc::{collections::VecDeque, string::String, vec::Vec};

use crate::{Song, lyrics::Lyrics};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Idle,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Draining,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct Player {
    pub state: PlaybackState,
    pub current: Option<Song>,
    pub queue: VecDeque<Song>,
    pub lyrics: Lyrics,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub stream_id: Option<String>,
    pub error: Option<String>,
    pending_audio: VecDeque<Vec<u8>>,
    pending_offset: usize,
}

pub trait AudioSink {
    type Error;

    fn configure_mp3(&mut self) -> Result<(), Self::Error>;
    fn start(&mut self) -> Result<(), Self::Error>;
    fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error>;
    fn pause(&mut self) -> Result<(), Self::Error>;
    fn resume(&mut self) -> Result<(), Self::Error>;
    fn stop(&mut self) -> Result<(), Self::Error>;
    fn drain(&mut self) -> Result<(), Self::Error>;
    fn set_volume(&mut self, percent: u8) -> Result<(), Self::Error>;
}

impl Player {
    pub fn select(&mut self, song: Song, queue: impl IntoIterator<Item = Song>) {
        self.current = Some(song.clone());
        self.duration_ms = song.duration_ms;
        self.position_ms = 0;
        self.queue.clear();
        self.queue
            .extend(queue.into_iter().filter(|item| item.id != song.id));
        self.lyrics = Lyrics::default();
        self.stream_id = None;
        self.pending_audio.clear();
        self.pending_offset = 0;
        self.error = None;
        self.state = PlaybackState::Resolving;
    }

    pub fn stream_opened<S: AudioSink>(
        &mut self,
        id: String,
        sink: &mut S,
    ) -> Result<(), S::Error> {
        sink.stop()?;
        sink.configure_mp3()?;
        sink.start()?;
        self.stream_id = Some(id);
        self.state = PlaybackState::Buffering;
        Ok(())
    }

    pub fn push_audio<S: AudioSink>(
        &mut self,
        bytes: Vec<u8>,
        sink: &mut S,
    ) -> Result<bool, S::Error> {
        if !bytes.is_empty() {
            self.pending_audio.push_back(bytes);
        }
        self.flush_audio(sink)
    }

    pub fn flush_audio<S: AudioSink>(&mut self, sink: &mut S) -> Result<bool, S::Error> {
        while let Some(front) = self.pending_audio.front() {
            let remaining = &front[self.pending_offset..];
            let accepted = sink.write(remaining)?;
            if accepted == 0 {
                return Ok(false);
            }
            self.pending_offset += accepted;
            if self.pending_offset < front.len() {
                return Ok(false);
            }
            self.pending_audio.pop_front();
            self.pending_offset = 0;
        }
        if self.state == PlaybackState::Buffering {
            self.state = PlaybackState::Playing;
        }
        Ok(true)
    }

    pub fn stream_ended<S: AudioSink>(&mut self, sink: &mut S) -> Result<(), S::Error> {
        let _ = self.flush_audio(sink)?;
        sink.drain()?;
        self.state = PlaybackState::Draining;
        Ok(())
    }

    pub fn toggle<S: AudioSink>(&mut self, sink: &mut S) -> Result<(), S::Error> {
        match self.state {
            PlaybackState::Playing | PlaybackState::Buffering => {
                sink.pause()?;
                self.state = PlaybackState::Paused;
            }
            PlaybackState::Paused => {
                sink.resume()?;
                self.state = PlaybackState::Playing;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn stop<S: AudioSink>(&mut self, sink: &mut S) -> Result<(), S::Error> {
        sink.stop()?;
        self.stream_id = None;
        self.pending_audio.clear();
        self.pending_offset = 0;
        self.state = PlaybackState::Idle;
        Ok(())
    }

    pub fn tick(&mut self, elapsed_ms: u32) {
        if self.state == PlaybackState::Playing {
            self.position_ms = self
                .position_ms
                .saturating_add(elapsed_ms)
                .min(self.duration_ms);
        }
    }

    pub fn take_next(&mut self) -> Option<Song> {
        self.queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ShortSink {
        bytes: Vec<u8>,
        max_write: usize,
    }
    impl AudioSink for ShortSink {
        type Error = ();
        fn configure_mp3(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn start(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error> {
            let count = bytes.len().min(self.max_write);
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn pause(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn resume(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn stop(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn drain(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn set_volume(&mut self, _: u8) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn retains_short_writes_until_flushed() {
        let mut player = Player {
            state: PlaybackState::Buffering,
            ..Player::default()
        };
        let mut sink = ShortSink {
            max_write: 2,
            ..ShortSink::default()
        };
        assert!(!player.push_audio(b"abcde".to_vec(), &mut sink).unwrap());
        assert!(!player.flush_audio(&mut sink).unwrap());
        assert!(player.flush_audio(&mut sink).unwrap());
        assert_eq!(sink.bytes, b"abcde");
        assert_eq!(player.state, PlaybackState::Playing);
    }
}
