//! Drives `smoke --stdin` (the Mirage AstroBox bridge) and speaks the Lyra import protocol over
//! CanopusConnect, with exactly the sender the AstroBox plugin uses.
//!
//!   lyra-mirage-import --smoke BIN --port P --key K --state DIR --band ADDR COMMAND ...
//!
//! Commands: `hello`, `list`, `import --id N --audio F [--cover F] [--lyrics F] [--name S]
//! [--artist S]... [--duration-ms N] [--palette 0xTOP,0xBOTTOM] [--pipeline N]`, `delete ID`, `move ID up|down`,
//! `play ID`, `control toggle|next|previous`, `bench BYTES COUNT DEPTH` (time COUNT `DATA` frames with
//! DEPTH host-queued calls; smoke still executes them serially), `listen` (print the watch's messages for a while).

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use lyra_protocol::{Asset, Control, Incoming, MODULE_ID, Reply, Request, Status, TrackMeta, VERSION};
use lyra_transfer::{Sender, Source, State, specs};

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    results: Receiver<bool>,
    frames: Receiver<Vec<u8>>,
    /// `connectSend` calls whose result has not come back yet.
    in_flight: usize,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\":");
    let start = line.find(&marker)? + marker.len();
    let rest = &line[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        Some(&quoted[..quoted.find('"')?])
    } else {
        Some(&rest[..rest.find([',', '}']).unwrap_or(rest.len())])
    }
}

impl Bridge {
    fn start(args: &Args) -> Self {
        let mut child = Command::new(&args.smoke)
            .args([&args.port, &args.key, &args.state, "--stdin"])
            .env("MAB_BAND", &args.band)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the bridge");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (result_tx, results) = mpsc::channel();
        let (frame_tx, frames) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut ready = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.contains("\"type\":\"ready\"") {
                    ready = true;
                    let _ = ready_tx.send(());
                } else if line.contains("\"type\":\"connect\"") {
                    if let Some(data) = field(&line, "dataHex") {
                        let _ = frame_tx.send(unhex(data));
                    }
                } else if ready && line.contains("\"type\":\"result\"") {
                    let _ = result_tx.send(line.contains("\"ok\":true"));
                    if !line.contains("\"ok\":true") {
                        eprintln!("bridge: {line}");
                    }
                } else if args_verbose() {
                    eprintln!("bridge: {line}");
                }
            }
        });
        if ready_rx.recv_timeout(Duration::from_secs(200)).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bridge did not finish connecting (rebuild smoke if it has no ready marker)");
        }
        Self { child, stdin, results, frames, in_flight: 0 }
    }

    /// One bridge call; waits for its result.
    fn call(&mut self, method: &str, params: &str) -> bool {
        writeln!(self.stdin, "{method}={params}").unwrap();
        self.stdin.flush().unwrap();
        self.results.recv_timeout(Duration::from_secs(120)).unwrap_or(false)
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        self.call("connectSend", &format!("{{\"module\":\"{MODULE_ID}\",\"dataHex\":\"{}\"}}", hex(frame)))
    }

    /// Queues at most `depth` host calls; smoke still waits for each device response.
    fn send_pipelined(&mut self, frame: &[u8], depth: usize) -> bool {
        while self.in_flight >= depth {
            if !self.results.recv_timeout(Duration::from_secs(120)).unwrap_or(false) {
                return false;
            }
            self.in_flight -= 1;
        }
        writeln!(
            self.stdin,
            "connectSend={{\"module\":\"{MODULE_ID}\",\"dataHex\":\"{}\"}}",
            hex(frame)
        )
        .unwrap();
        self.stdin.flush().unwrap();
        self.in_flight += 1;
        true
    }

    /// Waits for every queued send.
    fn drain(&mut self) -> bool {
        while self.in_flight > 0 {
            if !self.results.recv_timeout(Duration::from_secs(120)).unwrap_or(false) {
                return false;
            }
            self.in_flight -= 1;
        }
        true
    }

    fn wait_frame(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        self.frames.recv_timeout(timeout).ok()
    }

    fn finish(mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

fn args_verbose() -> bool {
    std::env::var_os("LYRA_VERBOSE").is_some()
}

struct Args {
    smoke: String,
    port: String,
    key: String,
    state: String,
    band: String,
    command: Vec<String>,
}

fn parse_args() -> Args {
    let mut values = std::collections::HashMap::new();
    let mut command = Vec::new();
    let mut iterator = std::env::args().skip(1);
    while let Some(arg) = iterator.next() {
        match arg.as_str() {
            "--smoke" | "--port" | "--key" | "--state" | "--band" => {
                values.insert(arg, iterator.next().expect("value"));
            }
            _ => {
                command.push(arg);
                command.extend(iterator.by_ref());
            }
        }
    }
    let get = |name: &str| values.get(name).cloned().unwrap_or_else(|| panic!("{name} is required"));
    Args {
        smoke: get("--smoke"),
        port: get("--port"),
        key: get("--key"),
        state: get("--state"),
        band: get("--band"),
        command,
    }
}

struct FileSource(File, u32);

impl Source for FileSource {
    fn len(&self) -> u32 {
        self.1
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, String> {
        self.0.seek(SeekFrom::Start(u64::from(offset))).map_err(|e| e.to_string())?;
        self.0.read(buf).map_err(|e| e.to_string())
    }
}

fn open(path: &str) -> Box<dyn Source> {
    let file = File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let len = file.metadata().unwrap().len() as u32;
    Box::new(FileSource(file, len))
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn options(args: &[String], name: &str) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect()
}

fn number(text: &str) -> u32 {
    text.strip_prefix("0x")
        .map_or_else(|| text.parse().unwrap(), |hex| u32::from_str_radix(hex, 16).unwrap())
}

fn request_reply(bridge: &mut Bridge, request: Request) -> Option<Reply> {
    let frame = request.encode().unwrap();
    let expected = [frame[0] | 0x80, frame[1], frame[2]];
    assert!(bridge.send(&frame), "send failed");
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let frame = bridge.wait_frame(left)?;
        if frame.starts_with(&expected)
            && let Ok(Incoming::Reply(reply)) = Incoming::decode(&frame)
        {
            return Some(reply);
        }
    }
    None
}

fn ack_ok(reply: Option<Reply>) -> bool {
    println!("{reply:?}");
    matches!(reply, Some(Reply::Ack { status: Status::Ok, .. }))
}

fn import(bridge: &mut Bridge, args: &[String]) -> bool {
    let id: u64 = option(args, "--id").expect("--id").parse().unwrap();
    let mut sources: Vec<(Asset, Box<dyn Source>)> = vec![(Asset::Audio, open(&option(args, "--audio").expect("--audio")))];
    if let Some(cover) = option(args, "--cover") {
        sources.push((Asset::Cover, open(&cover)));
    }
    if let Some(lyrics) = option(args, "--lyrics") {
        sources.push((Asset::Lyrics, open(&lyrics)));
    }
    let assets = specs(&mut sources).unwrap();
    let palette = option(args, "--palette").unwrap_or_else(|| "0x3A3F58,0x14161F".into());
    let (top, bottom) = palette.split_once(',').unwrap();
    let meta = TrackMeta {
        id,
        duration_ms: option(args, "--duration-ms").map_or(0, |v| number(&v)),
        name: option(args, "--name").unwrap_or_else(|| format!("Track {id}")),
        artists: options(args, "--artist"),
        album: option(args, "--album").unwrap_or_default(),
        palette_top: number(top),
        palette_bottom: number(bottom),
        assets,
    };
    let total: u64 = meta.assets.iter().map(|a| u64::from(a.size)).sum();
    let mut sender = Sender::new(meta, sources.into_iter().map(|(_, s)| s).collect());
    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut frames = 0u32;
    let depth: usize = option(args, "--pipeline").map_or(1, |v| v.parse().unwrap());
    assert!(depth > 0, "host queue depth must be positive");
    if depth > 1 {
        eprintln!("smoke executes requests serially; --pipeline only changes the host queue depth");
    }
    while !sender.is_finished() {
        // Everything the watch said since the last frame.
        while let Ok(frame) = bridge.frames.try_recv() {
            sender.on_frame(&frame);
        }
        match sender.poll() {
            Some(frame) => {
                frames += 1;
                if !bridge.send_pipelined(&frame, depth) {
                    eprintln!("send failed after {frames} frames");
                    return false;
                }
            }
            None => match bridge.wait_frame(Duration::from_secs(30)) {
                Some(frame) => sender.on_frame(&frame),
                None => {
                    eprintln!("the watch stopped answering in state {:?}", sender.state());
                    return false;
                }
            },
        }
        if last_report.elapsed() > Duration::from_secs(5) {
            last_report = Instant::now();
            eprintln!("  {} / {} KiB", sender.confirmed_bytes() / 1024, total / 1024);
        }
    }
    let drained = bridge.drain();
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "import {:?}: {} bytes in {:.1}s ({:.1} KiB/s, {} frames, {} resumes)",
        sender.state(),
        total,
        seconds,
        total as f64 / 1024.0 / seconds.max(0.001),
        frames,
        sender.resumes()
    );
    drained && sender.state() == State::Done
}

fn main() {
    let args = parse_args();
    let mut bridge = Bridge::start(&args);
    assert!(
        bridge.call("connectListen", &format!("{{\"module\":\"{MODULE_ID}\"}}")),
        "the Supervisor refused CanopusConnect listening"
    );
    let mut ok = true;
    let command = args.command.clone();
    match command.first().map(String::as_str) {
        Some("hello") => {
            let reply = request_reply(&mut bridge, Request::Hello { req: 1, version: VERSION });
            println!("{reply:?}");
            ok = matches!(reply, Some(Reply::Hello { status: Status::Ok, .. }));
        }
        Some("list") => {
            let mut offset = 0u16;
            loop {
                match request_reply(&mut bridge, Request::List { req: 2, offset, max: 8 }) {
                    Some(Reply::List { status: Status::Ok, total, entries, .. }) => {
                        for entry in &entries {
                            println!("{} {:>7} ms  {}  /  {}", entry.id, entry.duration_ms, entry.name, entry.artist);
                        }
                        offset += entries.len() as u16;
                        if entries.is_empty() || offset >= total {
                            println!("{total} tracks");
                            break;
                        }
                    }
                    other => {
                        println!("no list: {other:?}");
                        ok = false;
                        break;
                    }
                }
            }
        }
        Some("import") => ok = import(&mut bridge, &command[1..]),
        Some("bench") => {
            // bench BYTES COUNT DEPTH: COUNT DATA frames of BYTES payload bytes outside any transfer, to
            // time the link and the Supervisor without the importer's file work.
            let size: usize = command[1].parse().unwrap();
            let count: usize = command[2].parse().unwrap();
            let depth: usize = command[3].parse().unwrap();
            assert!(depth > 0, "host queue depth must be positive");
            if depth > 1 {
                eprintln!("smoke executes requests serially; this is not transport concurrency");
            }
            let frame = Request::Data { asset: Asset::Audio, offset: 0, bytes: vec![0x5A; size] }.encode().unwrap();
            let started = Instant::now();
            for _ in 0..count {
                ok &= bridge.send_pipelined(&frame, depth);
            }
            ok &= bridge.drain();
            let seconds = started.elapsed().as_secs_f64();
            println!(
                "bench {count} x {size} B, host queue {depth}: {:.2}s, {:.1} ms/frame, {:.1} KiB/s",
                seconds,
                seconds * 1000.0 / count as f64,
                (count * size) as f64 / 1024.0 / seconds
            );
        }
        Some("delete") => {
            let id = command[1].parse().unwrap();
            ok = ack_ok(request_reply(&mut bridge, Request::Delete { req: 3, id }));
        }
        Some("move") => {
            let id = command[1].parse().unwrap();
            let up = command[2] == "up";
            ok = ack_ok(request_reply(&mut bridge, Request::Move { req: 4, id, up }));
        }
        Some("play") => {
            let id = command[1].parse().unwrap();
            ok = ack_ok(request_reply(&mut bridge, Request::Play { req: 5, id }));
        }
        Some("control") => {
            let command = match command[1].as_str() {
                "next" => Control::Next,
                "previous" => Control::Previous,
                _ => Control::Toggle,
            };
            ok = ack_ok(request_reply(&mut bridge, Request::Control { req: 6, command }));
        }
        Some("listen") => {
            let until = Instant::now() + Duration::from_secs(20);
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                if let Some(frame) = bridge.wait_frame(left) {
                    println!("{:?}", Incoming::decode(&frame));
                }
            }
        }
        _ => {
            eprintln!("usage: lyra-mirage-import --smoke BIN --port P --key K --state DIR --band ADDR hello|list|import|delete|move|play|control|listen");
            ok = false;
        }
    }
    bridge.finish();
    std::process::exit(i32::from(!ok));
}
