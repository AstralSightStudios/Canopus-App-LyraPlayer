use alloc::{format, string::String};
use canopus_ui_core::{
    ActionRow, Layout, NavigationPage, Progress, Snapshot, StatusRow, Text, TextStyle, Tree,
    UiError, View, view,
};

use crate::{LyraApp, Route, Song, playback::PlaybackState};

pub const EVENT_BACK: u32 = 1;
pub const EVENT_LIBRARY: u32 = 2;
pub const EVENT_TOGGLE: u32 = 3;
pub const EVENT_NEXT: u32 = 4;
pub const EVENT_LYRICS: u32 = 5;
pub const EVENT_NOW_PLAYING: u32 = 6;
pub const EVENT_LOCAL_SONG_BASE: u32 = 1_000;

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
        Route::Lyrics => lyrics(app),
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
        "来自 Lyra Import 的音频、封面与歌词"
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
            SongRows { songs: &app.local_tracks },
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
    let progress = progress_text(app.player.position_ms, app.player.duration_ms);
    let lyric = app
        .player
        .lyrics
        .active_index(app.player.position_ms)
        .and_then(|index| app.player.lyrics.lines.get(index))
        .map(|line| line.text.as_str())
        .unwrap_or("暂无歌词");
    let toggle = if app.player.state == PlaybackState::Paused {
        "继续播放"
    } else {
        "暂停"
    };
    let view = view!(NavigationPage {
        key: 1,
        title: "正在播放",
        children: (
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
            Text {
                key: 4,
                text: lyric,
                style: TextStyle::Warning
            },
            (
                StatusRow {
                    key: 5,
                    label: progress.as_str(),
                    value: playback_label(app.player.state)
                },
                Progress {
                    key: 50,
                    value: app.player.position_ms.min(app.player.duration_ms).min(i32::MAX as u32) as i32,
                    minimum: 0,
                    maximum: app.player.duration_ms.max(1).min(i32::MAX as u32) as i32,
                    layout: Layout { width: 280, height: 14, ..Layout::default() }
                },
            ),
            (
                ActionRow {
                    key: 6,
                    label: toggle,
                    detail: "播放控制",
                    event: UiEvent(EVENT_TOGGLE),
                    enabled: matches!(app.player.state, PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering)
                },
                ActionRow {
                    key: 7,
                    label: "下一首",
                    detail: "播放队列中的下一首",
                    event: UiEvent(EVENT_NEXT),
                    enabled: !app.player.queue.is_empty()
                },
                ActionRow {
                    key: 8,
                    label: "歌词",
                    detail: "跟随播放进度",
                    event: UiEvent(EVENT_LYRICS),
                    enabled: !app.player.lyrics.lines.is_empty()
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

fn lyrics(app: &LyraApp) -> Result<Snapshot, UiError> {
    let active = app.player.lyrics.active_index(app.player.position_ms);
    let mut tree = Tree::begin();
    tree.navigation_page(1, "歌词")?;
    if app.player.lyrics.lines.is_empty() {
        tree.text(2, "这首歌没有可用歌词", TextStyle::Description)?;
    } else {
        let start = active.unwrap_or(0).saturating_sub(4);
        for (offset, line) in app.player.lyrics.lines.iter().skip(start).take(10).enumerate() {
            tree.text(
                100 + offset as u32,
                &line.text,
                if Some(start + offset) == active { TextStyle::Title } else { TextStyle::Description },
            )?;
            if let Some(translation) = &line.translation {
                tree.text(200 + offset as u32, translation, TextStyle::Description)?;
            }
        }
    }
    tree.action_row(3, "返回播放页", "继续播放", EVENT_BACK, true)?;
    tree.end()?;
    commit(tree, app.generation)
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
            40,
            self.song.id as u32,
            &self.song.album.cover_url,
            Layout { width: 180, height: 180, ..Layout::default() },
        )
    }
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

fn progress_text(position_ms: u32, duration_ms: u32) -> String {
    format!(
        "{:02}:{:02} / {:02}:{:02}",
        position_ms / 60_000,
        position_ms / 1_000 % 60,
        duration_ms / 60_000,
        duration_ms / 1_000 % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopus_ui_core::NodeKind;

    #[test]
    fn every_local_route_renders() {
        let mut app = LyraApp::default();
        for route in [Route::Home, Route::Library, Route::Player, Route::Lyrics] {
            app.route = route;
            let snapshot = render(&app).unwrap();
            assert_eq!(snapshot.nodes[0].kind(), Some(NodeKind::NavigationPage));
        }
    }

    #[test]
    fn imported_cover_is_rendered_on_player() {
        let mut app = LyraApp { route: Route::Player, ..LyraApp::default() };
        app.player.current = Some(Song {
            id: 1,
            name: "Test".into(),
            album: crate::AlbumRef {
                cover_url: alloc::format!("{}/tracks/1/cover.jpg", crate::persistence::IMPORT_ROOT),
                ..crate::AlbumRef::default()
            },
            ..Song::default()
        });
        let snapshot = render(&app).unwrap();
        assert!(snapshot.nodes.iter().take(snapshot.node_count as usize).any(|node| node.kind() == Some(NodeKind::Image)));
    }
}
