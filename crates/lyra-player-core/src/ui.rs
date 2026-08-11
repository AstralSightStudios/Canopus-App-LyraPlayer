use alloc::{format, string::String};
use canopus_ui_core::{
    ActionRow, NavigationPage, Section, Snapshot, StatusRow, Text, TextStyle, Tree, UiError, View,
    view,
};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{LyraApp, Playlist, QrStatus, Route, Song, playback::PlaybackState};

pub const EVENT_BACK: u32 = 1;
pub const EVENT_LOGIN: u32 = 2;
pub const EVENT_REFRESH: u32 = 3;
pub const EVENT_LIBRARY: u32 = 4;
pub const EVENT_SEARCH: u32 = 5;
pub const EVENT_LOGOUT: u32 = 6;
pub const EVENT_TOGGLE: u32 = 7;
pub const EVENT_NEXT: u32 = 8;
pub const EVENT_LYRICS: u32 = 9;
pub const EVENT_RETRY_LOGIN: u32 = 10;
pub const EVENT_NOW_PLAYING: u32 = 11;
pub const EVENT_PHONE_SEARCH: u32 = 12;
pub const EVENT_PLAYLIST_BASE: u32 = 1_000;
pub const EVENT_SONG_BASE: u32 = 2_000;
pub const EVENT_ARTIST_BASE: u32 = 3_000;
pub const EVENT_LOCAL_SONG_BASE: u32 = 4_000;

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
        Route::Login => login(app),
        Route::Library => library(app),
        Route::Playlist => playlist(app),
        Route::Search => search(app),
        Route::Artist => artist(app),
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
    let greeting = app
        .profile
        .as_ref()
        .map(|profile| profile.nickname.as_str())
        .unwrap_or("让旋律在腕上继续");
    let account_action = if app.session.is_some() {
        ("退出登录", "清除本机登录信息", EVENT_LOGOUT)
    } else {
        ("扫码登录", "同步你的网易云音乐", EVENT_LOGIN)
    };
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
                text: greeting,
                style: TextStyle::Title
            },
            ActionRow {
                key: 3,
                label: now,
                detail: playback_label(app.player.state),
                event: UiEvent(EVENT_NOW_PLAYING),
                enabled: app.player.current.is_some()
            },
            Section {
                key: 10,
                title: "为你而选",
                children: PlaylistRows {
                    playlists: &app.daily_playlists,
                    key_base: 100,
                    event_base: EVENT_PLAYLIST_BASE
                },
            },
            Section {
                key: 20,
                title: "我的歌单",
                children: PlaylistRows {
                    playlists: &app.playlists,
                    key_base: 200,
                    event_base: EVENT_PLAYLIST_BASE + 100
                },
            },
            ActionRow {
                key: 30,
                label: "本地音乐",
                detail: "通过手机导入的 MP3",
                event: UiEvent(EVENT_LIBRARY),
                enabled: true
            },
            ActionRow {
                key: 31,
                label: "搜索",
                detail: "在手机输入，手表浏览",
                event: UiEvent(EVENT_SEARCH),
                enabled: true
            },
            ActionRow {
                key: 32,
                label: account_action.0,
                detail: account_action.1,
                event: UiEvent(account_action.2),
                enabled: true
            },
            ErrorText { app, key: 90 },
        ),
    });
    let mut tree = Tree::begin();
    <_ as View<UiEvent>>::render(&view, &mut tree)?;
    commit(tree, app.generation)
}

fn login(app: &LyraApp) -> Result<Snapshot, UiError> {
    let status = match app.qr.status {
        QrStatus::Idle => "正在准备二维码…",
        QrStatus::WaitingScan => "用网易云音乐扫码",
        QrStatus::WaitingConfirm => "在手机上确认登录",
        QrStatus::Authorized => "登录成功，正在同步",
        QrStatus::Expired => "二维码已过期",
        QrStatus::Failed => "二维码加载失败",
    };
    let qr = qr_text(&app.qr.url).unwrap_or_else(|| app.qr.url.clone());
    let view = view!(NavigationPage {
        key: 1,
        title: "登录网易云音乐",
        children: (
            Text {
                key: 2,
                text: status,
                style: TextStyle::Title
            },
            Text {
                key: 3,
                text: qr.as_str(),
                style: TextStyle::Description
            },
            ActionRow {
                key: 4,
                label: "刷新二维码",
                detail: "生成新的登录码",
                event: UiEvent(EVENT_RETRY_LOGIN),
                enabled: matches!(app.qr.status, QrStatus::Expired | QrStatus::Failed)
            },
            ActionRow {
                key: 5,
                label: "返回",
                detail: "稍后再登录",
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

fn library(app: &LyraApp) -> Result<Snapshot, UiError> {
    let view = view!(NavigationPage {
        key: 1,
        title: "本地音乐",
        children: (
            Text {
                key: 2,
                text: "已导入到手表的 MP3",
                style: TextStyle::Description
            },
            SongRows {
                songs: &app.local_tracks,
                key_base: 100,
                event_base: EVENT_LOCAL_SONG_BASE
            },
            ActionRow {
                key: 3,
                label: "返回",
                detail: "回到 Lyra",
                event: UiEvent(EVENT_BACK),
                enabled: true
            },
        ),
    });
    let mut tree = Tree::begin();
    <_ as View<UiEvent>>::render(&view, &mut tree)?;
    commit(tree, app.generation)
}

fn playlist(app: &LyraApp) -> Result<Snapshot, UiError> {
    let empty = Playlist::default();
    let playlist = app.selected_playlist.as_ref().unwrap_or(&empty);
    let detail = if playlist.description.is_empty() {
        format!("{} 首歌曲", playlist.track_count)
    } else {
        playlist.description.clone()
    };
    let view = view!(NavigationPage {
        key: 1,
        title: playlist.name.as_str(),
        children: (
            Text {
                key: 2,
                text: detail.as_str(),
                style: TextStyle::Description
            },
            SongRows {
                songs: &playlist.tracks,
                key_base: 100,
                event_base: EVENT_SONG_BASE
            },
            ActionRow {
                key: 3,
                label: "返回",
                detail: "回到上一页",
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

fn search(app: &LyraApp) -> Result<Snapshot, UiError> {
    let title = if app.search.query.is_empty() {
        "搜索音乐"
    } else {
        app.search.query.as_str()
    };
    let view = view!(NavigationPage {
        key: 1,
        title,
        children: (
            ActionRow {
                key: 2,
                label: "在手机输入关键词",
                detail: "结果会自动出现在手表",
                event: UiEvent(EVENT_PHONE_SEARCH),
                enabled: true
            },
            Section {
                key: 10,
                title: "歌曲",
                children: SongRows {
                    songs: &app.search.songs,
                    key_base: 100,
                    event_base: EVENT_SONG_BASE
                }
            },
            Section {
                key: 20,
                title: "歌手",
                children: ArtistRows { app }
            },
            ActionRow {
                key: 3,
                label: "返回",
                detail: "回到上一页",
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

fn artist(app: &LyraApp) -> Result<Snapshot, UiError> {
    let name = app
        .selected_artist
        .as_ref()
        .map(|artist| artist.name.as_str())
        .unwrap_or("歌手");
    let description = app
        .selected_artist
        .as_ref()
        .map(|artist| artist.brief_desc.as_str())
        .unwrap_or("");
    let songs = app
        .selected_artist
        .as_ref()
        .map(|artist| artist.songs.as_slice())
        .unwrap_or(&[]);
    let view = view!(NavigationPage {
        key: 1,
        title: name,
        children: (
            Text {
                key: 2,
                text: description,
                style: TextStyle::Description
            },
            Section {
                key: 10,
                title: "热门歌曲",
                children: SongRows {
                    songs,
                    key_base: 100,
                    event_base: EVENT_SONG_BASE
                }
            },
            ActionRow {
                key: 3,
                label: "返回",
                detail: "回到上一页",
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
                    detail: "选择一首歌",
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
        .unwrap_or("歌词将在这里亮起");
    let toggle = if app.player.state == PlaybackState::Paused {
        "继续播放"
    } else {
        "暂停"
    };
    let view = view!(NavigationPage {
        key: 1,
        title: "正在播放",
        children: (
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
            StatusRow {
                key: 5,
                label: progress.as_str(),
                value: playback_label(app.player.state)
            },
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
            (
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
    let start = active.unwrap_or(0).saturating_sub(4);
    for (offset, line) in app
        .player
        .lyrics
        .lines
        .iter()
        .skip(start)
        .take(10)
        .enumerate()
    {
        tree.text(
            100 + offset as u32,
            &line.text,
            if Some(start + offset) == active {
                TextStyle::Title
            } else {
                TextStyle::Description
            },
        )?;
        if let Some(translation) = &line.translation {
            tree.text(200 + offset as u32, translation, TextStyle::Description)?;
        }
    }
    tree.action_row(3, "返回播放页", "继续播放", EVENT_BACK, true)?;
    tree.end()?;
    commit(tree, app.generation)
}

struct PlaylistRows<'a> {
    playlists: &'a [Playlist],
    key_base: u32,
    event_base: u32,
}
impl View<UiEvent> for PlaylistRows<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        for (index, playlist) in self.playlists.iter().take(8).enumerate() {
            let detail = format!("{} 首", playlist.track_count);
            tree.action_row(
                self.key_base + index as u32,
                &playlist.name,
                &detail,
                self.event_base + index as u32,
                true,
            )?;
        }
        Ok(())
    }
}

struct SongRows<'a> {
    songs: &'a [Song],
    key_base: u32,
    event_base: u32,
}
impl View<UiEvent> for SongRows<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        for (index, song) in self.songs.iter().take(20).enumerate() {
            let artist = song.artist_line();
            tree.action_row(
                self.key_base + index as u32,
                &song.name,
                &artist,
                self.event_base + index as u32,
                true,
            )?;
        }
        Ok(())
    }
}

struct ArtistRows<'a> {
    app: &'a LyraApp,
}
impl View<UiEvent> for ArtistRows<'_> {
    fn render(&self, tree: &mut Tree) -> Result<(), UiError> {
        for (index, artist) in self.app.search.artists.iter().take(8).enumerate() {
            tree.action_row(
                300 + index as u32,
                &artist.name,
                "查看热门歌曲与专辑",
                EVENT_ARTIST_BASE + index as u32,
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
        PlaybackState::Resolving => "正在获取音频",
        PlaybackState::Buffering => "缓冲中",
        PlaybackState::Playing => "播放中",
        PlaybackState::Paused => "已暂停",
        PlaybackState::Draining => "即将结束",
        PlaybackState::Failed => "播放失败",
    }
}

fn progress_text(position_ms: u32, duration_ms: u32) -> String {
    format!(
        "{:02}:{:02}  ━━━  {:02}:{:02}",
        position_ms / 60_000,
        position_ms / 1_000 % 60,
        duration_ms / 60_000,
        duration_ms / 1_000 % 60,
    )
}

fn qr_text(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    const MAX_VERSION: Version = Version::new(15);
    const BUFFER_LEN: usize = MAX_VERSION.buffer_len();
    let mut temporary = [0u8; BUFFER_LEN];
    let mut output_buffer = [0u8; BUFFER_LEN];
    let code = QrCode::encode_text(
        url,
        &mut temporary,
        &mut output_buffer,
        QrCodeEcc::Medium,
        Version::MIN,
        MAX_VERSION,
        None,
        true,
    )
    .ok()?;
    let width = code.size() as usize;
    let quiet = 2usize;
    let mut output = String::new();
    for y in (0..width + quiet * 2).step_by(2) {
        for x in 0..width + quiet * 2 {
            let top = qr_dark(&code, x, y, quiet);
            let bottom = qr_dark(&code, x, y + 1, quiet);
            output.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        output.push('\n');
    }
    Some(output)
}

fn qr_dark(code: &QrCode<'_>, x: usize, y: usize, quiet: usize) -> bool {
    let width = code.size() as usize;
    if x < quiet || y < quiet || x >= width + quiet || y >= width + quiet {
        return false;
    }
    code.get_module((x - quiet) as i32, (y - quiet) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopus_ui_core::NodeKind;

    #[test]
    fn every_route_renders_a_valid_snapshot() {
        let mut app = LyraApp {
            error: Some("network unavailable".into()),
            ..LyraApp::default()
        };
        for route in [
            Route::Home,
            Route::Login,
            Route::Library,
            Route::Playlist,
            Route::Search,
            Route::Artist,
            Route::Player,
            Route::Lyrics,
        ] {
            app.route = route;
            let snapshot = render(&app).unwrap();
            assert_eq!(snapshot.nodes[0].kind(), Some(NodeKind::NavigationPage));
        }
    }

    #[test]
    fn qr_fallback_is_scannable_shape() {
        let qr = qr_text("https://music.163.com/login?codekey=test").unwrap();
        assert!(qr.lines().count() > 10);
        assert!(qr.contains('█'));
    }
}
