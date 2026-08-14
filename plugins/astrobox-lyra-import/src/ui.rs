use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::astrobox::psys_host::{
    dialog::{self, FilterConfig, PickConfig},
    ui_v3 as ui,
};
use crate::{import, interconnect, netease, state};

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
const EVENT_COOKIE: &str = "input:netease.cookie";
const EVENT_QUERY: &str = "input:netease.query";
const EVENT_SEARCH_CLOUD: &str = "action:netease.search";
const EVENT_CLOUD_SELECT: &str = "input:netease.song";
const EVENT_START_CLOUD: &str = "action:netease.import";
const EVENT_QR_BEGIN: &str = "action:netease.qr.begin";
const EVENT_QR_POLL: &str = "action:netease.qr.poll";

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
    let payload = serde_json::from_str::<UiPayload>(payload).unwrap_or_default();
    match event_id {
        EVENT_PICK_AUDIO => pick_asset("audio", &["mp3"]),
        EVENT_PICK_COVER => pick_asset("cover", &["jpg", "jpeg", "png"]),
        EVENT_PICK_LYRICS => pick_asset("lyrics", &["lrc", "json", "txt"]),
        EVENT_REFRESH => {
            interconnect::refresh_devices();
            state::with_state(|state| state.status = "已刷新连接设备。".to_string());
        }
        EVENT_DEVICE => state::with_state(|state| state.selected_addr = payload.value.unwrap_or_default()),
        EVENT_NAME => state::with_state(|state| state.track_name = payload.value.unwrap_or_default()),
        EVENT_ARTIST => state::with_state(|state| state.artist = payload.value.unwrap_or_default()),
        EVENT_ALBUM => state::with_state(|state| state.album = payload.value.unwrap_or_default()),
        EVENT_DURATION => state::with_state(|state| {
            state.duration_ms = payload.value.as_deref().and_then(|value| value.parse().ok()).unwrap_or(0)
        }),
        EVENT_COOKIE => state::with_state(|state| state.netease_cookie = payload.value.unwrap_or_default()),
        EVENT_QUERY => state::with_state(|state| state.netease_query = payload.value.unwrap_or_default()),
        EVENT_CLOUD_SELECT => state::with_state(|state| {
            state.netease_selected = payload.value.as_deref().and_then(|value| value.parse().ok()).unwrap_or(0)
        }),
        EVENT_START_LOCAL => start_local(),
        EVENT_SEARCH_CLOUD => search_cloud(),
        EVENT_START_CLOUD => start_cloud(),
        EVENT_QR_BEGIN => begin_qr(),
        EVENT_QR_POLL => poll_qr(),
        EVENT_CANCEL => wit_bindgen::block_on(import::cancel()),
        _ => {}
    }
    rerender();
}

fn pick_asset(kind: &str, extensions: &[&str]) {
    let result = wit_bindgen::block_on(
        dialog::pick_file(
            &PickConfig { read: false, copy_to: Some("media".to_string()) },
            &FilterConfig {
                multiple: false,
                extensions: extensions.iter().map(|value| (*value).to_string()).collect(),
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
                state.track_name = selected.name.rsplit_once('.').map(|item| item.0).unwrap_or(&selected.name).to_string();
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
        let extension = cover.name.rsplit_once('.').map(|item| item.1).unwrap_or("jpg");
        assets.push(import::ImportAsset::cover(cover.path, cover.size, if extension.eq_ignore_ascii_case("png") { "png" } else { "jpg" }));
    }
    if let Some(lyrics) = snapshot.lyrics {
        let extension = lyrics.name.rsplit_once('.').map(|item| item.1).unwrap_or("lrc");
        assets.push(import::ImportAsset::lyrics(lyrics.path, lyrics.size, if extension.eq_ignore_ascii_case("json") { "json" } else { "lrc" }));
    }
    let artists = if snapshot.artist.trim().is_empty() { Vec::new() } else { vec![snapshot.artist.trim().to_string()] };
    let result = wit_bindgen::block_on(import::start(
        snapshot.selected_addr,
        local_track_id(),
        snapshot.track_name.trim().to_string(),
        artists,
        snapshot.album.trim().to_string(),
        0,
        snapshot.duration_ms,
        assets,
    ));
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
            state.netease_selected = 0;
            state.status = format!("找到 {} 首歌曲。", state.netease_results.len());
        }),
        Err(error) => state::with_state(|state| state.status = error),
    }
}

fn start_cloud() {
    let snapshot = state::snapshot();
    let Some(song) = snapshot.netease_results.get(snapshot.netease_selected).cloned() else {
        state::with_state(|state| state.status = "请先搜索并选择歌曲。".to_string());
        return;
    };
    state::with_state(|state| state.status = "正在下载音频、封面与歌词…".to_string());
    let prepared = match netease::prepare(&song, &snapshot.netease_cookie) {
        Ok(prepared) => prepared,
        Err(error) => {
            state::with_state(|state| state.status = error);
            return;
        }
    };
    let result = wit_bindgen::block_on(import::start(
        snapshot.selected_addr,
        prepared.song.id,
        prepared.song.name,
        prepared.song.artists,
        prepared.song.album,
        prepared.song.album_id,
        prepared.song.duration_ms,
        prepared.assets,
    ));
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
        Ok(Some(cookie)) => state::with_state(|state| {
            state.netease_cookie = cookie;
            state.qr_key.clear();
            state.qr_url.clear();
            state.status = "网易云登录成功。".to_string();
        }),
        Ok(None) => state::with_state(|state| state.status = "等待扫码或手机确认…".to_string()),
        Err(error) => state::with_state(|state| state.status = error),
    }
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
        .child(text("将本地 MP3 或网易云音乐导入 com.canopus.lyraimport 快应用。", 14, "#a1a1aa"));

    let mut device_select = ui::Element::new(ui::ElementType::Select, None)
        .width_full()
        .prop("value", &state.selected_addr)
        .on(ui::Event::Change, EVENT_DEVICE);
    for device in &state.devices {
        device_select = device_select.child(ui::Element::new(ui::ElementType::Option, Some(&device.name)).prop("value", &device.addr));
    }
    root = root
        .child(text("目标设备", 15, "#a1a1aa"))
        .child(device_select)
        .child(button("刷新设备", EVENT_REFRESH, "#27272a"));

    let local = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(12)
        .child(text("本地音乐", 22, "#f4f4f5"))
        .child(text(&file_summary("音频", state.audio.as_ref()), 14, "#d4d4d8"))
        .child(button("选择 MP3", EVENT_PICK_AUDIO, "#2563eb"))
        .child(text(&file_summary("封面", state.cover.as_ref()), 14, "#d4d4d8"))
        .child(button("选择封面（可选）", EVENT_PICK_COVER, "#4f46e5"))
        .child(text(&file_summary("歌词", state.lyrics.as_ref()), 14, "#d4d4d8"))
        .child(button("选择歌词（可选）", EVENT_PICK_LYRICS, "#4f46e5"))
        .child(input("曲名", &state.track_name, EVENT_NAME))
        .child(input("歌手（可空）", &state.artist, EVENT_ARTIST))
        .child(input("专辑（可空）", &state.album, EVENT_ALBUM))
        .child(input("时长毫秒（未知填 0）", &state.duration_ms.to_string(), EVENT_DURATION))
        .child(button("导入本地音乐", EVENT_START_LOCAL, "#16a34a"));
    root = root.child(local);

    let mut cloud = ui::Element::new(ui::ElementType::Card, None)
        .width_full()
        .padding(18)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(12)
        .child(text("网易云音乐", 22, "#f4f4f5"))
        .child(input("Cookie（可扫码自动填写）", &state.netease_cookie, EVENT_COOKIE))
        .child(button("生成登录二维码", EVENT_QR_BEGIN, "#dc2626"));
    if !state.qr_url.is_empty() {
        cloud = cloud
            .child(ui::Element::new(ui::ElementType::Image, None).width(220).height(220).prop("src", &qr_image_url(&state.qr_url)))
            .child(button("检查扫码状态", EVENT_QR_POLL, "#b91c1c"));
    }
    cloud = cloud
        .child(input("搜索歌曲", &state.netease_query, EVENT_QUERY))
        .child(button("搜索网易云", EVENT_SEARCH_CLOUD, "#2563eb"));
    if !state.netease_results.is_empty() {
        let mut select = ui::Element::new(ui::ElementType::Select, None)
            .width_full()
            .prop("value", &state.netease_selected.to_string())
            .on(ui::Event::Change, EVENT_CLOUD_SELECT);
        for (index, song) in state.netease_results.iter().enumerate() {
            let label = format!("{} — {}", song.name, song.artists.join(" / "));
            select = select.child(ui::Element::new(ui::ElementType::Option, Some(&label)).prop("value", &index.to_string()));
        }
        cloud = cloud.child(select).child(button("下载并导入所选歌曲", EVENT_START_CLOUD, "#16a34a"));
    }
    root = root.child(cloud);

    if state.total > 0 {
        let percent = state.sent.saturating_mul(100) / state.total;
        root = root.child(
            ui::Element::new(ui::ElementType::Progress, None)
                .width_full()
                .prop("value", &percent.to_string())
                .prop("max", "100"),
        );
    }
    root = root.child(text(&state.status, 14, "#60a5fa"));
    if state.active {
        root.child(button("取消导入", EVENT_CANCEL, "#7f1d1d"))
    } else {
        root
    }
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
        |file| format!("{label}：{} · {:.2} MiB", file.name, file.size as f64 / 1_048_576.0),
    )
}

fn asset_label(kind: &str) -> &'static str {
    match kind { "audio" => "音频", "cover" => "封面", "lyrics" => "歌词", _ => "文件" }
}

fn qr_image_url(value: &str) -> String {
    format!("https://api.qrserver.com/v1/create-qr-code/?size=260x260&data={}", percent_encode(value))
}

fn percent_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn input(placeholder: &str, value: &str, event_id: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Input, None)
        .width_full()
        .prop("placeholder", placeholder)
        .prop("value", value)
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

fn text(content: &str, size: u32, color: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::P, Some(content)).size(size).text_color(color)
}
