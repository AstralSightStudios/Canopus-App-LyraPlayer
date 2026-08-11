use alloc::{borrow::ToOwned, string::String, vec, vec::Vec};

use crate::{
    Artist, Playlist, Profile, QrLogin, QrStatus, SearchResults, Session, Song,
    api::{self, ApiRequest},
    lyrics,
    playback::{PlaybackState, Player},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Home,
    Login,
    Library,
    Playlist,
    Search,
    Artist,
    Player,
    Lyrics,
}

impl Route {
    pub const fn page_index(self) -> usize {
        match self {
            Self::Home => 0,
            Self::Login => 1,
            Self::Library => 2,
            Self::Playlist => 3,
            Self::Search => 4,
            Self::Artist => 5,
            Self::Player => 6,
            Self::Lyrics => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    QrKey,
    QrCreate,
    QrCheck,
    LoginStatus,
    UserPlaylists,
    DailyPlaylists,
    DailySongs,
    PlaylistDetail,
    PlaylistTracks,
    SearchSongs,
    SearchArtists,
    ArtistDetail,
    ArtistSongs,
    ArtistAlbums,
    SongUrl,
    Lyrics,
}

#[derive(Clone, Debug)]
pub enum Effect {
    Fetch {
        kind: RequestKind,
        request: ApiRequest,
    },
    StreamAudio {
        url: String,
    },
    PersistSession(Session),
    ClearSession,
    Navigate(Route),
    CancelAudio {
        stream_id: String,
    },
    RequestPhoneSearch,
}

#[derive(Clone, Debug)]
pub enum Action {
    Boot(Option<Session>),
    Open(Route),
    Back,
    StartLogin,
    PollLogin(u64),
    Logout,
    RefreshHome(u64),
    OpenPlaylist(u64, u64),
    OpenArtist(u64),
    Search(String),
    SelectSong(Song),
    SongUrlReady(String),
    ApiResponse {
        kind: RequestKind,
        body: String,
        now_ms: u64,
    },
    ApiFailed {
        kind: RequestKind,
        message: String,
    },
    TogglePlayback,
    Next,
    ShowLyrics,
    Tick(u32),
    LocalLibrary(Vec<Song>),
}

#[derive(Clone, Debug)]
pub struct LyraApp {
    pub route: Route,
    pub history: Vec<Route>,
    pub session: Option<Session>,
    pub qr: QrLogin,
    pub profile: Option<Profile>,
    pub playlists: Vec<Playlist>,
    pub daily_playlists: Vec<Playlist>,
    pub daily_songs: Vec<Song>,
    pub selected_playlist: Option<Playlist>,
    pub selected_artist: Option<Artist>,
    pub search: SearchResults,
    pub local_tracks: Vec<Song>,
    pub player: Player,
    pub loading: bool,
    pub error: Option<String>,
    pub generation: u32,
    pub api_base: String,
}

impl Default for LyraApp {
    fn default() -> Self {
        Self {
            route: Route::Home,
            history: Vec::new(),
            session: None,
            qr: QrLogin::default(),
            profile: None,
            playlists: Vec::new(),
            daily_playlists: Vec::new(),
            daily_songs: Vec::new(),
            selected_playlist: None,
            selected_artist: None,
            search: SearchResults::default(),
            local_tracks: Vec::new(),
            player: Player::default(),
            loading: false,
            error: None,
            generation: 1,
            api_base: api::DEFAULT_API_BASE.into(),
        }
    }
}

impl LyraApp {
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        match action {
            Action::Boot(session) => {
                self.session = session;
                self.profile = self.session.as_ref().map(|session| session.profile.clone());
                if let Some(cookie) = self.cookie() {
                    effects.push(fetch(
                        RequestKind::LoginStatus,
                        api::login_status(cookie, 1),
                    ));
                }
                effects.extend(self.home_effects(1));
            }
            Action::Open(route) => self.navigate(route, &mut effects),
            Action::Back => {
                if let Some(route) = self.history.pop() {
                    self.route = route;
                    effects.push(Effect::Navigate(route));
                }
            }
            Action::StartLogin => {
                self.qr = QrLogin {
                    status: QrStatus::WaitingScan,
                    ..QrLogin::default()
                };
                self.loading = true;
                self.navigate(Route::Login, &mut effects);
                effects.push(fetch(RequestKind::QrKey, api::qr_key()));
            }
            Action::PollLogin(now_ms) => {
                if !self.qr.key.is_empty()
                    && matches!(
                        self.qr.status,
                        QrStatus::WaitingScan | QrStatus::WaitingConfirm
                    )
                {
                    effects.push(fetch(
                        RequestKind::QrCheck,
                        api::qr_check(&self.qr.key, now_ms),
                    ));
                }
            }
            Action::Logout => {
                self.session = None;
                self.profile = None;
                self.playlists.clear();
                self.daily_playlists.clear();
                self.daily_songs.clear();
                effects.push(Effect::ClearSession);
                self.navigate(Route::Home, &mut effects);
            }
            Action::RefreshHome(now_ms) => effects.extend(self.home_effects(now_ms)),
            Action::OpenPlaylist(id, now_ms) => {
                let cookie = self.cookie().unwrap_or("").to_owned();
                self.selected_playlist = Some(Playlist {
                    id,
                    ..Playlist::default()
                });
                self.loading = true;
                effects.push(fetch(
                    RequestKind::PlaylistDetail,
                    api::playlist_detail(id, &cookie, now_ms),
                ));
                effects.push(fetch(
                    RequestKind::PlaylistTracks,
                    api::playlist_tracks(id, 0, &cookie),
                ));
            }
            Action::OpenArtist(id) => {
                self.selected_artist = Some(Artist {
                    id,
                    ..Artist::default()
                });
                self.loading = true;
                effects.push(fetch(
                    RequestKind::ArtistDetail,
                    api::artist_detail(id, self.cookie()),
                ));
                effects.push(fetch(
                    RequestKind::ArtistSongs,
                    api::artist_songs(id, 0, self.cookie()),
                ));
                effects.push(fetch(
                    RequestKind::ArtistAlbums,
                    api::artist_albums(id, 0, self.cookie()),
                ));
            }
            Action::Search(query) => {
                self.search = SearchResults {
                    query: query.clone(),
                    ..SearchResults::default()
                };
                self.loading = true;
                effects.push(fetch(
                    RequestKind::SearchSongs,
                    api::search_songs(&query, 0, self.cookie()),
                ));
                effects.push(fetch(
                    RequestKind::SearchArtists,
                    api::search_artists(&query, 0, self.cookie()),
                ));
                self.navigate(Route::Search, &mut effects);
            }
            Action::SelectSong(song) => {
                let mut queue = Vec::new();
                if let Some(playlist) = &self.selected_playlist {
                    queue.extend(playlist.tracks.iter().cloned());
                } else if !self.search.songs.is_empty() {
                    queue.extend(self.search.songs.iter().cloned());
                } else {
                    queue.extend(self.daily_songs.iter().cloned());
                }
                self.player.select(song.clone(), queue);
                self.navigate(Route::Player, &mut effects);
                if let Some(path) = song.local_path {
                    effects.push(Effect::StreamAudio { url: path });
                } else if let Some(cookie) = self.cookie() {
                    effects.push(fetch(
                        RequestKind::SongUrl,
                        api::song_url(song.id, cookie, self.generation as u64),
                    ));
                    effects.push(fetch(
                        RequestKind::Lyrics,
                        api::lyric(song.id, cookie, self.generation as u64),
                    ));
                }
            }
            Action::SongUrlReady(url) => effects.push(Effect::StreamAudio { url }),
            Action::ApiResponse { kind, body, now_ms } => {
                self.handle_api(kind, &body, now_ms, &mut effects);
            }
            Action::ApiFailed { kind: _, message } => {
                self.loading = false;
                self.error = Some(message);
            }
            Action::TogglePlayback => {
                // Device glue performs the synchronous audio ioctl then mirrors the state.
                self.player.state = match self.player.state {
                    PlaybackState::Playing | PlaybackState::Buffering => PlaybackState::Paused,
                    PlaybackState::Paused => PlaybackState::Playing,
                    state => state,
                };
            }
            Action::Next => {
                if let Some(song) = self.player.take_next() {
                    effects.extend(self.update(Action::SelectSong(song)));
                }
            }
            Action::ShowLyrics => self.navigate(Route::Lyrics, &mut effects),
            Action::Tick(ms) => self.player.tick(ms),
            Action::LocalLibrary(tracks) => self.local_tracks = tracks,
        }
        self.touch();
        effects
    }

    fn handle_api(
        &mut self,
        kind: RequestKind,
        body: &str,
        now_ms: u64,
        effects: &mut Vec<Effect>,
    ) {
        self.loading = false;
        self.error = None;
        let result: Result<(), api::ApiError> = match kind {
            RequestKind::QrKey => api::parse_qr_key(body).map(|key| {
                self.qr.key = key.clone();
                effects.push(fetch(RequestKind::QrCreate, api::qr_create(&key)));
            }),
            RequestKind::QrCreate => api::parse_qr_url(body).map(|url| {
                self.qr.url = url;
                self.qr.status = QrStatus::WaitingScan;
            }),
            RequestKind::QrCheck => api::parse_qr_check(body).and_then(|check| match check.code {
                800 => {
                    self.qr.status = QrStatus::Expired;
                    Ok(())
                }
                801 => {
                    self.qr.status = QrStatus::WaitingScan;
                    Ok(())
                }
                802 => {
                    self.qr.status = QrStatus::WaitingConfirm;
                    Ok(())
                }
                803 => {
                    self.qr.status = QrStatus::Authorized;
                    let cookie = check.cookie.ok_or(api::ApiError::Missing)?;
                    self.session = Some(Session {
                        cookie: cookie.clone(),
                        saved_at_ms: now_ms,
                        ..Session::default()
                    });
                    effects.push(fetch(
                        RequestKind::LoginStatus,
                        api::login_status(&cookie, now_ms),
                    ));
                    Ok(())
                }
                code => Err(api::ApiError::Server(code)),
            }),
            RequestKind::LoginStatus => api::parse_profile(body).map(|profile| {
                self.profile = Some(profile.clone());
                if let Some(session) = &mut self.session {
                    session.profile = profile;
                    session.saved_at_ms = now_ms;
                    effects.push(Effect::PersistSession(session.clone()));
                }
                effects.extend(self.home_effects(now_ms));
                self.navigate(Route::Home, effects);
            }),
            RequestKind::UserPlaylists => {
                api::parse_playlists(body).map(|items| self.playlists = items)
            }
            RequestKind::DailyPlaylists => {
                api::parse_playlists(body).map(|items| self.daily_playlists = items)
            }
            RequestKind::DailySongs => {
                api::parse_daily_songs(body).map(|items| self.daily_songs = items)
            }
            RequestKind::PlaylistDetail => api::parse_playlist(body).map(|mut playlist| {
                if let Some(existing) = &self.selected_playlist
                    && existing.id == playlist.id
                    && !existing.tracks.is_empty()
                {
                    playlist.tracks = existing.tracks.clone();
                }
                self.selected_playlist = Some(playlist);
                self.navigate(Route::Playlist, effects);
            }),
            RequestKind::PlaylistTracks => api::parse_songs(body).map(|tracks| {
                if let Some(playlist) = &mut self.selected_playlist {
                    playlist.track_count = playlist.track_count.max(tracks.len() as u32);
                    playlist.tracks = tracks;
                }
            }),
            RequestKind::SearchSongs => {
                api::parse_search_songs(body).map(|songs| self.search.songs = songs)
            }
            RequestKind::SearchArtists => {
                api::parse_search_artists(body).map(|artists| self.search.artists = artists)
            }
            RequestKind::ArtistDetail => api::parse_artist(body).map(|mut artist| {
                if let Some(existing) = &self.selected_artist
                    && existing.id == artist.id
                {
                    artist.songs = existing.songs.clone();
                    artist.albums = existing.albums.clone();
                }
                self.selected_artist = Some(artist);
                self.navigate(Route::Artist, effects);
            }),
            RequestKind::ArtistSongs => api::parse_artist_songs(body).map(|songs| {
                if let Some(artist) = &mut self.selected_artist {
                    artist.songs = songs;
                }
            }),
            RequestKind::ArtistAlbums => api::parse_artist_albums(body).map(|albums| {
                if let Some(artist) = &mut self.selected_artist {
                    artist.albums = albums;
                }
            }),
            RequestKind::SongUrl => api::parse_song_url(body).map(|url| {
                effects.push(Effect::StreamAudio { url });
            }),
            RequestKind::Lyrics => lyrics::parse_api_lyrics(body)
                .map(|value| self.player.lyrics = value)
                .map_err(|_| api::ApiError::Json),
        };
        if let Err(error) = result {
            self.error = Some(alloc::format!("{error:?}"));
            if matches!(
                kind,
                RequestKind::QrKey | RequestKind::QrCreate | RequestKind::QrCheck
            ) {
                self.qr.status = QrStatus::Failed;
            }
        }
    }

    fn home_effects(&self, nonce: u64) -> Vec<Effect> {
        let Some(cookie) = self.cookie() else {
            return Vec::new();
        };
        let mut effects = vec![
            fetch(
                RequestKind::DailyPlaylists,
                api::daily_playlists(cookie, nonce),
            ),
            fetch(RequestKind::DailySongs, api::daily_songs(cookie, nonce)),
        ];
        if let Some(profile) = self
            .profile
            .as_ref()
            .or_else(|| self.session.as_ref().map(|s| &s.profile))
        {
            if profile.user_id != 0 {
                effects.push(fetch(
                    RequestKind::UserPlaylists,
                    api::user_playlists(profile.user_id, 0, cookie),
                ));
            }
        }
        effects
    }

    fn navigate(&mut self, route: Route, effects: &mut Vec<Effect>) {
        if self.route != route {
            self.history.push(self.route);
            self.route = route;
        }
        effects.push(Effect::Navigate(route));
    }

    fn cookie(&self) -> Option<&str> {
        self.session
            .as_ref()
            .map(|session| session.cookie.as_str())
            .filter(|cookie| !cookie.is_empty())
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }
}

fn fetch(kind: RequestKind, request: ApiRequest) -> Effect {
    Effect::Fetch { kind, request }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_qr_login_persists_cookie_after_profile() {
        let mut app = LyraApp::default();
        app.update(Action::ApiResponse {
            kind: RequestKind::QrCheck,
            body: r#"{"code":803,"cookie":"MUSIC_U=abc"}"#.into(),
            now_ms: 12,
        });
        let effects = app.update(Action::ApiResponse {
            kind: RequestKind::LoginStatus,
            body: r#"{"data":{"code":200,"profile":{"userId":7,"nickname":"Lyra"}}}"#.into(),
            now_ms: 13,
        });
        assert_eq!(app.profile.as_ref().unwrap().user_id, 7);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::PersistSession(_)))
        );
    }

    #[test]
    fn detail_responses_preserve_early_tracks() {
        let mut app = LyraApp::default();
        let effects = app.update(Action::OpenPlaylist(7, 1));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Fetch {
                kind: RequestKind::PlaylistTracks,
                ..
            }
        )));
        app.update(Action::ApiResponse {
            kind: RequestKind::PlaylistTracks,
            body: r#"{"code":200,"songs":[{"id":1,"name":"First"}]}"#.into(),
            now_ms: 2,
        });
        app.update(Action::ApiResponse {
            kind: RequestKind::PlaylistDetail,
            body: r#"{"code":200,"playlist":{"id":7,"name":"Mix","trackCount":1}}"#.into(),
            now_ms: 3,
        });
        assert_eq!(app.selected_playlist.as_ref().unwrap().tracks.len(), 1);

        app.update(Action::OpenArtist(9));
        app.update(Action::ApiResponse {
            kind: RequestKind::ArtistSongs,
            body: r#"{"code":200,"songs":[{"id":2,"name":"Second"}]}"#.into(),
            now_ms: 4,
        });
        app.update(Action::ApiResponse {
            kind: RequestKind::ArtistDetail,
            body: r#"{"code":200,"data":{"artist":{"id":9,"name":"Artist"}}}"#.into(),
            now_ms: 5,
        });
        assert_eq!(app.selected_artist.as_ref().unwrap().songs.len(), 1);
    }

    #[test]
    fn selecting_song_opens_player_and_resolves_stream() {
        let mut app = LyraApp {
            session: Some(Session {
                cookie: "MUSIC_U=x".into(),
                ..Session::default()
            }),
            ..LyraApp::default()
        };
        let effects = app.update(Action::SelectSong(Song {
            id: 42,
            name: "Orbit".into(),
            ..Song::default()
        }));
        assert_eq!(app.route, Route::Player);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Fetch {
                kind: RequestKind::SongUrl,
                ..
            }
        )));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Fetch {
                kind: RequestKind::Lyrics,
                ..
            }
        )));
    }
}
