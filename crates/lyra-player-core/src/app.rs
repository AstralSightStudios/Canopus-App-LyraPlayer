use alloc::{string::String, vec::Vec};

use crate::{Song, lyrics, playback::Player};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Home,
    Library,
    Player,
    Lyrics,
}

impl Route {
    pub const fn page_index(self) -> usize {
        match self {
            Self::Home => 0,
            Self::Library => 1,
            Self::Player => 2,
            Self::Lyrics => 3,
        }
    }

    pub const fn from_page_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Home),
            1 => Some(Self::Library),
            2 => Some(Self::Player),
            3 => Some(Self::Lyrics),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Effect {
    StreamAudio { path: String },
    LoadLyrics { path: Option<String> },
    Navigate(Route),
}

#[derive(Clone, Debug)]
pub enum Action {
    Boot(Vec<Song>),
    Open(Route),
    Back,
    ReloadLibrary(Vec<Song>),
    SelectSong(Song),
    Next,
    ShowLyrics,
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
                let queue = self.local_tracks.iter().cloned();
                self.player.select(song.clone(), queue);
                self.navigate(Route::Player, &mut effects);
                effects.push(Effect::LoadLyrics {
                    path: song.lyrics_path.clone(),
                });
                match song.local_path {
                    Some(path) => effects.push(Effect::StreamAudio { path }),
                    None => {
                        self.player.state = crate::playback::PlaybackState::Failed;
                        self.error = Some(String::from("歌曲缺少本地音频文件"));
                    }
                }
            }
            Action::Next => {
                if let Some(song) = self.player.take_next() {
                    effects.extend(self.update(Action::SelectSong(song)));
                    return effects;
                }
            }
            Action::ShowLyrics => self.navigate(Route::Lyrics, &mut effects),
            Action::Tick(ms) => self.player.tick(ms),
        }
        self.touch();
        effects
    }

    pub fn set_lyrics_text(&mut self, text: Option<&str>) {
        self.player.lyrics = match text {
            Some(text) if text.trim_start().starts_with('{') => {
                lyrics::parse_api_lyrics(text).unwrap_or_default()
            }
            Some(text) => lyrics::parse_lrc(text),
            None => Default::default(),
        };
        self.touch();
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
        assert!(effects.iter().any(|effect| matches!(effect, Effect::StreamAudio { .. })));
        assert!(!effects.iter().any(|effect| matches!(effect, Effect::StreamAudio { path } if path.starts_with("http"))));
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
