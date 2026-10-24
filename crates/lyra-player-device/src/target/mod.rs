//! Target-selected local player integration. Audio feeding runs on the resident
//! Bluetooth owner timer; LVGL page callbacks only manage rendering and input.

use core::sync::atomic::Ordering;

use alloc::{string::String, vec::Vec};
use lyra_player_core::{
    Action, Collection, Effect, Route, Song,
    lyrics::Lyrics,
    playback::{MediaControl, PlaybackMode, PlaybackState},
    view::{PlayerView, RowTarget, Transport},
};

use runtime::{initialized, runtime, try_with_core, with_core};

pub mod audio;
mod audio_service;
pub mod connect;
mod feeder;
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
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub transport: Transport,
    pub has_cover: bool,
}

/// One row as a list page draws it.
pub struct FrameRow {
    pub target: LibraryTarget,
    pub title: String,
    pub subtitle: String,
    /// The track whose cover the row shows.
    pub art: Option<u64>,
}

pub struct LibraryFrame {
    /// Bumped by every import: artwork may have changed under an unchanged track id.
    pub revision: u32,
    pub now_playing: Option<NowPlaying>,
    /// The page's large title.
    pub title: String,
    pub rows: Vec<FrameRow>,
    /// How many songs the library holds, whatever the page shows.
    pub songs: usize,
    /// Whether this firmware has a phone channel the importer can use.
    pub import_supported: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum LibraryTarget {
    NowPlaying,
    Toggle,
    Song(u64),
    Collection(Collection),
    /// Start the open collection from the top in this mode (shuffle or repeat).
    PlayCollection(PlaybackMode),
}

/// Changes whenever the page or the now-playing row would draw differently.
pub fn library_key(page: usize) -> u64 {
    with_core(|core| {
        let current = core.app.player.current.as_ref().map_or(0, |song| song.id);
        let transport = PlayerView::of(&core.app).map_or(0, |view| view.transport as u64 + 1);
        let collection = match core.app.collection {
            Collection::Overview => 0,
            Collection::All => 1,
            Collection::Playlist(id) => 2 ^ (id << 2),
        };
        (u64::from(core.app.library.revision) << 40)
            ^ (current << 8)
            ^ transport
            ^ ((core.app.tracks().len() as u64) << 32)
            ^ (collection << 20)
            ^ ((page as u64) << 62)
    })
}

pub fn library_frame(page: usize) -> LibraryFrame {
    with_core(|core| {
        let view = PlayerView::of(&core.app);
        let (title, rows) = if page == native_app::PAGE_COLLECTION {
            lyra_player_core::view::collection_rows(&core.app)
        } else {
            (
                String::from("音乐"),
                lyra_player_core::view::overview_rows(&core.app),
            )
        };
        LibraryFrame {
            revision: core.app.library.revision,
            now_playing: view.map(|view| NowPlaying {
                id: view.id,
                has_cover: view.cover.is_some(),
                title: view.title,
                artist: view.artist,
                transport: view.transport,
            }),
            title,
            rows: rows
                .into_iter()
                .map(|row| FrameRow {
                    target: match row.target {
                        RowTarget::Song(id) => LibraryTarget::Song(id),
                        RowTarget::Collection(collection) => LibraryTarget::Collection(collection),
                    },
                    title: row.title,
                    subtitle: row.subtitle,
                    art: row.art,
                })
                .collect(),
            songs: core.app.tracks().len(),
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
        LibraryTarget::Toggle => {
            core.toggle_audio();
            Some(Vec::new())
        }
        LibraryTarget::Song(id) => {
            let song: Option<Song> = core.app.tracks().iter().find(|song| song.id == id).cloned();
            song.map(|song| core.app.update(Action::SelectSong(song)))
        }
        LibraryTarget::Collection(collection) => {
            Some(core.app.update(Action::OpenCollection(collection)))
        }
        LibraryTarget::PlayCollection(mode) => {
            let playlist = match core.app.collection {
                Collection::Playlist(id) => Some(id),
                _ => None,
            };
            Some(core.app.update(Action::PlayCollection { playlist, mode }))
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
            core.toggle_audio();
            Some(Vec::new())
        }
        PlayerEvent::VolumeDown | PlayerEvent::VolumeUp => {
            let current = core.app.player.volume_percent;
            let volume = if matches!(event, PlayerEvent::VolumeDown) {
                current.saturating_sub(10)
            } else {
                current.saturating_add(10).min(100)
            };
            core.app.player.volume_percent = volume;
            core.pending_volume = Some(volume);
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
        Route::Collection => native_app::PAGE_COLLECTION,
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
fn apply_headset_controls(core: &mut runtime::Core, audio: &mut audio::AudioDevice) {
    for _ in 0..MAX_HEADSET_CONTROLS_PER_TICK {
        let control = match audio.poll_headset_control() {
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
                    core.toggle_audio();
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

/// Runs on the Bluetooth owner. File IO and the audio endpoint are exclusively
/// owned here; contention with UI snapshots may delay publishing, never feeding.
pub fn audio_service_tick() {
    let tick = runtime().timer_ticks.fetch_add(1, Ordering::AcqRel) + 1;
    let owner = unsafe { runtime::audio_owner() };
    let commands = try_with_core(|core| {
        apply_headset_controls(core, &mut owner.audio);
        (
            core.audio_epoch,
            core.pending_audio.take(),
            core.pending_volume.take(),
        )
    });
    if let Some((epoch, command, volume)) = commands {
        if let Some(command) = command {
            let result = match command {
                runtime::PendingAudioCommand::Stream(path) => {
                    owner.epoch = epoch;
                    owner.error = None;
                    // The headset is the volume authority: start from what it has now, so the
                    // page does not show this player's default for a second.
                    if let Ok(volume) = owner.audio.volume_percent() {
                        owner.player.sync_volume(volume);
                    }
                    owner.audio.start_local(&path, &mut owner.player)
                }
                runtime::PendingAudioCommand::Toggle => owner.player.toggle(&mut owner.audio),
                runtime::PendingAudioCommand::Stop => {
                    owner.epoch = epoch;
                    owner.audio.stop_local();
                    owner.player.clear();
                    Ok(())
                }
            };
            if let Err(error) = result {
                fail_audio(owner, error);
            }
        }
        if let Some(volume) = volume {
            if let Err(error) = owner.player.set_volume(volume, &mut owner.audio) {
                runtime().last_error.store(error, Ordering::Release);
            }
        }
    }
    if let Err(error) = owner.audio.pump_local(&mut owner.player) {
        fail_audio(owner, error);
    }
    if tick % 20 == 0 && owner.audio.is_open() {
        if let Ok(volume) = owner.audio.volume_percent() {
            owner.player.sync_volume(volume);
        }
    }
    let _ = try_with_core(|core| {
        // A user can select another track while the old feed is running. Never
        // publish the old stream's state/position over that newer selection.
        if core.audio_epoch != owner.epoch || owner.epoch == 0 {
            return;
        }
        let mut changed = core.app.player.state != owner.player.state;
        core.app.player.state = owner.player.state;
        changed |= core.app.player.sync_position(owner.player.position_ms);
        // A volume the user just asked for is shown at once; the driver follows next tick.
        if core.pending_volume.is_none() {
            changed |= core.app.player.sync_volume(owner.player.volume_percent);
        }
        if owner.error.is_some() && core.app.error != owner.error {
            core.app.error = owner.error.clone();
            changed = true;
        }
        if changed {
            core.app.generation = core.app.generation.wrapping_add(1).max(1);
        }
        if owner.player.state == PlaybackState::Draining && !owner.audio.local_is_open() {
            let effects = core.app.update(Action::TrackEnded);
            queue_audio_effects(core, effects);
            owner.player.state = PlaybackState::Finished;
            runtime()
                .player_media_refresh_pending
                .store(true, Ordering::Release);
        }
    });
}

fn fail_audio(owner: &mut runtime::AudioOwner, error: i32) {
    owner.error = Some(alloc::format!(
        "audio {}: {}",
        owner.audio.failure_stage(),
        error
    ));
    owner.audio.abort_local();
    owner.player.state = PlaybackState::Failed;
    runtime().last_error.store(error, Ordering::Release);
}

pub fn ui_maintenance_tick() {
    // The track reader runs on this (UI owner) thread; starting it is cheap once it runs.
    feeder::start();
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
            core.audio_epoch = core.audio_epoch.wrapping_add(1).max(1);
            core.pending_audio = Some(runtime::PendingAudioCommand::Stream(path));
        }
        Effect::StopAudio => {
            core.audio_epoch = core.audio_epoch.wrapping_add(1).max(1);
            core.pending_audio = Some(runtime::PendingAudioCommand::Stop);
        }
        Effect::Navigate(_) | Effect::LoadLyrics { .. } => {}
    }
}
