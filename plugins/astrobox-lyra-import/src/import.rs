//! One import at a time: prepares the files, says hello to the module on the watch and
//! then follows the transfer with the shared sender (`lyra-transfer`), sending frames
//! over CanopusConnect and feeding it what the watch answers.

use futures::stream::{FuturesUnordered, StreamExt};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use lyra_protocol::{Asset, Incoming, MIN_VERSION, Reply, Request, Status, TrackMeta};
use lyra_transfer::{Failure, Sender, Source, State, specs};

use crate::{artwork::Palette, connect, lyrics, state};
#[cfg(not(test))]
use crate::astrobox::psys_host_v4::timer;

/// The watch has to answer within this time or the import is given up.
const STALL: Duration = Duration::from_secs(45);
const HELLO_REQ: u16 = 1;
const ABORT_BLOCKED_ERROR: &str = "正在等待取消操作确认，请稍后重试";
pub const STALL_TIMER_PAYLOAD: &str = "lyra-import-stall";

#[derive(Clone, Debug)]
pub struct ImportAsset {
    pub kind: Asset,
    pub path: String,
    /// For lyrics: "lrc" or "json".
    pub format: &'static str,
}

impl ImportAsset {
    pub fn audio(path: String) -> Self {
        Self {
            kind: Asset::Audio,
            path,
            format: "mp3",
        }
    }

    pub fn cover(path: String) -> Self {
        Self {
            kind: Asset::Cover,
            path,
            format: "bin",
        }
    }

    pub fn lyrics(path: String, format: &'static str) -> Self {
        Self {
            kind: Asset::Lyrics,
            path,
            format,
        }
    }
}

#[derive(Debug)]
pub struct TempDirectory(pub std::path::PathBuf);

impl Drop for TempDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            tracing::warn!(path = ?self.0, %error, "failed to remove import directory");
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImportRequest {
    pub addr: String,
    pub track_id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u32,
    pub palette: Palette,
    pub assets: Vec<ImportAsset>,
    pub temp_directory: Option<Arc<TempDirectory>>,
}

/// `len` bytes of a file from `start` on.
struct FileSource {
    file: File,
    start: u64,
    len: u32,
}

impl Source for FileSource {
    fn len(&self) -> u32 {
        self.len
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String> {
        let want = buf.len().min(self.len.saturating_sub(offset) as usize);
        self.file
            .seek(SeekFrom::Start(self.start + u64::from(offset)))
            .and_then(|_| self.file.read(&mut buf[..want]))
            .map_err(|error| error.to_string())
    }
}

struct Active {
    id: u64,
    addr: String,
    sender: Sender,
    _temp_directory: Option<Arc<TempDirectory>>,
    /// The module has answered `HELLO`; until then nothing else is sent.
    greeted: bool,
    total: u64,
    started: Instant,
    last_progress: Instant,
    /// Another call is already sending frames; others only feed events in.
    pumping: bool,
    speed_mark: (Instant, u64),
    /// Time from handing a frame to the host until the watch took it, summed over the frames
    /// answered since the speed was last published, and their count.
    round_trips: (Duration, u32),
}

static ACTIVE: OnceLock<Mutex<Option<Active>>> = OnceLock::new();
static ABORT: OnceLock<Mutex<Option<AbortBarrier>>> = OnceLock::new();

struct AbortBarrier {
    addr: String,
    req: u16,
    sending: bool,
    acknowledged: bool,
    started: Instant,
}

fn abort_barrier() -> &'static Mutex<Option<AbortBarrier>> {
    ABORT.get_or_init(|| Mutex::new(None))
}

fn lock_abort() -> std::sync::MutexGuard<'static, Option<AbortBarrier>> {
    abort_barrier().lock().unwrap_or_else(|item| item.into_inner())
}
static NEXT_ACTIVE_ID: AtomicU64 = AtomicU64::new(1);
// Do not reuse a request number: a late ACK must not clear a later barrier.
static NEXT_ABORT_REQ: AtomicU64 = AtomicU64::new(2);

fn next_abort_req() -> Option<u16> {
    NEXT_ABORT_REQ.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        (value <= u64::from(u16::MAX)).then_some(value + 1)
    }).ok().and_then(|value| u16::try_from(value).ok())
}

pub fn abort_blocked() -> bool {
    lock_abort().is_some()
}

fn take_matching<T>(slot: &mut Option<T>, predicate: impl FnOnce(&T) -> bool) -> bool {
    if slot.as_ref().is_some_and(predicate) {
        *slot = None;
        true
    } else {
        false
    }
}

fn active() -> &'static Mutex<Option<Active>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}

fn lock() -> std::sync::MutexGuard<'static, Option<Active>> {
    active().lock().unwrap_or_else(|item| item.into_inner())
}

/// Checks a picked file by its content rather than its name: on Android the
/// name is the last segment of a `content://` URI and has no extension.
pub fn inspect_file(path: &str, name: &str, kind: &str) -> Result<state::SelectedFile, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("cannot stat file: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("selected file is empty".to_string());
    }
    let limit = match kind {
        "audio" => u64::from(lyra_protocol::MAX_AUDIO_BYTES),
        "cover" => 4 * 1024 * 1024,
        "lyrics" => 2 * 1024 * 1024,
        _ => return Err("unknown asset kind".to_string()),
    };
    if metadata.len() > limit {
        return Err(format!("selected {kind} file exceeds import limit"));
    }
    let unreadable = |error: std::io::Error| format!("无法读取所选文件：{error}");
    let (format, duration_ms) = match kind {
        "audio" => {
            if !crate::sniff::is_mp3(Path::new(path)).map_err(unreadable)? {
                return Err("所选文件不是 MP3 音频".to_string());
            }
            let duration_ms = mp3_duration::from_path(path)
                .ok()
                .map(|duration| duration.as_millis().min(u128::from(u32::MAX)) as u32)
                .unwrap_or(0);
            ("mp3", duration_ms)
        }
        "cover" => match crate::sniff::image_format(Path::new(path)).map_err(unreadable)? {
            Some(crate::sniff::ImageFormat::Jpeg) => ("jpeg", 0),
            Some(crate::sniff::ImageFormat::Png) => ("png", 0),
            None => return Err("封面必须是 JPEG 或 PNG".to_string()),
        },
        _ => {
            let bytes = fs::read(path).map_err(unreadable)?;
            match crate::sniff::lyrics_format(&bytes) {
                Some(format) => (format.as_str(), 0),
                None => return Err("歌词必须是 LRC 或 JSON 文本".to_string()),
            }
        }
    };
    Ok(state::SelectedFile {
        name: name.to_string(),
        path: path.to_string(),
        size: metadata.len(),
        duration_ms,
        format,
    })
}

fn open(path: &str) -> Result<Box<dyn Source>, String> {
    let file = File::open(path).map_err(|error| format!("无法打开 {path}：{error}"))?;
    let len = file
        .metadata()
        .map_err(|error| format!("无法读取 {path}：{error}"))?
        .len();
    let len = u32::try_from(len).map_err(|_| "文件过大".to_string())?;
    Ok(Box::new(FileSource {
        file,
        start: 0,
        len,
    }))
}

/// The audio frames of an MP3 without its tags (see [`crate::id3::audio_range`]).
fn open_audio(path: &str) -> Result<Box<dyn Source>, String> {
    let mut file = File::open(path).map_err(|error| format!("无法打开 {path}：{error}"))?;
    let (start, end) =
        crate::id3::audio_range(&mut file).map_err(|error| format!("无法读取 {path}：{error}"))?;
    let len = u32::try_from(end - start).map_err(|_| "文件过大".to_string())?;
    Ok(Box::new(FileSource { file, start, len }))
}

/// The sender for a request: files opened, lyrics normalised to LRC, sizes and CRCs
/// computed.
fn prepare(request: &ImportRequest) -> Result<Sender, String> {
    if request.addr.is_empty() {
        return Err("请选择已连接设备".to_string());
    }
    let mut sources: Vec<(Asset, Box<dyn Source>)> = Vec::new();
    for asset in &request.assets {
        let source: Box<dyn Source> = match asset.kind {
            Asset::Lyrics => {
                let bytes = fs::read(&asset.path).map_err(|error| format!("无法读取歌词：{error}"))?;
                match lyrics::to_lrc(&bytes, asset.format) {
                    Ok(lrc) => Box::new(lrc),
                    Err(error) => {
                        tracing::warn!(%error, "lyrics skipped");
                        continue;
                    }
                }
            }
            Asset::Audio => open_audio(&asset.path)?,
            Asset::Cover => open(&asset.path)?,
        };
        sources.push((asset.kind, source));
    }
    if !sources.iter().any(|(kind, _)| *kind == Asset::Audio) {
        return Err("缺少音频文件".to_string());
    }
    sources.sort_by_key(|(kind, _)| *kind as u8);
    let assets = specs(&mut sources)?;
    let meta = TrackMeta {
        id: request.track_id,
        duration_ms: request.duration_ms,
        name: request.name.clone(),
        artists: request.artists.clone(),
        album: request.album.clone(),
        palette_top: request.palette.top,
        palette_bottom: request.palette.bottom,
        assets,
    };
    Ok(Sender::new(meta, sources.into_iter().map(|(_, source)| source).collect()))
}

pub async fn start(request: ImportRequest) -> Result<(), String> {
    let abort_guard = lock_abort();
    if abort_guard.is_some() {
        return Err(ABORT_BLOCKED_ERROR.to_string());
    }
    if lock().is_some() {
        return Err("已有导入任务正在运行".to_string());
    }
    let sender = prepare(&request)?;
    let total = sender.total_bytes();
    let addr = request.addr;
    let id = NEXT_ACTIVE_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut guard = lock();
        if guard.is_some() {
            return Err("已有导入任务正在运行".to_string());
        }
        *guard = Some(Active {
            id,
            addr: addr.clone(),
            sender,
            _temp_directory: request.temp_directory,
            greeted: false,
            total,
            started: Instant::now(),
            last_progress: Instant::now(),
            pumping: false,
            speed_mark: (Instant::now(), 0),
            round_trips: (Duration::ZERO, 0),
        });
        state::with_state(|state| {
            state.active = true;
            state.sent = 0;
            state.total = total;
            state.speed_bytes_per_second = 0;
            state.round_trip_ms = 0;
            state.status = "正在连接手表上的 Lyra Player…".to_string();
        });
    }
    drop(abort_guard);
    let hello = Request::Hello {
        req: HELLO_REQ,
        version: MIN_VERSION,
    }
    .encode()
    .map_err(|_| "内部错误".to_string());
    let result = match hello {
        Ok(frame) => match connect::ensure_listening(&addr).await {
            Ok(()) => connect::send(&addr, &frame).await,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        let message = format!("无法联系手表上的 Lyra Player（请确认模块已启用、手表已连接）：{error}");
        finish_active_error(id, &message);
        return Err(message);
    }
    arm_watchdog();
    Ok(())
}

fn arm_watchdog() {
    #[cfg(not(test))]
    timer::set_timeout(STALL.as_millis() as u64, STALL_TIMER_PAYLOAD);
}

/// A frame the Lyra Player module sent; `true` when it belonged to the running import.
pub async fn on_frame(addr: &str, frame: &[u8]) -> bool {
    let Ok(incoming) = Incoming::decode(frame) else {
        return false;
    };
    if let Incoming::Reply(Reply::Ack { op, req, status, .. }) = &incoming {
        if *op == lyra_protocol::op::ABORT {
            let mut barrier = lock_abort();
            if let Some(pending) = barrier.as_mut().filter(|pending| pending.req != 0 && pending.addr == addr && pending.req == *req) {
                if *status == Status::Ok {
                    pending.acknowledged = true;
                    if !pending.sending {
                        *barrier = None;
                        state::with_state(|state| state.status = "手表已确认取消。".into());
                    }
                } else {
                    state::with_state(|state| state.status = "手表拒绝取消，已暂停后续导入，请重试取消。".into());
                }
                return true;
            }
        }
    }
    let mut hello_failed = None;
    let mut start_pumping = None;
    {
        let mut guard = lock();
        let Some(active) = guard.as_mut() else {
            return false;
        };
        if active.addr != addr {
            return false;
        }
        active.last_progress = Instant::now();
        if active.greeted {
            active.sender.on_frame(frame);
            start_pumping = Some(active.id);
        } else if let Incoming::Reply(Reply::Hello {
            req,
            status,
            version,
            ..
        }) = incoming
        {
            if req == HELLO_REQ {
                if status == Status::Ok && version >= 1 {
                    active.greeted = true;
                    start_pumping = Some(active.id);
                } else {
                    hello_failed = Some((active.id, "手表上的 Lyra Player 版本过旧，请先更新模块。".to_string()));
                }
            }
        }
    }
    if let Some((id, error)) = hello_failed {
        finish_active_error(id, &error);
        return true;
    }
    if let Some(id) = start_pumping {
        pump(id).await;
    }
    true
}

/// Frames kept on their way to the watch at once. Every frame waits a Bluetooth round trip for its
/// answer, so this many frames per round trip is the speed of an import. The host decides how many
/// really are in flight: as many as the watch's request ring holds less the slot it keeps for the
/// Manager -- 15 on a Supervisor with the deep ring, 3 on older ones -- and holds the rest back,
/// in order. Asking for more than the host allows only queues them there.
const IN_FLIGHT: usize = 15;

/// Sends every frame the sender has ready, [`IN_FLIGHT`] at a time. Only one call does this at a
/// time; events that arrive meanwhile are fed in by `on_frame` and picked up by the running loop.
async fn pump(id: u64) {
    {
        let mut guard = lock();
        let Some(active) = guard.as_mut().filter(|active| active.id == id) else { return };
        if active.pumping {
            return;
        }
        active.pumping = true;
    }
    let mut sends = FuturesUnordered::new();
    loop {
        // Top the window up with whatever the sender has ready. The frames are started in the order
        // the sender made them, and the host queues them to the watch in that order.
        while sends.len() < IN_FLIGHT {
            let next = {
                let mut guard = lock();
                let Some(active) = guard.as_mut().filter(|active| active.id == id) else { return };
                if !active.greeted {
                    active.pumping = false;
                    return;
                }
                publish_progress(
                    active.total,
                    &mut active.speed_mark,
                    &mut active.round_trips,
                    &active.sender,
                );
                match active.sender.state() {
                    State::Done => {
                        active.pumping = false;
                        let seconds = active.started.elapsed().as_secs_f64().max(0.001);
                        let total = active.total;
                        *guard = None;
                        state::with_state(|state| {
                            state.active = false;
                            state.sent = total;
                            state.speed_bytes_per_second = (total as f64 / seconds) as u64;
                            state.status = "导入完成，歌曲已出现在手表的 Lyra Player 中。".to_string();
                        });
                        drop(guard);
                        crate::library::refresh().await;
                        return;
                    }
                    State::Failed(failure) => {
                        *guard = None;
                        finish_error(&failure_text(failure));
                        return;
                    }
                    _ => {}
                }
                active.sender.poll().map(|frame| (active.addr.clone(), frame))
            };
            match next {
                Some((addr, frame)) => sends.push(async move {
                    let started = Instant::now();
                    let result = connect::send(&addr, &frame).await;
                    (result, started.elapsed())
                }),
                None => break,
            }
        }
        if sends.is_empty() {
            // Nothing to send and nothing on its way: the watch's next frame starts the loop again.
            if let Some(active) = lock().as_mut().filter(|active| active.id == id) {
                active.pumping = false;
            }
            return;
        }
        match sends.next().await {
            Some((result, elapsed)) => {
                if !handle_send_result(id, result, elapsed) {
                    return;
                }
            }
            None => {}
        }
    }
}

fn handle_send_result(id: u64, result: Result<(), String>, elapsed: Duration) -> bool {
    let mut guard = lock();
    let Some(active) = guard.as_mut().filter(|active| active.id == id) else {
        return false;
    };
    match result {
        Ok(()) => {
            active.round_trips.0 += elapsed;
            active.round_trips.1 += 1;
            true
        }
        Err(error) => {
            *guard = None;
            finish_error(&format!("发送失败：{error}"));
            false
        }
    }
}

fn finish_active_error(id: u64, error: &str) {
    let mut guard = lock();
    if take_matching(&mut *guard, |active| active.id == id) {
        finish_error(error);
    }
}

/// Publishes the confirmed bytes, and every two seconds the speed and the average time a frame
/// took to be answered: with a fixed number of frames in flight, that round trip is what sets
/// the speed, so a slow import can be told apart from a slow link.
fn publish_progress(
    total: u64,
    mark: &mut (Instant, u64),
    round_trips: &mut (Duration, u32),
    sender: &Sender,
) {
    let confirmed = sender.confirmed_bytes();
    let elapsed = mark.0.elapsed();
    let speed = if elapsed >= Duration::from_secs(2) {
        let speed = confirmed.saturating_sub(mark.1) as f64 / elapsed.as_secs_f64();
        *mark = (Instant::now(), confirmed);
        let (sum, count) = std::mem::replace(round_trips, (Duration::ZERO, 0));
        let round_trip_ms = sum.as_millis() as u64 / u64::from(count.max(1));
        Some((speed as u64, round_trip_ms as u32))
    } else {
        None
    };
    state::with_state(|state| {
        state.sent = confirmed;
        state.total = total;
        if let Some((speed, round_trip_ms)) = speed {
            state.speed_bytes_per_second = speed;
            state.round_trip_ms = round_trip_ms;
        }
        state.status = match sender.state() {
            State::Streaming(_) | State::AwaitAssetEnd(_) => "正在传输到手表…".to_string(),
            State::AwaitCommit => "正在保存歌曲…".to_string(),
            _ => state.status.clone(),
        };
    });
}

fn failure_text(failure: Failure) -> String {
    match failure {
        Failure::Refused(Status::NoSpace) => "手表存储空间不足。".to_string(),
        Failure::Refused(Status::Crc) => "数据校验失败，请重试。".to_string(),
        Failure::Refused(Status::Io) => "手表写入文件失败。".to_string(),
        Failure::Refused(Status::Busy) => "手表正忙，请稍后重试。".to_string(),
        Failure::Refused(Status::BadRequest) => "手表拒绝了这首歌曲（文件或信息无效）。".to_string(),
        Failure::Refused(status) => format!("手表拒绝了导入：{status:?}"),
        Failure::Source => "读取本地文件失败。".to_string(),
        Failure::Protocol => "手表的回复与导入流程不符，请更新插件与模块。".to_string(),
    }
}

fn finish_error(error: &str) {
    state::with_state(|state| {
        state.active = false;
        state.status = error.to_string();
    });
}

/// The stall timer fired: give up when the watch has been silent for too long, otherwise
/// keep watching.
pub fn check_stall() {
    check_stall_with(|| {});
}

fn check_stall_with(after_active_lock: impl FnOnce()) {
    {
        let barrier = lock_abort();
        if let Some(pending) = barrier.as_ref() {
            if pending.started.elapsed() >= STALL {
                state::with_state(|state| state.status = if pending.sending {
                    "取消请求仍在发送，请等待发送结束。".into()
                } else {
                    "等待手表取消确认超时，后续导入已暂停，请重试取消。".into()
                });
            } else {
                arm_watchdog();
            }
            return;
        }
    }
    let mut guard = lock();
    after_active_lock();
    match guard.as_ref() {
        None => {},
        Some(active) if active.last_progress.elapsed() >= STALL => {
            *guard = None;
            finish_error("手表长时间没有响应，导入已中止。");
        }
        Some(_) => arm_watchdog(),
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    static TEST_ACTIVE: Mutex<()> = Mutex::new(());

    fn test_active(id: u64) -> Active {
        let meta = TrackMeta {
            id, duration_ms: 0, name: String::new(), artists: vec![],
            album: String::new(), palette_top: 0, palette_bottom: 0, assets: vec![],
        };
        Active {
            id, addr: "watch".into(), sender: Sender::new(meta, vec![]),
            _temp_directory: None, greeted: true, total: 0,
            started: Instant::now(), last_progress: Instant::now(), pumping: true,
            speed_mark: (Instant::now(), 0), round_trips: (Duration::ZERO, 0),
        }
    }

    #[test]
    fn failed_start_only_removes_its_own_active_task() {
        let mut slot = Some(12);
        assert!(!take_matching(&mut slot, |id| *id == 11));
        assert_eq!(slot, Some(12));
        assert!(take_matching(&mut slot, |id| *id == 12));
        assert!(slot.is_none());
    }

    #[test]
    fn stale_send_success_and_failure_leave_replacement_untouched() {
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        *lock() = Some(test_active(2));
        assert!(!handle_send_result(1, Ok(()), Duration::from_secs(1)));
        assert!(!handle_send_result(1, Err("old failure".into()), Duration::ZERO));
        let mut guard = lock();
        let active = guard.as_ref().unwrap();
        assert_eq!(active.id, 2);
        assert!(active.pumping);
        assert_eq!(active.round_trips, (Duration::ZERO, 0));
        *guard = None;
    }

    #[test]
    fn cancel_abort_completion_does_not_overwrite_replacement() {
        use std::{future::Future, pin::pin, task::{Context, Poll, Waker}};
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        *lock() = Some(test_active(1));
        state::with_state(|state| { state.active = true; state.status = "old".into(); });
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        let mut future = pin!(cancel_with(|_, _| async move {
            let _ = rx.await;
            Ok(())
        }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        assert!(!state::snapshot().active);
        assert!(lock_abort().is_some());
        let request = ImportRequest { addr: "watch".into(), track_id: 2, name: "new".into(), artists: vec![], album: String::new(), duration_ms: 0, palette: Palette::default(), assets: vec![], temp_directory: None };
        let mut start_future = pin!(start(request));
        assert!(matches!(start_future.as_mut().poll(&mut context), Poll::Ready(Err(_))));
        tx.send(()).unwrap();
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Ready(())));
        assert!(lock_abort().is_some());
        *lock_abort() = None;
        state::with_state(|state| { state.active = false; state.status.clear(); });
    }

    #[test]
    fn abort_ack_before_send_completion_keeps_barrier_until_send_finishes() {
        use std::{future::Future, pin::pin, task::{Context, Poll, Waker}};
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        *lock() = Some(test_active(1));
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        let mut future = pin!(cancel_with(|_, _| async move { let _ = rx.await; Ok(()) }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        let req = lock_abort().as_ref().unwrap().req;
        let ack = Reply::Ack { op: lyra_protocol::op::ABORT, req, status: Status::Ok, value: 0 }.encode().unwrap();
        let mut frame = pin!(on_frame("watch", &ack));
        assert!(matches!(frame.as_mut().poll(&mut context), Poll::Ready(true)));
        assert!(lock_abort().is_some());
        tx.send(()).unwrap();
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Ready(())));
        assert!(lock_abort().is_none());
        state::with_state(|state| { state.active = false; state.status.clear(); });
    }

    #[test]
    fn failed_abort_keeps_barrier_and_queue() {
        use std::{future::Future, pin::pin, task::{Context, Poll, Waker}};
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        *lock() = Some(test_active(1));
        let mut future = pin!(cancel_with(|_, _| async { Err("offline".into()) }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Ready(())));
        assert!(lock_abort().is_some());
        assert!(state::snapshot().status.contains("offline"));
        *lock_abort() = None;
        state::with_state(|state| { state.active = false; state.status.clear(); });
    }

    #[test]
    fn retry_ignores_old_and_wrong_device_ack() {
        use std::{future::Future, pin::pin, task::{Context, Poll, Waker}};
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        *lock() = Some(test_active(1));
        let mut context = Context::from_waker(Waker::noop());
        let mut first = pin!(cancel_with(|_, _| async { Err("offline".into()) }));
        assert!(matches!(first.as_mut().poll(&mut context), Poll::Ready(())));
        let old = lock_abort().as_ref().unwrap().req;
        let mut retry = pin!(cancel_with(|_, _| async { Ok(()) }));
        assert!(matches!(retry.as_mut().poll(&mut context), Poll::Ready(())));
        let current = lock_abort().as_ref().unwrap().req;
        assert_ne!(old, current);
        let ack = |req| Reply::Ack { op: lyra_protocol::op::ABORT, req, status: Status::Ok, value: 0 }.encode().unwrap();
        let old_ack = ack(old);
        let mut frame = pin!(on_frame("watch", &old_ack));
        assert!(matches!(frame.as_mut().poll(&mut context), Poll::Ready(false)));
        let current_ack = ack(current);
        let mut frame = pin!(on_frame("other", &current_ack));
        assert!(matches!(frame.as_mut().poll(&mut context), Poll::Ready(false)));
        assert!(lock_abort().is_some());
        let mut frame = pin!(on_frame("watch", &current_ack));
        assert!(matches!(frame.as_mut().poll(&mut context), Poll::Ready(true)));
        assert!(lock_abort().is_none());
        state::with_state(|state| { state.active = false; state.status.clear(); });
    }

    #[test]
    fn retry_waits_for_previous_send_to_finish() {
        use std::{future::Future, pin::pin, task::{Context, Poll, Waker}};
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        *lock() = Some(test_active(1));
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        let mut first = pin!(cancel_with(|_, _| async move { let _ = rx.await; Ok(()) }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(first.as_mut().poll(&mut context), Poll::Pending));
        let req = lock_abort().as_ref().unwrap().req;
        let mut retry = pin!(cancel_with(|_, _| async { panic!("concurrent retry sent") }));
        assert!(matches!(retry.as_mut().poll(&mut context), Poll::Ready(())));
        assert_eq!(lock_abort().as_ref().unwrap().req, req);
        tx.send(()).unwrap();
        assert!(matches!(first.as_mut().poll(&mut context), Poll::Ready(())));
        *lock_abort() = None;
        state::with_state(|state| { state.active = false; state.status.clear(); });
    }

    #[test]
    fn stall_check_cannot_remove_a_replacement() {
        use std::sync::mpsc;
        let _test_guard = TEST_ACTIVE.lock().unwrap();
        let mut old = test_active(1);
        old.last_progress = Instant::now() - STALL - Duration::from_secs(1);
        *lock() = Some(old);
        state::with_state(|state| { state.active = true; state.status = "old".into(); });
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let watchdog = std::thread::spawn(move || check_stall_with(|| {
            entered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
        }));
        entered_rx.recv().unwrap();
        assert!(active().try_lock().is_err());
        let replacement = std::thread::spawn(|| {
            let mut guard = lock();
            *guard = Some(test_active(2));
            state::with_state(|state| { state.active = true; state.status = "replacement".into(); });
        });
        resume_tx.send(()).unwrap();
        watchdog.join().unwrap();
        replacement.join().unwrap();
        assert_eq!(lock().as_ref().unwrap().id, 2);
        let snapshot = state::snapshot();
        assert!(snapshot.active);
        assert_eq!(snapshot.status, "replacement");
        *lock() = None;
        state::with_state(|state| { state.active = false; state.status.clear(); });
    }

    #[test]
    fn temporary_directory_lives_until_last_owner_drops() {
        let path = crate::artwork::unique_output_directory("queue-cleanup-test");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("audio.mp3"), b"test").unwrap();
        let owner = Arc::new(TempDirectory(path.clone()));
        let active = owner.clone();
        drop(owner);
        assert!(path.exists());
        drop(active);
        assert!(!path.exists());
    }
}

pub async fn cancel() {
    cancel_with(|addr, frame| async move {
        connect::ensure_listening(&addr).await?;
        connect::send(&addr, &frame).await
    }).await;
}

async fn cancel_with<F, Fut>(send: F)
where
    F: FnOnce(String, Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let (addr, req) = {
        let mut barrier = lock_abort();
        let mut guard = lock();
        if let Some(active) = guard.take() {
            *barrier = Some(AbortBarrier {
                addr: active.addr, req: 0, sending: false,
                acknowledged: false, started: Instant::now(),
            });
            state::with_state(|state| state.active = false);
        }
        let Some(pending) = barrier.as_mut() else { return };
        if pending.sending { return; }
        let Some(req) = next_abort_req() else {
            state::with_state(|state| state.status = "取消请求编号已耗尽，后续导入保持暂停；请联系开发者。".into());
            return;
        };
        pending.req = req;
        pending.sending = true;
        pending.acknowledged = false;
        pending.started = Instant::now();
        state::with_state(|state| state.status = "正在等待手表确认取消…".into());
        (pending.addr.clone(), req)
    };
    arm_watchdog();
    let result = match (Request::Abort { req }).encode() {
        Ok(frame) => send(addr.clone(), frame).await,
        Err(_) => Err("无法编码取消请求".into()),
    };
    let mut barrier = lock_abort();
    if let Some(pending) = barrier.as_mut().filter(|pending| pending.addr == addr && pending.req == req) {
        pending.sending = false;
        if pending.acknowledged {
            *barrier = None;
            state::with_state(|state| state.status = "手表已确认取消。".into());
        } else if let Err(error) = result {
            state::with_state(|state| state.status = format!("取消发送失败，后续导入已暂停，请重试取消：{error}"));
        }
    }
}
