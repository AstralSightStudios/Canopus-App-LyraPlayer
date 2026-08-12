//! Target-selected device integration. UI callbacks run on the page owner
//! thread; transport callbacks publish state through `runtime` and never touch
//! LVX directly.

use core::sync::atomic::Ordering;

use lyra_player_core::{Action, Effect, QrStatus, Route, playback::PlaybackState, ui};

use runtime::{initialized, runtime, try_with_core, with_core};

pub mod audio;
pub mod interconnect;
pub mod native_app;
pub mod runtime;
pub mod storage;
pub mod ui_backend;

pub fn prepare() {
    runtime::prepare();
}

pub fn activate() -> i32 {
    if !initialized() {
        return -1;
    }
    let result = canopus_target_private::canopus_identity_guard();
    if result != 0 {
        runtime().last_error.store(result, Ordering::Release);
        return result;
    }
    let (session, library) = storage::load();
    let effects = with_core(|core| {
        let mut effects = core.app.update(Action::Boot(session));
        effects.extend(core.app.update(Action::LocalLibrary(library)));
        effects
    });
    execute_effects(effects);

    // The firmware interconnect registry is not guaranteed to exist during
    // boot activation (for example before the quick-app proxy starts). Keep the
    // resident module active and retry from the page-owner timer instead of
    // turning a temporarily unavailable phone bridge into a module failure.
    if let Err(error) = interconnect::register() {
        runtime().last_error.store(error, Ordering::Release);
    }
    0
}

pub fn query_status() -> [u32; 10] {
    let r = runtime();
    let core = try_with_core(|core| {
        [
            core.app.generation,
            core.app.route.page_index() as u32,
            core.app.player.state as u32,
            core.pending.len() as u32,
            core.outbound.len() as u32,
        ]
    })
    .unwrap_or([u32::MAX; 5]);
    [
        r.app_state.load(Ordering::Acquire),
        r.app_error.load(Ordering::Acquire) as u32,
        r.last_error.load(Ordering::Acquire) as u32,
        r.connected.load(Ordering::Acquire) as u32,
        r.active_page.load(Ordering::Acquire),
        core[0],
        core[1],
        core[2],
        core[3],
        core[4],
    ]
}

fn ensure_interconnect() {
    if runtime().connection.load(Ordering::Acquire) == 0
        && let Err(error) = interconnect::register()
    {
        runtime().last_error.store(error, Ordering::Release);
    }
}

pub fn rebuild(page_index: usize) -> i32 {
    ensure_interconnect();
    let effects = interconnect::pump();
    execute_effects(effects);
    let snapshot = with_core(|core| lyra_player_core::ui::render(&core.app));
    match snapshot {
        Ok(snapshot) => ui_backend::apply_snapshot(page_index, &snapshot),
        Err(_) => -1,
    }
}

pub fn rebuild_if_changed(page_index: usize, rendered_generation: u32) -> i32 {
    ensure_interconnect();
    timer_tick();
    let effects = interconnect::pump();
    execute_effects(effects);
    let snapshot = match try_with_core(|core| {
        if core.app.generation == rendered_generation {
            None
        } else {
            Some(lyra_player_core::ui::render(&core.app))
        }
    }) {
        Some(snapshot) => snapshot,
        None => return 0,
    };
    match snapshot {
        None => 0,
        Some(Ok(snapshot)) => ui_backend::apply_snapshot(page_index, &snapshot),
        Some(Err(_)) => -1,
    }
}

pub fn handle_ui_event(page_index: usize, generation: u32, key: u32, event_id: u32) {
    if event_id == ui::EVENT_PHONE_SEARCH {
        interconnect::enqueue(alloc::string::String::from(
            r#"{"tag":"lyra-search-request","maxBytes":128}"#,
        ));
        return;
    }
    if event_id == ui::EVENT_BACK {
        let valid = with_core(|core| {
            if core.app.generation != generation {
                return false;
            }
            let _ = core.app.update(Action::Back);
            true
        });
        if valid {
            ui_backend::back(page_index);
        }
        return;
    }
    let effects = with_core(|core| {
        if core.app.generation != generation {
            return None;
        }
        if event_id == ui::EVENT_TOGGLE {
            if let Err(error) = core.app.player.toggle(&mut core.audio) {
                core.app.error = Some(alloc::format!("audio ioctl failed: {error}"));
                core.app.generation = core.app.generation.wrapping_add(1).max(1);
            }
            return Some(alloc::vec::Vec::new());
        }
        action_for_event(&core.app, key, event_id).map(|action| core.app.update(action))
    });
    let Some(effects) = effects else {
        return;
    };
    execute_effects(effects);
    let _ = rebuild(page_index);
}

fn action_for_event(app: &lyra_player_core::LyraApp, _key: u32, event_id: u32) -> Option<Action> {
    match event_id {
        ui::EVENT_BACK => Some(Action::Back),
        ui::EVENT_LOGIN | ui::EVENT_RETRY_LOGIN => Some(Action::StartLogin),
        ui::EVENT_REFRESH => Some(Action::RefreshHome(app.generation as u64)),
        ui::EVENT_LIBRARY => Some(Action::Open(Route::Library)),
        ui::EVENT_SEARCH => Some(Action::Open(Route::Search)),
        ui::EVENT_LOGOUT => Some(Action::Logout),
        ui::EVENT_TOGGLE => Some(Action::TogglePlayback),
        ui::EVENT_NEXT => Some(Action::Next),
        ui::EVENT_LYRICS => Some(Action::ShowLyrics),
        ui::EVENT_NOW_PLAYING => Some(Action::Open(Route::Player)),
        ui::EVENT_PHONE_SEARCH => None,
        event if (ui::EVENT_PLAYLIST_BASE..ui::EVENT_PLAYLIST_BASE + 8).contains(&event) => {
            let index = (event - ui::EVENT_PLAYLIST_BASE) as usize;
            app.daily_playlists
                .get(index)
                .map(|playlist| Action::OpenPlaylist(playlist.id, app.generation as u64))
        }
        event
            if (ui::EVENT_PLAYLIST_BASE + 100..ui::EVENT_PLAYLIST_BASE + 108).contains(&event) =>
        {
            let index = (event - ui::EVENT_PLAYLIST_BASE - 100) as usize;
            app.playlists
                .get(index)
                .map(|playlist| Action::OpenPlaylist(playlist.id, app.generation as u64))
        }
        event if (ui::EVENT_ARTIST_BASE..ui::EVENT_ARTIST_BASE + 8).contains(&event) => {
            let index = (event - ui::EVENT_ARTIST_BASE) as usize;
            app.search
                .artists
                .get(index)
                .map(|artist| Action::OpenArtist(artist.id))
        }
        event if (ui::EVENT_LOCAL_SONG_BASE..ui::EVENT_LOCAL_SONG_BASE + 20).contains(&event) => {
            let index = (event - ui::EVENT_LOCAL_SONG_BASE) as usize;
            app.local_tracks.get(index).cloned().map(Action::SelectSong)
        }
        event if (ui::EVENT_SONG_BASE..ui::EVENT_SONG_BASE + 20).contains(&event) => {
            let index = (event - ui::EVENT_SONG_BASE) as usize;
            let songs = match app.route {
                Route::Playlist => app
                    .selected_playlist
                    .as_ref()
                    .map(|playlist| playlist.tracks.as_slice()),
                Route::Search => Some(app.search.songs.as_slice()),
                Route::Artist => app
                    .selected_artist
                    .as_ref()
                    .map(|artist| artist.songs.as_slice()),
                _ => None,
            };
            songs
                .and_then(|songs| songs.get(index))
                .cloned()
                .map(Action::SelectSong)
        }
        _ => None,
    }
}

fn route_page(route: Route) -> usize {
    match route {
        Route::Home => native_app::PAGE_OVERVIEW,
        Route::Login => native_app::PAGE_LOGIN,
        Route::Library => native_app::PAGE_LIBRARY,
        Route::Playlist => native_app::PAGE_PLAYLIST,
        Route::Search => native_app::PAGE_SEARCH,
        Route::Artist => native_app::PAGE_ARTIST,
        Route::Player => native_app::PAGE_PLAYER,
        Route::Lyrics => native_app::PAGE_LYRICS,
    }
}

fn timer_tick() {
    let tick = runtime().timer_ticks.fetch_add(1, Ordering::AcqRel) + 1;
    let effects = with_core(|core| {
        let mut effects = alloc::vec::Vec::new();
        if let Err(error) = core.audio.pump_local(&mut core.app.player) {
            core.app.error = Some(alloc::format!("local audio failed: {error}"));
            core.app.generation = core.app.generation.wrapping_add(1).max(1);
        }
        if core.app.player.state == PlaybackState::Playing {
            effects.extend(core.app.update(Action::Tick(500)));
        }
        if tick.is_multiple_of(8)
            && core.app.route == Route::Login
            && matches!(
                core.app.qr.status,
                QrStatus::WaitingScan | QrStatus::WaitingConfirm
            )
        {
            effects.extend(core.app.update(Action::PollLogin(u64::from(tick) * 500)));
        }
        effects
    });
    execute_effects(effects);
}

fn execute_effects(effects: alloc::vec::Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::Fetch { kind, request } => {
                interconnect::enqueue_api(kind, request);
            }
            Effect::StreamAudio { url } if url.starts_with('/') => {
                with_core(|core| {
                    if let Err(error) = core.audio.start_local(&url, &mut core.app.player) {
                        core.app.error = Some(alloc::format!("local audio failed: {error}"));
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                    }
                });
            }
            Effect::StreamAudio { url } => interconnect::enqueue_audio(url),
            Effect::Navigate(route) => ui_backend::navigate(route_page(route)),
            Effect::CancelAudio { stream_id } => {
                let message = with_core(|core| core.bridge.cancel_stream(&stream_id, "replaced"));
                interconnect::enqueue(message);
            }
            Effect::PersistSession(session) => {
                if let Err(error) = storage::save_session(&session) {
                    runtime().last_error.store(error, Ordering::Release);
                }
            }
            Effect::ClearSession => {
                if let Err(error) = storage::clear_session() {
                    runtime().last_error.store(error, Ordering::Release);
                }
            }
            Effect::RequestPhoneSearch => {}
        }
    }
}
