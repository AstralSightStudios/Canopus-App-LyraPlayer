use serde::Deserialize;
use serde_json::json;

use crate::{interconnect, state};

pub const PAGE_SIZE: usize = 8;

#[derive(Deserialize)]
struct Page {
    #[serde(rename = "requestId")]
    request_id: String,
    revision: String,
    offset: usize,
    total: usize,
    tracks: Vec<WireTrack>,
}

#[derive(Deserialize)]
struct WireTrack {
    id: u64,
    name: String,
    #[serde(default)]
    artists: Vec<String>,
    #[serde(default)]
    album: String,
    #[serde(rename = "durationMs", default)]
    duration_ms: u32,
}

pub fn refresh() {
    let snapshot = state::snapshot();
    if snapshot.selected_addr.is_empty() {
        state::with_state(|state| state.status = "请先选择设备。".into());
        return;
    }
    if snapshot.active {
        state::with_state(|state| state.status = "请等待当前导入完成后再读取手表音乐。".into());
        return;
    }
    if snapshot.library_busy {
        state::with_state(|state| state.status = "请等待当前音乐库操作完成。".into());
        return;
    }

    let request_id = next_request_id();
    state::with_state(|state| {
        state.library_request_id = request_id.clone();
        state.device_library.clear();
        state.library_revision.clear();
        state.library_total = 0;
        state.library_busy = true;
        state.library_target = None;
        state.status = "正在读取手表音乐列表…".into();
    });
    send_list(&snapshot.selected_addr, &request_id, 0);
}

pub fn move_track(track_id: u64, direction: &str) {
    let snapshot = state::snapshot();
    if snapshot.selected_addr.is_empty() {
        state::with_state(|state| state.status = "请先选择设备。".into());
        return;
    }
    if snapshot.active {
        state::with_state(|state| state.status = "请等待当前导入完成后再调整排序。".into());
        return;
    }
    if snapshot.library_busy {
        state::with_state(|state| state.status = "请等待当前音乐库操作完成。".into());
        return;
    }
    if snapshot.library_revision.is_empty() {
        state::with_state(|state| state.status = "请先刷新手表音乐列表。".into());
        return;
    }
    if direction != "up" && direction != "down" {
        state::with_state(|state| state.status = "无效的排序方向。".into());
        return;
    }

    let request_id = next_request_id();
    state::with_state(|state| {
        state.library_request_id = request_id.clone();
        state.library_target = Some(track_id);
        state.library_busy = true;
        state.status = "正在调整手表音乐顺序…".into();
    });
    let payload = json!({
        "tag": "lyra-library-move",
        "version": 1,
        "requestId": request_id,
        "revision": snapshot.library_revision,
        "trackId": track_id,
        "direction": direction,
    });
    if let Err(error) = wit_bindgen::block_on(interconnect::send(&snapshot.selected_addr, &payload))
    {
        state::with_state(|state| {
            state.library_busy = false;
            state.library_target = None;
            state.status = format!("调整手表音乐顺序失败：{error}");
        });
    }
}

pub fn delete_track(track_id: u64) {
    let snapshot = state::snapshot();
    if snapshot.selected_addr.is_empty() {
        state::with_state(|state| state.status = "请先选择设备。".into());
        return;
    }
    if snapshot.active {
        state::with_state(|state| {
            state.status = "请等待当前导入完成后再删除音乐。".into()
        });
        return;
    }
    if snapshot.library_busy {
        state::with_state(|state| state.status = "请等待当前音乐库操作完成。".into());
        return;
    }
    if snapshot.library_revision.is_empty() {
        state::with_state(|state| state.status = "请先刷新手表音乐列表。".into());
        return;
    }

    let request_id = next_request_id();
    state::with_state(|state| {
        state.library_request_id = request_id.clone();
        state.library_target = Some(track_id);
        state.library_busy = true;
        state.status = "正在删除手表音乐…".into();
    });
    let payload = json!({
        "tag": "lyra-library-delete",
        "version": 1,
        "requestId": request_id,
        "revision": snapshot.library_revision,
        "trackId": track_id,
    });
    if let Err(error) = wit_bindgen::block_on(interconnect::send(&snapshot.selected_addr, &payload))
    {
        state::with_state(|state| {
            state.library_busy = false;
            state.library_target = None;
            state.status = format!("删除手表音乐失败：{error}")
        });
    }
}

fn next_request_id() -> String {
    state::with_state(|state| {
        state.library_nonce = state.library_nonce.wrapping_add(1);
        format!("library-{}", state.library_nonce)
    })
}

fn send_list(addr: &str, request_id: &str, offset: usize) {
    let payload = json!({
        "tag": "lyra-library-list",
        "version": 1,
        "requestId": request_id,
        "offset": offset,
        "limit": PAGE_SIZE,
    });
    if let Err(error) = wit_bindgen::block_on(interconnect::send(addr, &payload)) {
        state::with_state(|state| {
            if state.selected_addr == addr && state.library_request_id == request_id {
                state.library_busy = false;
                state.library_target = None;
            }
            state.status = format!("读取手表音乐失败：{error}");
        });
    }
}

pub fn handle(addr: &str, package: &str, payload: &str) -> bool {
    if package != interconnect::ROUTE_PACKAGE {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(tag) = value.get("tag").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if tag == "lyra-import-error"
        && value.get("code").and_then(serde_json::Value::as_str) == Some("invalid-request")
        && value.get("message").and_then(serde_json::Value::as_str) == Some("unknown message tag")
    {
        state::with_state(|state| {
            state.library_busy = false;
            state.library_target = None;
            state.status = "当前快应用不支持音乐库管理，请更新 Lyra Import 快应用。".into();
        });
        return true;
    }
    if !tag.starts_with("lyra-library-") {
        return false;
    }

    match tag {
        "lyra-library-page" => handle_page(addr, value),
        "lyra-library-error" => handle_error(addr, &value),
        "lyra-library-moved" => handle_moved(addr, &value),
        "lyra-library-deleted" => handle_deleted(addr, &value),
        _ => {}
    }
    true
}

fn handle_page(addr: &str, value: serde_json::Value) {
    let page = match serde_json::from_value::<Page>(value) {
        Ok(page) => page,
        Err(error) => {
            state::with_state(|state| {
                state.library_busy = false;
                state.library_target = None;
                state.status = format!("手表音乐响应无效：{error}");
            });
            return;
        }
    };

    let next = state::with_state(|state| {
        if state.selected_addr != addr || state.library_request_id != page.request_id {
            return None;
        }
        if page.offset != state.device_library.len() || page.total < page.offset {
            state.library_busy = false;
            state.library_target = None;
            state.status = "手表音乐分页顺序无效，请重新刷新。".into();
            return None;
        }
        if !state.library_revision.is_empty() && state.library_revision != page.revision {
            state.library_busy = false;
            state.library_target = None;
            state.status = "读取期间手表音乐列表已变化，请重新刷新。".into();
            return None;
        }

        state.library_revision = page.revision;
        state.library_total = page.total;
        let received = page.tracks.len();
        state
            .device_library
            .extend(
                page.tracks
                    .into_iter()
                    .map(|track| state::DeviceLibraryTrack {
                        id: track.id,
                        name: track.name,
                        artists: track.artists,
                        album: track.album,
                        duration_ms: track.duration_ms,
                    }),
            );
        if state.device_library.len() > state.library_total {
            state.device_library.truncate(state.library_total);
            state.library_busy = false;
            state.library_target = None;
            state.status = "手表音乐分页数量无效，请重新刷新。".into();
            return None;
        }
        if received == 0 && state.device_library.len() < state.library_total {
            state.library_busy = false;
            state.library_target = None;
            state.status = "手表音乐分页提前结束，请重新刷新。".into();
            return None;
        }

        state.status = format!(
            "已读取 {}/{} 首手表音乐。",
            state.device_library.len(),
            state.library_total
        );
        let has_next = state.device_library.len() < state.library_total;
        if !has_next {
            state.library_busy = false;
            state.library_target = None;
        }
        has_next.then(|| (state.library_request_id.clone(), state.device_library.len()))
    });

    if let Some((request_id, offset)) = next {
        send_list(addr, &request_id, offset);
    }
}

fn handle_error(addr: &str, value: &serde_json::Value) {
    let request_id = value
        .get("requestId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let code = value
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    state::with_state(|state| {
        if state.selected_addr != addr || state.library_request_id != request_id {
            return;
        }
        state.library_busy = false;
        state.library_target = None;
        state.status = match code {
            "invalid-request" => "当前快应用不支持该音乐库操作，请更新 Lyra Import 快应用。".into(),
            "busy" => "快应用正在导入音乐，请稍后重试。".into(),
            "conflict" => "手表音乐列表已变化，请刷新后再操作。".into(),
            "boundary" => "这首歌曲已经位于列表边界。".into(),
            "not-found" => "目标歌曲已不存在，请刷新手表音乐列表。".into(),
            "invalid-library" => "手表音乐库文件无效，未执行操作。".into(),
            "io-error" => "快应用读写手表音乐库失败，旧列表未被替换。".into(),
            "response-too-large" => "单首音乐元数据过长，无法通过设备消息读取。".into(),
            _ => format!("手表音乐操作失败：{code}"),
        };
    });
}

fn handle_moved(addr: &str, value: &serde_json::Value) {
    let request_id = value
        .get("requestId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let track_id = value.get("trackId").and_then(serde_json::Value::as_u64);
    let revision_valid = value
        .get("revision")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|revision| !revision.is_empty());
    let accepted = state::with_state(|state| {
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(1)
            || !revision_valid
            || state.selected_addr != addr
            || state.library_request_id != request_id
            || state.library_target != track_id
        {
            return false;
        }
        state.library_busy = false;
        state.library_target = None;
        true
    });
    if accepted {
        refresh();
    }
}

fn handle_deleted(addr: &str, value: &serde_json::Value) {
    let request_id = value
        .get("requestId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let track_id = value.get("trackId").and_then(serde_json::Value::as_u64);
    let revision_valid = value
        .get("revision")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|revision| !revision.is_empty());
    let cleanup_pending = value
        .get("cleanupPending")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let accepted = state::with_state(|state| {
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(1)
            || !revision_valid
            || state.selected_addr != addr
            || state.library_request_id != request_id
            || state.library_target != track_id
        {
            return false;
        }
        state.library_busy = false;
        state.library_target = None;
        state.status = if cleanup_pending {
            "音乐已从列表删除，文件清理将在稍后重试。".into()
        } else {
            "音乐已删除，正在刷新手表音乐列表…".into()
        };
        true
    });
    if accepted {
        refresh();
    }
}
