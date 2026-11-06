//! End-to-end tests: run the real `uci-viewer` binary, stand in for a shim
//! with a small TCP server, and talk to the viewer over HTTP like a browser.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use uci_core::{Dir, Event, SessionInfo};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "uci-viewer-test-{}-{}-{}",
        std::process::id(),
        name,
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sessions")).unwrap();
    fs::create_dir_all(dir.join("transcripts")).unwrap();
    dir
}

struct Viewer {
    child: Child,
    port: u16,
}

impl Viewer {
    fn start(home: &Path) -> Viewer {
        Viewer::start_with(home, &[], &[])
    }

    fn start_with(home: &Path, extra_args: &[&str], envs: &[(&str, &Path)]) -> Viewer {
        let mut command = Command::new(env!("CARGO_BIN_EXE_uci-viewer"));
        command
            .args(["--port", "0", "--no-open"])
            .args(extra_args)
            .env("UCI_VISUALIZER_HOME", home)
            .stdout(Stdio::piped());
        for (k, v) in envs {
            command.env(k, v);
        }
        let mut child = command.spawn().expect("start uci-viewer");
        let mut first = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut first)
            .unwrap();
        let port = first
            .trim()
            .rsplit(':')
            .next()
            .unwrap()
            .trim_end_matches('/')
            .parse()
            .unwrap_or_else(|_| panic!("no port in {first:?}"));
        Viewer { child, port }
    }

    fn get(&self, path: &str) -> Response {
        self.request("GET", path, &format!("127.0.0.1:{}", self.port))
    }

    fn request(&self, method: &str, path: &str, host: &str) -> Response {
        self.request_with(method, path, host, &[], "")
    }

    fn post(&self, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let host = format!("127.0.0.1:{}", self.port);
        self.request_with("POST", path, &host, headers, body)
    }

    fn request_with(
        &self,
        method: &str,
        path: &str,
        host: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut extra = String::new();
        for (k, v) in headers {
            extra.push_str(&format!("{k}: {v}\r\n"));
        }
        write!(
            conn,
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut raw = Vec::new();
        let _ = conn.read_to_end(&mut raw);
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Response {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

/// Pretends to be a shim: every connection gets the hello line, the given
/// events, and then the connection is closed.
fn fake_shim(home: &Path, pid: u32, events: Vec<Event>) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let info = SessionInfo {
        pid,
        port,
        engine: "C:\\engines\\stockfish.exe".to_string(),
        started_unix_ms: 1_700_000_000_000,
        transcript: String::new(),
        input: true,
    };
    fs::write(
        home.join("sessions").join(format!("{pid}.json")),
        info.to_json(),
    )
    .unwrap();
    let hello = info.hello_json("test");
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let _ = writeln!(conn, "{hello}");
            for e in &events {
                let _ = writeln!(conn, "{}", e.to_json());
            }
        }
    });
    port
}

/// Like `fake_shim`, but the connection stays open and every line the viewer
/// sends is handed back through the channel.
fn fake_shim_with_input(home: &Path, pid: u32) -> (u16, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let info = SessionInfo {
        pid,
        port,
        engine: "engine.exe".to_string(),
        started_unix_ms: 1,
        transcript: String::new(),
        input: true,
    };
    fs::write(
        home.join("sessions").join(format!("{pid}.json")),
        info.to_json(),
    )
    .unwrap();
    let hello = info.hello_json("test");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let _ = writeln!(conn, "{hello}");
            let tx = tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(conn).lines() {
                    match line {
                        Ok(l) => {
                            let _ = tx.send(l);
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });
    (port, rx)
}

fn ev(t_ms: u64, dir: Dir, line: &str) -> Event {
    Event {
        t_ms,
        dir,
        line: line.to_string(),
    }
}

#[test]
fn serves_the_page_and_refuses_strangers() {
    let home = temp_home("page");
    let viewer = Viewer::start(&home);

    let page = viewer.get("/");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("UCI Analyzer"));
    assert!(page.head.contains("Content-Security-Policy"));

    // A request addressed to another name is how a hostile web page would
    // reach a local server.
    let stranger = viewer.request("GET", "/api/sessions", "evil.example");
    assert_eq!(stranger.status, 403);

    assert_eq!(
        viewer
            .request("POST", "/", &format!("127.0.0.1:{}", viewer.port))
            .status,
        405
    );
    assert_eq!(viewer.get("/nope").status, 404);
    assert_eq!(viewer.get("/api/stream").status, 400);
}

#[test]
fn lists_running_shims_and_forgets_dead_ones() {
    let home = temp_home("sessions");
    let viewer = Viewer::start(&home);
    fake_shim(&home, 4242, vec![]);

    // A registry entry whose shim has died: nothing listens on its port.
    let dead_port = {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let dead = SessionInfo {
        pid: 777,
        port: dead_port,
        engine: "gone.exe".into(),
        started_unix_ms: 1,
        transcript: String::new(),
        input: true,
    };
    let dead_file = home.join("sessions").join("777.json");
    fs::write(&dead_file, dead.to_json()).unwrap();

    let list = viewer.get("/api/sessions");
    assert_eq!(list.status, 200);
    assert!(list.body.contains("\"pid\":4242"), "{}", list.body);
    assert!(!list.body.contains("\"pid\":777"), "{}", list.body);
    assert!(!dead_file.exists(), "stale entry is cleaned up");
}

#[test]
fn live_stream_decodes_packets_and_ends_with_the_shim() {
    let home = temp_home("live");
    let viewer = Viewer::start(&home);
    fake_shim(
        &home,
        4243,
        vec![
            ev(0, Dir::GuiToEngine, "go infinite"),
            ev(
                40,
                Dir::EngineToGui,
                "info depth 5 score cp 33 nodes 1000 pv e2e4 e7e5",
            ),
            ev(41, Dir::EngineStderr, "warning: \"quoted\" \\ text"),
            ev(50, Dir::EngineToGui, "bestmove e2e4 ponder e7e5"),
        ],
    );

    let stream = viewer.get("/api/stream?pid=4243");
    assert_eq!(stream.status, 200);
    assert!(stream.head.contains("text/event-stream"), "{}", stream.head);
    let body = stream.body;
    assert!(body.starts_with("event: hello\n"), "{body}");
    assert!(body.contains("\"hello\":\"uci-shim\""));
    // Packets carry their running number and a decoded form.
    assert!(body.contains("\"i\":0"));
    assert!(body.contains("\"type\":\"go\""));
    assert!(body.contains("\"i\":1"));
    assert!(body.contains("\"depth\":5"));
    assert!(body.contains("\"kind\":\"cp\""));
    assert!(body.contains("\"i\":3"));
    assert!(body.contains("\"type\":\"bestmove\""));
    // stderr is passed on verbatim, correctly escaped, with no decoding.
    assert!(
        body.contains(r#""line":"warning: \"quoted\" \\ text""#),
        "{body}"
    );
    assert!(body.trim_end().ends_with("event: end\ndata: {}"), "{body}");

    // Unknown session.
    assert_eq!(viewer.get("/api/stream?pid=999999").status, 404);
}

#[test]
fn saved_transcripts_can_be_listed_replayed_and_downloaded() {
    let home = temp_home("files");
    let viewer = Viewer::start(&home);
    let lines = [
        ev(0, Dir::GuiToEngine, "uci").to_json(),
        ev(5, Dir::EngineToGui, "uciok").to_json(),
        "this line is damaged".to_string(),
        ev(9, Dir::GuiToEngine, "quit").to_json(),
    ];
    fs::write(
        home.join("transcripts").join("game one.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();
    // A secret outside the transcripts folder.
    fs::write(
        home.join("secret.jsonl"),
        "{\"t\":0,\"dir\":\"shim\",\"line\":\"secret\"}\n",
    )
    .unwrap();

    let list = viewer.get("/api/sessions");
    assert!(
        list.body.contains("\"file\":\"game one.jsonl\""),
        "{}",
        list.body
    );

    let replay = viewer.get("/api/stream?file=game%20one.jsonl");
    assert_eq!(replay.status, 200);
    assert!(replay.body.contains("\"hello\":\"transcript\""));
    assert!(replay.body.contains("\"type\":\"uciok\""));
    // The damaged line is skipped, and numbering carries on without a gap.
    assert!(replay.body.contains("\"i\":2"));
    assert!(replay.body.contains("\"type\":\"quit\""));
    assert!(!replay.body.contains("damaged"));
    assert!(replay.body.trim_end().ends_with("event: end\ndata: {}"));

    let raw = viewer.get("/api/transcript?file=game%20one.jsonl");
    assert_eq!(raw.status, 200);
    assert!(raw.body.contains("this line is damaged"));

    // Names that point outside the folder are refused.
    for bad in [
        "../secret.jsonl",
        "..%2Fsecret.jsonl",
        "%2e%2e/secret.jsonl",
        "sub/x.jsonl",
        "game one.txt",
    ] {
        for route in ["/api/stream?file=", "/api/transcript?file="] {
            let r = viewer.get(&format!("{route}{bad}"));
            assert_eq!(r.status, 404, "{route}{bad}");
            assert!(!r.body.contains("secret"), "{route}{bad}");
        }
    }
}

fn page_token(viewer: &Viewer) -> String {
    let page = viewer.get("/").body;
    let start = page.find("const TOKEN = \"").expect("token in page") + "const TOKEN = \"".len();
    let end = page[start..].find('"').unwrap() + start;
    let token = page[start..end].to_string();
    assert_eq!(token.len(), 32, "the placeholder was replaced: {token}");
    token
}

/// Opens the live stream the way the page does and waits until it is up.
fn open_stream(viewer: &Viewer, pid: u32) -> TcpStream {
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, viewer.port)).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        conn,
        "GET /api/stream?pid={pid} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        viewer.port
    )
    .unwrap();
    let mut seen = String::new();
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    while !seen.contains("event: hello") {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "stream ended: {seen}"
        );
        seen.push_str(&line);
    }
    conn
}

#[test]
fn a_command_typed_in_the_page_reaches_the_shim() {
    let home = temp_home("send");
    let viewer = Viewer::start(&home);
    let (_port, received) = fake_shim_with_input(&home, 5150);
    let token = page_token(&viewer);
    let origin = format!("http://127.0.0.1:{}", viewer.port);

    // Nothing is open yet: the viewer has no connection to send on.
    let early = viewer.post(
        "/api/send?pid=5150",
        &[("X-UCI-Token", &token)],
        r#"{"cmd":"isready"}"#,
    );
    assert_eq!(early.status, 409);

    let _stream = open_stream(&viewer, 5150);

    let ok = viewer.post(
        "/api/send?pid=5150",
        &[("X-UCI-Token", &token), ("Origin", &origin)],
        r#"{"cmd":"setoption name Hash value \"64\""}"#,
    );
    assert_eq!(ok.status, 200, "{}", ok.body);
    let line = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(line, r#"{"cmd":"setoption name Hash value \"64\""}"#);

    // Without the page's secret, or from another site, nothing is sent.
    let no_token = viewer.post("/api/send?pid=5150", &[], r#"{"cmd":"quit"}"#);
    assert_eq!(no_token.status, 403);
    let wrong_token = viewer.post(
        "/api/send?pid=5150",
        &[("X-UCI-Token", "0000")],
        r#"{"cmd":"quit"}"#,
    );
    assert_eq!(wrong_token.status, 403);
    let other_site = viewer.post(
        "/api/send?pid=5150",
        &[("X-UCI-Token", &token), ("Origin", "https://evil.example")],
        r#"{"cmd":"quit"}"#,
    );
    assert_eq!(other_site.status, 403);

    // Bad commands are turned away before they reach the shim.
    for body in [r#"{"cmd":"two\nlines"}"#, r#"{"cmd":""}"#, "nonsense"] {
        let r = viewer.post("/api/send?pid=5150", &[("X-UCI-Token", &token)], body);
        assert_eq!(r.status, 400, "{body}");
    }
    assert_eq!(
        viewer
            .post("/api/send", &[("X-UCI-Token", &token)], r#"{"cmd":"x"}"#)
            .status,
        400
    );
    assert!(
        received.recv_timeout(Duration::from_millis(300)).is_err(),
        "nothing else may arrive"
    );
}

fn json_field(body: &str, key: &str) -> Option<String> {
    let at = body.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = body[at..].trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"').to_string())
}

#[test]
fn loading_an_engine_needs_the_page_token_and_a_real_file() {
    let home = temp_home("launch-refused");
    let viewer = Viewer::start(&home);
    let body = r#"{"engine":"/no/such/engine","args":""}"#;

    let r = viewer.post("/api/launch", &[], body);
    assert_eq!(r.status, 403, "no token: {}", r.body);

    let token = page_token(&viewer);
    let stranger = format!("http://evil.example:{}", viewer.port);
    let r = viewer.post(
        "/api/launch",
        &[
            ("X-UCI-Token", token.as_str()),
            ("Origin", stranger.as_str()),
        ],
        body,
    );
    assert_eq!(r.status, 403, "foreign origin: {}", r.body);

    let r = viewer.post("/api/launch", &[("X-UCI-Token", token.as_str())], body);
    assert_eq!(r.status, 422);
    assert!(r.body.contains("no file"), "{}", r.body);

    let r = viewer.post("/api/launch", &[("X-UCI-Token", token.as_str())], "{}");
    assert_eq!(r.status, 400);

    let r = viewer.post("/api/browse", &[], "");
    assert_eq!(r.status, 403);
    let r = viewer.post("/api/nothing", &[], "");
    assert_eq!(r.status, 405);
}

#[test]
fn no_launch_switches_loading_off() {
    let home = temp_home("no-launch");
    let viewer = Viewer::start_with(&home, &["--no-launch"], &[]);
    let r = viewer.get("/api/engines");
    assert_eq!(r.status, 200);
    assert_eq!(json_field(&r.body, "launch").as_deref(), Some("false"));
    let token = page_token(&viewer);
    let engine = std::env::current_exe().unwrap();
    let body = format!(
        "{{\"engine\":\"{}\",\"args\":\"\"}}",
        engine.display().to_string().replace('\\', "\\\\")
    );
    let r = viewer.post("/api/launch", &[("X-UCI-Token", token.as_str())], &body);
    assert_eq!(r.status, 422);
    assert!(r.body.contains("--no-launch"), "{}", r.body);
}

#[cfg(unix)]
#[test]
fn an_engine_loaded_in_the_viewer_runs_through_the_shim_and_is_remembered() {
    use std::os::unix::fs::PermissionsExt;

    let home = temp_home("launch-ok");
    // Stands in for uci-shim: records how it was started, then stays up
    // until its input is closed, as the real shim does.
    let shim = home.join("fake-shim.sh");
    let record = home.join("shim-args.txt");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > '{}'\ncat > /dev/null\n",
            record.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

    let viewer = Viewer::start_with(&home, &[], &[("UCI_SHIM_EXE", shim.as_path())]);
    let token = page_token(&viewer);
    let engine = shim.display().to_string();
    let body = format!("{{\"engine\":\"\\\"{engine}\\\"\",\"args\":\"--net \\\"a b\\\"\"}}");
    let r = viewer.post("/api/launch", &[("X-UCI-Token", token.as_str())], &body);
    assert_eq!(r.status, 200, "{}", r.body);
    let pid: u32 = json_field(&r.body, "pid").unwrap().parse().unwrap();

    thread::sleep(Duration::from_millis(400));
    assert!(
        Path::new(&format!("/proc/{pid}")).exists(),
        "the shim should still be running, held open by the viewer"
    );
    let args = fs::read_to_string(&record).unwrap();
    let args: Vec<&str> = args.lines().collect();
    let at = args.iter().position(|a| *a == "--engine").unwrap();
    assert_eq!(args[at + 1], engine, "quotes around the path are removed");
    assert_eq!(&args[at + 2..], ["--", "--net", "a b"]);

    let r = viewer.get("/api/engines");
    assert_eq!(json_field(&r.body, "launch").as_deref(), Some("true"));
    assert!(r.body.contains(&engine), "remembered: {}", r.body);
    assert!(r.body.contains("--net"), "{}", r.body);
}
