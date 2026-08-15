use alloc::{string::String, vec::Vec};

use crate::{Song, playback::Player};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Home,
    Library,
    Player,
}

impl Route {
    pub const fn page_index(self) -> usize {
        match self {
            Self::Home => 0,
            Self::Library => 1,
            Self::Player => 2,
        }
    }

    pub const fn from_page_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Home),
            1 => Some(Self::Library),
            2 => Some(Self::Player),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Effect {
    StreamAudio { path: String },
    Navigate(Route),
}

#[derive(Clone, Debug)]
pub enum Action {
    Boot(Vec<Song>),
    Open(Route),
    Back,
    ReloadLibrary(Vec<Song>),
    SelectSong(Song),
    Previous,
    Next,
    Tick(u32),
}

#[derive(Clone, Debug)]
pub struct LyraApp {
    pub route: Route,
    pub history: Vec<Route>,
    pub local_tracks: Vec<Song>,
    pub player: Player,
    pub error: Option<String>,
    pub generation: u32,
}

impl Default for LyraApp {
    fn default() -> Self {
        Self {
            route: Route::Home,
            history: Vec::new(),
            local_tracks: Vec::new(),
            player: Player::default(),
            error: None,
            generation: 1,
        }
    }
}

impl LyraApp {
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        match action {
            Action::Boot(tracks) | Action::ReloadLibrary(tracks) => {
                if self.local_tracks == tracks {
                    return effects;
                }
                self.local_tracks = tracks;
            }
            Action::Open(route) => self.navigate(route, &mut effects),
            Action::Back => {
                if let Some(route) = self.history.pop() {
                    self.route = route;
                }
            }
            Action::SelectSong(song) => {
                self.player.select(song.clone(), core::iter::empty());
                self.error = None;
                self.navigate(Route::Player, &mut effects);
                match song.local_path {
                    Some(path) => effects.push(Effect::StreamAudio { path }),
                    None => {
                        self.player.state = crate::playback::PlaybackState::Failed;
                        self.error = Some(String::from("歌曲缺少本地音频文件"));
                    }
                }
            }
            Action::Previous => {
                if let Some(song) = self.adjacent_song(false) {
                    effects.extend(self.update(Action::SelectSong(song)));
                    return effects;
                }
            }
            Action::Next => {
                if let Some(song) = self.adjacent_song(true) {
                    effects.extend(self.update(Action::SelectSong(song)));
                    return effects;
                }
            }
            Action::Tick(ms) => self.player.tick(ms),
        }
        self.touch();
        effects
    }

    pub fn has_previous(&self) -> bool {
        self.adjacent_song(false).is_some()
    }

    pub fn has_next(&self) -> bool {
        self.adjacent_song(true).is_some()
    }

    fn adjacent_song(&self, next: bool) -> Option<Song> {
        let id = self.player.current.as_ref()?.id;
        let index = self.local_tracks.iter().position(|song| song.id == id)?;
        let adjacent = if next {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        };
        self.local_tracks.get(adjacent).cloned()
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

    fn local_song(id: u64) -> Song {
        Song {
            id,
            name: alloc::format!("Track {id}"),
            local_path: Some(alloc::format!(
                "/data/files/com.canopus.lyraimport/lyra/tracks/{id}/audio.mp3"
            )),
            ..Song::default()
        }
    }

    #[test]
    fn selection_only_emits_local_effects() {
        let mut app = LyraApp::default();
        app.update(Action::Boot(alloc::vec![local_song(1), local_song(2)]));
        let effects = app.update(Action::SelectSong(local_song(1)));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::StreamAudio { .. }))
        );
        assert!(!effects.iter().any(
            |effect| matches!(effect, Effect::StreamAudio { path } if path.starts_with("http"))
        ));
    }

    #[test]
    fn previous_and_next_follow_local_library_order_without_wrapping() {
        let mut app = LyraApp::default();
        let tracks = alloc::vec![local_song(10), local_song(20), local_song(30)];
        app.update(Action::Boot(tracks.clone()));

        app.update(Action::SelectSong(tracks[0].clone()));
        assert!(!app.has_previous());
        assert!(app.has_next());
        assert!(app.update(Action::Previous).is_empty());
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(10));

        let effects = app.update(Action::Next);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::StreamAudio { .. }))
        );
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(20));
        assert!(app.has_previous());
        assert!(app.has_next());

        app.update(Action::Next);
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(30));
        assert!(app.has_previous());
        assert!(!app.has_next());
        assert!(app.update(Action::Next).is_empty());
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(30));
    }

    #[test]
    fn completed_track_can_still_select_previous_library_track() {
        let mut app = LyraApp::default();
        let tracks = alloc::vec![local_song(1), local_song(2), local_song(3)];
        app.update(Action::Boot(tracks.clone()));
        app.update(Action::SelectSong(tracks[2].clone()));
        app.player.state = crate::playback::PlaybackState::Draining;

        let effects = app.update(Action::Previous);

        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::StreamAudio { .. }))
        );
        assert_eq!(app.player.current.as_ref().map(|song| song.id), Some(2));
        assert_eq!(app.player.position_ms, 0);
        assert_eq!(app.player.state, crate::playback::PlaybackState::Resolving);
    }

    #[test]
    fn unchanged_library_does_not_invalidate_ui() {
        let mut app = LyraApp::default();
        let tracks = alloc::vec![local_song(1)];
        app.update(Action::Boot(tracks.clone()));
        let generation = app.generation;
        assert!(app.update(Action::ReloadLibrary(tracks)).is_empty());
        assert_eq!(app.generation, generation);
    }
}
