//! The JSON API and the event streams behind the page.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use uci_core::json::{self, Obj, Value};
use uci_core::{paths, view_json, Event, SessionInfo};

use crate::http::{self, Request};
use crate::launch;

const INDEX_HTML: &str = include_str!("ui/index.html");

/// Sends `: ping` to the browser this often while a session is quiet, so a
/// closed window is noticed.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// A random value made when the viewer starts and written into the page it
/// serves. Sending a command requires it, so only the page itself can send:
/// another web page cannot read this one and so cannot learn it.
static TOKEN: OnceLock<String> = OnceLock::new();

pub fn token() -> &'static str {
    TOKEN.get_or_init(make_token)
}

fn make_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for round in 0..2u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(now_ms());
        hasher.write_u64(round);
        hasher.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

/// The browser connection of each open live stream, so a command typed in
/// the page can be sent to the shim on the same connection.
type Senders = Mutex<HashMap<u32, (u64, Arc<Mutex<TcpStream>>)>>;
static SENDERS: OnceLock<Senders> = OnceLock::new();
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

fn senders() -> &'static Senders {
    SENDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes a stream from the registry when its thread ends.
struct Registered {
    pid: u32,
    id: u64,
}

impl Drop for Registered {
    fn drop(&mut self) {
        let mut map = lock(senders());
        if matches!(map.get(&self.pid), Some((id, _)) if *id == self.id) {
            map.remove(&self.pid);
        }
    }
}

pub fn route(req: &Request, stream: &mut TcpStream, port: u16) -> io::Result<()> {
    if !host_allowed(req.header("host"), port) {
        return http::respond_text(stream, 403, "unexpected Host header");
    }
    if req.method == "POST" {
        return match req.path.as_str() {
            "/api/send" => send_command(req, stream, port),
            "/api/launch" => launch_engine(req, stream, port),
            "/api/browse" => browse_for_engine(req, stream, port),
            _ => http::respond_text(stream, 405, "GET only"),
        };
    }
    if req.method != "GET" {
        return http::respond_text(stream, 405, "GET only");
    }
    match req.path.as_str() {
        "/" | "/index.html" => {
            let page = INDEX_HTML.replace("__UCI_TOKEN__", token());
            http::respond(stream, 200, "text/html; charset=utf-8", page.as_bytes())
        }
        "/api/engines" => {
            let body = engines_json();
            http::respond(stream, 200, "application/json", body.as_bytes())
        }
        "/api/sessions" => {
            let body = sessions_json();
            http::respond(stream, 200, "application/json", body.as_bytes())
        }
        "/api/stream" => {
            if let Some(pid) = req.param("pid").and_then(|p| p.parse::<u32>().ok()) {
                stream_live(stream, pid)
            } else if let Some(file) = req.param("file") {
                stream_file(stream, file)
            } else {
                http::respond_text(stream, 400, "need pid= or file=")
            }
        }
        "/api/transcript" => match req.param("file").and_then(transcript_path) {
            Some(path) => match fs::read(&path) {
                Ok(bytes) => http::respond(stream, 200, "application/x-ndjson", &bytes),
                Err(_) => http::respond_text(stream, 404, "no such transcript"),
            },
            None => http::respond_text(stream, 404, "no such transcript"),
        },
        _ => http::respond_text(stream, 404, "not found"),
    }
}

/// Only answer requests addressed to this machine. A web page on another
/// site can make a browser send requests to localhost, but it cannot make
/// the browser use a Host header other than the site's own name.
pub fn host_allowed(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// A request may come from our own page only: same origin when the browser
/// says where it came from, and the secret from the page when it does not.
pub fn origin_allowed(origin: Option<&str>, port: u16) -> bool {
    match origin {
        None => true,
        Some(o) => {
            o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
        }
    }
}

/// Longest manual command, matching the shim's own limit.
const MAX_COMMAND: usize = 16 * 1024;

/// What the page sends: `{"cmd":"isready"}`. Valid when it is one line of
/// plain text.
pub fn parse_command(body: &[u8]) -> Result<String, &'static str> {
    let text = std::str::from_utf8(body).map_err(|_| "not text")?;
    let fields = json::parse_flat_object(text).map_err(|_| "not a JSON object")?;
    let cmd = fields
        .into_iter()
        .find_map(|(k, v)| match (k.as_str(), v) {
            ("cmd", Value::Str(s)) => Some(s),
            _ => None,
        })
        .ok_or("missing cmd")?;
    let cmd = cmd.trim().to_string();
    if cmd.is_empty() {
        return Err("empty command");
    }
    if cmd.len() > MAX_COMMAND || cmd.chars().any(char::is_control) {
        return Err("a command must be one line of plain text, at most 16 KB");
    }
    Ok(cmd)
}

/// Requests that change something must come from our own page.
fn page_request(req: &Request, port: u16) -> bool {
    origin_allowed(req.header("origin"), port) && req.header("x-uci-token") == Some(token())
}

/// What the Load engine dialog needs: whether loading is on, whether the
/// file dialog exists, and the engines loaded before.
fn engines_json() -> String {
    let mut list = String::from("[");
    for (n, (path, args)) in launch::recent().iter().enumerate() {
        if n > 0 {
            list.push(',');
        }
        list.push_str(&Obj::new().str("path", path).str("args", args).finish());
    }
    list.push(']');
    Obj::new()
        .bool("launch", launch::allowed())
        .bool("browse", launch::can_browse())
        .raw("recent", &list)
        .finish()
}

fn launch_engine(req: &Request, stream: &mut TcpStream, port: u16) -> io::Result<()> {
    if !page_request(req, port) {
        return http::respond_text(stream, 403, "not allowed");
    }
    let (engine, args) = match launch::parse_request(&req.body) {
        Ok(v) => v,
        Err(why) => return http::respond_text(stream, 400, why),
    };
    match launch::launch(&engine, &args) {
        Ok(pid) => {
            let body = Obj::new().bool("ok", true).num("pid", pid).finish();
            http::respond(stream, 200, "application/json", body.as_bytes())
        }
        Err(why) => http::respond_text(stream, 422, &why),
    }
}

fn browse_for_engine(req: &Request, stream: &mut TcpStream, port: u16) -> io::Result<()> {
    if !page_request(req, port) {
        return http::respond_text(stream, 403, "not allowed");
    }
    match launch::browse() {
        Ok(path) => {
            let body = Obj::new().opt_str("path", path.as_deref()).finish();
            http::respond(stream, 200, "application/json", body.as_bytes())
        }
        Err(why) => http::respond_text(stream, 422, &why),
    }
}

fn send_command(req: &Request, stream: &mut TcpStream, port: u16) -> io::Result<()> {
    if !page_request(req, port) {
        return http::respond_text(stream, 403, "not allowed");
    }
    let Some(pid) = req.param("pid").and_then(|p| p.parse::<u32>().ok()) else {
        return http::respond_text(stream, 400, "need pid=");
    };
    let cmd = match parse_command(&req.body) {
        Ok(c) => c,
        Err(why) => return http::respond_text(stream, 400, why),
    };
    let target = lock(senders()).get(&pid).map(|(_, s)| Arc::clone(s));
    let Some(target) = target else {
        return http::respond_text(stream, 409, "that session is not open in the viewer");
    };
    let mut line = String::from("{\"cmd\":\"");
    json::escape_into(&mut line, &cmd);
    line.push_str("\"}\n");
    let written = {
        let mut conn = lock(&target);
        let _ = conn.set_write_timeout(Some(Duration::from_secs(2)));
        conn.write_all(line.as_bytes()).and_then(|_| conn.flush())
    };
    match written {
        Ok(()) => http::respond(stream, 200, "application/json", b"{\"ok\":true}"),
        Err(_) => http::respond_text(stream, 502, "the shim did not take the command"),
    }
}

/// A transcript is a bare `*.jsonl` file name inside the transcripts folder.
pub fn safe_transcript_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.ends_with(".jsonl")
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && !name.contains("..")
}

fn transcript_path(name: &str) -> Option<PathBuf> {
    safe_transcript_name(name).then(|| paths::transcripts_dir().join(name))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- sessions

/// Running shims. A registry entry whose port refuses connections belongs to
/// a shim that died without cleaning up, so it is deleted.
pub fn live_sessions(dir: &Path) -> Vec<SessionInfo> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(info) = SessionInfo::from_json(&text) else {
            continue;
        };
        match probe(&info) {
            Probe::Alive => found.push(info),
            Probe::Dead => {
                let _ = fs::remove_file(&path);
            }
            Probe::Unknown => {}
        }
    }
    found.sort_by_key(|s| std::cmp::Reverse(s.started_unix_ms));
    found
}

enum Probe {
    Alive,
    Dead,
    Unknown,
}

fn probe(info: &SessionInfo) -> Probe {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, info.port));
    let mut conn = match TcpStream::connect_timeout(&addr, Duration::from_millis(400)) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => return Probe::Dead,
        Err(_) => return Probe::Unknown,
    };
    let _ = conn.set_read_timeout(Some(Duration::from_millis(600)));
    let mut first = String::new();
    let _ = BufReader::new(&mut conn).read_line(&mut first);
    // Something else may have taken the port since the shim died.
    if first.contains("\"hello\":\"uci-shim\"") && first.contains(&format!("\"pid\":{}", info.pid))
    {
        Probe::Alive
    } else if first.is_empty() {
        Probe::Unknown
    } else {
        Probe::Dead
    }
}

fn sessions_json() -> String {
    let live: Vec<String> = live_sessions(&paths::sessions_dir())
        .iter()
        .map(|s| s.to_json())
        .collect();

    let mut files: Vec<(u64, String, u64)> = Vec::new();
    if let Ok(entries) = fs::read_dir(paths::transcripts_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !safe_transcript_name(&name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            files.push((modified, name, meta.len()));
        }
    }
    files.sort_by(|a, b| b.cmp(a));
    files.truncate(200);
    let transcripts: Vec<String> = files
        .iter()
        .map(|(modified, name, size)| {
            Obj::new()
                .str("file", name)
                .num("size", size)
                .num("modified_unix_ms", modified)
                .finish()
        })
        .collect();

    Obj::new()
        .raw("live", &format!("[{}]", live.join(",")))
        .raw("transcripts", &format!("[{}]", transcripts.join(",")))
        .num("now_unix_ms", now_ms())
        .finish()
}

// ----------------------------------------------------------------- streams

fn sse(stream: &mut impl Write, event: Option<&str>, data: &str) -> io::Result<()> {
    if let Some(name) = event {
        writeln!(stream, "event: {name}")?;
    }
    write!(stream, "data: {data}\n\n")
}

/// Splits bytes arriving in arbitrary pieces into lines, and survives read
/// timeouts without losing a half-received line.
struct LineReader<R: Read> {
    inner: R,
    pending: Vec<u8>,
}

enum Next {
    Line(String),
    Idle,
    End,
}

impl<R: Read> LineReader<R> {
    fn new(inner: R) -> Self {
        LineReader {
            inner,
            pending: Vec::new(),
        }
    }

    fn next(&mut self) -> Next {
        loop {
            if let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
                let rest = self.pending.split_off(pos + 1);
                let mut line = std::mem::replace(&mut self.pending, rest);
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Next::Line(String::from_utf8_lossy(&line).into_owned());
            }
            let mut buf = [0u8; 8192];
            match self.inner.read(&mut buf) {
                Ok(0) => return Next::End,
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Next::Idle
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Next::End,
            }
        }
    }
}

fn stream_live(out: &mut TcpStream, pid: u32) -> io::Result<()> {
    let Some(info) = live_sessions(&paths::sessions_dir())
        .into_iter()
        .find(|s| s.pid == pid)
    else {
        return http::respond_text(out, 404, "no such running session");
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, info.port));
    let conn = match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(c) => c,
        Err(_) => return http::respond_text(out, 502, "the shim is not answering"),
    };
    conn.set_read_timeout(Some(HEARTBEAT))?;
    let stream_id = NEXT_STREAM.fetch_add(1, Ordering::SeqCst);
    let _registered = conn.try_clone().ok().map(|writer| {
        lock(senders()).insert(pid, (stream_id, Arc::new(Mutex::new(writer))));
        Registered { pid, id: stream_id }
    });
    http::start_stream(out, "text/event-stream")?;
    out.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut lines = LineReader::new(conn);
    let mut index: u64 = 0;
    let mut hello_seen = false;
    loop {
        match lines.next() {
            Next::Line(line) if line.is_empty() => {}
            Next::Line(line) if !hello_seen => {
                hello_seen = true;
                sse(out, Some("hello"), &line)?;
                out.flush()?;
            }
            Next::Line(line) => {
                // One bad line must not end the whole stream.
                if let Ok(event) = Event::from_json(&line) {
                    sse(out, None, &view_json(index, &event))?;
                    index += 1;
                }
                // Hand over a whole burst in one go, but never hold a
                // lone event back.
                if lines.pending.is_empty() {
                    out.flush()?;
                }
            }
            Next::Idle => {
                out.write_all(b": ping\n\n")?;
                out.flush()?;
            }
            Next::End => {
                sse(out, Some("end"), "{}")?;
                return out.flush();
            }
        }
    }
}

fn stream_file(out: &mut TcpStream, name: &str) -> io::Result<()> {
    let Some(path) = transcript_path(name) else {
        return http::respond_text(out, 404, "no such transcript");
    };
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return http::respond_text(out, 404, "no such transcript"),
    };
    http::start_stream(out, "text/event-stream")?;
    out.set_write_timeout(Some(Duration::from_secs(5)))?;

    let hello = Obj::new()
        .str("hello", "transcript")
        .str("file", name)
        .finish();
    sse(out, Some("hello"), &hello)?;

    let mut index: u64 = 0;
    let mut batch = 0;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = Event::from_json(&line) {
            sse(out, None, &view_json(index, &event))?;
            index += 1;
            batch += 1;
            if batch == 256 {
                out.flush()?;
                batch = 0;
            }
        }
    }
    sse(out, Some("end"), "{}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_host_headers_are_accepted() {
        assert!(host_allowed(Some("127.0.0.1:7878"), 7878));
        assert!(host_allowed(Some("localhost:7878"), 7878));
        assert!(!host_allowed(Some("evil.example:7878"), 7878));
        assert!(!host_allowed(Some("127.0.0.1:80"), 7878));
        assert!(!host_allowed(None, 7878));
    }

    #[test]
    fn only_our_own_origin_may_send() {
        assert!(origin_allowed(None, 7878));
        assert!(origin_allowed(Some("http://127.0.0.1:7878"), 7878));
        assert!(origin_allowed(Some("http://localhost:7878"), 7878));
        assert!(!origin_allowed(Some("https://evil.example"), 7878));
        assert!(!origin_allowed(Some("http://127.0.0.1:9999"), 7878));
        assert!(!origin_allowed(Some("null"), 7878));
    }

    #[test]
    fn commands_must_be_one_line_of_text() {
        assert_eq!(
            parse_command(br#"{"cmd":"  isready "}"#).unwrap(),
            "isready"
        );
        assert!(parse_command(br#"{"cmd":""}"#).is_err());
        assert!(parse_command(br#"{"cmd":"a\nb"}"#).is_err());
        assert!(parse_command(br#"{"cmd":"a\u0007b"}"#).is_err());
        assert!(parse_command(br#"{"other":"x"}"#).is_err());
        assert!(parse_command(b"not json").is_err());
        assert!(parse_command(&[0xff, 0xfe]).is_err());
        let long = format!(r#"{{"cmd":"{}"}}"#, "x".repeat(MAX_COMMAND + 1));
        assert!(parse_command(long.as_bytes()).is_err());
    }

    #[test]
    fn tokens_are_long_and_stable_within_a_run() {
        assert_eq!(token().len(), 32);
        assert_eq!(token(), token());
        assert_ne!(make_token(), make_token());
    }

    #[test]
    fn transcript_names_cannot_escape_the_folder() {
        assert!(safe_transcript_name("2026-11-06_12-00-00_1234.jsonl"));
        assert!(safe_transcript_name("my engine.jsonl"));
        assert!(!safe_transcript_name("../secret.jsonl"));
        assert!(!safe_transcript_name("a/b.jsonl"));
        assert!(!safe_transcript_name("a\\b.jsonl"));
        assert!(!safe_transcript_name("C:evil.jsonl"));
        assert!(!safe_transcript_name(".hidden.jsonl"));
        assert!(!safe_transcript_name("notes.txt"));
        assert!(!safe_transcript_name(""));
    }

    #[test]
    fn line_reader_joins_split_lines_and_keeps_partials() {
        struct Chunks(Vec<io::Result<Vec<u8>>>);
        impl Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                match self.0.remove(0) {
                    Ok(bytes) => {
                        buf[..bytes.len()].copy_from_slice(&bytes);
                        Ok(bytes.len())
                    }
                    Err(e) => Err(e),
                }
            }
        }
        let mut r = LineReader::new(Chunks(vec![
            Ok(b"one\ntw".to_vec()),
            Err(io::Error::from(io::ErrorKind::TimedOut)),
            Ok(b"o\r\n\nthree".to_vec()),
        ]));
        assert!(matches!(r.next(), Next::Line(l) if l == "one"));
        assert!(matches!(r.next(), Next::Idle));
        assert!(matches!(r.next(), Next::Line(l) if l == "two"));
        assert!(matches!(r.next(), Next::Line(l) if l.is_empty()));
        assert!(matches!(r.next(), Next::End));
    }
}
