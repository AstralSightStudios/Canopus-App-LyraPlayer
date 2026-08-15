use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use crc32fast::Hasher as Crc32;
use serde_json::{Value, json};

use crate::{interconnect, state};

const PROTOCOL_VERSION: u64 = 2;
// Keep the serialized Base64/JSON chunk below the 48 KiB outgoing frame cap.
const DEFAULT_CHUNK_BYTES: usize = 35 * 1024;
const FAST_CHUNK_BYTES: usize = 36_608;
const ULTRA_CHUNK_BYTES: usize = 36_753;
const MAX_CHUNK_RETRIES: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChunkMode {
    Crc32,
    NoneFast,
    NoneUltra,
}

impl ChunkMode {
    fn checksum(self) -> &'static str {
        match self {
            Self::Crc32 => "crc32",
            Self::NoneFast | Self::NoneUltra => "none",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Crc32 => "crc32",
            Self::NoneFast => "none",
            Self::NoneUltra => "none-48k",
        }
    }

    fn max_chunk_bytes(self) -> usize {
        match self {
            Self::Crc32 => DEFAULT_CHUNK_BYTES,
            Self::NoneFast => FAST_CHUNK_BYTES,
            Self::NoneUltra => ULTRA_CHUNK_BYTES,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImportAsset {
    pub kind: &'static str,
    pub path: String,
    pub size: u64,
    pub extension: Option<&'static str>,
    pub format: Option<&'static str>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl ImportAsset {
    pub fn audio(path: String, size: u64) -> Self {
        Self {
            kind: "audio",
            path,
            size,
            extension: None,
            format: None,
            width: None,
            height: None,
        }
    }

    pub fn cover_bin(path: String, size: u64) -> Self {
        Self {
            kind: "cover",
            path,
            size,
            extension: Some("bin"),
            format: Some(crate::artwork::LVGL_V9_FORMAT),
            width: Some(crate::artwork::COVER_WIDTH),
            height: Some(crate::artwork::COVER_HEIGHT),
        }
    }

    pub fn background_bin(path: String, size: u64) -> Self {
        Self {
            kind: "background",
            path,
            size,
            extension: Some("bin"),
            format: Some(crate::artwork::LVGL_V9_FORMAT),
            width: Some(crate::artwork::BACKGROUND_WIDTH),
            height: Some(crate::artwork::BACKGROUND_HEIGHT),
        }
    }

    pub fn lyrics(path: String, size: u64, format: &'static str) -> Self {
        Self {
            kind: "lyrics",
            path,
            size,
            extension: None,
            format: Some(format),
            width: None,
            height: None,
        }
    }
}

struct TransferAsset {
    metadata: ImportAsset,
    file: File,
}

struct PendingChunk {
    asset: &'static str,
    seq: u64,
    offset: u64,
    bytes: Vec<u8>,
    crc32: Option<String>,
    retries: u8,
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
    pending_chunk: Option<PendingChunk>,
    last_ack_at: Option<Instant>,
    last_ack_bytes: u64,
    speed_bytes_per_second: u64,
    artwork_supported: bool,
    prefer_no_checksum: bool,
    prefer_ultra: bool,
    mode: ChunkMode,
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
    let duration_ms = if kind == "audio" {
        mp3_duration::from_path(path)
            .ok()
            .map(|duration| duration.as_millis().min(u128::from(u32::MAX)) as u32)
            .unwrap_or(0)
    } else {
        0
    };
    Ok(state::SelectedFile {
        name: name.to_string(),
        path: path.to_string(),
        size: metadata.len(),
        duration_ms,
    })
}

#[derive(Clone, Debug)]
pub struct ImportRequest {
    pub addr: String,
    pub track_id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_id: u64,
    pub duration_ms: u32,
    pub assets: Vec<ImportAsset>,
}

pub async fn start(request: ImportRequest) -> Result<(), String> {
    let ImportRequest {
        addr,
        track_id,
        name,
        artists,
        album,
        album_id,
        duration_ms,
        assets,
    } = request;
    if addr.is_empty() {
        return Err("请选择已连接设备".to_string());
    }
    validate_assets(&assets)?;
    let mut transfer_assets = Vec::with_capacity(assets.len());
    for asset in assets {
        let file = File::open(&asset.path)
            .map_err(|error| format!("cannot open {}: {error}", asset.kind))?;
        transfer_assets.push(TransferAsset {
            metadata: asset,
            file,
        });
    }
    let total = transfer_assets
        .iter()
        .map(|asset| asset.metadata.size)
        .sum();
    let id = new_id();
    let started_at = Instant::now();
    let transfer_mode = state::snapshot().transfer_mode;
    let prefer_no_checksum = transfer_mode != state::TransferMode::Crc32;
    let prefer_ultra = transfer_mode == state::TransferMode::Ultra;
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
            pending_chunk: None,
            last_ack_at: Some(started_at),
            last_ack_bytes: 0,
            speed_bytes_per_second: 0,
            artwork_supported: false,
            prefer_no_checksum,
            prefer_ultra,
            mode: ChunkMode::Crc32,
            awaiting: Awaiting::Hello,
        });
    }
    state::with_state(|state| {
        state.active = true;
        state.sent = 0;
        state.total = total;
        state.speed_bytes_per_second = 0;
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
        "lyra-import-retry" => handle_retry(addr, &value).await,
        "lyra-import-assets-done" => handle_assets_done(addr, &value).await,
        "lyra-import-done" => handle_done(addr, &value),
        "lyra-import-cancelled" => Err("快应用已取消导入".to_string()),
        "lyra-import-error" => {
            let code = value.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("quick app rejected import");
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
    let supports_no_checksum = value
        .get("checksums")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("none")));
    let supports_ultra = value
        .get("chunkModes")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("none-48k")));
    let supports_artwork = value
        .get("assets")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| item.as_str() == Some("cover"))
                && items.iter().any(|item| item.as_str() == Some("background"))
        })
        && value
            .get("imageFormats")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some(crate::artwork::LVGL_V9_FORMAT))
            });
    let begin = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(item, addr, Awaiting::Hello, None)?;
        item.artwork_supported = supports_artwork;
        item.mode = if item.prefer_ultra && supports_no_checksum && supports_ultra {
            ChunkMode::NoneUltra
        } else if item.prefer_no_checksum && supports_no_checksum {
            ChunkMode::NoneFast
        } else {
            ChunkMode::Crc32
        };
        let max = value
            .get("maxChunkBytes")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_CHUNK_BYTES as u64) as usize;
        let mode_limit = item.mode.max_chunk_bytes();
        if !supports_artwork {
            item.assets.retain(|asset| {
                asset.metadata.kind != "background"
                    && asset.metadata.format != Some(crate::artwork::LVGL_V9_FORMAT)
            });
            let total = item.assets.iter().map(|asset| asset.metadata.size).sum();
            state::with_state(|state| state.total = total);
        }
        item.chunk_bytes = mode_limit.min(max.max(1));
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
                if let Some(width) = asset.metadata.width {
                    value["width"] = Value::from(width);
                }
                if let Some(height) = asset.metadata.height {
                    value["height"] = Value::from(height);
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
            "checksum": item.mode.checksum(),
            "chunkMode": item.mode.as_str(),
        })
    };
    let mode = transfer()
        .lock()
        .unwrap_or_else(|item| item.into_inner())
        .as_ref()
        .map(|item| item.mode)
        .unwrap_or(ChunkMode::Crc32);
    let mode_label = match mode {
        ChunkMode::Crc32 => "CRC32 校验模式",
        ChunkMode::NoneFast => "无校验高速模式",
        ChunkMode::NoneUltra => "无校验超高速模式（48 KiB 单包上限）",
    };
    state::with_state(|state| {
        state.status = if supports_artwork {
            format!("快应用已连接，正在准备存储（{mode_label}）…")
        } else {
            format!("快应用版本不支持 BIN 封面/背景，将仅导入音频和歌词（{mode_label}）…")
        };
    });
    interconnect::send(addr, &begin).await
}

async fn handle_ready(addr: &str, value: &Value) -> Result<(), String> {
    {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(
            item,
            addr,
            Awaiting::Ready,
            value.get("id").and_then(Value::as_str),
        )?;
        let asset = item
            .assets
            .get(item.asset_index)
            .ok_or_else(|| "资源索引越界".to_string())?;
        if value.get("asset").and_then(Value::as_str) != Some(asset.metadata.kind)
            || value.get("nextSeq").and_then(Value::as_u64) != Some(0)
            || value.get("nextOffset").and_then(Value::as_u64) != Some(0)
            || value.get("window").and_then(Value::as_u64) != Some(1)
        {
            return Err("快应用返回了无效资源起点".to_string());
        }
        let ready_checksum = value.get("checksum").and_then(Value::as_str);
        if (ready_checksum.is_some() && ready_checksum != Some(item.mode.checksum()))
            || (item.mode != ChunkMode::Crc32 && ready_checksum.is_none())
        {
            return Err("快应用返回的校验模式不一致".to_string());
        }
        let ready_mode = value.get("chunkMode").and_then(Value::as_str);
        if (ready_mode.is_some() && ready_mode != Some(item.mode.as_str()))
            || (item.mode == ChunkMode::NoneUltra && ready_mode.is_none())
        {
            return Err("快应用返回的分片模式不一致".to_string());
        }
        let max = value
            .get("maxChunkBytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| "快应用未返回分片上限".to_string())? as usize;
        let mode_limit = item.mode.max_chunk_bytes();
        item.chunk_bytes = item.chunk_bytes.min(max.clamp(1, mode_limit));
    }
    update_asset_status();
    send_next_chunk(addr).await
}

async fn handle_ack(addr: &str, value: &Value) -> Result<(), String> {
    let (completed, confirmed, speed) = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(
            item,
            addr,
            Awaiting::Ack,
            value.get("id").and_then(Value::as_str),
        )?;
        let asset = &item.assets[item.asset_index].metadata;
        let pending = item
            .pending_chunk
            .as_ref()
            .ok_or_else(|| "没有等待确认的分片".to_string())?;
        if pending.asset != asset.kind
            || value.get("asset").and_then(Value::as_str) != Some(asset.kind)
            || value.get("nextSeq").and_then(Value::as_u64) != Some(item.seq)
            || value.get("nextOffset").and_then(Value::as_u64) != Some(item.offset)
            || value.get("receivedBytes").and_then(Value::as_u64) != Some(item.sent_total)
        {
            return Err("累计 ACK 与已发送位置不一致".to_string());
        }
        item.pending_chunk = None;
        let now = Instant::now();
        let speed = item.last_ack_at.map_or(0, |last| {
            let elapsed = now.duration_since(last).as_secs_f64();
            let delta = item.sent_total.saturating_sub(item.last_ack_bytes);
            if elapsed == 0.0 {
                item.speed_bytes_per_second
            } else {
                let sample = (delta as f64 / elapsed) as u64;
                if item.speed_bytes_per_second == 0 {
                    sample
                } else {
                    (item.speed_bytes_per_second.saturating_mul(3) + sample) / 4
                }
            }
        });
        item.last_ack_at = Some(now);
        item.last_ack_bytes = item.sent_total;
        item.speed_bytes_per_second = speed;
        (item.offset == asset.size, item.sent_total, speed)
    };
    state::with_state(|state| {
        state.sent = confirmed;
        state.speed_bytes_per_second = speed;
    });
    if completed {
        send_asset_end(addr).await
    } else {
        send_next_chunk(addr).await
    }
}

async fn handle_retry(addr: &str, value: &Value) -> Result<(), String> {
    let (packet, retry) = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(
            item,
            addr,
            Awaiting::Ack,
            value.get("id").and_then(Value::as_str),
        )?;
        let pending = item
            .pending_chunk
            .as_mut()
            .ok_or_else(|| "没有可重传的分片".to_string())?;
        let retry = claim_chunk_retry(
            pending,
            value.get("asset").and_then(Value::as_str),
            value.get("seq").and_then(Value::as_u64),
            value.get("offset").and_then(Value::as_u64),
        )?;
        let mut packet = json!({
            "tag": "lyra-import-chunk",
            "id": item.id,
            "asset": pending.asset,
            "seq": pending.seq,
            "offset": pending.offset,
            "encoding": "base64",
            "data": BASE64.encode(&pending.bytes),
        });
        if let Some(crc32) = pending.crc32.as_deref() {
            packet["crc32"] = Value::String(crc32.to_string());
        }
        (packet, retry)
    };
    state::with_state(|state| {
        state.status = format!("分片校验失败，正在进行第 {retry}/2 次重传…");
    });
    interconnect::send(addr, &packet).await
}

fn claim_chunk_retry(
    pending: &mut PendingChunk,
    asset: Option<&str>,
    seq: Option<u64>,
    offset: Option<u64>,
) -> Result<u8, String> {
    if asset != Some(pending.asset) || seq != Some(pending.seq) || offset != Some(pending.offset) {
        return Err("重传请求与等待确认的分片不一致".to_string());
    }
    if pending.retries >= MAX_CHUNK_RETRIES {
        return Err("分片已达到最多 2 次重传".to_string());
    }
    pending.retries += 1;
    Ok(pending.retries)
}

async fn send_next_chunk(addr: &str) -> Result<(), String> {
    let packet = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        let asset = item
            .assets
            .get_mut(item.asset_index)
            .ok_or_else(|| "资源索引越界".to_string())?;
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
        if item.pending_chunk.is_some() {
            return Err("上一分片仍在等待确认".to_string());
        }
        let crc32 = if item.mode == ChunkMode::Crc32 {
            let mut crc = Crc32::new();
            crc.update(&bytes);
            Some(format!("{:08x}", crc.finalize()))
        } else {
            None
        };
        let count = bytes.len() as u64;
        let mut packet = json!({
            "tag": "lyra-import-chunk",
            "id": item.id,
            "asset": asset.metadata.kind,
            "seq": item.seq,
            "offset": item.offset,
            "encoding": "base64",
            "data": BASE64.encode(&bytes),
        });
        if let Some(crc32) = crc32.as_deref() {
            packet["crc32"] = Value::String(crc32.to_string());
        }
        item.pending_chunk = Some(PendingChunk {
            asset: asset.metadata.kind,
            seq: item.seq,
            offset: item.offset,
            bytes,
            crc32,
            retries: 0,
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
        item.awaiting = if item.asset_index == item.assets.len() {
            Awaiting::AssetsDone
        } else {
            Awaiting::Ready
        };
        packet
    };
    interconnect::send(addr, &packet).await
}

async fn handle_assets_done(addr: &str, value: &Value) -> Result<(), String> {
    let packet = {
        let mut guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
        let item = guard.as_mut().ok_or_else(|| "没有活动导入".to_string())?;
        validate_peer(
            item,
            addr,
            Awaiting::AssetsDone,
            value.get("id").and_then(Value::as_str),
        )?;
        item.awaiting = Awaiting::Done;
        json!({ "tag": "lyra-import-commit", "id": item.id })
    };
    state::with_state(|state| state.status = "正在发布曲目与音乐库…".to_string());
    interconnect::send(addr, &packet).await
}

fn handle_done(addr: &str, value: &Value) -> Result<(), String> {
    let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
    let item = guard.as_ref().ok_or_else(|| "没有活动导入".to_string())?;
    validate_peer(
        item,
        addr,
        Awaiting::Done,
        value.get("id").and_then(Value::as_str),
    )?;
    drop(guard);
    *transfer().lock().unwrap_or_else(|item| item.into_inner()) = None;
    state::with_state(|state| {
        state.active = false;
        state.sent = state.total;
        state.speed_bytes_per_second = 0;
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
        state.speed_bytes_per_second = 0;
        state.status = "已取消导入。".to_string();
    });
}

fn validate_assets(assets: &[ImportAsset]) -> Result<(), String> {
    if assets.is_empty() || assets.len() > 4 || assets[0].kind != "audio" {
        return Err("导入必须以一个音频资源开始".to_string());
    }
    let mut seen = Vec::new();
    let mut previous_order = 0usize;
    for asset in assets {
        if asset.size == 0 || seen.contains(&asset.kind) {
            return Err("导入资源为空或重复".to_string());
        }
        let order = match asset.kind {
            "audio" => 0,
            "cover" => 1,
            "background" => 2,
            "lyrics" => 3,
            _ => return Err("导入包含未知资源".to_string()),
        };
        if order < previous_order {
            return Err("导入资源顺序无效".to_string());
        }
        previous_order = order;
        if asset.format == Some(crate::artwork::LVGL_V9_FORMAT) {
            let valid = match asset.kind {
                "cover" => {
                    asset.extension == Some("bin")
                        && asset.width == Some(crate::artwork::COVER_WIDTH)
                        && asset.height == Some(crate::artwork::COVER_HEIGHT)
                        && asset.size == crate::artwork::COVER_BIN_BYTES
                }
                "background" => {
                    asset.extension == Some("bin")
                        && asset.width == Some(crate::artwork::BACKGROUND_WIDTH)
                        && asset.height == Some(crate::artwork::BACKGROUND_HEIGHT)
                        && asset.size == crate::artwork::BACKGROUND_BIN_BYTES
                }
                _ => false,
            };
            if !valid {
                return Err("LVGL 图片资源元数据无效".to_string());
            }
        } else if asset.kind == "background" {
            return Err("背景必须是 LVGL v9 BIN".to_string());
        }
        seen.push(asset.kind);
    }
    Ok(())
}

fn update_asset_status() {
    let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
    if let Some(item) = guard.as_ref() {
        let kind = item.assets[item.asset_index].metadata.kind;
        let label = match kind {
            "audio" => "音频",
            "cover" => "封面",
            "background" => "播放页背景",
            "lyrics" => "歌词",
            _ => "资源",
        };
        let compatibility = if item.artwork_supported {
            ""
        } else {
            "（当前快应用不支持 BIN 封面/背景）"
        };
        state::with_state(|state| {
            state.status = format!("正在传输{label}{compatibility}…");
        });
    }
}

fn matches_active_peer(addr: &str, tag: &str, value: &Value) -> bool {
    let guard = transfer().lock().unwrap_or_else(|item| item.into_inner());
    let Some(item) = guard.as_ref() else {
        return false;
    };
    if item.addr != addr {
        return false;
    }
    if tag == "lyra-import-hello" {
        return item.awaiting == Awaiting::Hello;
    }
    value.get("id").and_then(Value::as_str) == Some(item.id.as_str())
}

fn validate_peer(
    item: &Transfer,
    addr: &str,
    awaiting: Awaiting,
    id: Option<&str>,
) -> Result<(), String> {
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
        state.speed_bytes_per_second = 0;
        state.status = format!("导入失败：{error}");
    });
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_requires_audio_first_and_unique_assets() {
        let audio = ImportAsset::audio("audio.mp3".into(), 10);
        let cover = ImportAsset {
            kind: "cover",
            path: "cover.jpg".into(),
            size: 5,
            extension: Some("jpg"),
            format: None,
            width: None,
            height: None,
        };
        assert!(validate_assets(&[audio.clone(), cover]).is_ok());
        assert!(validate_assets(&[audio.clone(), audio]).is_err());
        assert!(validate_assets(&[ImportAsset::lyrics("lyrics.lrc".into(), 4, "lrc")]).is_err());
    }

    #[test]
    fn accepts_fixed_lvgl_cover_and_background_assets() {
        let assets = [
            ImportAsset::audio("audio.mp3".into(), 10),
            ImportAsset::cover_bin("cover.bin".into(), crate::artwork::COVER_BIN_BYTES),
            ImportAsset::background_bin(
                "background.bin".into(),
                crate::artwork::BACKGROUND_BIN_BYTES,
            ),
        ];
        assert!(validate_assets(&assets).is_ok());
    }

    #[test]
    fn chunk_retry_is_exact_and_limited_to_two_attempts() {
        let mut pending = PendingChunk {
            asset: "audio",
            seq: 7,
            offset: 24_576,
            bytes: vec![1, 2, 3],
            crc32: Some("55bc801d".to_string()),
            retries: 0,
        };
        assert_eq!(
            claim_chunk_retry(&mut pending, Some("audio"), Some(7), Some(24_576)),
            Ok(1)
        );
        assert_eq!(
            claim_chunk_retry(&mut pending, Some("audio"), Some(7), Some(24_576)),
            Ok(2)
        );
        assert!(claim_chunk_retry(&mut pending, Some("audio"), Some(7), Some(24_576)).is_err());
        assert!(claim_chunk_retry(&mut pending, Some("cover"), Some(7), Some(24_576)).is_err());
    }

    #[test]
    fn no_checksum_fast_chunk_fits_outgoing_frame() {
        let bytes = vec![0xff; FAST_CHUNK_BYTES];
        let frame = json!({
            "tag": "lyra-import-chunk",
            "id": "0123456789abcdef0123456789abcdef",
            "asset": "audio",
            "seq": 99_999_999,
            "offset": 67_108_864,
            "encoding": "base64",
            "data": BASE64.encode(bytes),
        })
        .to_string();
        assert!(frame.len() <= interconnect::OUTGOING_FRAME_CAPACITY);
    }

    #[test]
    fn no_checksum_ultra_chunk_fits_expanded_outgoing_frame() {
        let bytes = vec![0xff; ULTRA_CHUNK_BYTES];
        let frame = json!({
            "tag": "lyra-import-chunk",
            "id": "0123456789abcdef0123456789abcdef",
            "asset": "audio",
            "seq": 99_999_999,
            "offset": 67_108_864,
            "encoding": "base64",
            "data": BASE64.encode(bytes),
        })
        .to_string();
        assert!(frame.len() <= interconnect::OUTGOING_FRAME_CAPACITY);
        assert_eq!(frame.len(), interconnect::OUTGOING_FRAME_CAPACITY - 2);
    }

    #[test]
    fn configured_base64_chunk_fits_outgoing_frame() {
        let bytes = vec![0xff; DEFAULT_CHUNK_BYTES];
        let frame = json!({
            "tag": "lyra-import-chunk",
            "id": "0123456789abcdef0123456789abcdef",
            "asset": "lyrics",
            "seq": 99_999_999,
            "offset": 67_108_864,
            "encoding": "base64",
            "data": BASE64.encode(bytes),
            "crc32": "ffffffff",
        })
        .to_string();
        assert!(frame.len() > interconnect::INCOMING_FRAME_CAPACITY);
        assert!(frame.len() <= interconnect::OUTGOING_FRAME_CAPACITY);
    }
}
