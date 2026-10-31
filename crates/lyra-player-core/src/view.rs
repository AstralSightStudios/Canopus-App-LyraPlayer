//! What the pages show, derived from the app state. The device backend diffs a
//! fresh view against the last drawn one and touches only what changed.

use alloc::{format, string::String, vec::Vec};

use crate::{
    Collection, LyraApp, Palette, Song,
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

impl Transport {
    pub fn of(state: PlaybackState) -> Self {
        match state {
            PlaybackState::Idle | PlaybackState::Finished => Self::Idle,
            PlaybackState::Resolving | PlaybackState::Buffering => Self::Loading,
            PlaybackState::Playing | PlaybackState::Draining => Self::Playing,
            PlaybackState::Paused => Self::Paused,
            PlaybackState::Failed => Self::Failed,
        }
    }
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
        let transport = Transport::of(app.player.state);
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

/// What tapping a list row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowTarget {
    Song(u64),
    Collection(Collection),
}

/// One row of the home page or of a collection page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub target: RowTarget,
    pub title: String,
    pub subtitle: String,
    /// The track whose cover this row shows: the song itself, or for a collection the first of its
    /// songs that has one. `None` shows the plain placeholder.
    pub art: Option<u64>,
}

pub const ALL_SONGS: &str = "全部歌曲";

/// "全部歌曲" first, then every playlist in creation order. Playlists are optional: with
/// none, the home page is just the one row.
pub fn overview_rows(app: &LyraApp) -> Vec<ListRow> {
    let mut rows = Vec::with_capacity(1 + app.library.playlists.len());
    rows.push(ListRow {
        target: RowTarget::Collection(Collection::All),
        title: String::from(ALL_SONGS),
        subtitle: format!("{} 首歌曲", app.library.tracks.len()),
        art: first_cover(app, &app.library.collection(None)),
    });
    for list in &app.library.playlists {
        let members = app.library.collection(Some(list.id));
        rows.push(ListRow {
            target: RowTarget::Collection(Collection::Playlist(list.id)),
            title: list.name.clone(),
            subtitle: format!("{} 首歌曲", members.len()),
            art: first_cover(app, &members),
        });
    }
    rows
}

fn first_cover(app: &LyraApp, ids: &[u64]) -> Option<u64> {
    ids.iter().copied().find(|id| {
        app.library
            .tracks
            .iter()
            .any(|song| song.id == *id && song.assets.has(crate::Assets::COVER))
    })
}

/// The title and song rows of `app.collection`; a playlist keeps its own order.
pub fn collection_rows(app: &LyraApp) -> (String, Vec<ListRow>) {
    let (title, ids) = match app.collection {
        Collection::Playlist(id) => match app.library.playlists.iter().find(|list| list.id == id) {
            Some(list) => (list.name.clone(), app.library.collection(Some(id))),
            None => (String::from(ALL_SONGS), app.library.collection(None)),
        },
        _ => (String::from(ALL_SONGS), app.library.collection(None)),
    };
    let rows = ids
        .iter()
        .filter_map(|id| app.library.tracks.iter().find(|song| song.id == *id))
        .map(|song| ListRow {
            target: RowTarget::Song(song.id),
            title: song.name.clone(),
            subtitle: song.artist_line(),
            art: song.assets.has(crate::Assets::COVER).then_some(song.id),
        })
        .collect();
    (title, rows)
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
            ..Library::default()
        }));
        app.update(Action::SelectSong(song));
        app.player.state = PlaybackState::Playing;
        app.player.position_ms = position_ms;
        app.player.duration_ms = duration_ms;
        app
    }

    #[test]
    fn home_lists_all_songs_then_playlists_and_pages_show_their_own_order() {
        use crate::{
            Action, Assets,
            persistence::{Library, Playlist},
        };
        let song = |id: u64| Song {
            id,
            name: format!("S{id}"),
            assets: Assets(Assets::COVER),
            ..Song::default()
        };
        let mut app = LyraApp::default();
        app.update(Action::Boot(Library {
            revision: 1,
            tracks: (1..=3).map(song).collect(),
            playlists: alloc::vec![
                Playlist {
                    id: 9,
                    name: "夜间".into(),
                    tracks: alloc::vec![3, 1, 77]
                },
                Playlist {
                    id: 10,
                    name: "空".into(),
                    tracks: Vec::new()
                },
            ],
        }));
        let home = overview_rows(&app);
        assert_eq!(home.len(), 3);
        assert_eq!(home[0].title, ALL_SONGS);
        assert_eq!(home[0].subtitle, "3 首歌曲");
        assert_eq!(
            home[1].target,
            RowTarget::Collection(Collection::Playlist(9))
        );
        // A track that is no longer in the library (77) is not counted.
        assert_eq!(home[1].subtitle, "2 首歌曲");
        assert_eq!(home[2].subtitle, "0 首歌曲");

        app.update(Action::OpenCollection(Collection::Playlist(9)));
        assert_eq!(app.route, crate::Route::Collection);
        let (title, rows) = collection_rows(&app);
        assert_eq!(title, "夜间");
        assert_eq!(
            rows.iter().map(|row| row.target).collect::<Vec<_>>(),
            [RowTarget::Song(3), RowTarget::Song(1)]
        );
        assert!(rows.iter().all(|row| row.art
            == Some(match row.target {
                RowTarget::Song(id) => id,
                RowTarget::Collection(_) => unreachable!(),
            })));
        // A collection shows the cover of its first song that has one: song 3 for the playlist,
        // the first track for all songs, and nothing for the empty playlist.
        assert_eq!(home[0].art, Some(1));
        assert_eq!(home[1].art, Some(3));
        assert_eq!(home[2].art, None);
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
