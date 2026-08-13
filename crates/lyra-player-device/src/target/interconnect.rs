//! Bounded native interconnect transport for FetchBridge JSON frames.
//!
//! Receive callbacks only copy into two fixed slots. Parsing, application
//! updates, audio writes, and LVX navigation are deferred to the UI-owner timer.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use canopus_target_private::*;
use lyra_player_core::{
    Action, Effect,
    app::RequestKind,
    bridge::{BridgeEvent, FetchOptions},
};
use serde_json::Value;

use super::runtime::{PendingRequest, runtime, try_with_core, with_core};

const FRAME_CAPACITY: usize = 8192;
#[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
const SERVER_NAME: &[u8] = b"miwear-server\0";
/// Interconnect routing key: this module's own package name, matching the
/// native-app descriptor. The phone routes interconnect messages to/from a
/// connection by this name only, so it must be `com.canopus.lyra-player`, not
/// the Mi Fitness APK's own `com.xiaomi.miwear.interconnect`.
const ROUTE_NAME: &[u8] = super::native_app::PACKAGE_NAME;

#[repr(C, align(4))]
struct ConnectionStorage([u32; 3]);

#[repr(C, align(4))]
struct FrameSlot {
    length: usize,
    bytes: [u8; FRAME_CAPACITY],
}

static mut CONNECTION: ConnectionStorage = ConnectionStorage([0; 3]);
static SLOT_STATES: [AtomicU8; 2] = [AtomicU8::new(0), AtomicU8::new(0)];
static mut SLOTS: [FrameSlot; 2] = [
    FrameSlot {
        length: 0,
        bytes: [0; FRAME_CAPACITY],
    },
    FrameSlot {
        length: 0,
        bytes: [0; FRAME_CAPACITY],
    },
];
static SEND_BUSY: AtomicBool = AtomicBool::new(false);
static HANDSHAKE_PENDING: AtomicBool = AtomicBool::new(false);
static mut SEND_BYTES: [u8; FRAME_CAPACITY] = [0; FRAME_CAPACITY];
static mut SEND_MESSAGE: core::mem::MaybeUninit<InterconnectConnMessage> =
    core::mem::MaybeUninit::uninit();

pub fn register() -> Result<(), i32> {
    if runtime().connection.load(Ordering::Acquire) != 0 {
        return Ok(());
    }
    let loop_handle = unsafe { interconnect_loop() };
    if loop_handle.is_null() {
        return Err(-1);
    }
    let connection = core::ptr::addr_of_mut!(CONNECTION).cast::<c_void>();
    #[cfg(feature = "target-xiaomi-band-9-pro-3-1-175")]
    let result = unsafe {
        interconnect_connect(
            loop_handle,
            connection,
            ROUTE_NAME.as_ptr(),
            SERVER_NAME.as_ptr(),
            receive,
        )
    };
    #[cfg(not(feature = "target-xiaomi-band-9-pro-3-1-175"))]
    let result =
        unsafe { interconnect_connect(loop_handle, connection, ROUTE_NAME.as_ptr(), receive) };
    if result < 0 {
        return Err(result);
    }
    runtime()
        .connection
        .store(connection as usize, Ordering::Release);
    Ok(())
}

extern "C" fn receive(
    _context: *mut c_void,
    callback_status: i32,
    message: *const InterconnectConnMessage,
    _name: *const u8,
) {
    if callback_status < 0 {
        runtime()
            .last_error
            .store(callback_status, Ordering::Release);
    }
    if message.is_null() {
        return;
    }
    let message_type = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*message).r#type)) };
    let length =
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*message).length)) } as usize;
    let value =
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*message).value)) }.cast::<u8>();

    if message_type == CONN_MSG_TYPE_EVENT {
        if value.is_null() || length < core::mem::size_of::<i32>() {
            runtime().last_error.store(-20, Ordering::Release);
            return;
        }
        let event_code = unsafe { core::ptr::read_unaligned(value.cast::<i32>()) };
        let detail = if length >= 2 * core::mem::size_of::<i32>() {
            unsafe { core::ptr::read_unaligned(value.cast::<i32>().add(1)) }
        } else {
            0
        };
        handle_connection_event(event_code, detail);
        return;
    }
    if message_type != CONN_MSG_TYPE_DATA {
        return;
    }
    if value.is_null() || length == 0 || length > FRAME_CAPACITY {
        runtime().last_error.store(-20, Ordering::Release);
        return;
    }
    for (index, state) in SLOT_STATES.iter().enumerate() {
        if state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        let slot = unsafe {
            core::ptr::addr_of_mut!(SLOTS)
                .cast::<FrameSlot>()
                .add(index)
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                value,
                core::ptr::addr_of_mut!((*slot).bytes).cast(),
                length,
            );
            core::ptr::addr_of_mut!((*slot).length).write(length);
        }
        state.store(2, Ordering::Release);
        return;
    }
    runtime().last_error.store(-21, Ordering::Release);
}

fn handle_connection_event(code: i32, detail: i32) {
    if matches!(code, 1 | CONN_STATUS_CONNECTED) {
        runtime().connected.store(true, Ordering::Release);
        HANDSHAKE_PENDING.store(true, Ordering::Release);
        send_next();
    } else if matches!(
        code,
        CONN_STATUS_DISCONNECTED
            | CONN_STATUS_UNINSTALLED
            | CONN_STATUS_FAILED
            | CONN_STATUS_CLOSED
    ) {
        if code == CONN_STATUS_FAILED && detail < 0 {
            runtime().last_error.store(detail, Ordering::Release);
        }
        runtime().connected.store(false, Ordering::Release);
        runtime().connection.store(0, Ordering::Release);
        HANDSHAKE_PENDING.store(false, Ordering::Release);
        SEND_BUSY.store(false, Ordering::Release);
    }
}

extern "C" fn send_done(
    _context: *mut c_void,
    status: i32,
    _message: *const InterconnectConnMessage,
    _argument: *mut c_void,
) {
    if status < 0 {
        runtime().last_error.store(status, Ordering::Release);
    }
    let _ = try_with_core(|core| core.sending = None);
    SEND_BUSY.store(false, Ordering::Release);
    send_next();
}

pub fn enqueue(message: String) {
    with_core(|core| core.outbound.push_back(message));
    send_next();
}

pub fn send_next() {
    if !runtime().connected.load(Ordering::Acquire)
        || HANDSHAKE_PENDING.load(Ordering::Acquire)
        || SEND_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    let message = match try_with_core(|core| {
        let message = core.outbound.pop_front();
        core.sending = message.clone();
        message
    }) {
        Some(Some(message)) => message,
        _ => {
            SEND_BUSY.store(false, Ordering::Release);
            return;
        }
    };
    if message.len() > FRAME_CAPACITY {
        runtime().last_error.store(-22, Ordering::Release);
        let _ = try_with_core(|core| core.sending = None);
        SEND_BUSY.store(false, Ordering::Release);
        send_next();
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(
            message.as_ptr(),
            core::ptr::addr_of_mut!(SEND_BYTES).cast::<u8>(),
            message.len(),
        );
        let packet = InterconnectConnMessage {
            r#type: CONN_MSG_TYPE_DATA,
            _pad_1: [0; 3],
            length: message.len() as u32,
            _pad_8: [0; 8],
            value: core::ptr::addr_of_mut!(SEND_BYTES).cast::<c_void>(),
        };
        core::ptr::addr_of_mut!(SEND_MESSAGE)
            .cast::<InterconnectConnMessage>()
            .write(packet);
        let result = interconnect_send(
            core::ptr::addr_of_mut!(CONNECTION).cast::<c_void>(),
            core::ptr::null(),
            core::ptr::addr_of!(SEND_MESSAGE).cast::<InterconnectConnMessage>(),
            send_done,
            core::ptr::null_mut(),
        );
        if result < 0 {
            runtime().last_error.store(result, Ordering::Release);
            let _ = try_with_core(|core| core.sending = None);
            SEND_BUSY.store(false, Ordering::Release);
        }
    }
}

/// Drains copied receive slots and returns effects that must execute outside the
/// core lock (navigation and follow-up fetches).
pub fn pump() -> Vec<Effect> {
    if HANDSHAKE_PENDING.swap(false, Ordering::AcqRel) {
        with_core(|core| {
            let handshake = core.bridge.handshake(0);
            core.outbound.push_front(handshake);
        });
        send_next();
    }
    let mut effects = Vec::new();
    for (index, state) in SLOT_STATES.iter().enumerate() {
        if state
            .compare_exchange(2, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        let slot = unsafe { core::ptr::addr_of!(SLOTS).cast::<FrameSlot>().add(index) };
        let length = unsafe { core::ptr::addr_of!((*slot).length).read() };
        let bytes = unsafe {
            core::slice::from_raw_parts(core::ptr::addr_of!((*slot).bytes).cast(), length)
        };
        if let Ok(text) = core::str::from_utf8(bytes) {
            effects.extend(process(text));
        } else {
            runtime().last_error.store(-23, Ordering::Release);
        }
        state.store(0, Ordering::Release);
    }

    let deferred = with_core(|core| {
        if core.audio_ending {
            match core.app.player.stream_ended(&mut core.audio) {
                Ok(true) => {
                    core.audio_ending = false;
                    core.audio_request = None;
                }
                Ok(false) => {}
                Err(error) => {
                    core.app.error = Some(alloc::format!("audio drain failed: {error}"));
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                    core.audio_ending = false;
                    core.audio_request = None;
                }
            }
        }
        if core.deferred_stream_reply.is_some() {
            match core.app.player.flush_audio(&mut core.audio) {
                Ok(true) => core.deferred_stream_reply.take(),
                Ok(false) => None,
                Err(error) => {
                    core.app.error = Some(alloc::format!("audio write failed: {error}"));
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                    core.deferred_stream_reply = None;
                    None
                }
            }
        } else {
            None
        }
    });
    if let Some(reply) = deferred {
        enqueue(reply);
    }
    send_next();
    effects
}

fn process(text: &str) -> Vec<Effect> {
    if let Some(effects) = process_control(text) {
        return effects;
    }
    let mut outbound = Vec::new();
    let effects = with_core(|core| {
        let result = match core.bridge.ingest(text) {
            Ok(result) => result,
            Err(error) => {
                core.app.error = Some(alloc::format!("bridge error: {error:?}"));
                core.app.generation = core.app.generation.wrapping_add(1).max(1);
                return Vec::new();
            }
        };
        let mut replies = result.replies;
        let effects = match result.event {
            BridgeEvent::Response {
                id, body, headers, ..
            } => {
                let Some(pending) = core.pending.remove(&id) else {
                    return Vec::new();
                };
                if !request_is_current(&core.app, pending.kind, pending.token) {
                    return Vec::new();
                }
                let kind = pending.kind;
                let cookie = response_cookie(&headers);
                match String::from_utf8(body) {
                    Ok(body) => core.app.update(Action::ApiResponse {
                        kind,
                        body,
                        cookie,
                        now_ms: core.app.generation as u64,
                    }),
                    Err(_) => core.app.update(Action::ApiFailed {
                        kind,
                        message: "response is not UTF-8".into(),
                    }),
                }
            }
            BridgeEvent::StreamOpened { id, .. } if core.audio_request.as_deref() == Some(&id) => {
                if let Err(error) = core.app.player.stream_opened(id, &mut core.audio) {
                    core.app.error = Some(alloc::format!("audio start failed: {error}"));
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                }
                Vec::new()
            }
            BridgeEvent::StreamChunk { id, bytes }
                if core.audio_request.as_deref() == Some(&id) =>
            {
                match core.app.player.push_audio(bytes, &mut core.audio) {
                    Ok(true) => {}
                    Ok(false) => core.deferred_stream_reply = replies.pop(),
                    Err(error) => {
                        core.app.error = Some(alloc::format!("audio write failed: {error}"));
                        core.app.generation = core.app.generation.wrapping_add(1).max(1);
                        replies.clear();
                    }
                }
                Vec::new()
            }
            BridgeEvent::StreamEnded { id, bytes, .. }
                if core.audio_request.as_deref() == Some(&id) =>
            {
                let final_queued = if bytes.is_empty() {
                    true
                } else {
                    match core.app.player.push_audio(bytes, &mut core.audio) {
                        Ok(queued) => queued,
                        Err(error) => {
                            core.app.error = Some(alloc::format!("audio write failed: {error}"));
                            core.app.generation = core.app.generation.wrapping_add(1).max(1);
                            false
                        }
                    }
                };
                if final_queued {
                    match core.app.player.stream_ended(&mut core.audio) {
                        Ok(true) => core.audio_request = None,
                        Ok(false) => core.audio_ending = true,
                        Err(error) => {
                            core.app.error = Some(alloc::format!("audio drain failed: {error}"));
                            core.app.generation = core.app.generation.wrapping_add(1).max(1);
                            core.audio_request = None;
                        }
                    }
                } else {
                    core.audio_ending = true;
                }
                Vec::new()
            }
            BridgeEvent::Failed { id, message } => {
                if core.audio_request.as_deref() == Some(&id) {
                    core.audio_request = None;
                    core.app.error = Some(message);
                    core.app.generation = core.app.generation.wrapping_add(1).max(1);
                    Vec::new()
                } else if let Some(pending) = core.pending.remove(&id) {
                    if request_is_current(&core.app, pending.kind, pending.token) {
                        core.app.update(Action::ApiFailed {
                            kind: pending.kind,
                            message,
                        })
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        outbound.append(&mut replies);
        effects
    });
    for reply in outbound {
        enqueue(reply);
    }
    effects
}

fn process_control(text: &str) -> Option<Vec<Effect>> {
    let value: Value = serde_json::from_str(text).ok()?;
    match value.get("tag").and_then(Value::as_str)? {
        "lyra-search" => {
            let query = value.get("query").and_then(Value::as_str)?.trim();
            if query.is_empty() || query.len() > 128 {
                runtime().last_error.store(-24, Ordering::Release);
                return Some(Vec::new());
            }
            Some(with_core(|core| {
                core.app.update(Action::Search(query.into()))
            }))
        }
        "lyra-api-base" => {
            let base = value.get("url").and_then(Value::as_str)?.trim();
            if base.len() > 256 || !(base.starts_with("http://") || base.starts_with("https://")) {
                runtime().last_error.store(-25, Ordering::Release);
                return Some(Vec::new());
            }
            with_core(|core| {
                core.app.api_base = base.into();
                core.app.generation = core.app.generation.wrapping_add(1).max(1);
            });
            Some(Vec::new())
        }
        _ => None,
    }
}

fn request_token(app: &lyra_player_core::LyraApp, kind: RequestKind) -> u64 {
    match kind {
        RequestKind::QrKey | RequestKind::QrCreate | RequestKind::QrCheck => {
            app.qr.key.as_bytes().iter().fold(0u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01B3)
            })
        }
        RequestKind::PlaylistDetail | RequestKind::PlaylistTracks => {
            app.selected_playlist.as_ref().map_or(0, |item| item.id)
        }
        RequestKind::ArtistDetail | RequestKind::ArtistSongs | RequestKind::ArtistAlbums => {
            app.selected_artist.as_ref().map_or(0, |item| item.id)
        }
        RequestKind::SearchSongs | RequestKind::SearchArtists => {
            app.search.query.as_bytes().iter().fold(0u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01B3)
            })
        }
        RequestKind::SongUrl | RequestKind::Lyrics => {
            app.player.current.as_ref().map_or(0, |song| song.id)
        }
        _ => 0,
    }
}

fn request_is_current(app: &lyra_player_core::LyraApp, kind: RequestKind, token: u64) -> bool {
    request_token(app, kind) == token
}

fn response_cookie(headers: &BTreeMap<String, String>) -> Option<String> {
    let values = headers.get("set-cookie")?;
    let mut cookie = String::new();
    for value in values.split('\n') {
        let pair = value.trim().split(';').next().unwrap_or("").trim();
        if pair.is_empty() || !pair.contains('=') {
            continue;
        }
        if !cookie.is_empty() {
            cookie.push_str("; ");
        }
        cookie.push_str(pair);
    }
    (!cookie.is_empty()).then_some(cookie)
}

pub fn enqueue_api(kind: RequestKind, request: lyra_player_core::api::ApiRequest) {
    let message = with_core(|core| {
        let mut options = FetchOptions {
            method: request.method,
            headers: request.headers,
            body: request.body,
            ..FetchOptions::default()
        };
        options
            .headers
            .push(("Accept".into(), "application/json".into()));
        let (id, message) = core.bridge.fetch(&request.url, &options);
        let token = request_token(&core.app, kind);
        core.pending.insert(id, PendingRequest { kind, token });
        message
    });
    enqueue(message);
}

pub fn enqueue_audio(url: String) {
    let messages = with_core(|core| {
        core.audio.stop_local();
        core.audio_ending = false;
        let mut messages = Vec::new();
        if let Some(previous) = core.audio_request.take() {
            messages.push(core.bridge.cancel_stream(&previous, "replaced"));
        }
        let mut options = FetchOptions::default();
        options.raw = true;
        options.stream = true;
        let (id, message) = core.bridge.fetch(&url, &options);
        core.audio_request = Some(id);
        messages.push(message);
        messages
    });
    for message in messages {
        enqueue(message);
    }
}
