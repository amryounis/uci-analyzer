//! uci-shim: a transparent proxy that sits between a chess GUI and a UCI
//! engine, and a server that live-streams the conversation to a viewer.
//!
//! The host GUI launches this exe as if it were the engine. The shim starts the
//! real engine and forwards every byte in both directions unmodified. Each line
//! is also recorded: to a transcript file, and to any viewer connected to the
//! shim's local server (`uci-viewer` finds running shims through the session
//! registry in the data folder).
//!
//! Rules: forwarding never waits on logging or on a viewer (events go through a
//! channel to a recorder thread); every forwarded line is flushed immediately;
//! when the GUI goes away the engine goes too; when the engine exits the shim
//! exits with the engine's exit code.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uci_core::json::{self, Value};
use uci_core::{paths, split_args, Dir, Event, SessionInfo};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
uci-shim - transparent UCI proxy that streams the conversation to a viewer

USAGE:
    uci-shim --engine <path> [OPTIONS] [-- <engine args>...]
    uci-shim                 (reads <exe name>.cfg next to the exe)

OPTIONS:
    --engine <path>        the real engine to run
    --config <file>        read this config file instead of <exe name>.cfg
    --log-dir <dir>        where to write transcripts
    --no-log               do not write a transcript file
    --no-viewer            do not start the viewer server
    --no-input             do not accept commands typed into the viewer
    --viewer-port <n>      fixed localhost port for the viewer server
                           (default: any free port)
    --eof-grace-ms <n>     how long the engine may take to exit after the GUI
                           closes its input before the shim kills it (default 2000)
    -h, --help             show this help
    -V, --version          show the version

CONFIG FILE (key=value lines): engine, args, log_dir, log (on|off),
viewer (on|off), viewer_port, input (on|off), eof_grace_ms. See
uci-shim.cfg.example.";

const DEFAULT_EOF_GRACE_MS: u64 = 2000;
const JOIN_WAIT: Duration = Duration::from_secs(2);
/// Most events kept in memory for viewers that connect late. The transcript
/// file keeps everything.
const BACKLOG_CAP: usize = 200_000;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Cli {
    engine: Option<PathBuf>,
    engine_args: Option<Vec<String>>,
    config: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    log: Option<bool>,
    viewer: Option<bool>,
    viewer_port: Option<u16>,
    input: Option<bool>,
    eof_grace_ms: Option<u64>,
    ignored: Vec<String>,
    help: bool,
    version: bool,
}

fn parse_cli(args: Vec<String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => cli.help = true,
            "-V" | "--version" => cli.version = true,
            "--no-log" => cli.log = Some(false),
            "--no-viewer" => cli.viewer = Some(false),
            "--no-input" => cli.input = Some(false),
            "--engine" => {
                cli.engine = Some(PathBuf::from(it.next().ok_or("--engine needs a path")?))
            }
            "--config" => {
                cli.config = Some(PathBuf::from(it.next().ok_or("--config needs a path")?))
            }
            "--log-dir" => {
                cli.log_dir = Some(PathBuf::from(it.next().ok_or("--log-dir needs a path")?))
            }
            "--viewer-port" => {
                let value = it.next().ok_or("--viewer-port needs a number")?;
                cli.viewer_port = Some(
                    value
                        .parse()
                        .map_err(|_| format!("bad --viewer-port value: {value}"))?,
                );
            }
            "--eof-grace-ms" => {
                let value = it.next().ok_or("--eof-grace-ms needs a number")?;
                cli.eof_grace_ms = Some(
                    value
                        .parse()
                        .map_err(|_| format!("bad --eof-grace-ms value: {value}"))?,
                );
            }
            "--" => {
                cli.engine_args = Some(it.by_ref().collect());
                break;
            }
            // A host GUI may pass arguments we do not know. Never refuse to
            // start because of that; note it in the transcript instead.
            other => cli.ignored.push(other.to_string()),
        }
    }
    Ok(cli)
}

#[derive(Default)]
struct CfgFile {
    engine: Option<PathBuf>,
    args: Option<Vec<String>>,
    log_dir: Option<PathBuf>,
    log: Option<bool>,
    viewer: Option<bool>,
    viewer_port: Option<u16>,
    input: Option<bool>,
    eof_grace_ms: Option<u64>,
}

fn strip_quotes(value: &str) -> &str {
    let v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

fn is_off(value: &str) -> bool {
    matches!(value, "off" | "false" | "no" | "0")
}

fn parse_cfg(text: &str) -> CfgFile {
    let mut cfg = CfgFile::default();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "engine" if !value.is_empty() => cfg.engine = Some(PathBuf::from(strip_quotes(value))),
            "args" => cfg.args = Some(split_args(value)),
            "log_dir" if !value.is_empty() => {
                cfg.log_dir = Some(PathBuf::from(strip_quotes(value)))
            }
            "log" => cfg.log = Some(!is_off(value)),
            "viewer" => cfg.viewer = Some(!is_off(value)),
            "viewer_port" => cfg.viewer_port = value.parse().ok(),
            "input" => cfg.input = Some(!is_off(value)),
            "eof_grace_ms" => cfg.eof_grace_ms = value.parse().ok(),
            _ => {}
        }
    }
    cfg
}

struct Config {
    engine: Option<PathBuf>,
    args: Vec<String>,
    log_dir: Option<PathBuf>,
    log: bool,
    viewer: bool,
    viewer_port: u16,
    input: bool,
    eof_grace: Duration,
}

fn cfg_path_for_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut name = exe.file_stem()?.to_os_string();
    name.push(".cfg");
    Some(exe.parent()?.join(name))
}

/// Command line wins over the config file; the config file's `engine` and
/// `args` apply only when the command line names no engine.
fn resolve(cli: &Cli) -> (Config, Option<String>) {
    let path = cli.config.clone().or_else(cfg_path_for_exe);
    let mut note = None;
    let cfg = match &path {
        Some(p) if p.exists() => match std::fs::read_to_string(p) {
            Ok(text) => {
                note = Some(format!("config file: {}", p.display()));
                parse_cfg(&text)
            }
            Err(e) => {
                note = Some(format!("cannot read config file {}: {e}", p.display()));
                CfgFile::default()
            }
        },
        _ => CfgFile::default(),
    };

    let (engine, args) = if cli.engine.is_some() {
        (
            cli.engine.clone(),
            cli.engine_args.clone().unwrap_or_default(),
        )
    } else {
        (
            cfg.engine,
            cli.engine_args.clone().or(cfg.args).unwrap_or_default(),
        )
    };
    let config = Config {
        engine,
        args,
        log_dir: cli.log_dir.clone().or(cfg.log_dir),
        log: cli.log.or(cfg.log).unwrap_or(true),
        viewer: cli.viewer.or(cfg.viewer).unwrap_or(true),
        viewer_port: cli.viewer_port.or(cfg.viewer_port).unwrap_or(0),
        input: cli.input.or(cfg.input).unwrap_or(true),
        eof_grace: Duration::from_millis(
            cli.eof_grace_ms
                .or(cfg.eof_grace_ms)
                .unwrap_or(DEFAULT_EOF_GRACE_MS),
        ),
    };
    (config, note)
}

// ---------------------------------------------------------------------------
// Recording: transcript file + viewer server (never blocks forwarding)
// ---------------------------------------------------------------------------

enum LogMsg {
    Ev(Event),
    Done,
}

/// Cheap, cloneable handle the forwarding threads use to record events.
#[derive(Clone)]
struct Logger {
    tx: Option<Sender<LogMsg>>,
    start: Instant,
}

impl Logger {
    fn event(&self, dir: Dir, line: String) {
        if let Some(tx) = &self.tx {
            let t_ms = self.start.elapsed().as_millis() as u64;
            let _ = tx.send(LogMsg::Ev(Event { t_ms, dir, line }));
        }
    }

    fn bytes(&self, dir: Dir, raw: &[u8]) {
        if self.tx.is_some() {
            let text = String::from_utf8_lossy(raw);
            self.event(dir, text.trim_end_matches(['\r', '\n']).to_string());
        }
    }

    fn shim(&self, text: impl Into<String>) {
        self.event(Dir::Shim, text.into());
    }

    fn done(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(LogMsg::Done);
        }
    }
}

/// Events kept for late-joining viewers, and the live viewer connections.
#[derive(Default)]
struct Hub {
    backlog: VecDeque<Arc<str>>,
    clients: Vec<Sender<Arc<str>>>,
}

impl Hub {
    fn publish(&mut self, line: Arc<str>) {
        self.backlog.push_back(Arc::clone(&line));
        if self.backlog.len() > BACKLOG_CAP {
            self.backlog.pop_front();
        }
        self.clients.retain(|c| c.send(Arc::clone(&line)).is_ok());
    }
}

type SharedHub = Arc<Mutex<Hub>>;

fn lock_hub(hub: &SharedHub) -> MutexGuard<'_, Hub> {
    hub.lock().unwrap_or_else(|e| e.into_inner())
}

fn warn(message: &str) {
    let _ = writeln!(io::stderr(), "uci-shim: {message}");
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn join_bounded(handle: JoinHandle<()>, wait: Duration) {
    let deadline = Instant::now() + wait;
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine's input pipe, shared by the host GUI's forwarding thread and
/// the viewer's manual commands. Every write holds the lock for a whole line,
/// so two sources never interleave inside a line.
#[derive(Clone)]
struct SharedStdin(Arc<Mutex<Option<ChildStdin>>>);

impl SharedStdin {
    fn new(stdin: ChildStdin) -> Self {
        SharedStdin(Arc::new(Mutex::new(Some(stdin))))
    }

    fn is_open(&self) -> bool {
        lock(&self.0).is_some()
    }

    /// Closes the pipe, which is how the engine learns the input has ended.
    fn close(&self) {
        *lock(&self.0) = None;
    }

    fn with<R>(&self, f: impl FnOnce(&mut ChildStdin) -> io::Result<R>) -> io::Result<R> {
        match lock(&self.0).as_mut() {
            Some(w) => f(w),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "engine input is closed",
            )),
        }
    }
}

impl Write for SharedStdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.with(|w| w.write(buf))
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.with(|w| w.write_all(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.with(|w| w.flush())
    }
}

/// Longest command a viewer may send, in bytes.
const MAX_COMMAND: usize = 16 * 1024;

/// Commands typed into the viewer. They are recorded as their own direction
/// and written to the engine; the host GUI is not told, and it still receives
/// whatever the engine answers.
struct Inject {
    allow: bool,
    engine_in: Mutex<Option<SharedStdin>>,
    logger: Mutex<Option<Logger>>,
}

impl Inject {
    fn new(allow: bool) -> Arc<Inject> {
        Arc::new(Inject {
            allow,
            engine_in: Mutex::new(None),
            logger: Mutex::new(None),
        })
    }

    fn note(&self, text: &str) {
        if let Some(logger) = lock(&self.logger).as_ref() {
            logger.shim(text);
        }
    }

    fn submit(&self, cmd: &str) {
        let cmd = cmd.trim();
        if !self.allow {
            return self.note("viewer command refused: manual input is switched off (input=off)");
        }
        if cmd.is_empty() {
            return;
        }
        if cmd.len() > MAX_COMMAND || cmd.chars().any(char::is_control) {
            return self
                .note("viewer command refused: it must be one line of plain text, at most 16 KB");
        }
        let Some(mut stdin) = lock(&self.engine_in).clone() else {
            return self.note("viewer command refused: no engine is running");
        };
        if !stdin.is_open() {
            return self.note("viewer command refused: the engine's input is closed");
        }
        // Record first, like the forwarding threads, so the order is causal.
        if let Some(logger) = lock(&self.logger).as_ref() {
            logger.event(Dir::ViewerToEngine, cmd.to_string());
        }
        let line = format!("{cmd}\n");
        if let Err(e) = stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()) {
            self.note(&format!("viewer command not delivered: {e}"));
        }
    }
}

/// Reads `{"cmd":"..."}` lines sent by a viewer.
fn read_commands(stream: TcpStream, inject: Arc<Inject>) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let limit = (MAX_COMMAND * 2) as u64;
        match reader.by_ref().take(limit).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.last() != Some(&b'\n') {
            if buf.len() as u64 >= limit {
                inject.note("viewer sent an over-long line; its input was ignored");
            }
            return;
        }
        let text = String::from_utf8_lossy(&buf);
        if let Ok(fields) = json::parse_flat_object(text.trim()) {
            for (key, value) in fields {
                if let ("cmd", Value::Str(cmd)) = (key.as_str(), value) {
                    inject.submit(&cmd);
                }
            }
        }
    }
}

fn write_line<W: Write>(out: &mut W, line: &str) -> io::Result<()> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")
}

/// Sends the hello line, then the backlog, then live events, to one viewer.
fn serve_client(stream: TcpStream, hub: &SharedHub, hello: &str, inject: Arc<Inject>) {
    let _ = stream.set_nodelay(true);
    if let Ok(reader) = stream.try_clone() {
        thread::spawn(move || read_commands(reader, inject));
    }
    // A viewer that stops reading must not hang this thread forever.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    // Register and snapshot under one lock: no event is missed or repeated.
    let (rx, backlog): (Receiver<Arc<str>>, Vec<Arc<str>>) = {
        let mut hub = lock_hub(hub);
        let (tx, rx) = mpsc::channel();
        let snapshot = hub.backlog.iter().cloned().collect();
        hub.clients.push(tx);
        (rx, snapshot)
    };
    let mut out = BufWriter::new(&stream);
    if write_line(&mut out, hello)
        .and_then(|_| out.flush())
        .is_err()
    {
        return;
    }
    for line in backlog {
        if write_line(&mut out, &line).is_err() {
            return;
        }
    }
    if out.flush().is_err() {
        return;
    }
    while let Ok(line) = rx.recv() {
        if write_line(&mut out, &line).is_err() {
            return;
        }
        while let Ok(more) = rx.try_recv() {
            if write_line(&mut out, &more).is_err() {
                return;
            }
        }
        if out.flush().is_err() {
            return;
        }
    }
}

/// Binds the localhost viewer server. Returns the port it listens on.
fn start_server(
    hub: &SharedHub,
    active: &Arc<AtomicUsize>,
    port: u16,
    info: &SessionInfo,
    inject: &Arc<Inject>,
) -> Option<u16> {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            warn(&format!("cannot start viewer server on port {port}: {e}"));
            return None;
        }
    };
    let bound = listener.local_addr().ok()?.port();
    let hello = info.hello_json(VERSION);
    let hub = Arc::clone(hub);
    let active = Arc::clone(active);
    let inject = Arc::clone(inject);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let hub = Arc::clone(&hub);
            let hello = hello.clone();
            let active = Arc::clone(&active);
            let inject = Arc::clone(&inject);
            active.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || {
                serve_client(stream, &hub, &hello, inject);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Some(bound)
}

struct Recorder {
    logger: Logger,
    writer: Option<JoinHandle<()>>,
    hub: SharedHub,
    active: Arc<AtomicUsize>,
    inject: Arc<Inject>,
    registry_file: Option<PathBuf>,
}

impl Recorder {
    /// Drains the recorder, lets connected viewers receive the last events,
    /// then closes them and removes the session registry entry.
    fn finish(self) {
        self.logger.done();
        if let Some(handle) = self.writer {
            join_bounded(handle, JOIN_WAIT);
        }
        lock_hub(&self.hub).clients.clear(); // viewer threads end after draining
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if let Some(path) = self.registry_file {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn start_recorder(config: &Config, start: Instant, stem: &str, engine_label: &str) -> Recorder {
    let hub: SharedHub = Arc::new(Mutex::new(Hub::default()));
    let active = Arc::new(AtomicUsize::new(0));
    let inject = Inject::new(config.input);
    let none = |hub: SharedHub, active: Arc<AtomicUsize>, inject: Arc<Inject>| Recorder {
        logger: Logger { tx: None, start },
        writer: None,
        hub,
        active,
        inject,
        registry_file: None,
    };

    // Transcript file (optional).
    let mut transcript_path = None;
    let mut file = None;
    if config.log {
        let dir = config
            .log_dir
            .clone()
            .unwrap_or_else(paths::transcripts_dir);
        match std::fs::create_dir_all(&dir) {
            Err(e) => warn(&format!("cannot create log dir {}: {e}", dir.display())),
            Ok(()) => {
                let path = dir.join(format!("{}-{}-{stem}.jsonl", unix_ms(), std::process::id()));
                match File::create(&path) {
                    Ok(f) => {
                        file = Some(f);
                        transcript_path = Some(path);
                    }
                    Err(e) => warn(&format!("cannot create transcript {}: {e}", path.display())),
                }
            }
        }
    }

    // Viewer server (optional).
    let mut registry_file = None;
    let mut serving = false;
    if config.viewer {
        let mut info = SessionInfo {
            pid: std::process::id(),
            port: 0,
            engine: engine_label.to_string(),
            started_unix_ms: unix_ms() as u64,
            transcript: transcript_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            input: config.input,
        };
        if let Some(port) = start_server(&hub, &active, config.viewer_port, &info, &inject) {
            serving = true;
            info.port = port;
            let dir = paths::sessions_dir();
            let path = dir.join(format!("{}.json", info.pid));
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, info.to_json()))
            {
                Ok(()) => registry_file = Some(path),
                Err(e) => warn(&format!("cannot publish session {}: {e}", path.display())),
            }
        }
    }

    if file.is_none() && !serving {
        return none(hub, active, inject);
    }

    let (tx, rx) = mpsc::channel::<LogMsg>();
    let recorder_hub = serving.then(|| Arc::clone(&hub));
    let writer = thread::spawn(move || {
        let mut out = file.map(BufWriter::new);
        for msg in rx {
            match msg {
                LogMsg::Ev(event) => {
                    let json = event.to_json();
                    if let Some(out) = out.as_mut() {
                        let _ = writeln!(out, "{json}");
                        // Flush per event so a crash or kill loses nothing.
                        let _ = out.flush();
                    }
                    if let Some(hub) = &recorder_hub {
                        lock_hub(hub).publish(Arc::from(json));
                    }
                }
                LogMsg::Done => break,
            }
        }
        if let Some(out) = out.as_mut() {
            let _ = out.flush();
        }
    });
    let logger = Logger {
        tx: Some(tx),
        start,
    };
    *lock(&inject.logger) = Some(logger.clone());
    Recorder {
        logger,
        writer: Some(writer),
        hub,
        active,
        inject,
        registry_file,
    }
}

// ---------------------------------------------------------------------------
// Forwarding
// ---------------------------------------------------------------------------

enum End {
    Eof,
    ReadFailed(String),
    WriteFailed(String),
}

/// Copies lines from `reader` to `writer` byte for byte. Each line is logged
/// first (a channel send, effectively free) so the record keeps causal order,
/// then forwarded and flushed.
fn forward<R: BufRead, W: Write>(mut reader: R, mut writer: W, dir: Dir, logger: &Logger) -> End {
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return End::Eof,
            Ok(_) => {
                logger.bytes(dir, &buf);
                if let Err(e) = writer.write_all(&buf).and_then(|_| writer.flush()) {
                    return End::WriteFailed(e.to_string());
                }
            }
            Err(e) => return End::ReadFailed(e.to_string()),
        }
    }
}

/// Used when no engine is configured or it cannot start: answers just enough
/// UCI for the host GUI to show why, instead of timing out silently.
fn fallback(message: &str, logger: &Logger) -> i32 {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match input.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return 1,
            Ok(_) => {}
        }
        logger.bytes(Dir::GuiToEngine, &buf);
        let text = String::from_utf8_lossy(&buf);
        let replies: Vec<String> = match text.split_whitespace().next() {
            Some("uci") => vec![
                "id name uci-shim (not configured)".into(),
                "id author uci-visualizer".into(),
                format!("info string uci-shim: {message}"),
                "uciok".into(),
            ],
            Some("isready") => vec!["readyok".into()],
            Some("quit") => return 1,
            _ => Vec::new(),
        };
        for reply in replies {
            logger.event(Dir::EngineToGui, reply.clone());
            if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
                return 1;
            }
        }
    }
}

fn file_stem_for_log(path: Option<&PathBuf>) -> String {
    let stem = path
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unconfigured".to_string());
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn run() -> i32 {
    let cli = match parse_cli(std::env::args().skip(1).collect()) {
        Ok(cli) => cli,
        Err(e) => {
            warn(&e);
            let _ = writeln!(io::stderr(), "\n{USAGE}");
            return 2;
        }
    };
    if cli.help {
        println!("{USAGE}");
        return 0;
    }
    if cli.version {
        println!("uci-shim {VERSION}");
        return 0;
    }

    let (config, config_note) = resolve(&cli);
    let start = Instant::now();
    let stem = file_stem_for_log(config.engine.as_ref());
    let engine_label = config
        .engine
        .as_ref()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(not configured)".to_string());
    let recorder = start_recorder(&config, start, &stem, &engine_label);
    let logger = recorder.logger.clone();

    logger.shim(format!(
        "shim start: version={VERSION} pid={} unix_ms={}",
        std::process::id(),
        unix_ms()
    ));
    if let Some(note) = config_note {
        logger.shim(note);
    }
    for arg in &cli.ignored {
        logger.shim(format!("ignored unknown argument: {arg}"));
    }

    let Some(engine) = config.engine.clone() else {
        let message = "no engine configured (use --engine or a .cfg file next to the exe)";
        logger.shim(message);
        let code = fallback(message, &logger);
        recorder.finish();
        return code;
    };

    let engine_abs = std::path::absolute(&engine).unwrap_or_else(|_| engine.clone());
    logger.shim(format!(
        "engine: {} args={:?}",
        engine_abs.display(),
        config.args
    ));

    let mut command = Command::new(&engine_abs);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Engines often look for files (networks, books) next to themselves.
    if let Some(dir) = engine_abs.parent().filter(|p| !p.as_os_str().is_empty()) {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: never flash a console window inside the host GUI.
        command.creation_flags(0x0800_0000);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let message = format!("cannot start engine {}: {e}", engine_abs.display());
            logger.shim(message.clone());
            let code = fallback(&message, &logger);
            recorder.finish();
            return code;
        }
    };
    let (Some(child_stdin), Some(child_stdout), Some(child_stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        logger.shim("engine started without piped stdio");
        let _ = child.kill();
        recorder.finish();
        return 1;
    };

    let engine_in = SharedStdin::new(child_stdin);
    *lock(&recorder.inject.engine_in) = Some(engine_in.clone());

    let gui_gone = Arc::new(AtomicBool::new(false));
    let eof_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    // GUI -> engine. Not joined: it may sit in a blocking read until we exit.
    {
        let logger = logger.clone();
        let eof_at = Arc::clone(&eof_at);
        let engine_in = engine_in.clone();
        thread::spawn(move || {
            let stdin = io::stdin();
            let end = forward(stdin.lock(), engine_in.clone(), Dir::GuiToEngine, &logger);
            // Close the pipe so the engine sees end of input (this also
            // ends manual commands from a viewer).
            engine_in.close();
            let mark_eof =
                || *eof_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            match end {
                End::Eof => {
                    logger.shim("gui closed its input");
                    mark_eof();
                }
                End::ReadFailed(e) => {
                    logger.shim(format!("gui input failed: {e}"));
                    mark_eof();
                }
                End::WriteFailed(e) => logger.shim(format!("cannot write to engine: {e}")),
            }
        });
    }

    // Engine -> GUI
    let out_thread = {
        let logger = logger.clone();
        let gui_gone = Arc::clone(&gui_gone);
        thread::spawn(move || {
            let reader = BufReader::with_capacity(64 * 1024, child_stdout);
            let stdout = io::stdout();
            match forward(reader, stdout.lock(), Dir::EngineToGui, &logger) {
                End::Eof => {}
                End::ReadFailed(e) => logger.shim(format!("engine output failed: {e}")),
                End::WriteFailed(e) => {
                    logger.shim(format!("gui closed its output: {e}"));
                    gui_gone.store(true, Ordering::SeqCst);
                }
            }
        })
    };

    // Engine stderr: recorded, and passed through to our own stderr.
    let err_thread = {
        let logger = logger.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(child_stderr);
            let mut sink = io::stderr();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        logger.bytes(Dir::EngineStderr, &buf);
                        let _ = sink.write_all(&buf);
                    }
                }
            }
        })
    };

    // Wait for the engine, killing it if the GUI is gone.
    let mut killed_by_shim = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) => {}
            Err(e) => {
                logger.shim(format!("waiting for engine failed: {e}"));
                break 1;
            }
        }
        let grace_over = matches!(
            *eof_at.lock().unwrap_or_else(|e| e.into_inner()),
            Some(t) if t.elapsed() >= config.eof_grace
        );
        if gui_gone.load(Ordering::SeqCst) || grace_over {
            logger.shim(if grace_over {
                "engine did not exit after the gui closed its input; killing it"
            } else {
                "gui is gone; killing engine"
            });
            let _ = child.kill();
            let _ = child.wait();
            killed_by_shim = true;
            break 0;
        }
        thread::sleep(Duration::from_millis(5));
    };

    // Let the engine's last output reach the GUI and the record.
    join_bounded(out_thread, JOIN_WAIT);
    join_bounded(err_thread, JOIN_WAIT);
    logger.shim(format!(
        "engine exited: code={code}{}",
        if killed_by_shim {
            " (killed by shim)"
        } else {
            ""
        }
    ));
    recorder.finish();
    code
}

fn main() {
    std::process::exit(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cli_parses_engine_and_trailing_args() {
        let cli = parse_cli(args(&[
            "--engine",
            "C:\\e\\sf.exe",
            "--no-log",
            "--no-viewer",
            "--viewer-port",
            "7000",
            "--eof-grace-ms",
            "300",
            "--",
            "--threads",
            "4",
        ]))
        .unwrap();
        assert_eq!(cli.engine, Some(PathBuf::from("C:\\e\\sf.exe")));
        assert_eq!(cli.log, Some(false));
        assert_eq!(cli.viewer, Some(false));
        assert_eq!(cli.viewer_port, Some(7000));
        assert_eq!(cli.eof_grace_ms, Some(300));
        assert_eq!(cli.engine_args, Some(args(&["--threads", "4"])));
    }

    #[test]
    fn cli_ignores_unknown_arguments_instead_of_failing() {
        let cli = parse_cli(args(&["-x", "--engine", "e"])).unwrap();
        assert_eq!(cli.ignored, vec!["-x"]);
        assert_eq!(cli.engine, Some(PathBuf::from("e")));
    }

    #[test]
    fn cli_reports_missing_values() {
        assert!(parse_cli(args(&["--engine"])).is_err());
        assert!(parse_cli(args(&["--eof-grace-ms", "soon"])).is_err());
        assert!(parse_cli(args(&["--viewer-port", "99999"])).is_err());
    }

    #[test]
    fn manual_input_is_on_unless_switched_off() {
        let cfg = parse_cfg("input=off\n");
        assert_eq!(cfg.input, Some(false));
        assert_eq!(parse_cfg("input=on\n").input, Some(true));
        assert_eq!(parse_cfg("").input, None);
        let cli = parse_cli(args(&["--no-input"])).unwrap();
        assert_eq!(cli.input, Some(false));
        let (config, _) = resolve(&parse_cli(args(&["--config", "/nonexistent.cfg"])).unwrap());
        assert!(config.input);
        let (config, _) = resolve(&cli);
        assert!(!config.input);
    }

    #[test]
    fn split_args_keeps_quoted_groups() {
        assert_eq!(
            split_args(r#"--a 1 "two words" "" last"#),
            args(&["--a", "1", "two words", "", "last"])
        );
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn cfg_file_parses_keys_and_ignores_noise() {
        let cfg = parse_cfg(
            "\u{feff}# comment\n\
             engine = C:\\Program Files\\SF\\sf.exe \n\
             args=--x \"a b\"\n\
             log=off\n\
             viewer=off\n\
             viewer_port=7001\n\
             eof_grace_ms=500\n\
             nonsense line\n\
             unknown=1\n",
        );
        assert_eq!(
            cfg.engine,
            Some(PathBuf::from("C:\\Program Files\\SF\\sf.exe"))
        );
        assert_eq!(cfg.args, Some(args(&["--x", "a b"])));
        assert_eq!(cfg.log, Some(false));
        assert_eq!(cfg.viewer, Some(false));
        assert_eq!(cfg.viewer_port, Some(7001));
        assert_eq!(cfg.eof_grace_ms, Some(500));
    }

    #[test]
    fn cli_engine_overrides_cfg_engine_and_args() {
        let cli = Cli {
            engine: Some(PathBuf::from("cli-engine")),
            config: Some(PathBuf::from("/nonexistent.cfg")),
            ..Cli::default()
        };
        let (config, _) = resolve(&cli);
        assert_eq!(config.engine, Some(PathBuf::from("cli-engine")));
        assert!(config.args.is_empty());
        assert!(config.log);
        assert!(config.viewer);
        assert_eq!(config.viewer_port, 0);
        assert_eq!(
            config.eof_grace,
            Duration::from_millis(DEFAULT_EOF_GRACE_MS)
        );
    }

    #[test]
    fn hub_keeps_a_bounded_backlog_and_drops_dead_clients() {
        let mut hub = Hub::default();
        let (tx_live, rx_live) = mpsc::channel();
        let (tx_dead, rx_dead) = mpsc::channel();
        drop(rx_dead);
        hub.clients.push(tx_live);
        hub.clients.push(tx_dead);
        for i in 0..(BACKLOG_CAP + 10) {
            hub.publish(Arc::from(format!("line {i}")));
        }
        assert_eq!(hub.backlog.len(), BACKLOG_CAP);
        assert_eq!(&*hub.backlog[0], "line 10");
        assert_eq!(hub.clients.len(), 1);
        assert_eq!(&*rx_live.recv().unwrap(), "line 0");
    }
}
