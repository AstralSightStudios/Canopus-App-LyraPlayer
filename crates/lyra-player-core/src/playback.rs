use alloc::{collections::VecDeque, string::String, vec::Vec};

use crate::Song;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Idle,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Draining,
    /// The queue played through to the end and nothing else is available.
    /// Distinct from `Idle`, which is the state before anything was chosen.
    Finished,
    Failed,
}

/// How the player picks the track that follows the current one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackMode {
    /// Play the library in order and stop after the last track.
    #[default]
    ListOnce,
    /// Play in order and wrap around at either end.
    RepeatAll,
    /// Pick another track pseudo-randomly.
    Shuffle,
}

impl PlaybackMode {
    pub fn next(self) -> Self {
        match self {
            Self::ListOnce => Self::RepeatAll,
            Self::RepeatAll => Self::Shuffle,
            Self::Shuffle => Self::ListOnce,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ListOnce => "列表播放",
            Self::RepeatAll => "循环播放",
            Self::Shuffle => "随机播放",
        }
    }
}

/// A media control a headset gesture asked for, as delivered by the
/// BluetoothAudio module's control-event queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaControl {
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub state: PlaybackState,
    pub current: Option<Song>,
    pub queue: VecDeque<Song>,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub volume_percent: u8,
    pub stream_id: Option<String>,
    pub error: Option<String>,
    pending_audio: VecDeque<Vec<u8>>,
    pending_offset: usize,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            state: PlaybackState::Idle,
            current: None,
            queue: VecDeque::new(),
            position_ms: 0,
            duration_ms: 0,
            volume_percent: 100,
            stream_id: None,
            error: None,
            pending_audio: VecDeque::new(),
            pending_offset: 0,
        }
    }
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
        self.prebuffered_stream_started(id);
        Ok(())
    }

    pub fn prebuffered_stream_started(&mut self, id: String) {
        self.prebuffered_stream_started_at(id, 0);
    }

    pub fn prebuffered_stream_started_at(&mut self, id: String, position_ms: u32) {
        self.stream_id = Some(id);
        self.pending_audio.clear();
        self.pending_offset = 0;
        self.position_ms = position_ms;
        self.state = PlaybackState::Buffering;
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

    pub fn stream_ended<S: AudioSink>(&mut self, sink: &mut S) -> Result<bool, S::Error> {
        if !self.flush_audio(sink)? {
            return Ok(false);
        }
        sink.drain()?;
        self.state = PlaybackState::Draining;
        Ok(true)
    }

    /// Whether a headset play/pause gesture would change anything right now.
    ///
    /// A headset asks for an absolute state rather than a toggle, so a gesture
    /// that matches what the player is already doing is ignored instead of
    /// bouncing playback. Track changes are not decided here; they depend on
    /// the queue and belong to the app.
    pub fn transport_control_applies(&self, control: MediaControl) -> bool {
        match control {
            MediaControl::Play => self.state == PlaybackState::Paused,
            MediaControl::Pause => {
                matches!(
                    self.state,
                    PlaybackState::Playing | PlaybackState::Buffering
                )
            }
            MediaControl::Next | MediaControl::Previous => false,
        }
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
            self.position_ms = if self.duration_ms == 0 {
                self.position_ms.saturating_add(elapsed_ms)
            } else {
                self.position_ms
                    .saturating_add(elapsed_ms)
                    .min(self.duration_ms)
            };
        }
    }

    pub fn sync_position(&mut self, position_ms: u32) -> bool {
        let position_ms = if self.duration_ms == 0 {
            position_ms
        } else {
            position_ms.min(self.duration_ms)
        };
        let visible_changed = self.position_ms / 500 != position_ms / 500;
        self.position_ms = position_ms;
        visible_changed
    }

    pub fn set_duration(&mut self, duration_ms: u32) {
        self.duration_ms = duration_ms;
        if let Some(current) = &mut self.current {
            current.duration_ms = duration_ms;
        }
        self.position_ms = self.position_ms.min(duration_ms);
    }

    pub fn sync_volume(&mut self, percent: u8) -> bool {
        let percent = percent.min(100);
        if self.volume_percent == percent {
            return false;
        }
        self.volume_percent = percent;
        true
    }

    pub fn set_volume<S: AudioSink>(&mut self, percent: u8, sink: &mut S) -> Result<(), S::Error> {
        let percent = percent.min(100);
        sink.set_volume(percent)?;
        self.volume_percent = percent;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.state = PlaybackState::Idle;
        self.current = None;
        self.queue.clear();
        self.position_ms = 0;
        self.duration_ms = 0;
        self.stream_id = None;
        self.error = None;
        self.pending_audio.clear();
        self.pending_offset = 0;
    }

    pub fn take_next(&mut self) -> Option<Song> {
        self.queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headset_play_applies_only_to_a_paused_stream() {
        let mut player = Player::default();
        for state in [
            PlaybackState::Idle,
            PlaybackState::Resolving,
            PlaybackState::Buffering,
            PlaybackState::Playing,
            PlaybackState::Draining,
            PlaybackState::Failed,
            PlaybackState::Finished,
        ] {
            player.state = state;
            assert!(
                !player.transport_control_applies(MediaControl::Play),
                "play must not act in {state:?}"
            );
        }
        player.state = PlaybackState::Paused;
        assert!(player.transport_control_applies(MediaControl::Play));
    }

    #[test]
    fn headset_pause_applies_only_while_audio_is_running() {
        let mut player = Player::default();
        for state in [PlaybackState::Playing, PlaybackState::Buffering] {
            player.state = state;
            assert!(
                player.transport_control_applies(MediaControl::Pause),
                "pause must act in {state:?}"
            );
        }
        for state in [
            PlaybackState::Idle,
            PlaybackState::Resolving,
            PlaybackState::Paused,
            PlaybackState::Draining,
            PlaybackState::Failed,
            PlaybackState::Finished,
        ] {
            player.state = state;
            assert!(
                !player.transport_control_applies(MediaControl::Pause),
                "pause must not act in {state:?}"
            );
        }
    }

    #[test]
    fn repeated_headset_gesture_is_idempotent_through_toggle() {
        let mut sink = ShortSink {
            max_write: 16,
            ..ShortSink::default()
        };
        let mut player = Player {
            state: PlaybackState::Playing,
            ..Player::default()
        };

        assert!(player.transport_control_applies(MediaControl::Pause));
        player.toggle(&mut sink).unwrap();
        assert_eq!(player.state, PlaybackState::Paused);
        // A second pause gesture must be ignored rather than resuming.
        assert!(!player.transport_control_applies(MediaControl::Pause));

        assert!(player.transport_control_applies(MediaControl::Play));
        player.toggle(&mut sink).unwrap();
        assert_eq!(player.state, PlaybackState::Playing);
        assert!(!player.transport_control_applies(MediaControl::Play));
    }

    #[test]
    fn track_change_gestures_are_not_decided_by_the_player() {
        let mut player = Player::default();
        for state in [
            PlaybackState::Playing,
            PlaybackState::Paused,
            PlaybackState::Idle,
            PlaybackState::Finished,
        ] {
            player.state = state;
            assert!(!player.transport_control_applies(MediaControl::Next));
            assert!(!player.transport_control_applies(MediaControl::Previous));
        }
    }

    #[derive(Default)]
    struct ShortSink {
        bytes: Vec<u8>,
        max_write: usize,
        volume_percent: u8,
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
        fn set_volume(&mut self, percent: u8) -> Result<(), Self::Error> {
            self.volume_percent = percent;
            Ok(())
        }
    }

    #[test]
    fn stream_end_waits_for_pending_short_writes() {
        let mut player = Player {
            state: PlaybackState::Buffering,
            ..Player::default()
        };
        let mut sink = ShortSink {
            max_write: 2,
            ..ShortSink::default()
        };
        assert!(!player.push_audio(b"abcde".to_vec(), &mut sink).unwrap());
        assert!(!player.stream_ended(&mut sink).unwrap());
        assert_eq!(player.state, PlaybackState::Buffering);
        assert!(player.stream_ended(&mut sink).unwrap());
        assert_eq!(sink.bytes, b"abcde");
        assert_eq!(player.state, PlaybackState::Draining);
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

    #[test]
    fn volume_is_clamped_and_sent_to_sink() {
        let mut player = Player::default();
        let mut sink = ShortSink::default();
        player.set_volume(110, &mut sink).unwrap();
        assert_eq!(player.volume_percent, 100);
        assert_eq!(sink.volume_percent, 100);
        player.set_volume(40, &mut sink).unwrap();
        assert_eq!(player.volume_percent, 40);
        assert_eq!(sink.volume_percent, 40);
    }

    #[test]
    fn discovered_duration_updates_current_song_and_clamps_position() {
        let mut player = Player {
            current: Some(Song {
                duration_ms: 0,
                ..Song::default()
            }),
            position_ms: 9_000,
            ..Player::default()
        };
        player.set_duration(5_000);
        assert_eq!(player.duration_ms, 5_000);
        assert_eq!(player.current.as_ref().unwrap().duration_ms, 5_000);
        assert_eq!(player.position_ms, 5_000);
    }

    #[test]
    fn external_volume_sync_only_reports_real_changes() {
        let mut player = Player::default();
        assert!(!player.sync_volume(100));
        assert!(player.sync_volume(41));
        assert_eq!(player.volume_percent, 41);
        assert!(!player.sync_volume(41));
    }

    #[test]
    fn driver_position_advances_when_duration_is_unknown() {
        let mut player = Player {
            state: PlaybackState::Playing,
            duration_ms: 0,
            ..Player::default()
        };
        assert!(player.sync_position(1_250));
        assert_eq!(player.position_ms, 1_250);
        player.tick(500);
        assert_eq!(player.position_ms, 1_750);
    }
}
