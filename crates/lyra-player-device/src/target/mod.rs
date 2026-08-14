//! Target-selected local player integration. UI callbacks, filesystem polling,
//! and audio pumping run on the page-owner thread.

use core::sync::atomic::Ordering;

use lyra_player_core::{Action, Effect, Route, playback::PlaybackState, ui};

use runtime::{initialized, runtime, try_with_core, with_core};

pub mod audio;
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
    let library = match storage::load_library() {
        Ok(library) => library,
        Err(error) => {
            runtime().last_error.store(error, Ordering::Release);
            alloc::vec::Vec::new()
        }
    };
    let effects = with_core(|core| core.app.update(Action::Boot(library)));
    execute_effects(effects);
    0
}

pub fn query_status() -> [u32; 7] {
    let r = runtime();
    let core = try_with_core(|core| {
        [
            core.app.generation,
            core.app.route.page_index() as u32,
            core.app.player.state as u32,
        ]
    })
    .unwrap_or([u32::MAX; 3]);
    [
        r.app_state.load(Ordering::Acquire),
        r.app_error.load(Ordering::Acquire) as u32,
        r.last_error.load(Ordering::Acquire) as u32,
        r.active_page.load(Ordering::Acquire),
        core[0],
        core[1],
        core[2],
    ]
}

pub fn rebuild(page_index: usize) -> i32 {
    if page_is_current(page_index) != 0 {
        return 0;
    }
    let snapshot = with_core(|core| lyra_player_core::ui::render(&core.app));
    match snapshot {
        Ok(snapshot) => ui_backend::apply_snapshot(page_index, &snapshot),
        Err(_) => -1,
    }
}

pub fn rebuild_if_changed(page_index: usize, rendered_generation: u32) -> i32 {
    timer_tick();
    if page_is_current(page_index) != 0 {
        return 0;
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
            } else if let Some(position) = core.app.history.iter().rposition(|item| *item == route) {
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

pub fn handle_ui_event(page_index: usize, generation: u32, _key: u32, event_id: u32) {
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
        action_for_event(&core.app, event_id).map(|action| core.app.update(action))
    });
    let Some(effects) = effects else {
        return;
    };
    execute_effects(effects);
    let _ = rebuild(page_index);
}

fn action_for_event(app: &lyra_player_core::LyraApp, event_id: u32) -> Option<Action> {
    match event_id {
        ui::EVENT_LIBRARY => Some(Action::Open(Route::Library)),
        ui::EVENT_NEXT => Some(Action::Next),
        ui::EVENT_LYRICS => Some(Action::ShowLyrics),
        ui::EVENT_NOW_PLAYING => Some(Action::Open(Route::Player)),
        event if (ui::EVENT_LOCAL_SONG_BASE..ui::EVENT_LOCAL_SONG_BASE + 20).contains(&event) => {
            app.local_tracks
                .get((event - ui::EVENT_LOCAL_SONG_BASE) as usize)
                .cloned()
                .map(Action::SelectSong)
        }
        _ => None,
    }
}

fn route_page(route: Route) -> usize {
    match route {
        Route::Home => native_app::PAGE_OVERVIEW,
        Route::Library => native_app::PAGE_LIBRARY,
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
        effects
    });
    execute_effects(effects);

    // The quick app publishes library.json only after an import transaction is
    // complete. Polling it every two seconds makes imports appear without
    // introducing a phone transport into the native player.
    if tick.is_multiple_of(4) {
        match storage::load_library() {
            Ok(library) => {
                let effects = with_core(|core| core.app.update(Action::ReloadLibrary(library)));
                execute_effects(effects);
            }
            Err(error) => runtime().last_error.store(error, Ordering::Release),
        }
    }
}

fn execute_effects(effects: alloc::vec::Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::StreamAudio { path } => with_core(|core| {
                if !lyra_player_core::persistence::is_safe_audio_path(&path) {
                    core.app.error = Some(alloc::string::String::from("invalid local audio path"));
                    core.app.player.state = PlaybackState::Failed;
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                    return;
                }
                if let Err(error) = core.audio.start_local(&path, &mut core.app.player) {
                    core.app.error = Some(alloc::format!("local audio failed: {error}"));
                    core.app.player.state = PlaybackState::Failed;
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                }
            }),
            Effect::LoadLyrics { path } => {
                let result = path.as_deref().map(storage::load_lyrics).transpose();
                with_core(|core| match result {
                    Ok(Some(Some(text))) => core.app.set_lyrics_text(Some(&text)),
                    Ok(_) => core.app.set_lyrics_text(None),
                    Err(error) => {
                        runtime().last_error.store(error, Ordering::Release);
                        core.app.set_lyrics_text(None);
                    }
                });
            }
            Effect::Navigate(route) => ui_backend::navigate(route_page(route)),
        }
    }
}
