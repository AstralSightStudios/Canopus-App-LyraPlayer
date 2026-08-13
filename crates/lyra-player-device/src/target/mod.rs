//! Target-selected device integration. UI callbacks run on the page owner
//! thread; transport callbacks publish state through `runtime` and never touch
//! LVX directly.

use core::sync::atomic::Ordering;

use lyra_player_core::{Action, Effect, QrStatus, Route, playback::PlaybackState, ui};

use runtime::{initialized, runtime, try_with_core, with_core};

pub mod audio;
pub mod image_resource;
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
    // The app model has a single `route` but the firmware keeps one LVX page
    // per route alive on a stack. Rendering the *current* route's snapshot into
    // a page that no longer hosts that route corrupts it: after a forward
    // navigation the paused source page would otherwise show the destination's
    // content. A pushed destination renders itself in `page_create`; a popped
    // page re-renders in `page_resume`, so skipping here is always correct.
    if page_is_current(page_index) != 0 {
        return 0;
    }
    let qr_result = with_core(|core| {
        if core.app.route == Route::Login {
            image_resource::ensure_qr(&core.app.qr.url)
        } else {
            Ok(())
        }
    });
    if let Err(error) = qr_result {
        runtime().last_error.store(error, Ordering::Release);
        return error;
    }
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
    // A paused (or otherwise off-route) page still owns a refresh timer; its
    // tick must keep advancing the global player/lyrics clock but must never
    // paint the current route into a stale page.
    if page_is_current(page_index) != 0 {
        return 0;
    }
    let qr_result = with_core(|core| {
        if core.app.route == Route::Login {
            image_resource::ensure_qr(&core.app.qr.url)
        } else {
            Ok(())
        }
    });
    if let Err(error) = qr_result {
        runtime().last_error.store(error, Ordering::Release);
        return error;
    }
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

/// Returns 0 when `page_index` is the page hosting the app's current route.
/// A page whose index differs from `app.route` is stale (paused on the stack).
fn page_is_current(page_index: usize) -> i32 {
    let current = with_core(|core| core.app.route.page_index());
    if current == page_index { 0 } else { 1 }
}

pub fn sync_resumed_page(page_index: usize) -> i32 {
    let Some(route) = Route::from_page_index(page_index) else {
        return -1;
    };
    with_core(|core| {
        if core.app.route != route {
            if core.app.history.last().copied() == Some(route) {
                core.app.history.pop();
            } else if let Some(position) = core.app.history.iter().rposition(|item| *item == route)
            {
                core.app.history.truncate(position);
            } else {
                core.app.history.clear();
            }
            core.app.route = route;
            core.app.generation = core.app.generation.wrapping_add(1).max(1);
        }
    });
    0
}

pub fn handle_back(page_index: usize) {
    let should_finish = with_core(|core| {
        if core.app.route.page_index() != page_index {
            return false;
        }
        let _ = core.app.update(Action::Back);
        true
    });
    if should_finish {
        ui_backend::back(page_index);
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
        let valid = with_core(|core| core.app.generation == generation);
        if valid {
            handle_back(page_index);
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
        ui::EVENT_LOGIN | ui::EVENT_RETRY_LOGIN => Some(Action::StartLogin),
        ui::EVENT_REFRESH => Some(Action::RefreshHome(app.generation as u64)),
        ui::EVENT_LIBRARY => Some(Action::Open(Route::Library)),
        ui::EVENT_SEARCH => Some(Action::Open(Route::Search)),
        ui::EVENT_LOGOUT => Some(Action::Logout),
        ui::EVENT_NEXT => Some(Action::Next),
        ui::EVENT_LYRICS => Some(Action::ShowLyrics),
        ui::EVENT_NOW_PLAYING => Some(Action::Open(Route::Player)),
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
                let request =
                    with_core(|core| lyra_player_core::api::with_base(request, &core.app.api_base));
                interconnect::enqueue_api(kind, request);
            }
            Effect::StreamAudio { url } if url.starts_with('/') => {
                let cancellations = with_core(|core| {
                    let mut cancellations = alloc::vec::Vec::new();
                    if let Some(previous) = core.audio_request.take() {
                        cancellations.push(core.bridge.cancel_stream(&previous, "local playback"));
                    }
                    core.audio_ending = false;
                    core.deferred_stream_reply = None;
                    if !lyra_player_core::persistence::is_safe_local_path(&url) {
                        core.app.error =
                            Some(alloc::string::String::from("invalid local audio path"));
                        core.app.player.state = PlaybackState::Failed;
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                        return cancellations;
                    }
                    if let Err(error) = core.audio.start_local(&url, &mut core.app.player) {
                        core.app.error = Some(alloc::format!("local audio failed: {error}"));
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                    }
                    cancellations
                });
                for message in cancellations {
                    interconnect::enqueue(message);
                }
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
