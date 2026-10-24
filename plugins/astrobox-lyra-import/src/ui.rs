use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use qrcode::{QrCode, render::svg};
use serde::Deserialize;

use crate::astrobox::psys_host_v4::{
    dialog::{self, DialogButton, DialogInfo, DialogStyle, DialogType, FilterConfig, PickConfig},
    ui,
};
use crate::state::{self, NeteaseView, Tab, UiState};
use crate::view::{Kind, On, View};
use crate::{artwork, connect, id3, import, library, netease};

const EVENT_TAB_PREFIX: &str = "action:tab.";
const EVENT_PICK_AUDIO: &str = "action:file.audio";
const EVENT_PICK_COVER: &str = "action:file.cover";
const EVENT_PICK_LYRICS: &str = "action:file.lyrics";
const EVENT_REFRESH: &str = "action:devices.refresh";
const EVENT_START_LOCAL: &str = "action:import.local";
const EVENT_CANCEL: &str = "action:import.cancel";
const EVENT_DISMISS: &str = "action:import.dismiss";
const EVENT_DEVICE: &str = "input:device";
const EVENT_NAME: &str = "input:name";
const EVENT_ARTIST: &str = "input:artist";
const EVENT_ALBUM: &str = "input:album";
const EVENT_QUERY: &str = "input:netease.query";
const EVENT_AUDIO_QUALITY: &str = "input:netease.audio-quality";
const EVENT_SEARCH_CLOUD: &str = "action:netease.search";
const EVENT_LOAD_PLAYLISTS: &str = "action:netease.playlists";
const EVENT_CLOSE_PLAYLIST: &str = "action:netease.playlist.close";
const EVENT_PLAYLIST_PAGE_PREVIOUS: &str = "action:netease.playlist.page.previous";
const EVENT_PLAYLIST_PAGE_NEXT: &str = "action:netease.playlist.page.next";
const PLAYLIST_PAGE_SIZE: usize = 50;
const EVENT_SEARCH_RESULT_PREFIX: &str = "action:netease.result.";
const EVENT_PLAYLIST_PREFIX: &str = "action:netease.playlist.";
const EVENT_PLAYLIST_TRACK_PREFIX: &str = "action:netease.track.";
const EVENT_QR_BEGIN: &str = "action:netease.qr.begin";
const EVENT_QR_POLL: &str = "action:netease.qr.poll";
const EVENT_LOGOUT: &str = "action:netease.logout";
const EVENT_LIBRARY_REFRESH: &str = "action:library.refresh";
const EVENT_LIBRARY_MOVE_UP_PREFIX: &str = "action:library.move.up.";
const EVENT_LIBRARY_MOVE_DOWN_PREFIX: &str = "action:library.move.down.";
const EVENT_LIBRARY_DELETE_PREFIX: &str = "action:library.delete.";
const EVENT_PL_CREATE: &str = "action:playlist.create";
const EVENT_PL_RENAME_PREFIX: &str = "action:playlist.rename.";
const EVENT_PL_DELETE_PREFIX: &str = "action:playlist.delete.";
const EVENT_PL_TOGGLE_PREFIX: &str = "action:playlist.toggle.";
/// `action:playlist.add.<playlist id>.<song id>`
const EVENT_PL_ADD_PREFIX: &str = "action:playlist.add.";
/// `action:playlist.remove.<playlist id>.<song id>`
const EVENT_PL_REMOVE_PREFIX: &str = "action:playlist.remove.";

#[derive(Default, Deserialize)]
struct UiPayload {
    #[serde(default)]
    value: Option<String>,
}

pub fn render_main_ui(root: &str) {
    state::with_state(|state| state.root = Some(root.to_string()));
    rerender();
}

pub fn rerender() {
    let state = state::snapshot();
    if let Some(root) = &state.root {
        ui::render(root, page(&state).into_element());
    }
}

fn ids(event: &str, prefix: &str) -> Option<(u64, u64)> {
    let (first, second) = event.strip_prefix(prefix)?.split_once('.')?;
    Some((first.parse().ok()?, second.parse().ok()?))
}

fn id(event: &str, prefix: &str) -> Option<u64> {
    event.strip_prefix(prefix)?.parse().ok()
}

pub async fn on_event(event_id: &str, payload: &str) {
    tracing::info!(event_id, "received Lyra Import UI event");
    let payload = serde_json::from_str::<UiPayload>(payload).unwrap_or_default();
    let value = payload.value.unwrap_or_default();
    match event_id {
        // Typing only updates the state; the input keeps its own text.
        EVENT_NAME => return state::with_state(|state| state.track_name = value),
        EVENT_ARTIST => return state::with_state(|state| state.artist = value),
        EVENT_ALBUM => return state::with_state(|state| state.album = value),
        EVENT_QUERY => return state::with_state(|state| state.netease_query = value),
        EVENT_DEVICE => state::with_state(|state| {
            if state.selected_addr != value {
                state.device_library.clear();
                state.device_playlists.clear();
                state.device_version = 0;
                state.library_revision.clear();
                state.library_total = 0;
                state.library_busy = false;
                state.library_target = None;
                state.expanded_playlist = None;
            }
            state.selected_addr = value;
        }),
        event if event.starts_with(EVENT_TAB_PREFIX) => {
            let tab = match &event[EVENT_TAB_PREFIX.len()..] {
                "local" => Tab::Local,
                "library" => Tab::Library,
                _ => Tab::Cloud,
            };
            state::with_state(|state| state.tab = tab);
            let snapshot = state::snapshot();
            // The first visit reads the watch's library without asking.
            if tab == Tab::Library
                && snapshot.library_revision.is_empty()
                && !snapshot.library_busy
                && !snapshot.active
                && !snapshot.selected_addr.is_empty()
            {
                rerender();
                library::refresh().await;
            }
        }
        EVENT_PICK_AUDIO => pick_asset("audio", &["mp3"]).await,
        EVENT_PICK_COVER => pick_asset("cover", &["jpg", "jpeg", "png"]).await,
        EVENT_PICK_LYRICS => pick_asset("lyrics", &["lrc", "json", "txt"]).await,
        EVENT_REFRESH => {
            connect::refresh_devices().await;
            state::with_state(|state| {
                state.status = if state.devices.is_empty() {
                    "没有找到已连接的手表。".to_string()
                } else {
                    "已刷新连接设备。".to_string()
                }
            });
        }
        EVENT_AUDIO_QUALITY => {
            let bitrate = value.parse().unwrap_or(netease::AUDIO_BITRATE_LOW);
            state::with_state(|state| {
                state.netease_audio_bitrate = netease::normalized_audio_bitrate(bitrate);
            });
        }
        EVENT_START_LOCAL => start_local().await,
        EVENT_SEARCH_CLOUD => search_cloud(),
        EVENT_LOAD_PLAYLISTS => load_playlists(),
        EVENT_CLOSE_PLAYLIST => state::with_state(|state| {
            state.netease_playlist_name.clear();
            state.netease_playlist_tracks.clear();
            state.netease_playlist_page = 0;
            state.netease_view = NeteaseView::Playlists;
        }),
        EVENT_PLAYLIST_PAGE_PREVIOUS => state::with_state(|state| {
            state.netease_playlist_page = state.netease_playlist_page.saturating_sub(1);
        }),
        EVENT_PLAYLIST_PAGE_NEXT => state::with_state(|state| {
            if (state.netease_playlist_page + 1) * PLAYLIST_PAGE_SIZE
                < state.netease_playlist_tracks.len()
            {
                state.netease_playlist_page += 1;
            }
        }),
        EVENT_QR_BEGIN => begin_qr(),
        EVENT_QR_POLL => poll_qr(),
        EVENT_LOGOUT => logout_netease(),
        EVENT_CANCEL => import::cancel().await,
        EVENT_DISMISS => state::with_state(|state| {
            if !state.active {
                state.sent = 0;
                state.total = 0;
                state.status.clear();
            }
        }),
        EVENT_LIBRARY_REFRESH => library::refresh().await,
        EVENT_PL_CREATE => {
            if let Some(name) = ask_name("新建歌单", "给歌单起个名字。", "新建").await {
                library::create_playlist(&name).await;
            }
        }
        event if event.starts_with(EVENT_PL_RENAME_PREFIX) => {
            if let Some(id) = id(event, EVENT_PL_RENAME_PREFIX) {
                let current = playlist_name(id);
                let content = format!("现在叫《{current}》，请输入新的名字。");
                if let Some(name) = ask_name("重命名歌单", &content, "保存").await {
                    library::rename_playlist(id, &name).await;
                }
            }
        }
        event if event.starts_with(EVENT_PL_DELETE_PREFIX) => {
            if let Some(id) = id(event, EVENT_PL_DELETE_PREFIX) {
                confirm_delete_playlist(id, &playlist_name(id)).await;
            }
        }
        event if event.starts_with(EVENT_PL_TOGGLE_PREFIX) => {
            if let Some(id) = id(event, EVENT_PL_TOGGLE_PREFIX) {
                state::with_state(|state| {
                    state.expanded_playlist =
                        (state.expanded_playlist != Some(id)).then_some(id);
                });
            }
        }
        event if event.starts_with(EVENT_PL_ADD_PREFIX) => {
            if let Some((playlist, song)) = ids(event, EVENT_PL_ADD_PREFIX) {
                library::playlist_track(playlist, song, true).await;
            }
        }
        event if event.starts_with(EVENT_PL_REMOVE_PREFIX) => {
            if let Some((playlist, song)) = ids(event, EVENT_PL_REMOVE_PREFIX) {
                library::playlist_track(playlist, song, false).await;
            }
        }
        event if event.starts_with(EVENT_LIBRARY_MOVE_UP_PREFIX) => {
            if let Some(track) = id(event, EVENT_LIBRARY_MOVE_UP_PREFIX) {
                library::move_track(track, "up").await;
            }
        }
        event if event.starts_with(EVENT_LIBRARY_MOVE_DOWN_PREFIX) => {
            if let Some(track) = id(event, EVENT_LIBRARY_MOVE_DOWN_PREFIX) {
                library::move_track(track, "down").await;
            }
        }
        event if event.starts_with(EVENT_LIBRARY_DELETE_PREFIX) => {
            if let Some(track) = id(event, EVENT_LIBRARY_DELETE_PREFIX) {
                let name = state::snapshot()
                    .device_library
                    .iter()
                    .find(|item| item.id == track)
                    .map(|item| item.name.clone())
                    .unwrap_or_else(|| "这首歌".to_string());
                confirm_delete(track, &name).await;
            }
        }
        event if event.starts_with(EVENT_SEARCH_RESULT_PREFIX) => {
            if let Some(index) = id(event, EVENT_SEARCH_RESULT_PREFIX) {
                start_search_result(index as usize).await;
            }
        }
        event if event.starts_with(EVENT_PLAYLIST_TRACK_PREFIX) => {
            if let Some(index) = id(event, EVENT_PLAYLIST_TRACK_PREFIX) {
                start_playlist_track(index as usize).await;
            }
        }
        event if event.starts_with(EVENT_PLAYLIST_PREFIX) => match id(event, EVENT_PLAYLIST_PREFIX) {
            Some(id) => open_playlist(id),
            None => tracing::warn!(event_id = event, "invalid NetEase playlist UI event"),
        },
        _ => {}
    }
    rerender();
}

fn playlist_name(id: u64) -> String {
    state::snapshot()
        .device_playlists
        .iter()
        .find(|list| list.id == id)
        .map(|list| list.name.clone())
        .unwrap_or_else(|| "这个歌单".to_string())
}

fn dialog_buttons(cancel: &str, confirm: &str) -> Vec<DialogButton> {
    vec![
        DialogButton {
            id: "cancel".to_string(),
            primary: false,
            content: cancel.to_string(),
        },
        DialogButton {
            id: "confirm".to_string(),
            primary: true,
            content: confirm.to_string(),
        },
    ]
}

/// Asks for a playlist name; `None` when cancelled or left empty.
async fn ask_name(title: &str, content: &str, confirm: &str) -> Option<String> {
    let result = dialog::show_dialog(
        DialogType::Input,
        DialogStyle::System,
        DialogInfo {
            title: title.to_string(),
            content: content.to_string(),
            buttons: dialog_buttons("取消", confirm),
        },
    )
    .await;
    let name = result.input_result.trim().to_string();
    (result.clicked_btn_id == "confirm" && !name.is_empty()).then_some(name)
}

async fn confirm(title: &str, content: String, confirm: &str) -> bool {
    dialog::show_dialog(
        DialogType::Alert,
        DialogStyle::System,
        DialogInfo {
            title: title.to_string(),
            content,
            buttons: dialog_buttons("取消", confirm),
        },
    )
    .await
    .clicked_btn_id
        == "confirm"
}

async fn confirm_delete(track_id: u64, track_name: &str) {
    let content = format!("确定从手表删除《{track_name}》吗？音频、封面、背景和歌词都会被永久删除。");
    if confirm("删除歌曲", content, "删除").await {
        library::delete_track(track_id).await;
    }
}

async fn confirm_delete_playlist(id: u64, name: &str) {
    let content = format!("确定删除歌单《{name}》吗？里面的歌曲仍会留在手表上。");
    if confirm("删除歌单", content, "删除歌单").await {
        library::delete_playlist(id).await;
    }
}

async fn pick_asset(kind: &str, extensions: &[&str]) {
    let result = dialog::pick_file(
        PickConfig {
            read: false,
            copy_to: Some("media".to_string()),
        },
        FilterConfig {
            multiple: false,
            extensions: extensions
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await;
    let result = match result {
        Ok(result) if !result.name.is_empty() => result,
        Ok(_) => return,
        Err(error) => {
            state::with_state(|state| state.status = format!("无法选择文件：{error}"));
            return;
        }
    };
    let path = format!("media/{}", result.name);
    let inspected = import::inspect_file(&path, &result.name, kind);
    // On Android the name is a content URI segment such as "audio%3A1000012345",
    // so the file's own tags are the only meaningful source for these fields.
    let tags = match &inspected {
        Ok(selected) if kind == "audio" => id3::read_tags(Path::new(&selected.path)),
        _ => id3::TrackTags::default(),
    };
    match inspected {
        Ok(selected) => state::with_state(|state| {
            if kind == "audio" {
                // A new audio file replaces the form's metadata wholesale, so
                // nothing from the previously picked song lingers.
                state.track_name = tags.title.unwrap_or_else(|| {
                    selected
                        .name
                        .rsplit_once('.')
                        .map(|item| item.0)
                        .unwrap_or(&selected.name)
                        .to_string()
                });
                state.artist = tags.artist.unwrap_or_default();
                state.album = tags.album.unwrap_or_default();
                state.duration_ms = selected.duration_ms;
                state.audio = Some(selected);
            } else if kind == "cover" {
                state.cover = Some(selected);
            } else {
                state.lyrics = Some(selected);
            }
            state.status.clear();
        }),
        Err(error) => state::with_state(|state| state.status = format!("文件读取失败：{error}")),
    }
}

async fn start_local() {
    let snapshot = state::snapshot();
    let Some(audio) = snapshot.audio else {
        state::with_state(|state| state.status = "请先选择 MP3。".to_string());
        return;
    };
    if snapshot.track_name.trim().is_empty() {
        state::with_state(|state| state.status = "曲名不能为空。".to_string());
        return;
    }
    let mut assets = vec![import::ImportAsset::audio(audio.path)];
    let mut palette = artwork::Palette::default();
    if let Some(cover) = snapshot.cover {
        let output = artwork::unique_output_directory("local-artwork");
        match artwork::prepare(Path::new(&cover.path), &output) {
            Ok(prepared) => {
                palette = prepared.palette;
                assets.push(import::ImportAsset::cover(prepared.cover_path));
            }
            Err(error) => {
                tracing::warn!(error = %error, "local cover processing skipped");
                state::with_state(|state| {
                    state.status = format!("封面处理失败，将仅导入音频：{error}");
                });
            }
        }
    }
    if let Some(lyrics) = snapshot.lyrics {
        assets.push(import::ImportAsset::lyrics(lyrics.path, lyrics.format));
    }
    let artists = if snapshot.artist.trim().is_empty() {
        Vec::new()
    } else {
        vec![snapshot.artist.trim().to_string()]
    };
    let name = snapshot.track_name.trim().to_string();
    state::with_state(|state| state.import_title = name.clone());
    let result = import::start(import::ImportRequest {
        addr: snapshot.selected_addr,
        track_id: local_track_id(),
        name,
        artists,
        album: snapshot.album.trim().to_string(),
        duration_ms: snapshot.duration_ms,
        palette,
        assets,
    })
    .await;
    if let Err(error) = result {
        state::with_state(|state| state.status = format!("无法开始导入：{error}"));
    }
}

fn search_cloud() {
    let snapshot = state::snapshot();
    if snapshot.netease_query.trim().is_empty() {
        state::with_state(|state| state.status = "请输入要搜索的歌曲。".to_string());
        return;
    }
    match netease::search(&snapshot.netease_query, &snapshot.netease_cookie) {
        Ok(results) => state::with_state(|state| {
            state.netease_results = results;
            state.netease_view = NeteaseView::SearchResults;
            state.status.clear();
        }),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn load_playlists() {
    let snapshot = state::snapshot();
    match netease::user_playlists(&snapshot.netease_cookie) {
        Ok(playlists) => state::with_state(|state| {
            state.netease_playlists = playlists;
            state.netease_playlist_name.clear();
            state.netease_playlist_tracks.clear();
            state.netease_playlist_page = 0;
            state.netease_view = NeteaseView::Playlists;
            state.status.clear();
        }),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn open_playlist(id: u64) {
    let snapshot = state::snapshot();
    tracing::info!(
        playlist_id = id,
        has_login = !snapshot.netease_cookie.is_empty(),
        "requesting NetEase playlist tracks"
    );
    match netease::playlist_tracks(id, &snapshot.netease_cookie) {
        Ok((name, tracks)) => state::with_state(|state| {
            tracing::info!(
                playlist_id = id,
                track_count = tracks.len(),
                playlist_name = %name,
                "NetEase playlist tracks loaded"
            );
            state.netease_playlist_name = name;
            state.netease_playlist_tracks = tracks;
            state.netease_playlist_page = 0;
            state.netease_view = NeteaseView::PlaylistTracks;
            state.status.clear();
        }),
        Err(error) => {
            tracing::warn!(playlist_id = id, error = %error, "NetEase playlist request failed");
            state::with_state(|state| state.status = error);
        }
    }
}

async fn start_search_result(index: usize) {
    let snapshot = state::snapshot();
    let Some(song) = snapshot.netease_results.get(index).cloned() else {
        state::with_state(|state| state.status = "搜索结果已失效，请重新搜索。".to_string());
        return;
    };
    start_cloud_song(song, snapshot).await;
}

async fn start_playlist_track(index: usize) {
    let snapshot = state::snapshot();
    let Some(song) = snapshot.netease_playlist_tracks.get(index).cloned() else {
        state::with_state(|state| state.status = "歌单歌曲已失效，请重新打开歌单。".to_string());
        return;
    };
    start_cloud_song(song, snapshot).await;
}

async fn start_cloud_song(song: state::CloudSong, snapshot: UiState) {
    state::with_state(|state| {
        state.import_title = song.name.clone();
        state.status = format!("正在下载《{}》…", song.name);
    });
    rerender();
    let prepared = match netease::prepare(
        &song,
        &snapshot.netease_cookie,
        snapshot.netease_audio_bitrate,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            state::with_state(|state| state.status = error);
            return;
        }
    };
    let result = import::start(import::ImportRequest {
        addr: snapshot.selected_addr,
        track_id: prepared.song.id,
        name: prepared.song.name,
        artists: prepared.song.artists,
        album: prepared.song.album,
        duration_ms: prepared.song.duration_ms,
        palette: prepared.palette,
        assets: prepared.assets,
    })
    .await;
    if let Err(error) = result {
        state::with_state(|state| state.status = format!("无法开始导入：{error}"));
    }
}

fn begin_qr() {
    match netease::begin_qr_login() {
        Ok((key, url)) => state::with_state(|state| {
            state.qr_key = key;
            state.qr_url = url;
            state.status.clear();
        }),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn poll_qr() {
    let snapshot = state::snapshot();
    if snapshot.qr_key.is_empty() {
        return;
    }
    match netease::poll_qr_login(&snapshot.qr_key) {
        Ok(Some(cookie)) => {
            let saved = state::save_netease_session(&cookie);
            state::with_state(|state| {
                state.netease_cookie = cookie;
                state.qr_key.clear();
                state.qr_url.clear();
                state.status = "网易云登录成功。".to_string();
            });
            load_playlists();
            if let Err(error) = saved {
                state::with_state(|state| state.status = error);
            }
        }
        Ok(None) => state::with_state(|state| {
            state.status = "还没有确认登录：请在网易云音乐 App 里扫码，并在手机上点确认。".to_string()
        }),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn logout_netease() {
    let result = state::clear_netease_session();
    state::with_state(|state| {
        state.netease_cookie.clear();
        state.netease_view = NeteaseView::Home;
        state.netease_results.clear();
        state.netease_playlists.clear();
        state.netease_playlist_name.clear();
        state.netease_playlist_tracks.clear();
        state.netease_playlist_page = 0;
        state.qr_key.clear();
        state.qr_url.clear();
        state.status = match result {
            Ok(()) => "已退出网易云登录。".to_string(),
            Err(error) => error,
        };
    });
}

// ---------------------------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------------------------

/// Colours come from the host's Radix theme, so the page follows light and dark mode.
mod tone {
    pub const TEXT: &str = "var(--gray-12)";
    pub const MUTED: &str = "var(--gray-11)";
    pub const FAINT: &str = "var(--gray-9)";
    pub const WELL: &str = "var(--gray-a3)";
    pub const ACCENT: &str = "var(--accent-11)";
    pub const RED_WELL: &str = "var(--red-a3)";
    pub const RED: &str = "var(--red-11)";
    pub const GREEN_WELL: &str = "var(--green-a3)";
    pub const GREEN: &str = "var(--green-11)";
}

/// The few controls whose icon is their label (Lucide outlines, MIT).
mod icon {
    pub const MORE: &str = r#"<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>"#;
    pub const CHEVRON_RIGHT: &str = r#"<path d="m9 18 6-6-6-6"/>"#;
    pub const CHEVRON_DOWN: &str = r#"<path d="m6 9 6 6 6-6"/>"#;
    pub const X: &str = r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#;
}

fn svg(paths: &str, size: u32, color: &str) -> View {
    let markup = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{paths}</svg>"#
    );
    View::new(Kind::Svg, Some(&markup)).row().color(color)
}

fn text(content: &str, size: u32, color: &str) -> View {
    View::new(Kind::P, Some(content)).font_size(size).color(color)
}

/// One line that ends in an ellipsis when it does not fit.
fn line(content: &str, size: u32, color: &str) -> View {
    text(content, size, color).class("rt-truncate")
}

fn title(content: &str, size: u32) -> View {
    line(content, size, tone::TEXT).class("rt-r-weight-medium")
}

fn heading(content: &str, size: u32) -> View {
    text(content, size, tone::TEXT).class("rt-r-weight-bold")
}

fn button(label: &str, event: &str, variant: &str, color: &str, size: &str) -> View {
    View::new(Kind::Button, None)
        .prop("variant", variant)
        .prop("color", color)
        .prop("size", size)
        .on(On::Click, event)
        .child(View::new(Kind::Span, Some(label)))
}

fn badge(label: &str, color: &str) -> View {
    View::new(Kind::Badge, Some(label))
        .prop("color", color)
        .prop("variant", "soft")
        .prop("radius", "full")
}

fn card() -> View {
    View::new(Kind::Card, None).prop("size", "2").column().gap(12)
}

/// A list inside one card, rows separated by hairlines.
fn list(rows: Vec<View>) -> View {
    let mut card = View::new(Kind::Card, None).prop("size", "1").column();
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        card = card.child(row.padding_y(8));
        if index + 1 < count {
            card = card.child(View::new(Kind::Separator, None).prop("size", "4"));
        }
    }
    card
}

fn row() -> View {
    View::new(Kind::Div, None).row().align_center().gap(12)
}

fn stack(gap: u32) -> View {
    View::new(Kind::Div, None).column().gap(gap)
}

fn spacer() -> View {
    View::new(Kind::Div, None).grow()
}

fn section_header(label: &str, detail: &str, action: Option<View>) -> View {
    row()
        .padding_x(4)
        .child(
            stack(2)
                .grow()
                .child(heading(label, 16))
                .maybe((!detail.is_empty()).then(|| line(detail, 12, tone::MUTED))),
        )
        .maybe(action)
}

fn empty_state(label: &str, detail: &str, action: Option<View>) -> View {
    card()
        .align_center()
        .padding(28)
        .child(heading(label, 16))
        .child(
            text(detail, 13, tone::MUTED)
                .class("rt-r-ta-center")
                .max_width(320),
        )
        .maybe(action)
}

/// A dropdown behind a `⋯` button.
fn menu(items: Vec<View>, disabled: bool) -> View {
    let trigger = View::new(Kind::Button, None)
        .prop("variant", "ghost")
        .prop("color", "gray")
        .prop("size", "1")
        .disabled_if(disabled)
        .child(svg(icon::MORE, 18, "currentColor"));
    View::new(Kind::MenuRoot, None)
        .child(View::new(Kind::MenuTrigger, None).child(trigger))
        .child(
            View::new(Kind::MenuContent, None)
                .prop("size", "2")
                .prop("align", "end")
                .children(items),
        )
}

fn menu_item(label: &str, event: &str, color: Option<&str>, disabled: bool) -> View {
    let item = View::new(Kind::MenuItem, Some(label)).on(On::Click, event);
    let item = match color {
        Some(color) => item.prop("color", color),
        None => item,
    };
    item.disabled_if(disabled)
}

fn input(placeholder: &str, value: &str, event_id: &str) -> View {
    View::new(Kind::Input, None)
        .width_full()
        .prop("size", "3")
        .prop("placeholder", placeholder)
        .prop("default-value", value)
        .prop("key", event_id)
        .on(On::Input, event_id)
}

fn field(label: &str, control: View) -> View {
    stack(6)
        .grow()
        .child(text(label, 12, tone::MUTED).class("rt-r-weight-medium"))
        .child(control)
}

/// The whole plugin page.
pub fn page(state: &UiState) -> View {
    let device = !state.selected_addr.is_empty();
    stack(16)
        .width_full()
        .padding(16)
        .child(header(state))
        .maybe(device_picker(state))
        .maybe((!device).then(no_device))
        .maybe(transfer(state))
        .maybe(notice(state))
        .child(tabs(state))
}

fn header(state: &UiState) -> View {
    let device_name = state
        .devices
        .iter()
        .find(|device| device.addr == state.selected_addr)
        .map(|device| device.name.as_str());
    let (dot, status) = match device_name {
        Some(name) => ("var(--green-9)", name.to_string()),
        None => ("var(--gray-8)", "未连接手表".to_string()),
    };
    row()
        .child(
            stack(4)
                .grow()
                .child(heading("Lyra 音乐导入", 20))
                .child(
                    row()
                        .gap(6)
                        .child(View::new(Kind::Div, None).square(8).radius(4).bg(dot))
                        .child(line(&status, 13, tone::MUTED)),
                ),
        )
        .child(button("刷新", EVENT_REFRESH, "soft", "gray", "2"))
}

/// Only worth showing when there is something to choose.
fn device_picker(state: &UiState) -> Option<View> {
    if state.devices.len() < 2 {
        return None;
    }
    let mut select = View::new(Kind::Select, None)
        .width_full()
        .prop("size", "2")
        .prop("default-value", &state.selected_addr)
        .prop("key", EVENT_DEVICE)
        .on(On::Change, EVENT_DEVICE);
    for device in &state.devices {
        select = select.child(View::new(Kind::Option, Some(&device.name)).prop("value", &device.addr));
    }
    Some(field("导入到", select))
}

fn no_device() -> View {
    callout("没有连接手表",
        "请先在 AstroBox 里连接手环，并确认手表上的 Lyra Player 已启用。",
        tone::WELL,
        tone::MUTED,
    )
}

fn callout(label: &str, detail: &str, background: &str, color: &str) -> View {
    View::new(Kind::Div, None)
        .row()
        .align_start()
        .gap(10)
        .padding(12)
        .radius(10)
        .bg(background)
        .child(
            stack(2)
                .grow()
                .maybe((!label.is_empty()).then(|| text(label, 14, color).class("rt-r-weight-medium")))
                .child(text(detail, 13, color)),
        )
}

/// The status line, when no transfer card carries it.
fn notice(state: &UiState) -> Option<View> {
    if state.status.is_empty() || transfer_visible(state) {
        return None;
    }
    let status = state.status.as_str();
    let failed = ["失败", "无法", "错误", "拒绝", "不足", "过旧", "不能", "失效"]
        .iter()
        .any(|word| status.contains(word));
    let succeeded = status.contains("成功") || status.contains("完成");
    Some(if failed {
        callout("", status, tone::RED_WELL, tone::RED)
    } else if succeeded {
        callout("", status, tone::GREEN_WELL, tone::GREEN)
    } else {
        callout("", status, tone::WELL, tone::MUTED)
    })
}

fn transfer_visible(state: &UiState) -> bool {
    state.active || state.total > 0
}

fn transfer(state: &UiState) -> Option<View> {
    if !transfer_visible(state) {
        return None;
    }
    let percent = state
        .sent
        .saturating_mul(100)
        .checked_div(state.total)
        .unwrap_or(0)
        .min(100);
    let done = !state.active && state.total > 0 && state.sent >= state.total;
    let (label, color) = if state.active {
        ("正在导入到手表", "blue")
    } else if done {
        ("已导入到手表", "green")
    } else {
        ("导入没有完成", "red")
    };
    let ink = match color {
        "green" => tone::GREEN,
        "red" => tone::RED,
        _ => tone::ACCENT,
    };
    let song = if state.import_title.is_empty() {
        "歌曲"
    } else {
        state.import_title.as_str()
    };
    let mut detail = format!(
        "{} / {}",
        size_text(state.sent),
        size_text(state.total)
    );
    if state.active && state.speed_bytes_per_second > 0 {
        detail.push_str(&format!(" · {}/s", size_text(state.speed_bytes_per_second)));
        if let Some(left) = remaining_text(
            state.total.saturating_sub(state.sent),
            state.speed_bytes_per_second,
        ) {
            detail.push_str(&format!(" · {left}"));
        }
    }
    let message = if state.active || done || state.status.is_empty() {
        None
    } else {
        Some(text(&state.status, 13, tone::RED))
    };
    let action = if state.active {
        button("取消", EVENT_CANCEL, "soft", "red", "1")
    } else {
        button("知道了", EVENT_DISMISS, "soft", "gray", "1")
    };
    Some(
        card()
            .child(
                row()
                    .child(
                        stack(2)
                            .grow()
                            .child(title(song, 15))
                            .child(line(label, 12, ink)),
                    )
                    .child(badge(&format!("{percent}%"), color)),
            )
            .child(
                View::new(Kind::Progress, None)
                    .prop("value", &percent.to_string())
                    .prop("max", "100")
                    .prop("size", "2")
                    .prop("color", color),
            )
            .maybe(message)
            .child(
                row()
                    .child(line(&detail, 12, tone::MUTED).grow())
                    .child(action),
            ),
    )
}

fn tabs(state: &UiState) -> View {
    let current = match state.tab {
        Tab::Cloud => "cloud",
        Tab::Local => "local",
        Tab::Library => "library",
    };
    let library_label = if state.device_library.is_empty() {
        "手表曲库".to_string()
    } else {
        format!("手表曲库 · {}", state.device_library.len())
    };
    let trigger = |value: &str, label: &str| {
        View::new(Kind::TabsTrigger, Some(label))
            .prop("value", value)
            .on(On::Click, &format!("{EVENT_TAB_PREFIX}{value}"))
    };
    let content = |value: &str, body: View| {
        View::new(Kind::TabsContent, None)
            .prop("value", value)
            .child(body.margin_top(16))
    };
    let body = match state.tab {
        Tab::Cloud => cloud_tab(state),
        Tab::Local => local_tab(state),
        Tab::Library => library_tab(state),
    };
    View::new(Kind::TabsRoot, None)
        .prop("value", current)
        .child(
            View::new(Kind::TabsList, None)
                .prop("size", "2")
                .child(trigger("cloud", "网易云音乐"))
                .child(trigger("local", "本地文件"))
                .child(trigger("library", &library_label)),
        )
        .child(content(current, body))
}

/// Why importing is not possible right now, if it is not.
fn import_blocked(state: &UiState) -> bool {
    state.active || state.selected_addr.is_empty()
}

// --- NetEase ---------------------------------------------------------------------------------

fn cloud_tab(state: &UiState) -> View {
    if state.netease_cookie.is_empty() {
        return cloud_login(state);
    }
    let bitrate = netease::normalized_audio_bitrate(state.netease_audio_bitrate);
    let search = row()
        .gap(8)
        .child(input("搜索歌曲、歌手或专辑", &state.netease_query, EVENT_QUERY).grow())
        .child(button("搜索", EVENT_SEARCH_CLOUD, "solid", "red", "3"));
    let toolbar = row()
        .gap(8)
        .child(button("我的歌单", EVENT_LOAD_PLAYLISTS, "soft", "gray", "2"))
        .child(quality_select(bitrate))
        .child(spacer())
        .child(button("退出", EVENT_LOGOUT, "ghost", "gray", "2"));
    let hint = (bitrate > netease::AUDIO_BITRATE_LOW).then(|| {
        callout("",
            "音质越高，下载和传到手表越慢，播放也更容易卡顿。一般选 128 kbps 就够了。",
            tone::WELL,
            tone::MUTED,
        )
    });
    let results = match state.netease_view {
        NeteaseView::SearchResults => song_section(
            "搜索结果",
            &format!("{} 首", state.netease_results.len()),
            &state.netease_results,
            EVENT_SEARCH_RESULT_PREFIX,
            0,
            state,
            None,
        ),
        NeteaseView::Playlists => cloud_playlists(state),
        NeteaseView::PlaylistTracks => cloud_playlist_tracks(state),
        NeteaseView::Home => empty_state("找一首歌",
            "在上面搜索，或者打开“我的歌单”，点歌曲旁的“导入”就能传到手表。",
            None,
        ),
    };
    stack(14).child(search).child(toolbar).maybe(hint).child(results)
}

fn cloud_login(state: &UiState) -> View {
    let mut panel = card()
        .align_center()
        .padding(28)
        .child(heading("登录网易云音乐", 18))
        .child(
            text(
                "登录后可以搜索歌曲、浏览你的歌单，一键导入到手表。登录信息只保存在本机。",
                13,
                tone::MUTED,
            )
            .class("rt-r-ta-center")
            .max_width(340),
        );
    if state.qr_url.is_empty() {
        panel = panel.child(button("扫码登录", EVENT_QR_BEGIN, "solid", "red", "3"));
    } else {
        panel = panel
            .child(
                View::new(Kind::Div, None)
                    .padding(12)
                    .radius(16)
                    .bg("#ffffff")
                    .child(
                        View::new(Kind::Image, Some(&qr_image_url(&state.qr_url)))
                            .width(196)
                            .height(196),
                    ),
            )
            .child(text("用网易云音乐 App 扫一扫，并在手机上确认", 13, tone::MUTED))
            .child(
                row()
                    .gap(8)
                    .child(button("我已确认", EVENT_QR_POLL, "solid", "red", "3"))
                    .child(button("换一张", EVENT_QR_BEGIN, "soft", "gray", "3")),
            );
    }
    panel
}

fn quality_select(bitrate: u32) -> View {
    let option = |label: &str, value: u32| {
        View::new(Kind::Option, Some(label)).prop("value", &value.to_string())
    };
    View::new(Kind::Select, None)
        .prop("size", "2")
        .prop("default-value", &bitrate.to_string())
        .prop("key", EVENT_AUDIO_QUALITY)
        .on(On::Change, EVENT_AUDIO_QUALITY)
        .child(option("128 kbps · 推荐", netease::AUDIO_BITRATE_LOW))
        .child(option("192 kbps", netease::AUDIO_BITRATE_MEDIUM))
        .child(option("320 kbps", netease::AUDIO_BITRATE_HIGH))
}

/// A small cover from NetEase's image service; the well behind it shows while it loads.
fn cover(url: &str) -> Option<View> {
    if url.is_empty() {
        return None;
    }
    let url = url.replacen("http://", "https://", 1);
    let separator = if url.contains('?') { '&' } else { '?' };
    Some(
        View::new(Kind::Image, Some(&format!("{url}{separator}param=96y96")))
            .square(44)
            .radius(8)
            .bg(tone::WELL),
    )
}

fn song_row(song: &state::CloudSong, event: &str, blocked: bool) -> View {
    let artists = if song.artists.is_empty() {
        "未知歌手".to_string()
    } else {
        song.artists.join(" / ")
    };
    let meta = if song.album.is_empty() {
        format!("{artists} · {}", duration_text(song.duration_ms))
    } else {
        format!("{artists} · {} · {}", song.album, duration_text(song.duration_ms))
    };
    row()
        .maybe(cover(&song.cover_url))
        .child(stack(2).grow().child(title(&song.name, 14)).child(line(&meta, 12, tone::MUTED)))
        .child(button("导入", event, "soft", "red", "2").disabled_if(blocked))
}

fn song_section(
    label: &str,
    detail: &str,
    songs: &[state::CloudSong],
    prefix: &str,
    start: usize,
    state: &UiState,
    action: Option<View>,
) -> View {
    if songs.is_empty() {
        return empty_state("没有找到歌曲", "换个关键词试试。", None);
    }
    let blocked = import_blocked(state);
    let rows = songs
        .iter()
        .enumerate()
        .skip(start)
        .take(PLAYLIST_PAGE_SIZE)
        .map(|(index, song)| song_row(song, &format!("{prefix}{index}"), blocked))
        .collect();
    stack(10).child(section_header(label, detail, action)).child(list(rows))
}

fn cloud_playlists(state: &UiState) -> View {
    if state.netease_playlists.is_empty() {
        return empty_state("没有歌单", "这个账号还没有创建或收藏歌单。", None);
    }
    let rows = state
        .netease_playlists
        .iter()
        .take(60)
        .map(|playlist| {
            let creator = if playlist.creator.is_empty() {
                String::new()
            } else {
                format!(" · {}", playlist.creator)
            };
            row()
                .on(On::Click, &format!("{EVENT_PLAYLIST_PREFIX}{}", playlist.id))
                .child(
                    stack(2)
                        .grow()
                        .child(title(&playlist.name, 14))
                        .child(line(
                            &format!("{} 首{creator}", playlist.track_count),
                            12,
                            tone::MUTED,
                        )),
                )
                .child(svg(icon::CHEVRON_RIGHT, 18, tone::FAINT))
        })
        .collect();
    stack(10)
        .child(section_header(
            "我的歌单",
            &format!("{} 个 · 点开后选择要导入的歌曲", state.netease_playlists.len()),
            None,
        ))
        .child(list(rows))
}

fn cloud_playlist_tracks(state: &UiState) -> View {
    let pages = state
        .netease_playlist_tracks
        .len()
        .div_ceil(PLAYLIST_PAGE_SIZE)
        .max(1);
    let page = state.netease_playlist_page.min(pages - 1);
    let back = button("返回我的歌单", EVENT_CLOSE_PLAYLIST, "ghost", "gray", "2");
    let mut body = stack(10).child(row().child(back)).child(song_section(
        &state.netease_playlist_name,
        &format!("{} 首", state.netease_playlist_tracks.len()),
        &state.netease_playlist_tracks,
        EVENT_PLAYLIST_TRACK_PREFIX,
        page * PLAYLIST_PAGE_SIZE,
        state,
        None,
    ));
    if pages > 1 {
        body = body.child(
            row()
                .justify_center()
                .child(
                    button("上一页", EVENT_PLAYLIST_PAGE_PREVIOUS, "soft", "gray", "2")
                        .disabled_if(page == 0),
                )
                .child(text(&format!("{} / {}", page + 1, pages), 13, tone::MUTED))
                .child(
                    button("下一页", EVENT_PLAYLIST_PAGE_NEXT, "soft", "gray", "2")
                        .disabled_if(page + 1 >= pages),
                ),
        );
    }
    body
}

// --- Local files -----------------------------------------------------------------------------

fn local_tab(state: &UiState) -> View {
    let audio = match &state.audio {
        None => card()
            .align_center()
            .padding(28)
            .child(heading("选择一首 MP3", 18))
            .child(
                text("会自动读取歌名、歌手、专辑和时长。", 13, tone::MUTED)
                    .class("rt-r-ta-center"),
            )
            .child(button("选择文件", EVENT_PICK_AUDIO, "solid", "blue", "3")),
        Some(file) => {
            let mut meta = size_text(file.size);
            if file.duration_ms != 0 {
                meta.push_str(&format!(" · {}", duration_text(file.duration_ms)));
            }
            card().child(
                row()
                    .child(stack(2).grow().child(title(&file.name, 14)).child(line(&meta, 12, tone::MUTED)))
                    .child(button("更换", EVENT_PICK_AUDIO, "soft", "gray", "2")),
            )
        }
    };
    let Some(_) = &state.audio else {
        return audio;
    };
    let details = card()
        .child(heading("歌曲信息", 15))
        .child(field("歌名", input("歌名", &state.track_name, EVENT_NAME)))
        .child(
            row()
                .gap(12)
                .align_start()
                .child(field("歌手", input("可不填", &state.artist, EVENT_ARTIST)))
                .child(field("专辑", input("可不填", &state.album, EVENT_ALBUM))),
        );
    let extra = |label: &str, file: Option<&state::SelectedFile>, empty: &str, event: &str| {
        let (detail, action) = match file {
            Some(file) => (file.name.clone(), "更换"),
            None => (empty.to_string(), "选择"),
        };
        row()
            .child(stack(2).grow().child(title(label, 14)).child(line(&detail, 12, tone::MUTED)))
            .child(button(action, event, "soft", "gray", "2"))
    };
    let extras = card()
        .child(heading("封面和歌词", 15))
        .child(extra(
            "封面",
            state.cover.as_ref(),
            "JPG 或 PNG，会自动裁剪并取色",
            EVENT_PICK_COVER,
        ))
        .child(View::new(Kind::Separator, None).prop("size", "4"))
        .child(extra(
            "歌词",
            state.lyrics.as_ref(),
            "LRC、JSON 或 TXT",
            EVENT_PICK_LYRICS,
        ));
    let start = View::new(Kind::Button, None)
        .prop("variant", "solid")
        .prop("color", "blue")
        .prop("size", "3")
        .width_full()
        .on(On::Click, EVENT_START_LOCAL)
        .disabled_if(import_blocked(state) || state.track_name.trim().is_empty())
        .child(View::new(Kind::Span, Some("导入到手表")));
    stack(14).child(audio).child(details).child(extras).child(start)
}

// --- The watch's library ---------------------------------------------------------------------

fn library_tab(state: &UiState) -> View {
    if state.selected_addr.is_empty() {
        return empty_state("没有连接手表", "连接手表后，这里会列出手表上的歌曲和歌单。", None);
    }
    let loaded = !state.library_revision.is_empty();
    if !loaded {
        return empty_state(if state.library_busy { "正在读取手表曲库…" } else { "读取手表曲库" },
            "查看手表上的歌曲，调整播放顺序、删除歌曲或整理歌单。",
            Some(
                button("读取", EVENT_LIBRARY_REFRESH, "solid", "blue", "3")
                    .disabled_if(state.library_busy || state.active),
            ),
        );
    }
    let refresh = button("刷新", EVENT_LIBRARY_REFRESH, "soft", "gray", "2")
        .disabled_if(state.library_busy || state.active);
    let playlists_ok = state.device_version >= library::PLAYLIST_VERSION;
    let summary = if playlists_ok {
        format!(
            "{} 首歌曲 · {} 个歌单 · 顺序就是手表上的播放顺序",
            state.device_library.len(),
            state.device_playlists.len()
        )
    } else {
        format!("{} 首歌曲 · 顺序就是手表上的播放顺序", state.device_library.len())
    };
    let mut body = stack(12).child(section_header("歌曲", &summary, Some(refresh)));
    if state.device_library.is_empty() {
        body = body.child(empty_state("手表上还没有歌曲",
            "从网易云音乐或本地文件导入一首吧。",
            Some(button("去导入", &format!("{EVENT_TAB_PREFIX}cloud"), "soft", "blue", "2")),
        ));
    } else {
        body = body.child(list(library_rows(state, playlists_ok)));
    }
    if state.device_version != 0 && !playlists_ok {
        body = body.child(callout("",
            "手表上的 Lyra Player 版本较旧，更新后才能使用歌单。",
            tone::WELL,
            tone::MUTED,
        ));
    } else if playlists_ok {
        body = body.child(playlists_section(state));
    }
    body
}

fn library_rows(state: &UiState, playlists_ok: bool) -> Vec<View> {
    let busy = state.library_busy || state.active;
    let last = state.device_library.len().saturating_sub(1);
    state
        .device_library
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let artists = if track.artists.is_empty() {
                "未知歌手".to_string()
            } else {
                track.artists.join(" / ")
            };
            let mut items = vec![
                menu_item("上移", &format!("{EVENT_LIBRARY_MOVE_UP_PREFIX}{}", track.id), None, index == 0),
                menu_item("下移", &format!("{EVENT_LIBRARY_MOVE_DOWN_PREFIX}{}", track.id), None, index == last),
            ];
            if playlists_ok && !state.device_playlists.is_empty() {
                items.push(View::new(Kind::MenuSeparator, None));
                for list in &state.device_playlists {
                    let member = list.tracks.contains(&track.id);
                    items.push(if member {
                        menu_item(
                            &format!("从《{}》移出", list.name),
                            &format!("{EVENT_PL_REMOVE_PREFIX}{}.{}", list.id, track.id),
                            None,
                            false,
                        )
                    } else {
                        menu_item(
                            &format!("加入《{}》", list.name),
                            &format!("{EVENT_PL_ADD_PREFIX}{}.{}", list.id, track.id),
                            None,
                            false,
                        )
                    });
                }
            }
            items.push(View::new(Kind::MenuSeparator, None));
            items.push(menu_item(
                "从手表删除",
                &format!("{EVENT_LIBRARY_DELETE_PREFIX}{}", track.id),
                Some("red"),
                false,
            ));
            let working = state.library_target == Some(track.id);
            row()
                .child(
                    text(&(index + 1).to_string(), 13, tone::FAINT)
                        .width(22)
                        .class("rt-r-ta-center"),
                )
                .child(
                    stack(2)
                        .grow()
                        .child(title(&track.name, 14))
                        .child(line(
                            &if track.album.is_empty() {
                                format!("{artists} · {}", duration_text(track.duration_ms))
                            } else {
                                format!("{artists} · {} · {}", track.album, duration_text(track.duration_ms))
                            },
                            12,
                            tone::MUTED,
                        )),
                )
                .maybe(working.then(|| badge("处理中", "blue")))
                .child(menu(items, busy))
        })
        .collect()
}

fn playlists_section(state: &UiState) -> View {
    let busy = state.library_busy || state.active;
    let create = button("新建", EVENT_PL_CREATE, "soft", "blue", "2").disabled_if(busy);
    let header = section_header("歌单", "歌单只引用歌曲：同一首歌可以放进多个歌单", Some(create));
    if state.device_playlists.is_empty() {
        return stack(12).child(header).child(empty_state("还没有歌单",
            "新建一个歌单，然后在歌曲的“⋯”菜单里把歌加进来。",
            None,
        ));
    }
    let rows = state
        .device_playlists
        .iter()
        .map(|list| {
            let open = state.expanded_playlist == Some(list.id);
            let mut item = stack(8).child(
                row()
                    .child(
                        stack(2)
                            .grow()
                            .child(title(&list.name, 14))
                            .child(line(&format!("{} 首", list.tracks.len()), 12, tone::MUTED)),
                    )
                    .child(
                        View::new(Kind::Button, None)
                            .prop("variant", "ghost")
                            .prop("color", "gray")
                            .prop("size", "1")
                            .on(On::Click, &format!("{EVENT_PL_TOGGLE_PREFIX}{}", list.id))
                            .disabled_if(list.tracks.is_empty())
                            .child(svg(if open { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT }, 18, "currentColor")),
                    )
                    .child(menu(
                        vec![
                            menu_item("重命名", &format!("{EVENT_PL_RENAME_PREFIX}{}", list.id), None, false),
                            View::new(Kind::MenuSeparator, None),
                            menu_item("删除歌单", &format!("{EVENT_PL_DELETE_PREFIX}{}", list.id), Some("red"), false),
                        ],
                        busy,
                    )),
            );
            if open {
                let songs = list.tracks.iter().map(|song| {
                    let name = state
                        .device_library
                        .iter()
                        .find(|track| track.id == *song)
                        .map_or("（手表上已不存在的歌曲）", |track| track.name.as_str());
                    row()
                        .gap(8)
                        .padding(8)
                        .radius(8)
                        .bg(tone::WELL)
                        .child(line(name, 13, tone::TEXT).grow())
                        .child(
                            View::new(Kind::Button, None)
                                .prop("variant", "ghost")
                                .prop("color", "gray")
                                .prop("size", "1")
                                .on(On::Click, &format!("{EVENT_PL_REMOVE_PREFIX}{}.{song}", list.id))
                                .disabled_if(busy)
                                .child(svg(icon::X, 14, "currentColor")),
                        )
                });
                item = item.child(stack(6).children(songs));
            }
            item
        })
        .collect();
    stack(12).child(header).child(list(rows))
}

// --- Formatting ------------------------------------------------------------------------------

/// The time left at the measured speed, e.g. `剩余约 3 分 20 秒`.
fn remaining_text(left: u64, speed: u64) -> Option<String> {
    if left == 0 || speed == 0 {
        return None;
    }
    let seconds = left.div_ceil(speed);
    Some(if seconds >= 60 {
        format!("剩余约 {} 分 {} 秒", seconds / 60, seconds % 60)
    } else {
        format!("剩余约 {seconds} 秒")
    })
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn duration_text(duration_ms: u32) -> String {
    if duration_ms == 0 {
        return "--:--".to_string();
    }
    format!("{}:{:02}", duration_ms / 60_000, duration_ms / 1_000 % 60)
}

fn local_track_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn qr_image_url(value: &str) -> String {
    let Ok(code) = QrCode::new(value.as_bytes()) else {
        return String::new();
    };
    let image = code
        .render::<svg::Color>()
        .min_dimensions(260, 260)
        .quiet_zone(false)
        .build();
    format!("data:image/svg+xml;base64,{}", BASE64.encode(image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CloudPlaylist, CloudSong, DeviceInfo, DeviceLibraryTrack, DevicePlaylist, SelectedFile};

    #[test]
    fn remaining_time_rounds_up_and_needs_a_speed() {
        assert_eq!(remaining_text(0, 10_000), None);
        assert_eq!(remaining_text(10_000, 0), None);
        assert_eq!(remaining_text(10_001, 10_000).as_deref(), Some("剩余约 2 秒"));
        assert_eq!(remaining_text(5 * 1_048_576, 12_000).as_deref(), Some("剩余约 7 分 17 秒"));
    }

    #[test]
    fn qr_login_image_is_embedded_data_uri() {
        let uri = qr_image_url("https://music.163.com/login?codekey=test");
        assert!(uri.starts_with("data:image/svg+xml;base64,"));
        let encoded = uri.split_once(',').unwrap().1;
        let svg = BASE64.decode(encoded).unwrap();
        assert!(svg.starts_with(b"<?xml"));
    }

    #[test]
    fn event_ids_carry_their_numbers() {
        assert_eq!(ids("action:playlist.add.7.42", EVENT_PL_ADD_PREFIX), Some((7, 42)));
        assert_eq!(ids("action:playlist.add.7", EVENT_PL_ADD_PREFIX), None);
        assert_eq!(id("action:library.delete.9", EVENT_LIBRARY_DELETE_PREFIX), Some(9));
    }

    fn song(id: u64, name: &str, artist: &str, album: &str, seconds: u32) -> CloudSong {
        CloudSong {
            id,
            name: name.into(),
            artists: vec![artist.into()],
            album: album.into(),
            duration_ms: seconds * 1000,
            cover_url: String::new(),
        }
    }

    fn connected() -> UiState {
        UiState {
            devices: vec![DeviceInfo {
                addr: "AA".into(),
                name: "Xiaomi Smart Band 10 Pro 8FE4".into(),
            }],
            selected_addr: "AA".into(),
            ..UiState::default()
        }
    }

    /// The page for a few typical states, for previews.
    fn scenes() -> Vec<(&'static str, UiState)> {
        let songs = vec![
            song(1, "晴天", "周杰伦", "叶惠美", 269),
            song(2, "夜空中最亮的星", "逃跑计划", "世界", 252),
            song(3, "Shape of You", "Ed Sheeran", "÷ (Deluxe)", 233),
            song(4, "起风了", "买辣椒也用券", "起风了", 325),
        ];
        let mut scenes = Vec::new();

        scenes.push(("cloud-login", connected()));

        let mut search = connected();
        search.netease_cookie = "x".into();
        search.netease_query = "周杰伦".into();
        search.netease_view = NeteaseView::SearchResults;
        search.netease_results = songs.clone();
        search.netease_audio_bitrate = netease::AUDIO_BITRATE_LOW;
        scenes.push(("cloud-search", search.clone()));

        let mut importing = search.clone();
        importing.active = true;
        importing.import_title = "晴天".into();
        importing.sent = 2_300_000;
        importing.total = 4_300_000;
        importing.speed_bytes_per_second = 72_000;
        importing.status = "正在传输到手表…".into();
        scenes.push(("cloud-importing", importing));

        let mut playlists = search.clone();
        playlists.netease_view = NeteaseView::Playlists;
        playlists.netease_playlists = vec![
            CloudPlaylist { id: 1, name: "我喜欢的音乐".into(), creator: "Lyra".into(), track_count: 328 },
            CloudPlaylist { id: 2, name: "通勤路上".into(), creator: "Lyra".into(), track_count: 46 },
            CloudPlaylist { id: 3, name: "跑步 BPM 170".into(), creator: "跑者".into(), track_count: 21 },
        ];
        scenes.push(("cloud-playlists", playlists));

        let mut local = connected();
        local.tab = Tab::Local;
        scenes.push(("local-empty", local.clone()));
        local.audio = Some(SelectedFile {
            name: "Jay Chou - 晴天.mp3".into(),
            size: 4_300_000,
            duration_ms: 269_000,
            ..SelectedFile::default()
        });
        local.track_name = "晴天".into();
        local.artist = "周杰伦".into();
        local.album = "叶惠美".into();
        local.cover = Some(SelectedFile { name: "cover.jpg".into(), ..SelectedFile::default() });
        scenes.push(("local-ready", local));

        let mut library = connected();
        library.tab = Tab::Library;
        library.library_revision = "r1".into();
        library.device_version = library::PLAYLIST_VERSION;
        library.device_library = songs
            .iter()
            .map(|song| DeviceLibraryTrack {
                id: song.id,
                name: song.name.clone(),
                artists: song.artists.clone(),
                album: song.album.clone(),
                duration_ms: song.duration_ms,
            })
            .collect();
        library.device_playlists = vec![
            DevicePlaylist { id: 10, name: "跑步".into(), tracks: vec![2, 4] },
            DevicePlaylist { id: 11, name: "睡前".into(), tracks: vec![] },
        ];
        library.expanded_playlist = Some(10);
        scenes.push(("library", library));

        let mut done = connected();
        done.tab = Tab::Library;
        done.import_title = "夜空中最亮的星".into();
        done.sent = 3_900_000;
        done.total = 3_900_000;
        done.status = "导入完成，歌曲已出现在手表的 Lyra Player 中。".into();
        scenes.push(("done-library-unread", done));

        scenes.push(("offline", UiState::default()));
        scenes
    }

    #[test]
    fn every_scene_builds() {
        for (_, state) in scenes() {
            let mut id = 0;
            assert!(page(&state).to_json(&mut id)["children"].is_array());
        }
    }

    /// `LYRA_UI_PREVIEW_DIR=dir cargo test preview` writes each scene's tree as JSON.
    #[test]
    fn preview() {
        let Ok(dir) = std::env::var("LYRA_UI_PREVIEW_DIR") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        for (name, state) in scenes() {
            let mut id = 0;
            let json = page(&state).to_json(&mut id);
            std::fs::write(format!("{dir}/{name}.json"), json.to_string()).unwrap();
        }
    }
}
