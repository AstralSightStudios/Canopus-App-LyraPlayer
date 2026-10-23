//! One import at a time: prepares the files, says hello to the module on the watch and
//! then follows the transfer with the shared sender (`lyra-transfer`), sending frames
//! over CanopusConnect and feeding it what the watch answers.

use futures::stream::{FuturesUnordered, StreamExt};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use lyra_protocol::{Asset, Incoming, MIN_VERSION, Reply, Request, Status, TrackMeta};
use lyra_transfer::{Failure, Sender, Source, State, specs};

use crate::{artwork::Palette, astrobox::psys_host_v4::timer, connect, lyrics, state};

/// The watch has to answer within this time or the import is given up.
const STALL: Duration = Duration::from_secs(45);
const HELLO_REQ: u16 = 1;
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
}

struct FileSource {
    file: File,
    len: u32,
}

impl Source for FileSource {
    fn len(&self) -> u32 {
        self.len
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String> {
        self.file
            .seek(SeekFrom::Start(u64::from(offset)))
            .and_then(|_| self.file.read(buf))
            .map_err(|error| error.to_string())
    }
}

struct Active {
    addr: String,
    sender: Sender,
    /// The module has answered `HELLO`; until then nothing else is sent.
    greeted: bool,
    total: u64,
    started: Instant,
    last_progress: Instant,
    /// Another call is already sending frames; others only feed events in.
    pumping: bool,
    speed_mark: (Instant, u64),
}

static ACTIVE: OnceLock<Mutex<Option<Active>>> = OnceLock::new();

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
    Ok(Box::new(FileSource { file, len }))
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
            _ => open(&asset.path)?,
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
    if lock().is_some() {
        return Err("已有导入任务正在运行".to_string());
    }
    let sender = prepare(&request)?;
    let total = sender.total_bytes();
    let addr = request.addr;
    {
        let mut guard = lock();
        if guard.is_some() {
            return Err("已有导入任务正在运行".to_string());
        }
        *guard = Some(Active {
            addr: addr.clone(),
            sender,
            greeted: false,
            total,
            started: Instant::now(),
            last_progress: Instant::now(),
            pumping: false,
            speed_mark: (Instant::now(), 0),
        });
    }
    state::with_state(|state| {
        state.active = true;
        state.sent = 0;
        state.total = total;
        state.speed_bytes_per_second = 0;
        state.status = "正在连接手表上的 Lyra Player…".to_string();
    });
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
        finish_error(&message);
        return Err(message);
    }
    arm_watchdog();
    Ok(())
}

fn arm_watchdog() {
    timer::set_timeout(STALL.as_millis() as u64, STALL_TIMER_PAYLOAD);
}

/// A frame the Lyra Player module sent; `true` when it belonged to the running import.
pub async fn on_frame(addr: &str, frame: &[u8]) -> bool {
    let Ok(incoming) = Incoming::decode(frame) else {
        return false;
    };
    let mut hello_failed = None;
    let mut start_pumping = false;
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
            start_pumping = true;
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
                    start_pumping = true;
                } else {
                    hello_failed = Some("手表上的 Lyra Player 版本过旧，请先更新模块。".to_string());
                }
            }
        }
    }
    if let Some(error) = hello_failed {
        *lock() = None;
        finish_error(&error);
        return true;
    }
    if start_pumping {
        pump().await;
    }
    true
}

/// Frames kept on their way to the watch at once. The host lets this many CanopusConnect messages
/// be in flight per device (it keeps the last slot of the watch's queue for the Manager), so the
/// round trip of one frame no longer sets the pace: over a real Bluetooth link a frame takes
/// several connection intervals to be answered, and one answered frame at a time is what held the
/// transfer under 10 KiB/s.
const IN_FLIGHT: usize = 3;

/// Sends every frame the sender has ready, [`IN_FLIGHT`] at a time. Only one call does this at a
/// time; events that arrive meanwhile are fed in by `on_frame` and picked up by the running loop.
async fn pump() {
    {
        let mut guard = lock();
        let Some(active) = guard.as_mut() else { return };
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
                let Some(active) = guard.as_mut() else { return };
                if !active.greeted {
                    active.pumping = false;
                    return;
                }
                publish_progress(active.total, &mut active.speed_mark, &active.sender);
                match active.sender.state() {
                    State::Done => {
                        active.pumping = false;
                        let seconds = active.started.elapsed().as_secs_f64().max(0.001);
                        let total = active.total;
                        *guard = None;
                        drop(guard);
                        state::with_state(|state| {
                            state.active = false;
                            state.sent = total;
                            state.speed_bytes_per_second = (total as f64 / seconds) as u64;
                            state.status = "导入完成，歌曲已出现在手表的 Lyra Player 中。".to_string();
                        });
                        crate::library::refresh().await;
                        return;
                    }
                    State::Failed(failure) => {
                        *guard = None;
                        drop(guard);
                        finish_error(&failure_text(failure));
                        return;
                    }
                    _ => {}
                }
                active.sender.poll().map(|frame| (active.addr.clone(), frame))
            };
            match next {
                Some((addr, frame)) => sends.push(async move { connect::send(&addr, &frame).await }),
                None => break,
            }
        }
        if sends.is_empty() {
            // Nothing to send and nothing on its way: the watch's next frame starts the loop again.
            if let Some(active) = lock().as_mut() {
                active.pumping = false;
            }
            return;
        }
        if let Some(Err(error)) = sends.next().await {
            *lock() = None;
            finish_error(&format!("发送失败：{error}"));
            return;
        }
    }
}

fn publish_progress(total: u64, mark: &mut (Instant, u64), sender: &Sender) {
    let confirmed = sender.confirmed_bytes();
    let elapsed = mark.0.elapsed();
    let speed = if elapsed >= Duration::from_secs(2) {
        let speed = confirmed.saturating_sub(mark.1) as f64 / elapsed.as_secs_f64();
        *mark = (Instant::now(), confirmed);
        Some(speed as u64)
    } else {
        None
    };
    state::with_state(|state| {
        state.sent = confirmed;
        state.total = total;
        if let Some(speed) = speed {
            state.speed_bytes_per_second = speed;
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
    let stalled = {
        let guard = lock();
        match guard.as_ref() {
            None => return,
            Some(active) => active.last_progress.elapsed() >= STALL,
        }
    };
    if stalled {
        *lock() = None;
        finish_error("手表长时间没有响应，导入已中止。");
    } else {
        arm_watchdog();
    }
}

pub async fn cancel() {
    let target = lock().take().map(|active| active.addr);
    if let Some(addr) = target {
        if let Ok(frame) = (Request::Abort { req: 2 }).encode() {
            let _ = connect::send(&addr, &frame).await;
        }
    }
    state::with_state(|state| {
        state.active = false;
        state.status = "已取消导入。".to_string();
    });
}
