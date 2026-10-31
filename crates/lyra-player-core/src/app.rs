use alloc::{string::String, vec::Vec};

use crate::{
    Song,
    lyrics::Lyrics,
    persistence::Library,
    playback::{PlaybackMode, PlaybackState, Player},
};

/// Cheap deterministic spread for shuffle. The device has no RNG we can rely
/// on, so the step is derived from the track id and the library generation:
/// stable within a generation, different after the library is rebuilt.
fn shuffle_noise(id: u64, generation: u32) -> u32 {
    let mut x = (id as u32) ^ generation.rotate_left(13) ^ ((id >> 32) as u32);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// The home page: "all songs" and the playlists.
    #[default]
    Library,
    Player,
    /// The songs of `LyraApp::collection`.
    Collection,
}

impl Route {
    pub const fn page_index(self) -> usize {
        match self {
            Self::Library => 0,
            Self::Player => 1,
            Self::Collection => 2,
        }
    }

    pub const fn from_page_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Library),
            1 => Some(Self::Player),
            2 => Some(Self::Collection),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Effect {
    StreamAudio { path: String },
    StopAudio,
    LoadLyrics { id: u64, path: String },
    Navigate(Route),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Collection {
    #[default]
    Overview,
    All,
    Playlist(u64),
}

#[derive(Clone, Debug)]
pub enum Action {
    Boot(Library),
    Open(Route),
    Back,
    ReloadLibrary(Library),
    SelectSong(Song),
    Browse(Collection),
    /// Show the songs of a collection on their own page.
    OpenCollection(Collection),
    PlayCollection {
        playlist: Option<u64>,
        mode: PlaybackMode,
    },
    /// Start a track without leaving the current page (remote control).
    Play(Song),
    Previous,
    Next,
    /// The audio driver, not the file reader, has finished draining the track.
    TrackEnded,
    Tick(u32),
    /// Advance the repeat/shuffle mode by one step.
    CycleMode,
    /// The queue ran out with nothing left to play.
    PlaybackExhausted,
    /// The lyrics file of track `id` was read; `None` when it has none.
    LyricsLoaded {
        id: u64,
        lyrics: Option<Lyrics>,
    },
}

#[derive(Clone, Debug)]
pub struct LyraApp {
    pub route: Route,
    pub history: Vec<Route>,
    pub library: Library,
    pub player: Player,
    pub error: Option<String>,
    pub generation: u32,
    pub mode: PlaybackMode,
    pub collection: Collection,
    /// A snapshot of the selected collection, never the whole library by accident.
    pub queue: Vec<u64>,
    shuffle_seed: u32,
    /// Lyrics of the current track, with the id they belong to so a late
    /// answer for a skipped track is dropped.
    pub lyrics: Lyrics,
    pub lyrics_for: Option<u64>,
}

impl Default for LyraApp {
    fn default() -> Self {
        Self {
            route: Route::Library,
            history: Vec::new(),
            library: Library::default(),
            player: Player::default(),
            error: None,
            generation: 1,
            mode: PlaybackMode::default(),
            collection: Collection::Overview,
            queue: Vec::new(),
            shuffle_seed: 0x6d2b_79f5,
            lyrics: Lyrics::default(),
            lyrics_for: None,
        }
    }
}

impl LyraApp {
    pub fn tracks(&self) -> &[Song] {
        &self.library.tracks
    }

    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        match action {
            Action::Boot(library) => {
                if self.library == library {
                    return effects;
                }
                self.library = library;
            }
            Action::ReloadLibrary(library) => {
                if self.library == library {
                    return effects;
                }
                let current_id = self.player.current.as_ref().map(|song| song.id);
                let current_removed =
                    current_id.is_some_and(|id| !library.tracks.iter().any(|song| song.id == id));
                self.library = library;
                self.queue
                    .retain(|id| self.library.tracks.iter().any(|song| song.id == *id));
                if let Collection::Playlist(id) = self.collection {
                    if !self.library.playlists.iter().any(|list| list.id == id) {
                        self.collection = Collection::Overview;
                    }
                }
                if current_removed {
                    self.player.clear();
                    self.lyrics = Lyrics::default();
                    self.lyrics_for = None;
                    self.error = Some(String::from("当前歌曲已从音乐库删除"));
                    effects.push(Effect::StopAudio);
                } else if let Some(id) = current_id {
                    if let Some(updated) = self.library.tracks.iter().find(|song| song.id == id) {
                        self.player.current = Some(updated.clone());
                        self.player.duration_ms = updated.duration_ms;
                    }
                }
            }
            Action::Open(route) => self.navigate(route, &mut effects),
            Action::Back => {
                if let Some(route) = self.history.pop() {
                    self.route = route;
                }
            }
            Action::SelectSong(song) => {
                let playlist = match self.collection {
                    Collection::Playlist(id) => Some(id),
                    _ => None,
                };
                self.queue = self.library.collection(playlist);
                if !self.queue.contains(&song.id) {
                    return effects;
                }
                self.navigate(Route::Player, &mut effects);
                self.play(song, &mut effects);
            }
            Action::Browse(collection) => {
                if !self.has_collection(collection) {
                    return effects;
                }
                self.collection = collection;
            }
            Action::OpenCollection(collection) => {
                if collection == Collection::Overview || !self.has_collection(collection) {
                    return effects;
                }
                self.collection = collection;
                self.navigate(Route::Collection, &mut effects);
            }
            Action::PlayCollection { playlist, mode } => {
                let queue = self.library.collection(playlist);
                if queue.is_empty() {
                    return effects;
                }
                self.queue = queue;
                self.mode = mode;
                let index = if mode == PlaybackMode::Shuffle {
                    self.shuffle_seed as usize % self.queue.len()
                } else {
                    0
                };
                if let Some(song) = self
                    .library
                    .tracks
                    .iter()
                    .find(|song| song.id == self.queue[index])
                    .cloned()
                {
                    self.navigate(Route::Player, &mut effects);
                    self.play(song, &mut effects);
                }
            }
            Action::Play(song) => {
                self.queue = self.library.collection(None);
                self.play(song, &mut effects);
            }
            // Track changes never navigate. Besides the player page's own
            // buttons, they come from headset gestures and from a track
            // finishing, both handled on the Bluetooth thread, where the page
            // manager must not be touched; the user may also be on another
            // page or outside the app entirely.
            Action::Previous => {
                if let Some(song) = self.adjacent_song(false).cloned() {
                    self.play(song, &mut effects);
                }
            }
            Action::Next => {
                if let Some(song) = self.adjacent_song(true).cloned() {
                    self.play(song, &mut effects);
                }
            }
            Action::TrackEnded => {
                let following = if self.mode == PlaybackMode::RepeatOne {
                    self.player.current.clone()
                } else {
                    self.adjacent_song(true).cloned()
                };
                if let Some(song) = following {
                    self.play(song, &mut effects);
                } else {
                    return self.update(Action::PlaybackExhausted);
                }
            }
            Action::Tick(ms) => self.player.tick(ms),
            Action::CycleMode => self.mode = self.mode.next(),
            Action::PlaybackExhausted => {
                self.player.clear();
                self.lyrics = Lyrics::default();
                self.lyrics_for = None;
                self.player.state = PlaybackState::Finished;
            }
            Action::LyricsLoaded { id, lyrics } => {
                if self.player.current.as_ref().map(|song| song.id) == Some(id) {
                    self.lyrics = lyrics.unwrap_or_default();
                    self.lyrics_for = Some(id);
                }
            }
        }
        self.touch();
        effects
    }

    fn has_collection(&self, collection: Collection) -> bool {
        match collection {
            Collection::Overview | Collection::All => true,
            Collection::Playlist(id) => self.library.playlists.iter().any(|list| list.id == id),
        }
    }

    pub fn has_previous(&self) -> bool {
        self.adjacent_song(false).is_some()
    }

    pub fn has_next(&self) -> bool {
        self.adjacent_song(true).is_some()
    }

    /// The line of the current track's lyrics being sung right now.
    pub fn active_lyric(&self) -> Option<usize> {
        if self.lyrics_for != self.player.current.as_ref().map(|song| song.id) {
            return None;
        }
        self.lyrics.active_index(self.player.position_ms)
    }

    fn adjacent_song(&self, next: bool) -> Option<&Song> {
        let id = self.player.current.as_ref()?.id;
        let tracks = &self.queue;
        let index = tracks.iter().position(|item| *item == id)?;
        let count = tracks.len();
        if count == 0 {
            return None;
        }
        let adjacent = match self.mode {
            PlaybackMode::ListOnce => {
                if next {
                    index.checked_add(1)?
                } else {
                    index.checked_sub(1)?
                }
            }
            PlaybackMode::RepeatAll | PlaybackMode::RepeatOne => {
                if next {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                }
            }
            PlaybackMode::Shuffle => {
                if count == 1 {
                    return self.player.current.as_ref();
                }
                // A whole shuffled ordering would have to survive library
                // reloads and page changes; stepping to another track by a
                // rotating offset keeps the queue stateless and never repeats
                // the track that is already playing.
                let step = 1 + (shuffle_noise(id, self.shuffle_seed) as usize) % (count - 1);
                (index + step) % count
            }
        };
        let adjacent = tracks.get(adjacent)?;
        self.library.tracks.iter().find(|song| song.id == *adjacent)
    }

    fn play(&mut self, song: Song, effects: &mut Vec<Effect>) {
        self.shuffle_seed = shuffle_noise(song.id, self.shuffle_seed).max(1);
        self.player.select(song.clone(), core::iter::empty());
        self.error = None;
        self.lyrics = Lyrics::default();
        self.lyrics_for = None;
        match song.lyrics_path() {
            Some(path) => effects.push(Effect::LoadLyrics { id: song.id, path }),
            None => self.lyrics_for = Some(song.id),
        }
        effects.push(Effect::StreamAudio {
            path: song.audio_path(),
        });
    }

    fn navigate(&mut self, route: Route, effects: &mut Vec<Effect>) {
        if self.route == route {
            return;
        }
        self.history.push(self.route);
        self.route = route;
        effects.push(Effect::Navigate(route));
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assets, lyrics::parse_lrc};

    fn song(id: u64) -> Song {
        Song {
            id,
            name: alloc::format!("Track {id}"),
            assets: Assets(Assets::LYRICS),
            ..Song::default()
        }
    }

    fn library(ids: &[u64]) -> Library {
        Library {
            revision: 1,
            tracks: ids.iter().map(|id| song(*id)).collect(),
            ..Library::default()
        }
    }

    fn streams(effects: &[Effect]) -> bool {
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::StreamAudio { .. }))
    }

    #[test]
    fn selecting_a_song_navigates_streams_and_asks_for_lyrics() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2])));
        let effects = app.update(Action::SelectSong(song(1)));
        assert!(streams(&effects));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Navigate(Route::Player)))
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadLyrics { id: 1, .. }))
        );
        assert_eq!(app.route, Route::Player);
    }

    #[test]
    fn previous_and_next_follow_library_order_without_wrapping() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[10, 20, 30])));
        app.update(Action::SelectSong(song(10)));
        assert!(!app.has_previous());
        assert!(app.has_next());
        assert!(app.update(Action::Previous).is_empty());
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(10));

        assert!(streams(&app.update(Action::Next)));
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(20));
        app.update(Action::Next);
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(30));
        assert!(!app.has_next());
        assert!(app.update(Action::Next).is_empty());
    }

    #[test]
    fn completed_track_can_still_select_previous_library_track() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2, 3])));
        app.update(Action::SelectSong(song(3)));
        app.player.state = PlaybackState::Draining;
        assert!(streams(&app.update(Action::Previous)));
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(2));
        assert_eq!(app.player.position_ms, 0);
        assert_eq!(app.player.state, PlaybackState::Resolving);
    }

    #[test]
    fn reloading_library_clears_deleted_current_song_and_stops_audio() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2])));
        app.update(Action::SelectSong(song(1)));
        app.player.state = PlaybackState::Playing;
        let effects = app.update(Action::ReloadLibrary(library(&[2])));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::StopAudio))
        );
        assert_eq!(app.player.state, PlaybackState::Idle);
        assert!(app.player.current.is_none());
        assert_eq!(app.tracks().len(), 1);
    }

    #[test]
    fn reloading_library_without_current_song_keeps_playback() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2])));
        app.update(Action::SelectSong(song(1)));
        app.player.state = PlaybackState::Playing;
        let reloaded = Library {
            revision: 2,
            ..library(&[1])
        };
        let effects = app.update(Action::ReloadLibrary(reloaded));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::StopAudio))
        );
        assert_eq!(app.player.state, PlaybackState::Playing);
    }

    /// The mode toggle lives on the player page, but a track ends while the
    /// user may be anywhere. The automatic advance runs on the Bluetooth thread
    /// and must not ask for navigation, which would drive the page manager from
    /// that thread.
    #[test]
    fn track_changes_in_every_mode_stay_on_the_current_page() {
        for mode in [
            PlaybackMode::ListOnce,
            PlaybackMode::RepeatAll,
            PlaybackMode::Shuffle,
        ] {
            let mut app = LyraApp::default();
            app.update(Action::Boot(library(&[1, 2, 3])));
            app.update(Action::SelectSong(song(2)));
            app.update(Action::Back);
            app.mode = mode;
            assert_eq!(app.route, Route::Library);
            let history = app.history.clone();
            for action in [Action::Next, Action::Previous] {
                let effects = app.update(action);
                assert!(
                    !effects
                        .iter()
                        .any(|effect| matches!(effect, Effect::Navigate(_))),
                    "{mode:?} track change must not navigate"
                );
                assert!(streams(&effects), "{mode:?} track change must start audio");
                assert_eq!(app.route, Route::Library);
                assert_eq!(app.history, history);
            }
        }
    }

    #[test]
    fn repeat_wraps_and_shuffle_never_replays_the_current_track() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2, 3])));
        app.mode = PlaybackMode::RepeatAll;
        app.update(Action::SelectSong(song(3)));
        app.update(Action::Next);
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(1));
        app.update(Action::Previous);
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(3));

        app.mode = PlaybackMode::Shuffle;
        for _ in 0..32 {
            let before = app.player.current.as_ref().map(|song| song.id);
            assert!(app.has_next());
            app.update(Action::Next);
            assert_ne!(app.player.current.as_ref().map(|song| song.id), before);
        }
    }

    #[test]
    fn unchanged_library_does_not_invalidate_ui() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1])));
        let generation = app.generation;
        assert!(app.update(Action::ReloadLibrary(library(&[1]))).is_empty());
        assert_eq!(app.generation, generation);
    }

    #[test]
    fn lyrics_follow_the_position_and_drop_answers_for_skipped_tracks() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2])));
        app.update(Action::SelectSong(song(1)));
        assert_eq!(app.active_lyric(), None);

        let lyrics = parse_lrc("[00:01.00]a\n[00:05.00]b");
        app.update(Action::LyricsLoaded {
            id: 2,
            lyrics: Some(lyrics.clone()),
        });
        assert!(
            app.lyrics.is_empty(),
            "an answer for another track is dropped"
        );

        app.update(Action::LyricsLoaded {
            id: 1,
            lyrics: Some(lyrics),
        });
        app.player.position_ms = 6_000;
        assert_eq!(app.active_lyric(), Some(1));

        app.update(Action::Next);
        assert!(app.lyrics.is_empty());
        assert_eq!(app.active_lyric(), None);
    }

    #[test]
    fn single_repeat_is_eof_only_and_never_navigates() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2, 3])));
        app.update(Action::SelectSong(song(2)));
        app.update(Action::Back);
        app.mode = PlaybackMode::RepeatOne;
        app.player.position_ms = 50_000;
        let effects = app.update(Action::TrackEnded);
        assert!(streams(&effects));
        assert_eq!(app.player.current.as_ref().unwrap().id, 2);
        assert_eq!(app.player.position_ms, 0);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Navigate(_)))
        );
        app.update(Action::Next);
        assert_eq!(app.player.current.as_ref().unwrap().id, 3);
        app.update(Action::Previous);
        assert_eq!(app.player.current.as_ref().unwrap().id, 2);
    }

    #[test]
    fn playlist_replaces_queue_and_all_modes_stay_inside_it() {
        for mode in [
            PlaybackMode::ListOnce,
            PlaybackMode::RepeatAll,
            PlaybackMode::Shuffle,
            PlaybackMode::RepeatOne,
        ] {
            let mut lib = library(&[1, 2, 3, 4]);
            lib.playlists.push(crate::persistence::Playlist {
                id: 7,
                name: "夜间".into(),
                tracks: alloc::vec![3, 1],
            });
            let mut app = LyraApp::default();
            app.update(Action::Boot(lib));
            app.update(Action::SelectSong(song(4)));
            app.update(Action::PlayCollection {
                playlist: Some(7),
                mode,
            });
            assert_eq!(app.queue, alloc::vec![3, 1]);
            for _ in 0..20 {
                let effects = app.update(Action::TrackEnded);
                assert!(
                    !effects
                        .iter()
                        .any(|effect| matches!(effect, Effect::Navigate(_)))
                );
                if let Some(song) = &app.player.current {
                    assert!([3, 1].contains(&song.id));
                } else {
                    assert_eq!(mode, PlaybackMode::ListOnce);
                    break;
                }
            }
        }
    }

    #[test]
    fn browsing_does_not_replace_a_running_queue_and_songs_need_no_playlist() {
        let mut lib = library(&[1, 2, 3]);
        lib.playlists.push(crate::persistence::Playlist {
            id: 7,
            name: "空歌单".into(),
            tracks: Vec::new(),
        });
        let mut app = LyraApp::default();
        app.update(Action::Boot(lib));
        app.update(Action::SelectSong(song(2)));
        let queue = app.queue.clone();
        app.update(Action::Browse(Collection::Playlist(7)));
        assert_eq!(app.queue, queue);
        assert!(
            app.update(Action::PlayCollection {
                playlist: Some(7),
                mode: PlaybackMode::Shuffle
            })
            .is_empty()
        );
        assert_eq!(app.player.current.as_ref().unwrap().id, 2);
        app.update(Action::Browse(Collection::All));
        app.update(Action::SelectSong(song(3)));
        assert_eq!(app.queue, alloc::vec![1, 2, 3]);
    }

    #[test]
    fn shuffled_single_song_and_many_eof_transitions_are_safe() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1])));
        app.update(Action::SelectSong(song(1)));
        app.mode = PlaybackMode::Shuffle;
        app.update(Action::Back);
        for _ in 0..1_000 {
            let effects = app.update(Action::TrackEnded);
            assert!(streams(&effects));
            assert!(
                !effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Navigate(_)))
            );
            assert_eq!(app.player.current.as_ref().unwrap().id, 1);
            assert_eq!(app.history.len(), 0);
        }
    }

    #[test]
    fn exhausting_the_queue_reports_nothing_left_to_play() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1])));
        app.update(Action::SelectSong(song(1)));
        app.player.state = PlaybackState::Draining;
        app.update(Action::PlaybackExhausted);
        assert_eq!(app.player.state, PlaybackState::Finished);
        assert!(app.player.current.is_none());
    }

    #[test]
    fn shuffle_reaches_every_track_and_never_repeats_the_current_one() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(library(&[1, 2, 3, 4, 5])));
        app.mode = PlaybackMode::Shuffle;
        app.update(Action::SelectSong(song(1)));
        let mut seen = alloc::collections::BTreeSet::new();
        for _ in 0..200 {
            let before = app.player.current.as_ref().unwrap().id;
            app.update(Action::TrackEnded);
            let after = app.player.current.as_ref().unwrap().id;
            assert_ne!(before, after);
            seen.insert(after);
        }
        assert_eq!(seen.len(), 5, "every track comes up: {seen:?}");
    }
}
