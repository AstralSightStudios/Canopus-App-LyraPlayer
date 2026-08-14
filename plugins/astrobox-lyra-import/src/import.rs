use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use crc32fast::Hasher as Crc32;
use serde_json::{Value, json};

use crate::{interconnect, state};

const PROTOCOL_VERSION: u64 = 2;
const DEFAULT_CHUNK_BYTES: usize = 4096;
const MAX_CHUNK_BYTES: usize = 4096;

#[derive(Clone, Debug)]
pub struct ImportAsset {
    pub kind: &'static str,
    pub path: String,
    pub size: u64,
    pub extension: Option<&'static str>,
    pub format: Option<&'static str>,
}

impl ImportAsset {
    pub fn audio(path: String, size: u64) -> Self {
        Self { kind: "audio", path, size, extension: None, format: None }
    }

    pub fn cover(path: String, size: u64, extension: &'static str) -> Self {
        Self { kind: "cover", path, size, extension: Some(extension), format: None }
    }

    pub fn lyrics(path: String, size: u64, format: &'static str) -> Self {
        Self { kind: "lyrics", path, size, extension: None, format: Some(format) }
    }
}

struct TransferAsset {
    metadata: ImportAsset,
    file: File,
}

struct Transfer {
    addr: String,
    id: String,
    track_id: u64,
    name: String,
    artists: Vec<String>,
    album: String,
    album_id: u64,
    duration_ms: u32,
    assets: Vec<TransferAsset>,
    asset_index: usize,
    chunk_bytes: usize,
    seq: u64,
    offset: u64,
    sent_total: u64,
    awaiting: Awaiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Awaiting {
    Hello,
    Ready,
    Ack,
    AssetsDone,
    Done,
}

static TRANSFER: OnceLock<Mutex<Option<Transfer>>> = OnceLock::new();

fn transfer() -> &'static Mutex<Option<Transfer>> {
    TRANSFER.get_or_init(|| Mutex::new(None))
}

pub fn inspect_file(path: &str, name: &str, kind: &str) -> Result<state::SelectedFile, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("cannot stat file: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("selected file is empty".to_string());
    }
    let allowed = match kind {
        "audio" => ["mp3"].as_slice(),
        "cover" => ["jpg", "jpeg", "png"].as_slice(),
        "lyrics" => ["lrc", "json", "txt"].as_slice(),
        _ => return Err("unknown asset kind".to_string()),
    };
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !allowed.contains(&extension.as_str()) {
        return Err(format!("unsupported {kind} file extension"));
    }
    let limit = match kind {
        "audio" => 64 * 1024 * 1024,
        "cover" => 4 * 1024 * 1024,
        _ => 2 * 1024 * 1024,
    };
    if metadata.len() > limit {
        return Err(format!("selected {kind} file exceeds import limit"));
    }
    Ok(state::SelectedFile {
        name: name.to_string(),
        path: path.to_string(),
        size: metadata.len(),
    })
}

pub async fn start(
    addr: String,
    track_id: u64,
    name: String,
    artists: Vec<String>,
    album: String,
    album_id: u64,
    duration_ms: u32,
    assets: Vec<ImportAsset>,
) -> Result<(), String> {
    if addr.is_empty() {
        return Err("请选择已连接设备".to_string());
    }
    validate_assets(&assets)?;
    let mut transfer_assets = Vec::with_capacity(assets.len());
    for asset in assets {
        let file = File::open(&asset.path)
            .map_err(|error| format!("cannot open {}: {error}", asset.kind))?;
        transfer_assets.push(TransferAsset { metadata: asset, file });
    }
    let total = transfer_assets.iter().map(|asset| asset.metadata.size).sum();
    let id = new_id();
    {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        if guard.is_some() {
            return Err("已有导入任务正在运行".to_string());
        }
        *guard = Some(Transfer {
            addr: addr.clone(),
            id,
            track_id,
            name,
            artists,
            album,
            album_id,
            duration_ms,
            assets: transfer_assets,
            asset_index: 0,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            seq: 0,
            offset: 0,
            sent_total: 0,
            awaiting: Awaiting::Hello,
        });
    }
    state::with_state(|state| {
        state.active = true;
        state.sent = 0;
        state.total = total;
        state.status = "正在连接 Lyra Import 快应用…".to_string();
    });
    if let Err(error) = interconnect::send(
        &addr,
        &json!({ "tag": "lyra-import-hello", "version": PROTOCOL_VERSION }),
    )
    .await
    {
        finish_error(&error);
        return Err(error);
    }
    Ok(())
}

pub async fn handle(addr: &str, package: &str, payload: &str) {
    if package != interconnect::ROUTE_PACKAGE {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return;
    };
    let Some(tag) = value.get("tag").and_then(Value::as_str) else {
        return;
    };
    if !tag.starts_with("lyra-import-") || !matches_active_peer(addr, tag, &value) {
        return;
    }
    let result = match tag {
        "lyra-import-hello" => handle_hello(addr, &value).await,
        "lyra-import-ready" => handle_ready(addr, &value).await,
        "lyra-import-ack" => handle_ack(addr, &value).await,
        "lyra-import-assets-done" => handle_assets_done(addr, &value).await,
        "lyra-import-done" => handle_done(addr, &value),
        "lyra-import-cancelled" => Err("快应用已取消导入".to_string()),
        "lyra-import-error" => {
            let code = value.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = value.get("message").and_then(Value::as_str).unwrap_or("quick app rejected import");
            Err(format!("{code}: {message}"))
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        finish_error(&error);
    }
}

async fn handle_hello(addr: &str, value: &Value) -> Result<(), String> {
    if value.get("version").and_then(Value::as_u64) != Some(PROTOCOL_VERSION)
        || value.get("window").and_then(Value::as_u64) != Some(1)
    {
        return Err("快应用不支持 Lyra Import v2 单窗口协议".to_string());
    }
    let supports_base64 = value
        .get("encodings")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("base64")));
    if !supports_base64 {
        return Err("快应用不支持 base64 分片".to_string());
    }
    let max = value
        .get("maxChunkBytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_CHUNK_BYTES as u64) as usize;
    let begin = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(item, addr, Awaiting::Hello, None)?;
        item.chunk_bytes = max.clamp(1, MAX_CHUNK_BYTES);
        item.awaiting = Awaiting::Ready;
        let assets = item
            .assets
            .iter()
            .map(|asset| {
                let mut value = json!({ "kind": asset.metadata.kind, "size": asset.metadata.size });
                if let Some(extension) = asset.metadata.extension {
                    value["extension"] = Value::String(extension.to_string());
                }
                if let Some(format) = asset.metadata.format {
                    value["format"] = Value::String(format.to_string());
                }
                value
            })
            .collect::<Vec<_>>();
        json!({
            "tag": "lyra-import-begin",
            "version": PROTOCOL_VERSION,
            "id": item.id,
            "track": {
                "id": item.track_id,
                "name": item.name,
                "artists": item.artists,
                "album": item.album,
                "albumId": item.album_id,
                "durationMs": item.duration_ms,
            },
            "assets": assets,
        })
    };
    state::with_state(|state| state.status = "快应用已连接，正在准备存储…".to_string());
    interconnect::send(addr, &begin).await
}

async fn handle_ready(addr: &str, value: &Value) -> Result<(), String> {
    {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(item, addr, Awaiting::Ready, value.get("id").and_then(Value::as_str))?;
        let asset = item.assets.get(item.asset_index).ok_or_else(|| "资源索引越界".to_string())?;
        if value.get("asset").and_then(Value::as_str) != Some(asset.metadata.kind)
            || value.get("nextSeq").and_then(Value::as_u64) != Some(0)
            || value.get("nextOffset").and_then(Value::as_u64) != Some(0)
            || value.get("window").and_then(Value::as_u64) != Some(1)
        {
            return Err("快应用返回了无效资源起点".to_string());
        }
        let max = value
            .get("maxChunkBytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| "快应用未返回分片上限".to_string())? as usize;
        item.chunk_bytes = item.chunk_bytes.min(max.clamp(1, MAX_CHUNK_BYTES));
    }
    update_asset_status();
    send_next_chunk(addr).await
}

async fn handle_ack(addr: &str, value: &Value) -> Result<(), String> {
    let (completed, confirmed) = {
        let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_ref().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(item, addr, Awaiting::Ack, value.get("id").and_then(Value::as_str))?;
        let asset = &item.assets[item.asset_index].metadata;
        if value.get("asset").and_then(Value::as_str) != Some(asset.kind)
            || value.get("nextSeq").and_then(Value::as_u64) != Some(item.seq)
            || value.get("nextOffset").and_then(Value::as_u64) != Some(item.offset)
            || value.get("receivedBytes").and_then(Value::as_u64) != Some(item.sent_total)
        {
            return Err("累计 ACK 与已发送位置不一致".to_string());
        }
        (item.offset == asset.size, item.sent_total)
    };
    state::with_state(|state| state.sent = confirmed);
    if completed {
        send_asset_end(addr).await
    } else {
        send_next_chunk(addr).await
    }
}

async fn send_next_chunk(addr: &str) -> Result<(), String> {
    let packet = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        let asset = item.assets.get_mut(item.asset_index).ok_or_else(|| "资源索引越界".to_string())?;
        if item.addr != addr || item.offset >= asset.metadata.size {
            return Err("导入会话设备或资源偏移无效".to_string());
        }
        asset
            .file
            .seek(SeekFrom::Start(item.offset))
            .map_err(|error| format!("cannot seek {}: {error}", asset.metadata.kind))?;
        let remaining = (asset.metadata.size - item.offset) as usize;
        let mut bytes = vec![0u8; item.chunk_bytes.min(remaining)];
        asset
            .file
            .read_exact(&mut bytes)
            .map_err(|error| format!("cannot read {} chunk: {error}", asset.metadata.kind))?;
        let mut crc = Crc32::new();
        crc.update(&bytes);
        let count = bytes.len() as u64;
        let packet = json!({
            "tag": "lyra-import-chunk",
            "id": item.id,
            "asset": asset.metadata.kind,
            "seq": item.seq,
            "offset": item.offset,
            "encoding": "base64",
            "data": BASE64.encode(&bytes),
            "crc32": format!("{:08x}", crc.finalize()),
        });
        item.seq += 1;
        item.offset += count;
        item.sent_total += count;
        item.awaiting = Awaiting::Ack;
        packet
    };
    interconnect::send(addr, &packet).await
}

async fn send_asset_end(addr: &str) -> Result<(), String> {
    let packet = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        let kind = item.assets[item.asset_index].metadata.kind;
        let packet = json!({ "tag": "lyra-import-asset-end", "id": item.id, "asset": kind });
        item.asset_index += 1;
        item.seq = 0;
        item.offset = 0;
        item.awaiting = if item.asset_index == item.assets.len() { Awaiting::AssetsDone } else { Awaiting::Ready };
        packet
    };
    interconnect::send(addr, &packet).await
}

async fn handle_assets_done(addr: &str, value: &Value) -> Result<(), String> {
    let packet = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(item, addr, Awaiting::AssetsDone, value.get("id").and_then(Value::as_str))?;
        item.awaiting = Awaiting::Done;
        json!({ "tag": "lyra-import-commit", "id": item.id })
    };
    state::with_state(|state| state.status = "正在发布曲目与音乐库…".to_string());
    interconnect::send(addr, &packet).await
}

fn handle_done(addr: &str, value: &Value) -> Result<(), String> {
    let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
    let item = guard.as_ref().ok_or_else(|| "没有活动导入".to_string())?;
    validate_peer(item, addr, Awaiting::Done, value.get("id").and_then(Value::as_str))?;
    drop(guard);
    *transfer().lock().unwrap_or_else(|item| item.into_inner()) = None;
    state::with_state(|state| {
        state.active = false;
        state.sent = state.total;
        state.status = "导入完成；Lyra Player 将自动刷新音乐库。".to_string();
    });
    Ok(())
}

pub async fn cancel() {
    let pending = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        guard.take().map(|item| (item.addr, item.id))
    };
    if let Some((addr, id)) = pending {
        let _ = interconnect::send(&addr, &json!({ "tag": "lyra-import-cancel", "id": id })).await;
    }
    state::with_state(|state| {
        state.active = false;
        state.status = "已取消导入。".to_string();
    });
}

fn validate_assets(assets: &[ImportAsset]) -> Result<(), String> {
    if assets.is_empty() || assets.len() > 3 || assets[0].kind != "audio" {
        return Err("导入必须以一个音频资源开始".to_string());
    }
    let mut seen = Vec::new();
    for asset in assets {
        if asset.size == 0 || seen.contains(&asset.kind) {
            return Err("导入资源为空或重复".to_string());
        }
        seen.push(asset.kind);
    }
    Ok(())
}

fn update_asset_status() {
    let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
    if let Some(item) = guard.as_ref() {
        let kind = item.assets[item.asset_index].metadata.kind;
        state::with_state(|state| {
            state.status = format!("正在传输{}…", match kind { "audio" => "音频", "cover" => "封面", "lyrics" => "歌词", _ => "资源" });
        });
    }
}

fn matches_active_peer(addr: &str, tag: &str, value: &Value) -> bool {
    let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
    let Some(item) = guard.as_ref() else { return false };
    if item.addr != addr {
        return false;
    }
    if tag == "lyra-import-hello" {
        return item.awaiting == Awaiting::Hello;
    }
    value.get("id").and_then(Value::as_str) == Some(item.id.as_str())
}

fn validate_peer(item: &Transfer, addr: &str, awaiting: Awaiting, id: Option<&str>) -> Result<(), String> {
    if item.addr != addr || item.awaiting != awaiting {
        return Err("意外的导入响应状态".to_string());
    }
    if awaiting == Awaiting::Hello && id.is_none() {
        return Ok(());
    }
    if id != Some(item.id.as_str()) {
        return Err("导入响应 ID 不匹配".to_string());
    }
    Ok(())
}

fn finish_error(error: &str) {
    *transfer().lock().unwrap_or_else(|item| item.into_inner()) = None;
    state::with_state(|state| {
        state.active = false;
        state.status = format!("导入失败：{error}");
    });
}

fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{nanos:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_requires_audio_first_and_unique_assets() {
        let audio = ImportAsset::audio("audio.mp3".into(), 10);
        let cover = ImportAsset::cover("cover.jpg".into(), 5, "jpg");
        assert!(validate_assets(&[audio.clone(), cover]).is_ok());
        assert!(validate_assets(&[audio.clone(), audio]).is_err());
        assert!(validate_assets(&[ImportAsset::lyrics("lyrics.lrc".into(), 4, "lrc")]).is_err());
    }

    #[test]
    fn maximum_base64_chunk_fits_interconnect_frame() {
        let bytes = vec![0xff; MAX_CHUNK_BYTES];
        let frame = json!({
            "tag": "lyra-import-chunk",
            "id": "0123456789abcdef0123456789abcdef",
            "asset": "audio",
            "seq": 16_384,
            "offset": 67_108_864,
            "encoding": "base64",
            "data": BASE64.encode(bytes),
            "crc32": "ffffffff",
        })
        .to_string();
        assert!(frame.len() < 8192, "frame was {} bytes", frame.len());
    }
}
