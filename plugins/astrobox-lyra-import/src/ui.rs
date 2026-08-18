use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use qrcode::{QrCode, render::svg};
use serde::Deserialize;

use crate::astrobox::psys_host::{
    dialog::{self, DialogButton, DialogInfo, DialogStyle, DialogType, FilterConfig, PickConfig},
    ui_v3 as ui,
};
use crate::{artwork, import, interconnect, library, netease, state};

const EVENT_PICK_AUDIO: &str = "action:file.audio";
const EVENT_PICK_COVER: &str = "action:file.cover";
const EVENT_PICK_LYRICS: &str = "action:file.lyrics";
const EVENT_REFRESH: &str = "action:devices.refresh";
const EVENT_START_LOCAL: &str = "action:import.local";
const EVENT_CANCEL: &str = "action:import.cancel";
const EVENT_DEVICE: &str = "input:device";
const EVENT_NAME: &str = "input:name";
const EVENT_ARTIST: &str = "input:artist";
const EVENT_ALBUM: &str = "input:album";
const EVENT_DURATION: &str = "input:duration";
const EVENT_QUERY: &str = "input:netease.query";
const EVENT_AUDIO_QUALITY: &str = "input:netease.audio-quality";
const EVENT_CHECKSUM_MODE: &str = "input:transfer.checksum";
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
    if let Some(root) = state::snapshot().root {
        ui::render(&root, build_root());
    }
}

pub fn on_event(event_id: &str, payload: &str) {
    tracing::info!(event_id, "received Lyra Import UI event");
    let payload = serde_json::from_str::<UiPayload>(payload).unwrap_or_default();
    match event_id {
        EVENT_PICK_AUDIO => pick_asset("audio", &["mp3"]),
        EVENT_PICK_COVER => pick_asset("cover", &["jpg", "jpeg", "png"]),
        EVENT_PICK_LYRICS => pick_asset("lyrics", &["lrc", "json", "txt"]),
        EVENT_REFRESH => {
            interconnect::refresh_devices();
            state::with_state(|state| state.status = "已刷新连接设备。".to_string());
        }
        EVENT_DEVICE => {
            state::with_state(|state| {
                let selected_addr = payload.value.unwrap_or_default();
                if state.selected_addr != selected_addr {
                    state.device_library.clear();
                    state.library_request_id.clear();
                    state.library_revision.clear();
                    state.library_total = 0;
                    state.library_busy = false;
                    state.library_target = None;
                }
                state.selected_addr = selected_addr;
            });
            return;
        }
        EVENT_NAME => {
            state::with_state(|state| state.track_name = payload.value.unwrap_or_default());
            return;
        }
        EVENT_ARTIST => {
            state::with_state(|state| state.artist = payload.value.unwrap_or_default());
            return;
        }
        EVENT_ALBUM => {
            state::with_state(|state| state.album = payload.value.unwrap_or_default());
            return;
        }
        EVENT_DURATION => {
            state::with_state(|state| {
                state.duration_ms = payload
                    .value
                    .as_deref()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0)
            });
            return;
        }
        EVENT_QUERY => {
            state::with_state(|state| state.netease_query = payload.value.unwrap_or_default());
            return;
        }
        EVENT_AUDIO_QUALITY => {
            let bitrate = payload
                .value
                .as_deref()
                .and_then(|value| value.parse().ok())
                .unwrap_or(netease::AUDIO_BITRATE_LOW);
            state::with_state(|state| {
                state.netease_audio_bitrate = netease::normalized_audio_bitrate(bitrate);
            });
        }
        EVENT_CHECKSUM_MODE => {
            let mode = match payload.value.as_deref() {
                Some("none-48k") => state::TransferMode::Ultra,
                Some("none") => state::TransferMode::Fast,
                _ => state::TransferMode::Crc32,
            };
            state::with_state(|state| {
                state.transfer_mode = mode;
                state.status = match mode {
                    state::TransferMode::Crc32 => "已启用 CRC32 分片校验。".to_string(),
                    state::TransferMode::Fast => {
                        "已选择无校验高速模式；损坏分片可能无法被发现。".to_string()
                    }
                    state::TransferMode::Ultra => {
                        "已选择无校验超高速模式（48 KiB 单包上限）；强烈不建议用于重要文件。"
                            .to_string()
                    }
                };
            });
        }
        EVENT_START_LOCAL => start_local(),
        EVENT_SEARCH_CLOUD => search_cloud(),
        EVENT_LOAD_PLAYLISTS => load_playlists(),
        EVENT_CLOSE_PLAYLIST => state::with_state(|state| {
            state.netease_playlist_name.clear();
            state.netease_playlist_tracks.clear();
            state.netease_playlist_page = 0;
            state.netease_view = state::NeteaseView::Playlists;
            state.status = "已返回个人歌单。".to_string();
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
        EVENT_LIBRARY_REFRESH => library::refresh(),
        EVENT_CANCEL => wit_bindgen::block_on(import::cancel()),
        event if event.starts_with(EVENT_LIBRARY_MOVE_UP_PREFIX) => {
            if let Some(track_id) = event
                .strip_prefix(EVENT_LIBRARY_MOVE_UP_PREFIX)
                .and_then(|value| value.parse::<u64>().ok())
            {
                library::move_track(track_id, "up");
            }
        }
        event if event.starts_with(EVENT_LIBRARY_MOVE_DOWN_PREFIX) => {
            if let Some(track_id) = event
                .strip_prefix(EVENT_LIBRARY_MOVE_DOWN_PREFIX)
                .and_then(|value| value.parse::<u64>().ok())
            {
                library::move_track(track_id, "down");
            }
        }
        event if event.starts_with(EVENT_LIBRARY_DELETE_PREFIX) => {
            if let Some(track_id) = event
                .strip_prefix(EVENT_LIBRARY_DELETE_PREFIX)
                .and_then(|value| value.parse::<u64>().ok())
            {
                let track_name = state::snapshot()
                    .device_library
                    .iter()
                    .find(|track| track.id == track_id)
                    .map(|track| track.name.clone())
                    .unwrap_or_else(|| "这首音乐".to_string());
                confirm_delete(track_id, &track_name);
            }
        }
        event if event.starts_with(EVENT_SEARCH_RESULT_PREFIX) => {
            if let Some(index) = event
                .strip_prefix(EVENT_SEARCH_RESULT_PREFIX)
                .and_then(|value| value.parse::<usize>().ok())
            {
                start_search_result(index);
            }
        }
        event if event.starts_with(EVENT_PLAYLIST_TRACK_PREFIX) => {
            if let Some(index) = event
                .strip_prefix(EVENT_PLAYLIST_TRACK_PREFIX)
                .and_then(|value| value.parse::<usize>().ok())
            {
                start_playlist_track(index);
            }
        }
        event if event.starts_with(EVENT_PLAYLIST_PREFIX) => {
            match event
                .strip_prefix(EVENT_PLAYLIST_PREFIX)
                .and_then(|value| value.parse::<u64>().ok())
            {
                Some(id) => {
                    tracing::info!(playlist_id = id, "opening NetEase playlist from UI");
                    open_playlist(id);
                }
                None => tracing::warn!(event_id = event, "invalid NetEase playlist UI event"),
            }
        }
        _ => {}
    }
    rerender();
}

fn confirm_delete(track_id: u64, track_name: &str) {
    let result = wit_bindgen::block_on(dialog::show_dialog(
        DialogType::Alert,
        DialogStyle::System,
        &DialogInfo {
            title: "删除音乐".to_string(),
            content: format!(
                "确定删除《{}》吗？音频、封面、背景和歌词都会被永久删除。",
                track_name
            ),
            buttons: vec![
                DialogButton {
                    id: "cancel".to_string(),
                    primary: false,
                    content: "取消".to_string(),
                },
                DialogButton {
                    id: "delete".to_string(),
                    primary: true,
                    content: "删除".to_string(),
                },
            ],
        },
    )
    .into_future());
    if result.clicked_btn_id == "delete" {
        library::delete_track(track_id);
    }
}

fn pick_asset(kind: &str, extensions: &[&str]) {
    let result = wit_bindgen::block_on(
        dialog::pick_file(
            &PickConfig {
                read: false,
                copy_to: Some("media".to_string()),
            },
            &FilterConfig {
                multiple: false,
                extensions: extensions
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
                default_directory: String::new(),
                default_file_name: String::new(),
            },
        )
        .into_future(),
    );
    if result.name.is_empty() {
        return;
    }
    let path = format!("media/{}", result.name);
    match import::inspect_file(&path, &result.name, kind) {
        Ok(selected) => state::with_state(|state| {
            if kind == "audio" {
                state.track_name = selected
                    .name
                    .rsplit_once('.')
                    .map(|item| item.0)
                    .unwrap_or(&selected.name)
                    .to_string();
                if selected.duration_ms != 0 {
                    state.duration_ms = selected.duration_ms;
                }
                state.audio = Some(selected);
            } else if kind == "cover" {
                state.cover = Some(selected);
            } else {
                state.lyrics = Some(selected);
            }
            state.status = format!("已选择{}。", asset_label(kind));
        }),
        Err(error) => state::with_state(|state| state.status = format!("文件读取失败：{error}")),
    }
}

fn start_local() {
    let snapshot = state::snapshot();
    let Some(audio) = snapshot.audio else {
        state::with_state(|state| state.status = "请先选择 MP3。".to_string());
        return;
    };
    if snapshot.track_name.trim().is_empty() {
        state::with_state(|state| state.status = "曲名不能为空。".to_string());
        return;
    }
    let mut assets = vec![import::ImportAsset::audio(audio.path, audio.size)];
    if let Some(cover) = snapshot.cover {
        let output = artwork::unique_output_directory("local-artwork");
        match artwork::prepare(Path::new(&cover.path), &output) {
            Ok(prepared) => {
                assets.push(import::ImportAsset::cover_bin(
                    prepared.cover_path,
                    prepared.cover_size,
                ));
                if let (Some(path), Some(size)) =
                    (prepared.background_path, prepared.background_size)
                {
                    assets.push(import::ImportAsset::background_bin(path, size));
                }
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
        let extension = lyrics
            .name
            .rsplit_once('.')
            .map(|item| item.1)
            .unwrap_or("lrc");
        assets.push(import::ImportAsset::lyrics(
            lyrics.path,
            lyrics.size,
            if extension.eq_ignore_ascii_case("json") {
                "json"
            } else {
                "lrc"
            },
        ));
    }
    let artists = if snapshot.artist.trim().is_empty() {
        Vec::new()
    } else {
        vec![snapshot.artist.trim().to_string()]
    };
    let result = wit_bindgen::block_on(import::start(import::ImportRequest {
        addr: snapshot.selected_addr,
        track_id: local_track_id(),
        name: snapshot.track_name.trim().to_string(),
        artists,
        album: snapshot.album.trim().to_string(),
        album_id: 0,
        duration_ms: snapshot.duration_ms,
        assets,
    }));
    if let Err(error) = result {
        state::with_state(|state| state.status = format!("无法开始导入：{error}"));
    }
}

fn search_cloud() {
    let snapshot = state::snapshot();
    if snapshot.netease_query.trim().is_empty() {
        state::with_state(|state| state.status = "请输入网易云搜索关键词。".to_string());
        return;
    }
    state::with_state(|state| state.status = "正在搜索网易云音乐…".to_string());
    match netease::search(&snapshot.netease_query, &snapshot.netease_cookie) {
        Ok(results) => state::with_state(|state| {
            state.netease_results = results;
            state.netease_view = state::NeteaseView::SearchResults;
            state.status = format!("找到 {} 首歌曲。", state.netease_results.len());
        }),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn load_playlists() {
    let snapshot = state::snapshot();
    state::with_state(|state| state.status = "正在读取个人歌单…".to_string());
    match netease::user_playlists(&snapshot.netease_cookie) {
        Ok(playlists) => state::with_state(|state| {
            state.netease_playlists = playlists;
            state.netease_playlist_name.clear();
            state.netease_playlist_tracks.clear();
            state.netease_playlist_page = 0;
            state.netease_view = state::NeteaseView::Playlists;
            state.status = format!("已加载 {} 个个人歌单。", state.netease_playlists.len());
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
    state::with_state(|state| state.status = "正在读取歌单歌曲…".to_string());
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
            state.netease_view = state::NeteaseView::PlaylistTracks;
            state.status = format!("歌单中有 {} 首歌曲。", state.netease_playlist_tracks.len());
        }),
        Err(error) => {
            tracing::warn!(playlist_id = id, error = %error, "NetEase playlist request failed");
            state::with_state(|state| state.status = error);
        }
    }
}

fn start_search_result(index: usize) {
    let snapshot = state::snapshot();
    let Some(song) = snapshot.netease_results.get(index).cloned() else {
        state::with_state(|state| state.status = "搜索结果已失效，请重新搜索。".to_string());
        return;
    };
    start_cloud_song(song, snapshot);
}

fn start_playlist_track(index: usize) {
    let snapshot = state::snapshot();
    let Some(song) = snapshot.netease_playlist_tracks.get(index).cloned() else {
        state::with_state(|state| state.status = "歌单歌曲已失效，请重新打开歌单。".to_string());
        return;
    };
    start_cloud_song(song, snapshot);
}

fn start_cloud_song(song: state::CloudSong, snapshot: state::UiState) {
    state::with_state(|state| state.status = format!("正在准备《{}》…", song.name));
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
    let result = wit_bindgen::block_on(import::start(import::ImportRequest {
        addr: snapshot.selected_addr,
        track_id: prepared.song.id,
        name: prepared.song.name,
        artists: prepared.song.artists,
        album: prepared.song.album,
        album_id: prepared.song.album_id,
        duration_ms: prepared.song.duration_ms,
        assets: prepared.assets,
    }));
    if let Err(error) = result {
        state::with_state(|state| state.status = format!("无法开始导入：{error}"));
    }
}

fn begin_qr() {
    state::with_state(|state| state.status = "正在生成网易云登录二维码…".to_string());
    match netease::begin_qr_login() {
        Ok((key, url)) => state::with_state(|state| {
            state.qr_key = key;
            state.qr_url = url;
            state.status = "请用网易云音乐扫码并确认，然后点击检查状态。".to_string();
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
        Ok(None) => state::with_state(|state| state.status = "等待扫码或手机确认…".to_string()),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn logout_netease() {
    let result = state::clear_netease_session();
    state::with_state(|state| {
        state.netease_cookie.clear();
        state.netease_view = state::NeteaseView::Home;
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

fn build_root() -> ui::Element {
    let state = state::snapshot();
    let mut root = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .padding(28)
        .gap(18)
        .child(text("Lyra Import", 28, "#f4f4f5"))
        .child(text(
            "将本地 MP3 或网易云音乐导入 com.canopus.lyraimport 快应用。",
            14,
            "#a1a1aa",
        ));

    let mut device_select = ui::Element::new(ui::ElementType::Select, None)
        .width_full()
        .prop("default-value", &state.selected_addr)
        .prop("key", EVENT_DEVICE)
        .on(ui::Event::Change, EVENT_DEVICE);
    for device in &state.devices {
        device_select = device_select.child(
            ui::Element::new(ui::ElementType::Option, Some(&device.name))
                .prop("value", &device.addr),
        );
    }
    let checksum = checksum_select(state.transfer_mode);
    let device = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(10)
        .child(text("目标设备", 18, "#f4f4f5"))
        .child(text(
            "音乐将传输到所选设备上的 Lyra Import。",
            13,
            "#a1a1aa",
        ))
        .child(device_select)
        .child(checksum)
        .child(button("刷新设备", EVENT_REFRESH, "#27272a"));
    root = root.child(device);

    if !state.status.is_empty() || state.total > 0 || state.active {
        root = root.child(transfer_card(&state));
    }

    root = root.child(device_library_card(&state));

    let local = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(12)
        .child(text("本地音乐", 22, "#f4f4f5"))
        .child(text("选择本地文件并补充曲目信息。", 13, "#a1a1aa"))
        .child(text(
            &file_summary("音频", state.audio.as_ref()),
            14,
            "#d4d4d8",
        ))
        .child(button("选择 MP3", EVENT_PICK_AUDIO, "#2563eb"))
        .child(text(
            &file_summary("封面", state.cover.as_ref()),
            14,
            "#d4d4d8",
        ))
        .child(button("选择封面（可选）", EVENT_PICK_COVER, "#4f46e5"))
        .child(text(
            &file_summary("歌词", state.lyrics.as_ref()),
            14,
            "#d4d4d8",
        ))
        .child(button("选择歌词（可选）", EVENT_PICK_LYRICS, "#4f46e5"))
        .child(input("曲名", &state.track_name, EVENT_NAME))
        .child(input("歌手（可空）", &state.artist, EVENT_ARTIST))
        .child(input("专辑（可空）", &state.album, EVENT_ALBUM))
        .child(input(
            "时长毫秒（未知填 0）",
            &state.duration_ms.to_string(),
            EVENT_DURATION,
        ))
        .child(button("导入本地音乐", EVENT_START_LOCAL, "#16a34a"));
    root = root.child(local);

    let mut cloud = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(12)
        .child(text("网易云音乐", 22, "#f4f4f5"));
    if state.netease_cookie.is_empty() {
        cloud = cloud
            .child(text("扫码登录后可搜索歌曲或浏览个人歌单。", 13, "#a1a1aa"))
            .child(button("生成登录二维码", EVENT_QR_BEGIN, "#dc2626"));
        if !state.qr_url.is_empty() {
            let qr = qr_image_url(&state.qr_url);
            cloud = cloud
                .child(
                    ui::Element::new(ui::ElementType::Image, Some(&qr))
                        .width(220)
                        .height(220),
                )
                .child(button("检查扫码状态", EVENT_QR_POLL, "#b91c1c"));
        }
    } else {
        let audio_bitrate = netease::normalized_audio_bitrate(state.netease_audio_bitrate);
        cloud = cloud
            .child(text("已登录 · 登录状态已保存在插件私有目录", 13, "#86efac"))
            .child(text(
                "音质越高，下载和导入越慢，手表播放也越可能卡顿。推荐选择 128 kbps。",
                13,
                "#fbbf24",
            ))
            .child(audio_quality_select(audio_bitrate))
            .child(input("搜索歌曲", &state.netease_query, EVENT_QUERY))
            .child(button("搜索网易云", EVENT_SEARCH_CLOUD, "#2563eb"))
            .child(button("我的歌单", EVENT_LOAD_PLAYLISTS, "#4f46e5"))
            .child(button("退出登录", EVENT_LOGOUT, "#7f1d1d"));
    }
    root = root.child(cloud);

    if state.netease_view == state::NeteaseView::SearchResults && !state.netease_results.is_empty()
    {
        root = root.child(song_list(
            "搜索结果",
            "选择歌曲后将立即下载音频、封面与歌词。",
            &state.netease_results,
            EVENT_SEARCH_RESULT_PREFIX,
            0,
            PLAYLIST_PAGE_SIZE,
        ));
    } else if state.netease_view == state::NeteaseView::PlaylistTracks {
        let page_count = state
            .netease_playlist_tracks
            .len()
            .div_ceil(PLAYLIST_PAGE_SIZE)
            .max(1);
        let page = state.netease_playlist_page.min(page_count - 1);
        let page_start = page * PLAYLIST_PAGE_SIZE;
        let tracks = song_list(
            &state.netease_playlist_name,
            "个人歌单中的歌曲",
            &state.netease_playlist_tracks,
            EVENT_PLAYLIST_TRACK_PREFIX,
            page_start,
            PLAYLIST_PAGE_SIZE,
        );
        root = root
            .child(button("返回我的歌单", EVENT_CLOSE_PLAYLIST, "#27272a"))
            .child(text(
                &format!("第 {} / {} 页", page + 1, page_count),
                13,
                "#a1a1aa",
            ));
        if page > 0 {
            root = root.child(button("上一页", EVENT_PLAYLIST_PAGE_PREVIOUS, "#27272a"));
        }
        if page + 1 < page_count {
            root = root.child(button("下一页", EVENT_PLAYLIST_PAGE_NEXT, "#4f46e5"));
        }
        root = root.child(tracks);
    } else if state.netease_view == state::NeteaseView::Playlists
        && !state.netease_playlists.is_empty()
    {
        root = root.child(playlist_list(&state.netease_playlists));
    }

    root
}

fn device_library_card(state: &state::UiState) -> ui::Element {
    let mut card = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(10)
        .child(text("手表音乐", 22, "#f4f4f5"))
        .child(text(
            "列表顺序也是 Lyra 上一首、下一首的播放顺序。",
            13,
            "#a1a1aa",
        ))
        .child(if state.library_busy {
            button("刷新手表音乐", EVENT_LIBRARY_REFRESH, "#0f766e").disabled()
        } else {
            button("刷新手表音乐", EVENT_LIBRARY_REFRESH, "#0f766e")
        });

    if state.device_library.is_empty() {
        let message = if state.library_total == 0 && !state.library_revision.is_empty() {
            "手表音乐库为空。"
        } else {
            "刷新后可查看并调整当前音乐顺序。"
        };
        return card.child(text(message, 13, "#a1a1aa"));
    }

    for (index, track) in state.device_library.iter().enumerate() {
        let artists = if track.artists.is_empty() {
            "未知歌手".to_string()
        } else {
            track.artists.join(" / ")
        };
        let album = if track.album.is_empty() {
            "未知专辑"
        } else {
            &track.album
        };
        let metadata = format!(
            "{} · {} · {}",
            artists,
            album,
            duration_text(track.duration_ms)
        );
        let mut item = ui::Element::new(ui::ElementType::Card, None)
            .width_full()
            .padding(14)
            .radius(10)
            .bg("#18181b")
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .gap(6)
            .child(text(
                &format!("{}. {}", index + 1, track.name),
                17,
                "#fafafa",
            ))
            .child(text(&metadata, 13, "#a1a1aa"));
        if index > 0 {
            item = item.child(library_button(
                "上移",
                &format!("{EVENT_LIBRARY_MOVE_UP_PREFIX}{}", track.id),
                "#3f3f46",
                state.library_busy,
            ));
        }
        if index + 1 < state.device_library.len() {
            item = item.child(library_button(
                "下移",
                &format!("{EVENT_LIBRARY_MOVE_DOWN_PREFIX}{}", track.id),
                "#4f46e5",
                state.library_busy,
            ));
        }
        item = item.child(library_button(
            "删除",
            &format!("{EVENT_LIBRARY_DELETE_PREFIX}{}", track.id),
            "#b91c1c",
            state.library_busy,
        ));
        card = card.child(item);
    }
    card
}

fn transfer_card(state: &state::UiState) -> ui::Element {
    let title = if state.active {
        "正在传输"
    } else {
        "任务状态"
    };
    let status = if state.status.is_empty() {
        "准备就绪"
    } else {
        &state.status
    };
    let mut card = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .bg("#0f172a")
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(10)
        .child(text(title, 18, "#f8fafc"))
        .child(text(status, 14, "#93c5fd"));
    if state.total > 0 {
        let percent = state
            .sent
            .saturating_mul(100)
            .checked_div(state.total)
            .unwrap_or(0)
            .min(100);
        let detail = if state.speed_bytes_per_second > 0 {
            let speed_kib = (state.speed_bytes_per_second + 512) / 1024;
            format!(
                "{}% · {:.2} / {:.2} MiB · {}k/s",
                percent,
                state.sent as f64 / 1_048_576.0,
                state.total as f64 / 1_048_576.0,
                speed_kib.max(1)
            )
        } else {
            format!(
                "{}% · {:.2} / {:.2} MiB",
                percent,
                state.sent as f64 / 1_048_576.0,
                state.total as f64 / 1_048_576.0
            )
        };
        card = card
            .child(
                ui::Element::new(ui::ElementType::Progress, None)
                    .width_full()
                    .prop("value", &percent.to_string())
                    .prop("max", "100"),
            )
            .child(text(&detail, 13, "#cbd5e1"));
    }
    if state.active {
        card = card.child(button("取消传输", EVENT_CANCEL, "#7f1d1d"));
    }
    card
}

fn song_list(
    title: &str,
    description: &str,
    songs: &[state::CloudSong],
    event_prefix: &str,
    start: usize,
    limit: usize,
) -> ui::Element {
    let mut section = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(10)
        .child(text(title, 22, "#f4f4f5"))
        .child(text(description, 13, "#a1a1aa"));
    for (index, song) in songs.iter().enumerate().skip(start).take(limit) {
        let artists = if song.artists.is_empty() {
            "未知歌手".to_string()
        } else {
            song.artists.join(" / ")
        };
        let metadata = format!(
            "{} · {} · {}",
            artists,
            if song.album.is_empty() {
                "未知专辑"
            } else {
                &song.album
            },
            duration_text(song.duration_ms)
        );
        let event = format!("{event_prefix}{index}");
        let mut item = ui::Element::new(ui::ElementType::Card, None)
            .width_full()
            .padding(14)
            .radius(10)
            .bg("#18181b")
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .gap(6);
        item = item
            .child(text(&song.name, 17, "#fafafa"))
            .child(text(&metadata, 13, "#a1a1aa"))
            .child(button("导入这首歌", &event, "#16a34a"));
        section = section.child(item);
    }
    section
}

fn playlist_list(playlists: &[state::CloudPlaylist]) -> ui::Element {
    let mut section = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .radius(12)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(10)
        .child(text("我的歌单", 22, "#f4f4f5"))
        .child(text("打开歌单后可直接选择其中的歌曲导入。", 13, "#a1a1aa"));
    for playlist in playlists.iter().take(40) {
        let creator = if playlist.creator.is_empty() {
            "网易云音乐"
        } else {
            &playlist.creator
        };
        let metadata = format!("{} 首歌曲 · {}", playlist.track_count, creator);
        let event = format!("{EVENT_PLAYLIST_PREFIX}{}", playlist.id);
        let mut item = ui::Element::new(ui::ElementType::Card, None)
            .width_full()
            .padding(14)
            .radius(10)
            .bg("#18181b")
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .gap(6);
        item = item
            .child(text(&playlist.name, 17, "#fafafa"))
            .child(text(&metadata, 13, "#a1a1aa"))
            .child(button("查看歌单", &event, "#4f46e5"));
        section = section.child(item);
    }
    if playlists.len() > 40 {
        section = section.child(text("当前显示前 40 个歌单。", 13, "#f59e0b"));
    }
    section
}

fn duration_text(duration_ms: u32) -> String {
    if duration_ms == 0 {
        return "--:--".to_string();
    }
    format!(
        "{:02}:{:02}",
        duration_ms / 60_000,
        duration_ms / 1_000 % 60
    )
}

fn local_track_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn file_summary(label: &str, file: Option<&state::SelectedFile>) -> String {
    file.map_or_else(
        || format!("{label}：未选择"),
        |file| {
            format!(
                "{label}：{} · {:.2} MiB",
                file.name,
                file.size as f64 / 1_048_576.0
            )
        },
    )
}

fn asset_label(kind: &str) -> &'static str {
    match kind {
        "audio" => "音频",
        "cover" => "封面",
        "lyrics" => "歌词",
        _ => "文件",
    }
}

fn qr_image_url(value: &str) -> String {
    let Ok(code) = QrCode::new(value.as_bytes()) else {
        return String::new();
    };
    let image = code
        .render::<svg::Color>()
        .min_dimensions(260, 260)
        .quiet_zone(true)
        .build();
    format!("data:image/svg+xml;base64,{}", BASE64.encode(image))
}

fn checksum_select(mode: state::TransferMode) -> ui::Element {
    let value = match mode {
        state::TransferMode::Crc32 => "crc32",
        state::TransferMode::Fast => "none",
        state::TransferMode::Ultra => "none-48k",
    };
    ui::Element::new(ui::ElementType::Select, None)
        .width_full()
        .prop("default-value", value)
        .prop("key", EVENT_CHECKSUM_MODE)
        .on(ui::Event::Change, EVENT_CHECKSUM_MODE)
        .child(
            ui::Element::new(ui::ElementType::Option, Some("CRC32 校验（推荐，可靠）"))
                .prop("value", "crc32"),
        )
        .child(
            ui::Element::new(ui::ElementType::Option, Some("无校验高速（可能损坏文件）"))
                .prop("value", "none"),
        )
        .child(
            ui::Element::new(
                ui::ElementType::Option,
                Some("无校验超高速 · 48 KiB 单包上限（高风险）"),
            )
            .prop("value", "none-48k"),
        )
}

fn audio_quality_select(bitrate: u32) -> ui::Element {
    ui::Element::new(ui::ElementType::Select, None)
        .width_full()
        .prop("default-value", &bitrate.to_string())
        .prop("key", EVENT_AUDIO_QUALITY)
        .on(ui::Event::Change, EVENT_AUDIO_QUALITY)
        .child(
            ui::Element::new(ui::ElementType::Option, Some("低音质 · 128 kbps（推荐）"))
                .prop("value", &netease::AUDIO_BITRATE_LOW.to_string()),
        )
        .child(
            ui::Element::new(ui::ElementType::Option, Some("中音质 · 192 kbps"))
                .prop("value", &netease::AUDIO_BITRATE_MEDIUM.to_string()),
        )
        .child(
            ui::Element::new(ui::ElementType::Option, Some("高音质 · 320 kbps"))
                .prop("value", &netease::AUDIO_BITRATE_HIGH.to_string()),
        )
}

fn input(placeholder: &str, value: &str, event_id: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Input, None)
        .width_full()
        .prop("placeholder", placeholder)
        .prop("default-value", value)
        .prop("key", event_id)
        .on(ui::Event::Input, event_id)
}

fn button(label: &str, event_id: &str, background: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(label))
        .width_full()
        .padding(12)
        .radius(8)
        .bg(background)
        .text_color("#ffffff")
        .on(ui::Event::Click, event_id)
}

fn library_button(label: &str, event_id: &str, background: &str, disabled: bool) -> ui::Element {
    let button = button(label, event_id, background);
    if disabled {
        button.disabled()
    } else {
        button
    }
}

fn text(content: &str, size: u32, color: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::P, Some(content))
        .size(size)
        .text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_login_image_is_embedded_data_uri() {
        let uri = qr_image_url("https://music.163.com/login?codekey=test");
        assert!(uri.starts_with("data:image/svg+xml;base64,"));
        let encoded = uri.split_once(',').unwrap().1;
        let svg = BASE64.decode(encoded).unwrap();
        assert!(svg.starts_with(b"<?xml"));
    }
}
