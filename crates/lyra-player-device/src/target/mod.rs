//! Target-selected local player integration. Audio feeding runs on the resident
//! Bluetooth owner timer; LVGL page callbacks only manage rendering and input.

use core::sync::atomic::Ordering;

use alloc::{string::String, vec::Vec};
use lyra_player_core::{
    Action, Effect, Route, Song,
    lyrics::Lyrics,
    playback::{MediaControl, PlaybackState},
    view::PlayerView,
};

use runtime::{initialized, runtime, try_with_core, with_core};

pub mod audio;
mod audio_service;
pub mod connect;
pub mod native_app;
pub mod runtime;
pub mod storage;
pub mod ui;

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
    // Module activation runs in the loader's 7.9 KiB `insmod` task. Reading
    // the library belongs on the page-owner timer, which has a larger stack.
    let result = audio_service::start();
    if result != 0 {
        runtime().last_error.store(result, Ordering::Release);
    }
    // Targets without a CanopusConnect backend answer -38: the player then
    // has no phone import path, which the library page says.
    let result = connect::start();
    runtime().connect_result.store(result, Ordering::Release);
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

// ---------------------------------------------------------------------------
// What the pages draw
// ---------------------------------------------------------------------------

pub struct PlayerFrame {
    pub view: Option<PlayerView>,
    pub active_lyric: Option<usize>,
    /// Set once the lyrics of the current track are known (possibly empty).
    pub lyrics_id: Option<u64>,
}

pub fn player_frame() -> PlayerFrame {
    with_core(|core| {
        let current = core.app.player.current.as_ref().map(|song| song.id);
        PlayerFrame {
            view: PlayerView::of(&core.app),
            active_lyric: core.app.active_lyric(),
            lyrics_id: core.app.lyrics_for.filter(|id| Some(*id) == current),
        }
    })
}

pub fn lyrics_snapshot() -> Lyrics {
    with_core(|core| core.app.lyrics.clone())
}

pub struct NowPlaying {
    pub title: String,
    pub state: &'static str,
    pub palette: lyra_player_core::Palette,
}

pub struct TrackRow {
    pub id: u64,
    pub name: String,
    pub artist: String,
    pub palette: lyra_player_core::Palette,
}

pub struct LibraryFrame {
    pub now_playing: Option<NowPlaying>,
    pub current: Option<u64>,
    pub tracks: Vec<TrackRow>,
    /// Whether this firmware has a phone channel the importer can use.
    pub import_supported: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum LibraryTarget {
    NowPlaying,
    Song(u64),
}

/// Changes whenever the song list or the now-playing row would draw differently.
pub fn library_key() -> u64 {
    with_core(|core| {
        let current = core.app.player.current.as_ref().map_or(0, |song| song.id);
        let transport = PlayerView::of(&core.app).map_or(0, |view| view.transport as u64 + 1);
        (u64::from(core.app.library.revision) << 40)
            ^ (current << 8)
            ^ transport
            ^ ((core.app.tracks().len() as u64) << 32)
    })
}

pub fn library_frame() -> LibraryFrame {
    with_core(|core| {
        let view = PlayerView::of(&core.app);
        LibraryFrame {
            now_playing: view.as_ref().map(|view| NowPlaying {
                title: view.title.clone(),
                state: ui::library::transport_label(view.transport),
                palette: view.palette,
            }),
            current: core.app.player.current.as_ref().map(|song| song.id),
            tracks: core
                .app
                .tracks()
                .iter()
                .map(|song| TrackRow {
                    id: song.id,
                    name: song.name.clone(),
                    artist: song.artist_line(),
                    palette: song.palette,
                })
                .collect(),
            import_supported: runtime().connect_result.load(Ordering::Acquire) == 0,
        }
    })
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum PlayerEvent {
    Back,
    Previous,
    Toggle,
    Next,
    Mode,
    VolumeDown,
    VolumeUp,
}

pub fn handle_library_event(target: LibraryTarget) {
    let effects = with_core(|core| match target {
        LibraryTarget::NowPlaying => Some(core.app.update(Action::Open(Route::Player))),
        LibraryTarget::Song(id) => {
            let song: Option<Song> = core.app.tracks().iter().find(|song| song.id == id).cloned();
            song.map(|song| core.app.update(Action::SelectSong(song)))
        }
    });
    if let Some(effects) = effects {
        execute_effects(effects);
    }
}

pub fn handle_player_event(event: PlayerEvent) {
    let page = Route::Player.page_index();
    let effects = with_core(|core| match event {
        PlayerEvent::Back => None,
        PlayerEvent::Toggle => {
            core.pending_audio = Some(runtime::PendingAudioCommand::Toggle);
            Some(Vec::new())
        }
        PlayerEvent::VolumeDown | PlayerEvent::VolumeUp => {
            let current = core.app.player.volume_percent;
            let volume = if matches!(event, PlayerEvent::VolumeDown) {
                current.saturating_sub(10)
            } else {
                current.saturating_add(10).min(100)
            };
            core.pending_audio = Some(runtime::PendingAudioCommand::SetVolume(volume));
            Some(Vec::new())
        }
        PlayerEvent::Previous => Some(core.app.update(Action::Previous)),
        PlayerEvent::Next => Some(core.app.update(Action::Next)),
        PlayerEvent::Mode => Some(core.app.update(Action::CycleMode)),
    });
    match effects {
        None => handle_back(page),
        Some(effects) => execute_effects(effects),
    }
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
        ui::back(page_index);
    }
}

fn route_page(route: Route) -> usize {
    match route {
        Route::Library => native_app::PAGE_LIBRARY,
        Route::Player => native_app::PAGE_PLAYER,
    }
}

/// The module's control queue holds this many records and drops the newest
/// when full, so draining exactly that many per tick can never leave a gesture
/// stranded, while still bounding the work one tick may do.
const MAX_HEADSET_CONTROLS_PER_TICK: usize = 8;

/// Turns AVRCP gestures from the headset into the same actions the on-screen
/// controls produce.
///
/// Play/pause is routed through the player's own transport rule rather than
/// applied blindly: the headset repeats a gesture whenever it is unsure of our
/// state, and toggling on a repeat would invert playback instead of confirming
/// it. Track changes are the app's decision, not the player's, so they go
/// straight to `update`.
fn apply_headset_controls(core: &mut runtime::Core) {
    for _ in 0..MAX_HEADSET_CONTROLS_PER_TICK {
        let control = match core.audio.poll_headset_control() {
            Ok(Some(control)) => control,
            Ok(None) => return,
            Err(error) => {
                runtime().last_error.store(error, Ordering::Release);
                return;
            }
        };
        match control {
            MediaControl::Play | MediaControl::Pause => {
                if core.app.player.transport_control_applies(control) {
                    core.pending_audio = Some(runtime::PendingAudioCommand::Toggle);
                }
            }
            MediaControl::Next => {
                let effects = core.app.update(Action::Next);
                queue_audio_effects(core, effects);
            }
            MediaControl::Previous => {
                let effects = core.app.update(Action::Previous);
                queue_audio_effects(core, effects);
            }
        }
    }
}

/// Runs on the Bluetooth owner thread. Everything it does must stay inside the
/// non-blocking `try_with_core`: this thread can preempt a UI thread that holds
/// the core lock, so spinning in `with_core` here would never see it released.
pub fn audio_service_tick() {
    let tick = runtime().timer_ticks.fetch_add(1, Ordering::AcqRel) + 1;
    let _ = try_with_core(|core| {
        apply_headset_controls(core);
        if let Some(command) = core.pending_audio.take() {
            match command {
                runtime::PendingAudioCommand::Stream(path) => {
                    if let Ok(volume) = core.audio.volume_percent() {
                        if core.app.player.sync_volume(volume) {
                            core.app.generation = core.app.generation.wrapping_add(1).max(1);
                        }
                    }
                    if let Err(error) = core.audio.start_local(&path, &mut core.app.player) {
                        core.app.error = Some(alloc::format!(
                            "local audio start {}: {}",
                            core.audio.failure_stage(),
                            error
                        ));
                        core.app.player.state = PlaybackState::Failed;
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                        runtime().last_error.store(error, Ordering::Release);
                        return;
                    }
                    core.app.error = None;
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                }
                runtime::PendingAudioCommand::Toggle => {
                    if let Err(error) = core.app.player.toggle(&mut core.audio) {
                        core.app.error = Some(alloc::format!("audio ioctl failed: {error}"));
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                        runtime().last_error.store(error, Ordering::Release);
                        return;
                    }
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                }
                runtime::PendingAudioCommand::SetVolume(volume) => {
                    if let Err(error) = core.app.player.set_volume(volume, &mut core.audio) {
                        core.app.error = Some(alloc::format!("volume ioctl failed: {error}"));
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                        runtime().last_error.store(error, Ordering::Release);
                        return;
                    }
                }
                runtime::PendingAudioCommand::Stop => {
                    core.audio.stop_local();
                }
            }
        }
        if let Err(error) = core.audio.pump_local(&mut core.app.player) {
            core.audio.abort_local();
            core.app.player.state = PlaybackState::Failed;
            core.app.error = Some(alloc::format!(
                "local audio pump {}: {}",
                core.audio.failure_stage(),
                error
            ));
            core.app.generation = core.app.generation.wrapping_add(1).max(1);
            runtime().last_error.store(error, Ordering::Release);
            return;
        }
        if core.app.player.state == PlaybackState::Draining && !core.audio.local_is_open() {
            let effects = if core.app.has_next() {
                core.app.update(Action::Next)
            } else {
                // Nothing follows. Without this the player would sit in
                // Draining forever and the home row would keep advertising the
                // last track as "即将结束" long after the audio stopped.
                core.app.update(Action::PlaybackExhausted)
            };
            queue_audio_effects(core, effects);
            runtime()
                .player_media_refresh_pending
                .store(true, Ordering::Release);
        }
        if tick % 5 == 0 {
            if let Some(position_ms) = core.audio.playback_position_ms() {
                if core.app.player.sync_position(position_ms) {
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                }
            } else if core.app.player.state == PlaybackState::Playing {
                let _ = core.app.update(Action::Tick(250));
            }
        }
        if tick % 20 == 0 && core.audio.is_open() {
            match core.audio.volume_percent() {
                Ok(volume) if core.app.player.sync_volume(volume) => {
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                }
                Ok(_) => {}
                Err(error) => runtime().last_error.store(error, Ordering::Release),
            }
        }
    });
}

pub fn ui_maintenance_tick() {
    let service_result = audio_service::start();
    if service_result != 0 {
        runtime()
            .last_error
            .store(service_result, Ordering::Release);
    }
    let rt = runtime();
    let tick = rt.ui_ticks.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    // The library file is polled once a second; the importer also raises the
    // dirty flag the moment it commits a change.
    if tick % 10 == 1 || rt.library_dirty.swap(false, Ordering::AcqRel) {
        reload_library_if_changed();
    }
    load_pending_lyrics();
}

fn reload_library_if_changed() {
    let stamp = storage::library_stamp();
    let rt = runtime();
    if stamp == rt.library_stamp.load(Ordering::Acquire) && stamp != 0 {
        return;
    }
    match storage::load_library() {
        Ok(library) => {
            rt.library_stamp.store(stamp, Ordering::Release);
            let first = !rt.library_loaded.swap(true, Ordering::AcqRel);
            let effects = with_core(|core| {
                if first {
                    core.app.update(Action::Boot(library))
                } else {
                    core.app.update(Action::ReloadLibrary(library))
                }
            });
            execute_effects(effects);
        }
        Err(error) => rt.last_error.store(error, Ordering::Release),
    }
}

/// Reads the lyrics file of the current track once it is selected. This runs
/// on the UI thread; the Bluetooth thread only changes the selection.
fn load_pending_lyrics() {
    let wanted = with_core(|core| {
        let song = core.app.player.current.as_ref()?;
        if core.app.lyrics_for == Some(song.id) {
            return None;
        }
        Some((song.id, song.lyrics_path()))
    });
    let Some((id, path)) = wanted else { return };
    let lyrics = path.and_then(|path| storage::read_lyrics(&path));
    with_core(|core| core.app.update(Action::LyricsLoaded { id, lyrics }));
}

/// Executes effects on the UI thread: navigation drives the page manager and
/// LVGL, so it is only valid here.
pub(crate) fn execute_effects(effects: Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::Navigate(route) => ui::navigate(route_page(route)),
            effect => with_core(|core| queue_audio_effect(core, effect)),
        }
    }
}

/// Queues the audio side of effects on a core the caller already holds, for
/// the Bluetooth thread. Track changes never navigate (see `LyraApp::update`),
/// so there is no navigation to lose here.
fn queue_audio_effects(core: &mut runtime::Core, effects: alloc::vec::Vec<Effect>) {
    for effect in effects {
        queue_audio_effect(core, effect);
    }
}

fn queue_audio_effect(core: &mut runtime::Core, effect: Effect) {
    match effect {
        Effect::StreamAudio { path } => {
            if !storage::is_module_path(&path) {
                core.app.error = Some(alloc::string::String::from("invalid local audio path"));
                core.app.player.state = PlaybackState::Failed;
                core.app.generation = core.app.generation.wrapping_add(1).max(1);
                return;
            }
            core.pending_audio = Some(runtime::PendingAudioCommand::Stream(path));
        }
        Effect::StopAudio => core.pending_audio = Some(runtime::PendingAudioCommand::Stop),
        Effect::Navigate(_) | Effect::LoadLyrics { .. } => {}
    }
}
