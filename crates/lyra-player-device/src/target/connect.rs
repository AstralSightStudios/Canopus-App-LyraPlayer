//! The importer endpoint: the AstroBox plugin talks to the module over
//! CanopusConnect (ADR-CAN-009), one binary frame per message.
//!
//! Frames arrive in `on_message` on the Supervisor's owner thread and are
//! handled there, synchronously, by the portable [`Importer`]; its replies go
//! into an outbox that is flushed at once and, when the Supervisor's two-slot
//! send queue is full, again by a short background timer (which also runs with
//! the screen off).

use alloc::{collections::VecDeque, format, vec::Vec};
use core::ffi::{c_char, c_void};
use core::sync::atomic::Ordering;

use canopus_abi::{CONNECT_BINARY, CanopusContext, ContextV1, FS_CREATE, FS_TRUNC, FS_WRITE};
use canopus_runtime::{
    connect::{self, Delivery},
    fs,
};
use lyra_player_core::{
    Action, Song,
    import::{Change, ImportStore, Importer, Platform},
    persistence::{LIBRARY_FILE, Library, encode_library},
};
use lyra_protocol::{Asset, Control, FRAME_MAX, Request, Status};

use super::{execute_effects, runtime, storage};
use crate::module::context;

const ERR_BUSY: i32 = -16;
const ERR_NOSPACE: i32 = -28;
/// Imported bytes are written in blocks of this size. They arrive 464 at a time, and each small
/// append costs the flash file system nearly as much as a whole block (the handler runs on the UI
/// owner and holds up the reply to the phone meanwhile).
const WRITE_BLOCK: usize = 4 * 1024;
const OUTBOX_MAX: usize = 48;
const FLUSH_MS: u32 = 20;
/// A transfer that has not heard from the phone for this long is dropped.
const STALL_MS: u32 = 60_000;

/// `canopus_services_v1.group_version[CANOPUS_SVC_CONNECT]`: the Supervisor delivers frames that
/// ask for no reply from group version 2. The runtime crate has no accessor for it, so the table
/// is read directly: `u32 struct_size` followed by one version byte per service group.
fn supports_stream(ctx: *const CanopusContext) -> bool {
    const CONNECT_GROUP: usize = 13;
    const STREAM_VERSION: u8 = 2;
    let services = unsafe { (*ctx).services }.cast::<u8>();
    if services.is_null() {
        return false;
    }
    let size = unsafe { services.cast::<u32>().read_unaligned() } as usize;
    size > 4 + CONNECT_GROUP && unsafe { *services.add(4 + CONNECT_GROUP) } >= STREAM_VERSION
}

/// The screen of this build and what the firmware's Connect service can do.
fn platform(ctx: *const CanopusContext) -> Platform {
    let (width, height) = if super::ui::lv::BAND_11 {
        (212, 520)
    } else {
        (336, 480)
    };
    Platform {
        width,
        height,
        caps: if supports_stream(ctx) {
            lyra_protocol::CAP_STREAM
        } else {
            0
        },
    }
}

fn status_of(error: i32) -> Status {
    match error {
        ERR_NOSPACE => Status::NoSpace,
        -2 => Status::NotFound,
        -17 => Status::Exists,
        _ => Status::Io,
    }
}

fn c_path(path: &str) -> Vec<u8> {
    let mut bytes = Vec::from(path.as_bytes());
    bytes.push(0);
    bytes
}

fn mkdir(ctx: *const CanopusContext, path: &str) -> Result<(), i32> {
    let fs = unsafe { (*ctx).fs };
    if fs.is_null() {
        return Err(-38);
    }
    let path = c_path(path);
    let rc = unsafe { ((*fs).mkdir)(ctx as *const ContextV1, path.as_ptr().cast::<c_char>()) };
    if rc < 0 && rc != -17 { Err(rc) } else { Ok(()) }
}

/// Track files below the module directory, through the module's file service.
struct DeviceStore {
    ctx: *const CanopusContext,
    open: Option<(Asset, fs::File)>,
    /// Bytes of the open asset not written yet, less than [`WRITE_BLOCK`] after each frame.
    pending: Vec<u8>,
}

fn write_all(file: &mut fs::File, mut bytes: &[u8]) -> Result<(), Status> {
    while !bytes.is_empty() {
        match file.write(bytes) {
            Ok(0) => return Err(Status::Io),
            Ok(count) => bytes = &bytes[count..],
            Err(error) => return Err(status_of(error)),
        }
    }
    Ok(())
}

impl DeviceStore {
    const ASSETS: [Asset; 3] = [Asset::Audio, Asset::Cover, Asset::Lyrics];

    /// Writes what the open asset still holds back.
    fn flush(&mut self) -> Result<(), Status> {
        let pending = core::mem::take(&mut self.pending);
        let result = match &mut self.open {
            Some((_, file)) => write_all(file, &pending),
            None => Ok(()),
        };
        self.pending = pending;
        self.pending.clear();
        result
    }

    fn remove_dir_contents(&self, dir: &str) {
        for asset in Self::ASSETS {
            let _ = fs::remove(self.ctx, &format!("{dir}/{}", asset.file_name()));
        }
        let _ = fs::remove(self.ctx, &format!("{dir}/{}", lyra_player_core::THUMB_FILE));
    }
}

impl ImportStore for DeviceStore {
    fn stage_begin(&mut self, id: u64) -> Result<(), Status> {
        let _ = id;
        self.open = None;
        self.pending.clear();
        mkdir(self.ctx, "tracks").map_err(status_of)?;
        self.remove_dir_contents("stage");
        mkdir(self.ctx, "stage").map_err(status_of)
    }

    fn stage_write(&mut self, asset: Asset, offset: u32, bytes: &[u8]) -> Result<(), Status> {
        let reopen = !matches!(&self.open, Some((open, _)) if *open == asset);
        if reopen {
            self.flush()?;
            self.open = None;
            let flags = FS_WRITE | FS_CREATE | FS_TRUNC;
            if offset != 0 {
                return Err(Status::Sequence);
            }
            let file = fs::File::open(self.ctx, &format!("stage/{}", asset.file_name()), flags)
                .map_err(status_of)?;
            self.open = Some((asset, file));
        }
        let Some((_, file)) = &mut self.open else {
            return Err(Status::Io);
        };
        self.pending.extend_from_slice(bytes);
        if self.pending.len() >= WRITE_BLOCK {
            let whole = self.pending.len() / WRITE_BLOCK * WRITE_BLOCK;
            let result = write_all(file, &self.pending[..whole]);
            self.pending.drain(..whole);
            result?;
        }
        Ok(())
    }

    fn stage_finish(&mut self, asset: Asset) -> Result<(), Status> {
        if !matches!(&self.open, Some((open, _)) if *open == asset) {
            return Ok(());
        }
        self.flush()?;
        if let Some((_, file)) = self.open.take() {
            let _ = file.sync();
        }
        Ok(())
    }

    fn stage_commit(&mut self, id: u64) -> Result<(), Status> {
        self.open = None;
        self.pending.clear();
        let target = format!("tracks/{id}");
        mkdir(self.ctx, &target).map_err(status_of)?;
        // The list thumbnail is rebuilt from the new cover by the library page.
        let _ = fs::remove(
            self.ctx,
            &format!("{target}/{}", lyra_player_core::THUMB_FILE),
        );
        // The firmware nests a renamed directory inside an existing target.
        for asset in Self::ASSETS {
            let destination = format!("{target}/{}", asset.file_name());
            if fs::exists(self.ctx, &destination) {
                fs::remove(self.ctx, &destination).map_err(status_of)?;
            }
            let source = format!("stage/{}", asset.file_name());
            if fs::exists(self.ctx, &source) {
                fs::rename(self.ctx, &source, &destination).map_err(status_of)?;
            }
        }
        Ok(())
    }

    fn stage_abort(&mut self) {
        self.open = None;
        self.pending.clear();
        self.remove_dir_contents("stage");
    }

    fn remove_track(&mut self, id: u64) -> Result<(), Status> {
        let dir = format!("tracks/{id}");
        self.remove_dir_contents(&dir);
        fs::remove(self.ctx, &dir).map_err(status_of)
    }

    fn save_library(&mut self, library: &Library) -> Result<(), Status> {
        let bytes = encode_library(library);
        let temporary = format!("{LIBRARY_FILE}.tmp");
        fs::write_all(self.ctx, &temporary, &bytes).map_err(status_of)?;
        fs::rename(self.ctx, &temporary, LIBRARY_FILE).map_err(status_of)
    }

    fn free_kib(&mut self) -> u32 {
        u32::MAX
    }
}

struct Endpoint {
    ctx: *const CanopusContext,
    importer: Importer,
    store: DeviceStore,
    library: Option<Library>,
    outbox: VecDeque<Vec<u8>>,
    timer: i32,
    last_activity_ms: u32,
}

static mut ENDPOINT: Option<Endpoint> = None;

fn endpoint() -> Option<&'static mut Endpoint> {
    // SAFETY: everything below runs on the Supervisor's owner thread: the
    // message callback and the timer callback are serialised there.
    unsafe { (*core::ptr::addr_of_mut!(ENDPOINT)).as_mut() }
}

fn now_ms(ctx: *const CanopusContext) -> u32 {
    let time = unsafe { (*ctx).time };
    if time.is_null() {
        return 0;
    }
    unsafe { ((*time).monotonic_ms)(ctx as *const ContextV1) }
}

/// Registers the importer with the Supervisor. Targets without a CanopusConnect
/// backend answer `-38`; the player then simply has no import path.
pub fn start() -> i32 {
    let ctx = context();
    if ctx.is_null() {
        return -22;
    }
    unsafe {
        ENDPOINT = Some(Endpoint {
            ctx,
            importer: Importer::new(platform(ctx)),
            store: DeviceStore {
                ctx,
                open: None,
                pending: Vec::new(),
            },
            library: None,
            outbox: VecDeque::new(),
            timer: -1,
            last_activity_ms: 0,
        });
    }
    match connect::listen(ctx, on_message, core::ptr::null_mut()) {
        Ok(()) => 0,
        Err(error) => error,
    }
}

unsafe extern "C" fn on_message(
    _ctx: *const ContextV1,
    kind: u32,
    data: *const c_void,
    len: u32,
    _user: *mut c_void,
) {
    if kind != CONNECT_BINARY || data.is_null() || len == 0 || len as usize > FRAME_MAX {
        return;
    }
    let frame = unsafe { core::slice::from_raw_parts(data.cast::<u8>(), len as usize) };
    let Some(endpoint) = endpoint() else { return };
    endpoint.last_activity_ms = now_ms(endpoint.ctx);
    if let Ok(request) = Request::decode(frame) {
        endpoint.handle(request);
    }
    endpoint.ensure_timer();
    endpoint.flush();
}

unsafe extern "C" fn flush_timer(_ctx: *const ContextV1, _timer: i32, _user: *mut c_void) {
    let Some(endpoint) = endpoint() else { return };
    endpoint.flush();
    if endpoint.importer.is_importing()
        && now_ms(endpoint.ctx).wrapping_sub(endpoint.last_activity_ms) > STALL_MS
    {
        endpoint.importer.reset(&mut endpoint.store);
    }
}

impl Endpoint {
    fn library(&mut self) -> &mut Library {
        self.library
            .get_or_insert_with(|| storage::load_library().unwrap_or_default())
    }

    fn handle(&mut self, request: Request) {
        let mut library = core::mem::take(self.library());
        let outcome = self.importer.handle(request, &mut library, &mut self.store);
        self.library = Some(library);
        for frame in outcome.frames {
            if self.outbox.len() == OUTBOX_MAX {
                self.outbox.pop_front();
            }
            self.outbox.push_back(frame);
        }
        for change in outcome.changes {
            self.apply(change);
        }
    }

    fn apply(&mut self, change: Change) {
        // A phone can start playback before any page has run; the reader belongs to this thread.
        super::feeder::start();
        match change {
            Change::Library => {
                runtime().library_dirty.store(true, Ordering::Release);
                let library = self.library().clone();
                let effects = super::runtime::with_core(|core| {
                    runtime().library_loaded.store(true, Ordering::Release);
                    core.app.update(Action::ReloadLibrary(library))
                });
                execute_effects(effects);
            }
            Change::Play(id) => {
                let song: Option<Song> = self
                    .library()
                    .tracks
                    .iter()
                    .find(|song| song.id == id)
                    .cloned();
                if let Some(song) = song {
                    let effects =
                        super::runtime::with_core(|core| core.app.update(Action::Play(song)));
                    execute_effects(effects);
                }
            }
            Change::Control(command) => {
                let effects = super::runtime::with_core(|core| match command {
                    Control::Toggle => {
                        core.toggle_audio();
                        Vec::new()
                    }
                    Control::Next => core.app.update(Action::Next),
                    Control::Previous => core.app.update(Action::Previous),
                });
                execute_effects(effects);
            }
        }
    }

    fn ensure_timer(&mut self) {
        if self.timer >= 0 {
            return;
        }
        let time = unsafe { (*self.ctx).time };
        if time.is_null() {
            return;
        }
        let ctx = self.ctx as *const ContextV1;
        unsafe {
            let id = ((*time).timer_create)(ctx, FLUSH_MS, 1, flush_timer, core::ptr::null_mut());
            if id < 0 {
                return;
            }
            let _ = ((*time).timer_set_background)(ctx, id, 1);
            if ((*time).timer_start)(ctx, id) < 0 {
                let _ = ((*time).timer_destroy)(ctx, id);
                return;
            }
            self.timer = id;
        }
    }

    fn flush(&mut self) {
        while let Some(frame) = self.outbox.front() {
            match connect::send_binary(self.ctx, frame) {
                Ok(Delivery::Queued) => {
                    self.outbox.pop_front();
                }
                // Nobody listens any more: the replies would only pile up.
                Ok(Delivery::NotListening) => {
                    self.outbox.clear();
                    self.importer.reset(&mut self.store);
                }
                Err(ERR_BUSY) => return,
                Err(_) => {
                    self.outbox.pop_front();
                }
            }
        }
    }
}
