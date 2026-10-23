//! What the pages show, derived from the app state. The device backend diffs a
//! fresh view against the last drawn one and touches only what changed.

use alloc::{format, string::String};

use crate::{
    LyraApp, Palette, Song,
    playback::{PlaybackMode, PlaybackState},
};

/// `m:ss`, or `h:mm:ss` from an hour on.
pub fn clock(ms: u32) -> String {
    let seconds = ms / 1_000;
    let (hours, minutes, seconds) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{}:{seconds:02}", minutes)
    }
}

/// Playback state as the player shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Nothing is selected.
    Idle,
    /// A track is being opened or buffered; the play button already shows
    /// "pause" because the user has asked for playback.
    Loading,
    Playing,
    Paused,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerView {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub palette: Palette,
    pub cover: Option<String>,
    pub transport: Transport,
    pub elapsed: String,
    pub remaining: String,
    /// 0..=1000 of the track.
    pub progress: u32,
    pub has_previous: bool,
    pub has_next: bool,
    pub mode: PlaybackMode,
    pub volume: u8,
    pub error: Option<String>,
}

impl PlayerView {
    pub fn of(app: &LyraApp) -> Option<Self> {
        let song: &Song = app.player.current.as_ref()?;
        let position = app.player.position_ms;
        let duration = app.player.duration_ms.max(song.duration_ms);
        let transport = match app.player.state {
            PlaybackState::Idle | PlaybackState::Finished => Transport::Idle,
            PlaybackState::Resolving | PlaybackState::Buffering => Transport::Loading,
            PlaybackState::Playing | PlaybackState::Draining => Transport::Playing,
            PlaybackState::Paused => Transport::Paused,
            PlaybackState::Failed => Transport::Failed,
        };
        let progress = if duration == 0 {
            0
        } else {
            (u64::from(position.min(duration)) * 1_000 / u64::from(duration)) as u32
        };
        Some(Self {
            id: song.id,
            title: song.name.clone(),
            artist: song.artist_line(),
            palette: song.palette,
            cover: song.cover_path(),
            transport,
            elapsed: clock(position),
            remaining: if duration == 0 {
                String::from("-:--")
            } else {
                format!("-{}", clock(duration.saturating_sub(position)))
            },
            progress,
            has_previous: app.has_previous(),
            has_next: app.has_next(),
            mode: app.mode,
            volume: app.player.volume_percent,
            error: app.error.clone(),
        })
    }
}

/// The text of the player-mode button, for the accessibility-less band this is
/// only used by tests and the volume toast.
pub fn mode_name(mode: PlaybackMode) -> &'static str {
    mode.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, persistence::Library};

    fn app_playing(position_ms: u32, duration_ms: u32) -> LyraApp {
        let song = Song {
            id: 1,
            name: "Name".into(),
            artists: alloc::vec!["A".into(), "B".into()],
            duration_ms,
            ..Song::default()
        };
        let mut app = LyraApp::default();
        app.update(Action::Boot(Library {
            revision: 1,
            tracks: alloc::vec![song.clone()],
        }));
        app.update(Action::SelectSong(song));
        app.player.state = PlaybackState::Playing;
        app.player.position_ms = position_ms;
        app.player.duration_ms = duration_ms;
        app
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(59_999), "0:59");
        assert_eq!(clock(61_000), "1:01");
        assert_eq!(clock(3_725_000), "1:02:05");
    }

    #[test]
    fn view_reports_progress_and_remaining_time() {
        let view = PlayerView::of(&app_playing(30_000, 120_000)).unwrap();
        assert_eq!(view.progress, 250);
        assert_eq!(view.elapsed, "0:30");
        assert_eq!(view.remaining, "-1:30");
        assert_eq!(view.artist, "A / B");
        assert_eq!(view.transport, Transport::Playing);
    }

    #[test]
    fn unknown_duration_and_overshoot_are_safe() {
        let view = PlayerView::of(&app_playing(5_000, 0)).unwrap();
        assert_eq!((view.progress, view.remaining.as_str()), (0, "-:--"));
        let view = PlayerView::of(&app_playing(500_000, 120_000)).unwrap();
        assert_eq!(view.progress, 1_000);
        assert_eq!(view.remaining, "-0:00");
    }

    #[test]
    fn loading_states_show_as_playing_intent() {
        let mut app = app_playing(0, 1_000);
        app.player.state = PlaybackState::Resolving;
        assert_eq!(PlayerView::of(&app).unwrap().transport, Transport::Loading);
        app.player.state = PlaybackState::Paused;
        assert_eq!(PlayerView::of(&app).unwrap().transport, Transport::Paused);
        assert!(PlayerView::of(&LyraApp::default()).is_none());
    }
}
