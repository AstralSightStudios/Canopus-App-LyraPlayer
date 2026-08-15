use alloc::format;
use canopus_ui_core::{
    ActionRow, Layout, NavigationPage, Snapshot, StatusRow, Text, TextStyle, Tree, UiError, View,
    view,
};

use crate::{LyraApp, Route, Song, playback::PlaybackState};

pub const EVENT_BACK: u32 = 1;
pub const EVENT_LIBRARY: u32 = 2;
pub const EVENT_TOGGLE: u32 = 3;
pub const EVENT_NEXT: u32 = 4;
pub const EVENT_PREVIOUS: u32 = 5;
pub const EVENT_NOW_PLAYING: u32 = 6;
pub const EVENT_VOLUME_DOWN: u32 = 7;
pub const EVENT_VOLUME_UP: u32 = 8;
pub const EVENT_LOCAL_SONG_BASE: u32 = 1_000;
pub const PLAYER_BACKGROUND_KEY: u32 = 39;
pub const PLAYER_COVER_KEY: u32 = 40;

#[derive(Clone, Copy)]
pub struct UiEvent(pub u32);
impl From<UiEvent> for u32 {
    fn from(value: UiEvent) -> Self {
        value.0
    }
}

pub fn render(app: &LyraApp) -> Result<Snapshot, UiError> {
    match app.route {
        Route::Home => home(app),
        Route::Library => library(app),
        Route::Player => player(app),
    }
}

fn commit(mut tree: Tree, generation: u32) -> Result<Snapshot, UiError> {
    let mut snapshot = tree.commit()?;
    snapshot.generation = generation;
    Ok(snapshot)
}

fn home(app: &LyraApp) -> Result<Snapshot, UiError> {
    let count = format!("{} 首歌曲", app.local_tracks.len());
    let now = app
        .player
        .current
        .as_ref()
        .map(|song| song.name.as_str())
        .unwrap_or("暂无播放");
    let view = view!(NavigationPage {
        key: 1,
        title: "Lyra",
        children: (
            Text {
                key: 2,
                text: "腕上本地音乐",
                style: TextStyle::Title
            },
            ActionRow {
                key: 3,
                label: now,
                detail: playback_label(app.player.state),
                event: UiEvent(EVENT_NOW_PLAYING),
                enabled: app.player.current.is_some()
            },
            ActionRow {
                key: 4,
                label: "本地音乐",
                detail: count.as_str(),
                event: UiEvent(EVENT_LIBRARY),
                enabled: true
            },
            Text {
                key: 5,
                text: "请通过 Lyra Import 快应用导入音乐",
                style: TextStyle::Description
            },
            ErrorText { app, key: 90 },
        ),
    });
    let mut tree = Tree::begin();
    <_ as View<UiEvent>>::render(&view, &mut tree)?;
    commit(tree, app.generation)
}

fn library(app: &LyraApp) -> Result<Snapshot, UiError> {
    let hint = if app.local_tracks.is_empty() {
        "暂无音乐，请先打开 Lyra Import"
    } else {
        "来自 Lyra Import 的音频与封面"
    };
    let view = view!(NavigationPage {
        key: 1,
        title: "本地音乐",
        children: (
            Text {
                key: 2,
                text: hint,
                style: TextStyle::Description
            },
            SongRows {
                songs: &app.local_tracks
            },
            ActionRow {
                key: 3,
                label: "返回",
                detail: "回到 Lyra",
                event: UiEvent(EVENT_BACK),
                enabled: true
            },
            ErrorText { app, key: 90 },
        ),
    });
    let mut tree = Tree::begin();
    <_ as View<UiEvent>>::render(&view, &mut tree)?;
    commit(tree, app.generation)
}

fn player(app: &LyraApp) -> Result<Snapshot, UiError> {
    let Some(song) = &app.player.current else {
        let view = view!(NavigationPage {
            key: 1,
            title: "正在播放",
            children: (
                Text {
                    key: 2,
                    text: "还没有选择歌曲",
                    style: TextStyle::Description
                },
                ActionRow {
                    key: 3,
                    label: "返回",
                    detail: "选择一首本地音乐",
                    event: UiEvent(EVENT_BACK),
                    enabled: true
                },
            ),
        });
        let mut tree = Tree::begin();
        <_ as View<UiEvent>>::render(&view, &mut tree)?;
        return commit(tree, app.generation);
    };
    let artist = song.artist_line();
    let toggle = if app.player.state == PlaybackState::Paused {
        "继续播放"
    } else {
        "暂停"
    };
    let volume = format!("{}%", app.player.volume_percent);
    let view = view!(NavigationPage {
        key: 1,
        title: "正在播放",
        children: (
            BackgroundImage { song },
            CoverImage { song },
            Text {
                key: 2,
                text: song.name.as_str(),
                style: TextStyle::Title
            },
            Text {
                key: 3,
                text: artist.as_str(),
                style: TextStyle::Description
            },
            StatusRow {
                key: 5,
                label: "状态",
                value: playback_label(app.player.state)
            },
            (
                StatusRow {
                    key: 10,
                    label: "音量",
                    value: volume.as_str()
                },
                ActionRow {
                    key: 11,
                    label: "降低音量",
                    detail: "降低 10%",
                    event: UiEvent(EVENT_VOLUME_DOWN),
                    enabled: app.player.volume_percent > 0
                },
                ActionRow {
                    key: 12,
                    label: "提高音量",
                    detail: "提高 10%",
                    event: UiEvent(EVENT_VOLUME_UP),
                    enabled: app.player.volume_percent < 100
                },
            ),
            (
                ActionRow {
                    key: 6,
                    label: toggle,
                    detail: "播放控制",
                    event: UiEvent(EVENT_TOGGLE),
                    enabled: matches!(
                        app.player.state,
                        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
                    )
                },
                ActionRow {
                    key: 7,
                    label: "上一首",
                    detail: "本地音乐列表中的上一首",
                    event: UiEvent(EVENT_PREVIOUS),
                    enabled: app.has_previous()
                },
                ActionRow {
                    key: 8,
                    label: "下一首",
                    detail: "本地音乐列表中的下一首",
                    event: UiEvent(EVENT_NEXT),
                    enabled: app.has_next()
                },
                ActionRow {
                    key: 9,
                    label: "返回",
                    detail: "音乐会继续播放",
                    event: UiEvent(EVENT_BACK),
                    enabled: true
                },
                ErrorText { app, key: 90 },
            ),
        ),
    });
    let mut tree = Tree::begin();
    <_ as View<UiEvent>>::render(&view, &mut tree)?;
    commit(tree, app.generation)
}

struct BackgroundImage<'a> {
    song: &'a Song,
}
impl View<UiEvent> for BackgroundImage<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        if self.song.album.background_url.is_empty() {
            return Ok(());
        }
        tree.image(
            PLAYER_BACKGROUND_KEY,
            image_resource_id(&self.song.album.background_url, 0xBACC_600D),
            &self.song.album.background_url,
            Layout {
                width: 336,
                height: 480,
                ..Layout::default()
            },
        )
    }
}

struct CoverImage<'a> {
    song: &'a Song,
}
impl View<UiEvent> for CoverImage<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        if self.song.album.cover_url.is_empty() {
            return Ok(());
        }
        tree.image(
            PLAYER_COVER_KEY,
            image_resource_id(&self.song.album.cover_url, 0xC0DE_1234),
            &self.song.album.cover_url,
            Layout {
                width: 180,
                height: 180,
                ..Layout::default()
            },
        )
    }
}

fn image_resource_id(path: &str, seed: u32) -> u32 {
    let mut hash = seed;
    for byte in path.as_bytes() {
        hash = (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
    }
    if hash == 0 { 1 } else { hash }
}

struct SongRows<'a> {
    songs: &'a [Song],
}
impl View<UiEvent> for SongRows<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        for (index, song) in self.songs.iter().take(20).enumerate() {
            let artist = song.artist_line();
            tree.action_row(
                100 + index as u32,
                &song.name,
                &artist,
                EVENT_LOCAL_SONG_BASE + index as u32,
                true,
            )?;
        }
        Ok(())
    }
}

struct ErrorText<'a> {
    app: &'a LyraApp,
    key: u32,
}
impl View<UiEvent> for ErrorText<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        if let Some(error) = &self.app.error {
            tree.text(self.key, error, TextStyle::Warning)?;
        }
        Ok(())
    }
}

fn playback_label(state: PlaybackState) -> &'static str {
    match state {
        PlaybackState::Idle => "未播放",
        PlaybackState::Resolving => "正在打开文件",
        PlaybackState::Buffering => "缓冲中",
        PlaybackState::Playing => "播放中",
        PlaybackState::Paused => "已暂停",
        PlaybackState::Draining => "即将结束",
        PlaybackState::Failed => "播放失败",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopus_ui_core::NodeKind;

    #[test]
    fn every_local_route_renders() {
        let mut app = LyraApp::default();
        for route in [Route::Home, Route::Library, Route::Player] {
            app.route = route;
            let snapshot = render(&app).unwrap();
            assert_eq!(snapshot.nodes[0].kind(), Some(NodeKind::NavigationPage));
        }
    }

    #[test]
    fn imported_cover_is_rendered_on_player() {
        let mut app = LyraApp {
            route: Route::Player,
            ..LyraApp::default()
        };
        app.player.current = Some(Song {
            id: 1,
            name: "Test".into(),
            album: crate::AlbumRef {
                cover_url: alloc::format!("{}/tracks/1/cover.jpg", crate::persistence::IMPORT_ROOT),
                background_url: alloc::format!(
                    "{}/tracks/1/background.bin",
                    crate::persistence::IMPORT_ROOT
                ),
                ..crate::AlbumRef::default()
            },
            ..Song::default()
        });
        let snapshot = render(&app).unwrap();
        assert!(
            snapshot
                .nodes
                .iter()
                .take(snapshot.node_count as usize)
                .any(|node| node.kind() == Some(NodeKind::Image))
        );
        let image_keys: alloc::vec::Vec<u32> = snapshot
            .nodes
            .iter()
            .take(snapshot.node_count as usize)
            .filter(|node| node.kind() == Some(NodeKind::Image))
            .map(|node| node.key)
            .collect();
        assert_eq!(
            image_keys,
            alloc::vec![PLAYER_BACKGROUND_KEY, PLAYER_COVER_KEY]
        );
    }
}
