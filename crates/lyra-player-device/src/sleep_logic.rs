use core::sync::atomic::{AtomicBool, Ordering};
use lyra_player_core::{LyraApp, playback::PlaybackState};

// Called only on the serialized UI owner. Clear dirty before the snapshot so
// a concurrent edit remains pending even if this save succeeds.
pub fn persist_settings<E>(
    loaded: &AtomicBool,
    dirty: &AtomicBool,
    mut load: impl FnMut(),
    mut save: impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    if !loaded.load(Ordering::Acquire) {
        load();
        loaded.store(true, Ordering::Release);
    }
    if dirty.swap(false, Ordering::AcqRel) {
        if let Err(error) = save() {
            dirty.store(true, Ordering::Release);
            return Err(error);
        }
    }
    Ok(())
}

pub const MAX_HEADSET_CONTROLS_PER_TICK: usize = 8;

pub fn drain_headset_controls<T, E>(
    discarding: &mut bool,
    expired: bool,
    mut poll: impl FnMut() -> Result<Option<T>, E>,
    mut apply: impl FnMut(T),
) -> Result<(), E> {
    *discarding |= expired;
    for _ in 0..MAX_HEADSET_CONTROLS_PER_TICK {
        match poll()? {
            Some(control) if !*discarding => apply(control),
            Some(_) => {}
            None => {
                *discarding = false;
                break;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct SleepState {
    pub started: bool,
    pub started_at: Option<u64>,
    pub duration_edited: bool,
    pub auto_edited: bool,
    pub timer_edited: bool,
}

impl SleepState {
    pub fn transition(&mut self, before: u32, after: u32, now: Option<u64>) {
        if before == 0 && after != 0 {
            self.started = true;
            self.started_at = now;
        }
    }

    pub fn elapsed(&mut self, previous: &mut Option<u64>, now: Option<u64>) -> u32 {
        if self.started {
            self.started = false;
            *previous = self.started_at.take();
        }
        let Some(now) = now else { return 0 };
        let Some(start) = *previous else {
            *previous = Some(now);
            return 0;
        };
        let elapsed = now.saturating_sub(start).min(u64::from(u32::MAX));
        *previous = Some(start + elapsed);
        elapsed as u32
    }

    pub fn expired(before: u32, after: u32) -> bool {
        before != 0 && after == 0
    }

    pub fn apply_loaded(&self, app: &mut LyraApp, duration: u32, auto: bool) {
        if !self.duration_edited {
            app.sleep_timer_duration_ms = duration;
        }
        if !self.auto_edited {
            app.sleep_timer_auto = auto;
        }
        if !self.timer_edited
            && app.sleep_timer_auto
            && app.sleep_timer_ms == 0
            && matches!(
                app.player.state,
                PlaybackState::Resolving | PlaybackState::Buffering | PlaybackState::Playing
            )
        {
            app.sleep_timer_ms = app.sleep_timer_duration_ms;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_exit_persists_edited_settings_after_load_merge() {
        use core::cell::RefCell;
        let loaded = AtomicBool::new(false);
        let dirty = AtomicBool::new(true);
        let app = RefCell::new(LyraApp::default());
        app.borrow_mut().sleep_timer_duration_ms = 720_000;
        let state = SleepState {
            duration_edited: true,
            ..Default::default()
        };
        let saved = RefCell::new(None);
        persist_settings(
            &loaded,
            &dirty,
            || {
                state.apply_loaded(&mut app.borrow_mut(), 300_000, true);
            },
            || {
                let app = app.borrow();
                *saved.borrow_mut() = Some((app.sleep_timer_duration_ms, app.sleep_timer_auto));
                Ok::<_, i32>(())
            },
        )
        .unwrap();
        assert_eq!(*saved.borrow(), Some((720_000, true)));
        assert!(loaded.load(Ordering::Acquire));
        assert!(!dirty.load(Ordering::Acquire));
    }

    #[test]
    fn failed_save_retries_and_concurrent_edits_remain_dirty() {
        let loaded = AtomicBool::new(false);
        let dirty = AtomicBool::new(true);
        let mut loads = 0;
        let result = persist_settings(&loaded, &dirty, || loads += 1, || Err::<(), _>(-5));
        assert_eq!(result, Err(-5));
        assert!(dirty.load(Ordering::Acquire));
        persist_settings(
            &loaded,
            &dirty,
            || loads += 1,
            || {
                dirty.store(true, Ordering::Release);
                Ok::<_, i32>(())
            },
        )
        .unwrap();
        assert_eq!(loads, 1);
        assert!(dirty.load(Ordering::Acquire));
        persist_settings(&loaded, &dirty, || unreachable!(), || Ok::<_, i32>(())).unwrap();
        assert!(!dirty.load(Ordering::Acquire));
    }

    #[test]
    fn new_timer_excludes_time_before_start_and_restarts() {
        let mut state = SleepState::default();
        let mut previous = Some(100);
        state.transition(0, 600_000, Some(190));
        assert_eq!(state.elapsed(&mut previous, Some(200)), 10);
        assert_eq!(state.elapsed(&mut previous, Some(250)), 50);
        state.transition(0, 600_000, Some(290));
        assert_eq!(state.elapsed(&mut previous, Some(300)), 10);
        state.transition(0, 600_000, None);
        assert_eq!(state.elapsed(&mut previous, Some(350)), 0);
    }

    #[test]
    fn actions_that_start_a_timer_use_their_own_start_time() {
        use lyra_player_core::{Action, Song, persistence::Library, playback::PlaybackMode};
        let song = Song {
            id: 1,
            ..Song::default()
        };
        for entry in 0..6 {
            let mut app = LyraApp::default();
            app.update(Action::Boot(Library {
                tracks: vec![song.clone()],
                ..Library::default()
            }));
            app.update(Action::SetSleepAuto(true));
            app.mode = PlaybackMode::RepeatAll;
            if entry == 3 || entry == 4 || entry == 5 {
                app.update(Action::Play(song.clone()));
                app.update(Action::SetSleepTimer(0));
            }
            let before = app.sleep_timer_ms;
            match entry {
                0 => app.update(Action::SetSleepTimer(600_000)),
                1 => app.update(Action::SelectSong(song.clone())),
                2 => app.update(Action::PlayCollection {
                    playlist: None,
                    mode: PlaybackMode::RepeatAll,
                }),
                3 => app.update(Action::Next),
                4 => app.update(Action::Previous),
                _ => app.update(Action::TrackEnded),
            };
            let mut state = SleepState::default();
            state.transition(before, app.sleep_timer_ms, Some(190));
            assert_ne!(app.sleep_timer_ms, 0, "entry {entry}");
            let elapsed = state.elapsed(&mut Some(100), Some(200));
            app.update(Action::SleepTick(elapsed));
            assert_eq!(app.sleep_timer_ms, 599_990, "entry {entry}");
        }
    }

    #[test]
    fn clock_failures_and_backwards_samples_never_double_count() {
        let mut state = SleepState::default();
        let mut last = Some(1000);
        assert_eq!(state.elapsed(&mut last, Some(1100)), 100);
        assert_eq!(state.elapsed(&mut last, Some(1050)), 0);
        assert_eq!(state.elapsed(&mut last, Some(1080)), 0);
        assert_eq!(state.elapsed(&mut last, None), 0);
        assert_eq!(state.elapsed(&mut last, None), 0);
        assert_eq!(state.elapsed(&mut last, Some(1200)), 100);
        assert_eq!(last, Some(1200));
        assert_eq!(state.elapsed(&mut last, Some(1230)), 30);
        state.transition(0, 60_000, None);
        assert_eq!(state.elapsed(&mut last, None), 0);
        assert_eq!(state.elapsed(&mut last, None), 0);
        assert_eq!(state.elapsed(&mut last, Some(1500)), 0);
        assert_eq!(state.elapsed(&mut last, Some(1550)), 50);
    }

    #[test]
    fn expiration_discards_same_tick_headset_track_changes() {
        use lyra_player_core::{
            Action, Effect, Song, persistence::Library, playback::PlaybackMode,
        };
        for action in [Action::Next, Action::Previous] {
            let song = Song {
                id: 1,
                ..Song::default()
            };
            let mut app = LyraApp::default();
            app.update(Action::Boot(Library {
                tracks: vec![song.clone()],
                ..Library::default()
            }));
            app.mode = PlaybackMode::RepeatAll;
            app.update(Action::SetSleepAuto(true));
            app.update(Action::Play(song));
            let before = app.sleep_timer_ms;
            let effects = app.update(Action::SleepTick(before));
            let expired = SleepState::expired(before, app.sleep_timer_ms);
            let mut pending_stop = effects
                .iter()
                .any(|effect| matches!(effect, Effect::StopAudio));
            let mut queue = std::collections::VecDeque::from([action]);
            let mut discarding = false;
            drain_headset_controls(
                &mut discarding,
                expired,
                || Ok::<_, ()>(queue.pop_front()),
                |action| {
                    pending_stop = !app
                        .update(action)
                        .iter()
                        .any(|effect| matches!(effect, Effect::StreamAudio { .. }));
                },
            )
            .unwrap();
            assert!(pending_stop);
            assert_eq!(app.sleep_timer_ms, 0);
            assert!(app.player.current.is_none());
            assert!(!discarding);
        }
    }

    #[test]
    fn expiration_drains_backlog_across_ticks_and_recovers_after_empty() {
        use lyra_player_core::playback::MediaControl;
        use std::collections::VecDeque;
        let mut queue = VecDeque::from(vec![MediaControl::Next; 17]);
        let mut discarding = false;
        let mut applied = Vec::new();
        for (expired, remaining) in [(true, 9), (false, 1), (false, 0)] {
            let mut polls = 0;
            drain_headset_controls(
                &mut discarding,
                expired,
                || {
                    polls += 1;
                    Ok::<_, ()>(queue.pop_front())
                },
                |control| applied.push(control),
            )
            .unwrap();
            assert!(polls <= MAX_HEADSET_CONTROLS_PER_TICK);
            assert_eq!(queue.len(), remaining);
            assert!(applied.is_empty());
            assert_eq!(discarding, remaining != 0);
        }
        queue.push_back(MediaControl::Next);
        drain_headset_controls(
            &mut discarding,
            false,
            || Ok::<_, ()>(queue.pop_front()),
            |control| applied.push(control),
        )
        .unwrap();
        assert_eq!(applied.len(), 1);
        assert!(matches!(applied[0], MediaControl::Next));
    }

    #[test]
    fn unknown_event_is_not_an_empty_queue() {
        use std::collections::VecDeque;
        let mut queue = VecDeque::from([None, Some(1)]);
        let mut discarding = false;
        let mut applied = Vec::new();
        drain_headset_controls(
            &mut discarding,
            true,
            || Ok::<_, ()>(queue.pop_front()),
            |event| applied.push(event),
        )
        .unwrap();
        assert!(queue.is_empty());
        assert!(applied.is_empty());
        assert!(!discarding);
    }

    #[test]
    fn drain_error_and_exact_budget_do_not_clear_discard_state() {
        use std::collections::VecDeque;
        let mut queue = VecDeque::from(vec![1; MAX_HEADSET_CONTROLS_PER_TICK]);
        let mut discarding = false;
        drain_headset_controls(
            &mut discarding,
            true,
            || Ok::<_, ()>(queue.pop_front()),
            |_| panic!("unexpected event"),
        )
        .unwrap();
        assert!(discarding);
        assert!(
            drain_headset_controls::<i32, i32>(&mut discarding, false, || Err(7), |_| {}).is_err()
        );
        assert!(discarding);
        drain_headset_controls(
            &mut discarding,
            false,
            || Ok::<_, ()>(queue.pop_front()),
            |_| {},
        )
        .unwrap();
        assert!(!discarding);
    }

    #[test]
    fn loaded_settings_preserve_edits_and_manual_stop() {
        let mut app = LyraApp::default();
        app.sleep_timer_duration_ms = 720_000;
        app.sleep_timer_auto = false;
        app.player.state = PlaybackState::Playing;
        let mut state = SleepState {
            duration_edited: true,
            auto_edited: true,
            timer_edited: true,
            ..Default::default()
        };
        state.apply_loaded(&mut app, 300_000, true);
        assert_eq!(
            (
                app.sleep_timer_duration_ms,
                app.sleep_timer_auto,
                app.sleep_timer_ms
            ),
            (720_000, false, 0)
        );
        state.auto_edited = false;
        state.apply_loaded(&mut app, 300_000, true);
        assert_eq!(
            (
                app.sleep_timer_duration_ms,
                app.sleep_timer_auto,
                app.sleep_timer_ms
            ),
            (720_000, true, 0)
        );
        state.timer_edited = false;
        state.apply_loaded(&mut app, 300_000, true);
        assert_eq!(app.sleep_timer_ms, 720_000);
    }
}
