//! The watch's song list and playlists as the plugin shows them: read page by page,
//! reordered and deleted through the same CanopusConnect channel the importer uses.
//!
//! A refresh is one chain of requests that share a request id: `HELLO` (which version
//! does the module speak?), the songs, then -- for version 2 and up -- the playlists and
//! the members of each.

use std::{
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use lyra_protocol::{
    MAX_PLAYLIST_NAME, MAX_PLAYLISTS, MIN_VERSION, Incoming, Reply, Request, Status, op,
};

use crate::{connect, state};

pub const PAGE_SIZE: u8 = 8;
/// Playlist members per request; the module never answers with more than 57 per frame.
const MEMBER_PAGE: u8 = 56;
const PLAYLIST_PAGE: u8 = 4;
/// Request ids of this module start here; the importer's are below.
const REQ_BASE: u16 = 0x8000;
/// The first protocol version that has playlists.
pub const PLAYLIST_VERSION: u8 = 2;

enum Phase {
    Hello,
    Songs,
    Playlists,
    /// Members of the playlist at this index.
    Members(usize),
}

struct Refresh {
    req: u16,
    phase: Phase,
    tracks: Vec<state::DeviceLibraryTrack>,
    playlists: Vec<state::DevicePlaylist>,
}

static NEXT_REQ: Mutex<u16> = Mutex::new(REQ_BASE);
static LISTING: Mutex<Option<Refresh>> = Mutex::new(None);
/// The request id of a pending move, delete or playlist change.
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

pub async fn refresh() {
    let Some(addr) = precheck("读取手表音乐") else {
        return;
    };
    let req = next_req();
    *LISTING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Refresh {
        req,
        phase: Phase::Hello,
        tracks: Vec::new(),
        playlists: Vec::new(),
    });
    state::with_state(|state| {
        state.device_library.clear();
        state.device_playlists.clear();
        state.library_revision.clear();
        state.library_total = 0;
        state.library_busy = true;
        state.library_target = None;
        state.status = "正在读取手表音乐列表…".into();
    });
    let hello = Request::Hello {
        req,
        version: MIN_VERSION,
    };
    if let Err(error) = send(&addr, hello).await {
        fail(&format!("无法读取手表音乐：{error}"));
    }
}

pub async fn move_track(track_id: u64, direction: &str) {
    let Some(addr) = precheck("调整顺序") else {
        return;
    };
    let request = Request::Move {
        req: begin_operation(Some(track_id), "正在调整手表音乐顺序…"),
        id: track_id,
        up: direction == "up",
    };
    run(&addr, request, "调整手表音乐顺序失败").await;
}

pub async fn delete_track(track_id: u64) {
    let Some(addr) = precheck("删除音乐") else {
        return;
    };
    let request = Request::Delete {
        req: begin_operation(Some(track_id), "正在删除手表音乐…"),
        id: track_id,
    };
    run(&addr, request, "删除手表音乐失败").await;
}

/// A new id for a playlist; the module only needs it to be non-zero and unused.
fn new_playlist_id() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(1);
    nanos.max(1)
}

fn playlists_supported() -> bool {
    let version = state::snapshot().device_version;
    if version < PLAYLIST_VERSION {
        state::with_state(|state| {
            state.status = if version == 0 {
                "请先刷新手表音乐，再管理歌单。".into()
            } else {
                "手表上的 Lyra Player 版本过旧，请先更新模块才能使用歌单。".into()
            }
        });
        return false;
    }
    true
}

pub async fn create_playlist(name: &str) {
    let name = name.trim();
    if name.is_empty() {
        state::with_state(|state| state.status = "请先输入歌单名称。".into());
        return;
    }
    if name.len() > MAX_PLAYLIST_NAME {
        state::with_state(|state| state.status = "歌单名称太长。".into());
        return;
    }
    if !playlists_supported() {
        return;
    }
    if state::snapshot().device_playlists.len() >= MAX_PLAYLISTS {
        state::with_state(|state| state.status = format!("最多只能有 {MAX_PLAYLISTS} 个歌单。"));
        return;
    }
    let Some(addr) = precheck("新建歌单") else {
        return;
    };
    let request = Request::PlaylistUpsert {
        req: begin_operation(None, "正在新建歌单…"),
        id: new_playlist_id(),
        name: name.to_string(),
    };
    run(&addr, request, "新建歌单失败").await;
}

pub async fn rename_playlist(id: u64, name: &str) {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_PLAYLIST_NAME {
        state::with_state(|state| state.status = "歌单名称不能为空，也不能太长。".into());
        return;
    }
    if !playlists_supported() {
        return;
    }
    let Some(addr) = precheck("重命名歌单") else {
        return;
    };
    let request = Request::PlaylistUpsert {
        req: begin_operation(None, "正在重命名歌单…"),
        id,
        name: name.to_string(),
    };
    run(&addr, request, "重命名歌单失败").await;
}

pub async fn delete_playlist(id: u64) {
    if !playlists_supported() {
        return;
    }
    let Some(addr) = precheck("删除歌单") else {
        return;
    };
    let request = Request::PlaylistDelete {
        req: begin_operation(None, "正在删除歌单…"),
        id,
    };
    run(&addr, request, "删除歌单失败").await;
}

pub async fn playlist_track(id: u64, track: u64, add: bool) {
    if !playlists_supported() {
        return;
    }
    let Some(addr) = precheck("调整歌单") else {
        return;
    };
    let request = Request::PlaylistTrack {
        req: begin_operation(Some(track), "正在调整歌单…"),
        id,
        track,
        add,
    };
    run(&addr, request, "调整歌单失败").await;
}

fn begin_operation(target: Option<u64>, status: &str) -> u16 {
    let req = next_req();
    *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = Some(req);
    state::with_state(|state| {
        state.library_target = target;
        state.library_busy = true;
        state.status = status.to_string();
    });
    req
}

async fn run(addr: &str, request: Request, failure: &str) {
    if let Err(error) = send(addr, request).await {
        fail(&format!("{failure}：{error}"));
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

/// What the listing does after a reply: send the next request, or it is complete.
enum Next {
    Send(Request),
    Done,
}

/// A frame from the module; `true` when it answered one of this module's requests.
pub async fn on_frame(addr: &str, frame: &[u8]) -> bool {
    let Ok(Incoming::Reply(reply)) = Incoming::decode(frame) else {
        return false;
    };
    match reply {
        Reply::Hello {
            req,
            status,
            version,
            ..
        } => {
            let mut guard = LISTING.lock().unwrap_or_else(|e| e.into_inner());
            let Some(listing) = guard.as_mut().filter(|l| l.req == req) else {
                return false;
            };
            if status != Status::Ok {
                drop(guard);
                fail("手表上的 Lyra Player 版本过旧，请先更新模块。");
                return true;
            }
            listing.phase = Phase::Songs;
            let next = Request::List {
                req,
                offset: 0,
                max: PAGE_SIZE,
            };
            drop(guard);
            state::with_state(|state| state.device_version = version);
            advance(addr, Next::Send(next)).await;
            true
        }
        Reply::List {
            req,
            status,
            total,
            offset,
            entries,
        } => {
            let mut guard = LISTING.lock().unwrap_or_else(|e| e.into_inner());
            let Some(listing) = guard.as_mut().filter(|l| l.req == req) else {
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
            let next = if count > 0 && loaded < usize::from(total) {
                Next::Send(Request::List {
                    req,
                    offset: loaded as u16,
                    max: PAGE_SIZE,
                })
            } else if state::snapshot().device_version >= PLAYLIST_VERSION {
                listing.phase = Phase::Playlists;
                Next::Send(Request::Playlists {
                    req,
                    offset: 0,
                    max: PLAYLIST_PAGE,
                })
            } else {
                Next::Done
            };
            drop(guard);
            advance(addr, next).await;
            true
        }
        Reply::Playlists {
            req,
            status,
            total,
            offset,
            entries,
        } => {
            let mut guard = LISTING.lock().unwrap_or_else(|e| e.into_inner());
            let Some(listing) = guard.as_mut().filter(|l| l.req == req) else {
                return false;
            };
            if status != Status::Ok {
                drop(guard);
                fail("手表拒绝了歌单列表请求。");
                return true;
            }
            let count = entries.len();
            listing.playlists.extend(entries.into_iter().map(|entry| state::DevicePlaylist {
                id: entry.id,
                name: entry.name,
                tracks: Vec::new(),
            }));
            let loaded = usize::from(offset) + count;
            let next = if count > 0 && loaded < usize::from(total) {
                Next::Send(Request::Playlists {
                    req,
                    offset: loaded as u16,
                    max: PLAYLIST_PAGE,
                })
            } else {
                members_or_done(listing, 0)
            };
            drop(guard);
            advance(addr, next).await;
            true
        }
        Reply::PlaylistTracks {
            req,
            status,
            id,
            total,
            offset,
            tracks,
        } => {
            let mut guard = LISTING.lock().unwrap_or_else(|e| e.into_inner());
            let Some(listing) = guard.as_mut().filter(|l| l.req == req) else {
                return false;
            };
            let Phase::Members(index) = listing.phase else {
                return false;
            };
            if status != Status::Ok || listing.playlists.get(index).map(|list| list.id) != Some(id) {
                drop(guard);
                fail("手表拒绝了歌单内容请求。");
                return true;
            }
            let count = tracks.len();
            listing.playlists[index].tracks.extend(tracks);
            let loaded = usize::from(offset) + count;
            let next = if count > 0 && loaded < usize::from(total) {
                Next::Send(Request::PlaylistTracks {
                    req,
                    id,
                    offset: loaded as u16,
                    max: MEMBER_PAGE,
                })
            } else {
                members_or_done(listing, index + 1)
            };
            drop(guard);
            advance(addr, next).await;
            true
        }
        Reply::Ack {
            op: reply_op,
            req,
            status,
            ..
        } if matches!(
            reply_op,
            op::MOVE
                | op::DELETE
                | op::PLAYLIST_UPSERT
                | op::PLAYLIST_DELETE
                | op::PLAYLIST_TRACK
        ) =>
        {
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
                if reply_op == op::PLAYLIST_UPSERT {
                    state::with_state(|state| state.playlist_editor = None);
                }
                refresh().await;
            } else {
                state::with_state(|state| state.status = format!("手表拒绝了该操作：{status:?}"));
            }
            true
        }
        _ => false,
    }
}

/// The request for the members of the playlist at `index`, or `Done` past the last one.
fn members_or_done(listing: &mut Refresh, index: usize) -> Next {
    match listing.playlists.get(index) {
        Some(list) => {
            listing.phase = Phase::Members(index);
            Next::Send(Request::PlaylistTracks {
                req: listing.req,
                id: list.id,
                offset: 0,
                max: MEMBER_PAGE,
            })
        }
        None => Next::Done,
    }
}

async fn advance(addr: &str, next: Next) {
    match next {
        Next::Send(request) => {
            if let Err(error) = send(addr, request).await {
                fail(&format!("无法读取手表音乐：{error}"));
            }
        }
        Next::Done => {
            let finished = LISTING.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(listing) = finished else { return };
            state::with_state(|state| {
                state.library_total = listing.tracks.len();
                state.library_revision = "ok".to_string();
                state.device_library = listing.tracks;
                state.device_playlists = listing.playlists;
                state.library_busy = false;
                state.library_target = None;
                state.status = format!(
                    "手表上有 {} 首歌曲、{} 个歌单。",
                    state.library_total,
                    state.device_playlists.len()
                );
            });
        }
    }
}
