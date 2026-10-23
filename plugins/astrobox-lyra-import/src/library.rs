//! The watch's song list as the plugin shows it: read page by page, reordered and
//! deleted through the same CanopusConnect channel the importer uses.

use std::sync::Mutex;

use lyra_protocol::{Incoming, Reply, Request, Status, op};

use crate::{connect, state};

pub const PAGE_SIZE: u8 = 8;
/// Request ids of this module start here; the importer's are below.
const REQ_BASE: u16 = 0x8000;

struct Listing {
    req: u16,
    tracks: Vec<state::DeviceLibraryTrack>,
}

static NEXT_REQ: Mutex<u16> = Mutex::new(REQ_BASE);
static LISTING: Mutex<Option<Listing>> = Mutex::new(None);
/// The request id of a pending move or delete.
static OPERATION: Mutex<Option<u16>> = Mutex::new(None);

fn next_req() -> u16 {
    let mut next = NEXT_REQ.lock().unwrap_or_else(|e| e.into_inner());
    *next = if *next == u16::MAX { REQ_BASE } else { *next + 1 };
    *next
}

async fn send(addr: &str, request: Request) -> Result<(), String> {
    let frame = request.encode().map_err(|_| "内部错误".to_string())?;
    connect::ensure_listening(addr).await?;
    connect::send(addr, &frame).await
}

fn precheck(action: &str) -> Option<String> {
    let snapshot = state::snapshot();
    if snapshot.selected_addr.is_empty() {
        state::with_state(|state| state.status = "请先选择设备。".into());
        return None;
    }
    if snapshot.active {
        state::with_state(|state| state.status = format!("请等待当前导入完成后再{action}。"));
        return None;
    }
    if snapshot.library_busy {
        state::with_state(|state| state.status = "请等待当前音乐库操作完成。".into());
        return None;
    }
    Some(snapshot.selected_addr)
}

pub fn refresh() {
    wit_bindgen::block_on(refresh_async());
}

pub async fn refresh_async() {
    let Some(addr) = precheck("读取手表音乐") else {
        return;
    };
    let req = next_req();
    *LISTING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Listing {
        req,
        tracks: Vec::new(),
    });
    state::with_state(|state| {
        state.device_library.clear();
        state.library_revision.clear();
        state.library_total = 0;
        state.library_busy = true;
        state.library_target = None;
        state.status = "正在读取手表音乐列表…".into();
    });
    if let Err(error) = send(&addr, Request::List { req, offset: 0, max: PAGE_SIZE }).await {
        fail(&format!("无法读取手表音乐：{error}"));
    }
}

pub fn move_track(track_id: u64, direction: &str) {
    wit_bindgen::block_on(move_track_async(track_id, direction));
}

async fn move_track_async(track_id: u64, direction: &str) {
    let Some(addr) = precheck("调整顺序") else {
        return;
    };
    let req = next_req();
    *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = Some(req);
    state::with_state(|state| {
        state.library_target = Some(track_id);
        state.library_busy = true;
        state.status = "正在调整手表音乐顺序…".into();
    });
    let request = Request::Move {
        req,
        id: track_id,
        up: direction == "up",
    };
    if let Err(error) = send(&addr, request).await {
        fail(&format!("调整手表音乐顺序失败：{error}"));
    }
}

pub fn delete_track(track_id: u64) {
    wit_bindgen::block_on(delete_track_async(track_id));
}

async fn delete_track_async(track_id: u64) {
    let Some(addr) = precheck("删除音乐") else {
        return;
    };
    let req = next_req();
    *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = Some(req);
    state::with_state(|state| {
        state.library_target = Some(track_id);
        state.library_busy = true;
        state.status = "正在删除手表音乐…".into();
    });
    if let Err(error) = send(&addr, Request::Delete { req, id: track_id }).await {
        fail(&format!("删除手表音乐失败：{error}"));
    }
}

fn fail(message: &str) {
    *LISTING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    state::with_state(|state| {
        state.library_busy = false;
        state.library_target = None;
        state.status = message.to_string();
    });
}

/// A frame from the module; `true` when it answered one of this module's requests.
pub async fn on_frame(addr: &str, frame: &[u8]) -> bool {
    let Ok(Incoming::Reply(reply)) = Incoming::decode(frame) else {
        return false;
    };
    match reply {
        Reply::List {
            req,
            status,
            total,
            offset,
            entries,
        } => {
            let mut guard = LISTING.lock().unwrap_or_else(|e| e.into_inner());
            let Some(listing) = guard.as_mut().filter(|listing| listing.req == req) else {
                return false;
            };
            if status != Status::Ok {
                drop(guard);
                fail("手表拒绝了音乐列表请求。");
                return true;
            }
            let count = entries.len();
            listing.tracks.extend(entries.into_iter().map(|entry| state::DeviceLibraryTrack {
                id: entry.id,
                name: entry.name,
                artists: entry
                    .artist
                    .split(" / ")
                    .filter(|artist| !artist.is_empty())
                    .map(str::to_string)
                    .collect(),
                album: String::new(),
                duration_ms: entry.duration_ms,
            }));
            let loaded = usize::from(offset) + count;
            if count > 0 && loaded < usize::from(total) {
                let next = Request::List {
                    req,
                    offset: loaded as u16,
                    max: PAGE_SIZE,
                };
                drop(guard);
                if let Err(error) = send(addr, next).await {
                    fail(&format!("无法读取手表音乐：{error}"));
                }
            } else {
                let tracks = std::mem::take(&mut listing.tracks);
                *guard = None;
                drop(guard);
                state::with_state(|state| {
                    state.library_total = tracks.len();
                    state.library_revision = "ok".to_string();
                    state.device_library = tracks;
                    state.library_busy = false;
                    state.library_target = None;
                    state.status = format!("手表上有 {} 首歌曲。", state.library_total);
                });
            }
            true
        }
        Reply::Ack {
            op: reply_op,
            req,
            status,
            ..
        } if matches!(reply_op, op::MOVE | op::DELETE) => {
            let mut guard = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
            if *guard != Some(req) {
                return false;
            }
            *guard = None;
            drop(guard);
            state::with_state(|state| {
                state.library_busy = false;
                state.library_target = None;
            });
            if status == Status::Ok {
                refresh_async().await;
            } else {
                state::with_state(|state| state.status = format!("手表拒绝了该操作：{status:?}"));
            }
            true
        }
        _ => false,
    }
}
