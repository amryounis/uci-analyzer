//! End-to-end tests: run the real `uci-shim` binary in front of the scripted
//! `uci-fake-engine` and check what the host GUI sees and what the transcript
//! records.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use uci_core::{Dir, Event, SessionInfo};

const SHIM: &str = env!("CARGO_BIN_EXE_uci-shim");
const FAKE: &str = env!("CARGO_BIN_EXE_uci-fake-engine");

struct Shim {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<String>,
}

impl Shim {
    fn spawn(args: &[&str], envs: &[(&str, &str)]) -> Shim {
        let mut child = Command::new(SHIM)
            .args(args)
            .env("UCI_VISUALIZER_HOME", test_home())
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start uci-shim");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let _ = tx.send(line.trim_end_matches(['\r', '\n']).to_string());
                    }
                }
            }
        });
        Shim { child, stdin, rx }
    }

    fn send(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        stdin.write_all(text.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// Reads lines until `pred` matches one; returns everything read.
    fn read_until(&self, pred: impl Fn(&str) -> bool) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            match self.rx.recv_timeout(Duration::from_secs(10)) {
                Ok(line) => {
                    let done = pred(&line);
                    seen.push(line);
                    if done {
                        return seen;
                    }
                }
                Err(e) => panic!("gave up waiting for a line ({e}); saw {seen:?}"),
            }
        }
    }

    fn wait_exit(&mut self, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("shim did not exit within {timeout:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Keeps the session registry of test shims out of the real data folder.
fn test_home() -> PathBuf {
    std::env::temp_dir().join(format!("uci-shim-home-{}", std::process::id()))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uci-shim-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn read_transcript(dir: &Path) -> Vec<Event> {
    let files: Vec<_> = std::fs::read_dir(dir)
        .expect("log dir exists")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    assert_eq!(files.len(), 1, "expected exactly one transcript in {dir:?}");
    std::fs::read_to_string(&files[0])
        .unwrap()
        .lines()
        .map(|l| Event::from_json(l).unwrap_or_else(|e| panic!("bad transcript line {l:?}: {e}")))
        .collect()
}

fn lines_for(events: &[Event], dir: Dir) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.dir == dir)
        .map(|e| e.line.clone())
        .collect()
}

#[test]
fn forwards_a_full_session_and_records_it() {
    let dir = temp_dir("session");
    let mut shim = Shim::spawn(&["--engine", FAKE, "--log-dir", path(&dir)], &[]);
    let mut received = Vec::new();

    shim.send("uci\n");
    received.extend(shim.read_until(|l| l == "uciok"));
    assert!(received.contains(&"id name FakeFish 0.1".to_string()));
    assert!(
        received
            .iter()
            .filter(|l| l.starts_with("option name"))
            .count()
            >= 6
    );

    shim.send("isready\n");
    received.extend(shim.read_until(|l| l == "readyok"));

    shim.send("go infinite\n");
    received.extend(shim.read_until(|l| l.starts_with("info depth 4")));
    shim.send("stop\n");
    received.extend(shim.read_until(|l| l.starts_with("bestmove e2e4")));

    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);

    let events = read_transcript(&dir);
    assert_eq!(
        lines_for(&events, Dir::GuiToEngine),
        ["uci", "isready", "go infinite", "stop", "quit"]
    );
    assert_eq!(
        lines_for(&events, Dir::EngineToGui),
        received,
        "the transcript must hold exactly what the GUI received"
    );
    let shim_notes = lines_for(&events, Dir::Shim);
    assert!(shim_notes.first().unwrap().starts_with("shim start"));
    assert!(shim_notes
        .last()
        .unwrap()
        .starts_with("engine exited: code=0"));
    let uci_at = events.iter().position(|e| e.line == "uci").unwrap();
    let uciok_at = events.iter().position(|e| e.line == "uciok").unwrap();
    assert!(uci_at < uciok_at, "a command is logged before its reply");
}

#[test]
fn forwards_bytes_unmodified() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log"], &[]);
    shim.send("echorawline a\tb  c\r\n");
    let seen = shim.read_until(|l| l.starts_with("rawline"));
    assert_eq!(
        seen.last().unwrap(),
        "rawline \"echorawline a\\tb  c\\r\\n\"",
        "tabs, double spaces and CRLF must reach the engine untouched"
    );
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn engine_exit_code_is_propagated() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log"], &[("FAKE_EXIT_CODE", "3")]);
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 3);
}

#[test]
fn engine_stderr_is_recorded_and_crash_code_propagated() {
    let dir = temp_dir("crash");
    let mut shim = Shim::spawn(&["--engine", FAKE, "--log-dir", path(&dir)], &[]);
    shim.send("crash\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 7);
    let events = read_transcript(&dir);
    let stderr = lines_for(&events, Dir::EngineStderr);
    assert!(
        stderr.iter().any(|l| l.contains("fake engine crashing")),
        "stderr lines: {stderr:?}"
    );
}

#[test]
fn closing_the_input_ends_a_cooperative_engine() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log"], &[]);
    shim.send("isready\n");
    shim.read_until(|l| l == "readyok");
    shim.close_stdin();
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn a_stubborn_engine_is_killed_after_the_grace_period() {
    let dir = temp_dir("stubborn");
    let mut shim = Shim::spawn(
        &[
            "--engine",
            FAKE,
            "--log-dir",
            path(&dir),
            "--eof-grace-ms",
            "300",
        ],
        &[("FAKE_IGNORE_EOF", "1")],
    );
    shim.send("isready\n");
    shim.read_until(|l| l == "readyok");
    shim.close_stdin();
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
    let notes = lines_for(&read_transcript(&dir), Dir::Shim);
    assert!(
        notes.iter().any(|l| l.contains("killing it")),
        "shim notes: {notes:?}"
    );
}

#[test]
fn a_missing_engine_explains_itself_to_the_gui() {
    let mut shim = Shim::spawn(
        &["--engine", "/definitely/not/here/engine", "--no-log"],
        &[],
    );
    shim.send("uci\n");
    let seen = shim.read_until(|l| l == "uciok");
    assert!(
        seen.iter().any(|l| l.contains("cannot start engine")),
        "lines: {seen:?}"
    );
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 1);
}

#[test]
fn no_engine_configured_explains_itself_to_the_gui() {
    let mut shim = Shim::spawn(&["--config", "/definitely/not/here.cfg", "--no-log"], &[]);
    shim.send("uci\n");
    let seen = shim.read_until(|l| l == "uciok");
    assert!(
        seen.iter().any(|l| l.contains("no engine configured")),
        "lines: {seen:?}"
    );
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 1);
}

#[test]
fn engine_args_after_double_dash_reach_the_engine() {
    // The fake engine ignores arguments, so check the shim records them.
    let dir = temp_dir("args");
    let mut shim = Shim::spawn(
        &[
            "--engine",
            FAKE,
            "--log-dir",
            path(&dir),
            "--",
            "--threads",
            "4",
        ],
        &[],
    );
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
    let notes = lines_for(&read_transcript(&dir), Dir::Shim);
    assert!(
        notes.iter().any(|l| l.contains("\"--threads\", \"4\"")),
        "shim notes: {notes:?}"
    );
}

#[test]
fn no_log_writes_nothing() {
    let dir = temp_dir("nolog");
    let mut shim = Shim::spawn(
        &["--engine", FAKE, "--no-log", "--log-dir", path(&dir)],
        &[],
    );
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
    assert!(!dir.exists(), "no transcript folder should be created");
}

// ---------------------------------------------------------------------------
// Viewer server
// ---------------------------------------------------------------------------

struct Viewer {
    reader: BufReader<TcpStream>,
}

impl Viewer {
    fn connect(port: u16) -> Viewer {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the shim");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Viewer {
            reader: BufReader::new(stream),
        }
    }

    /// Sends a manual command the way the viewer program does.
    fn send_cmd(&mut self, cmd: &str) {
        let mut line = String::from("{\"cmd\":\"");
        uci_core::json::escape_into(&mut line, cmd);
        line.push_str("\"}\n");
        self.send_raw(&line);
    }

    fn send_raw(&mut self, text: &str) {
        let mut stream = self.reader.get_ref();
        stream.write_all(text.as_bytes()).unwrap();
        stream.flush().unwrap();
    }

    /// `None` at end of stream.
    fn next_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line.trim_end().to_string()),
            Err(e) => panic!("viewer read failed: {e}"),
        }
    }

    fn read_until(&mut self, pred: impl Fn(&str) -> bool) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            let line = self
                .next_line()
                .unwrap_or_else(|| panic!("stream ended early; saw {seen:?}"));
            let done = pred(&line);
            seen.push(line);
            if done {
                return seen;
            }
        }
    }
}

fn wait_for_session(pid: u32) -> SessionInfo {
    let file = test_home().join("sessions").join(format!("{pid}.json"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(info) = SessionInfo::from_json(&text) {
                return info;
            }
        }
        assert!(Instant::now() < deadline, "no session file at {file:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn is_line(text: &'static str) -> impl Fn(&str) -> bool {
    move |l| l.contains(&format!("\"line\":\"{text}\""))
}

#[test]
fn viewers_get_hello_backlog_and_live_events() {
    let log_dir = temp_dir("viewer");
    let mut shim = Shim::spawn(&["--engine", FAKE, "--log-dir", path(&log_dir)], &[]);
    let pid = shim.child.id();
    let info = wait_for_session(pid);
    assert_eq!(info.pid, pid);
    assert!(info.engine.contains("uci-fake-engine"));
    assert!(info.transcript.ends_with(".jsonl"));

    shim.send("uci\n");
    shim.read_until(|l| l == "uciok");

    // A viewer that joins late still sees everything from the start.
    let mut a = Viewer::connect(info.port);
    let hello = a.next_line().unwrap();
    assert!(hello.contains("\"hello\":\"uci-shim\""), "{hello}");
    assert!(hello.contains(&format!("\"pid\":{pid}")), "{hello}");
    let mut a_events = a.read_until(is_line("uciok"));
    assert!(a_events[0].contains("shim start"), "{:?}", a_events[0]);
    assert!(a_events
        .iter()
        .any(|l| l.contains("\"dir\":\"gui>engine\"") && is_line("uci")(l)));

    // Live events reach it as they happen.
    shim.send("isready\n");
    shim.read_until(|l| l == "readyok");
    a_events.extend(a.read_until(is_line("readyok")));

    // A second late viewer gets exactly the same history: nothing lost or doubled.
    let mut b = Viewer::connect(info.port);
    b.next_line().unwrap();
    let b_events = b.read_until(is_line("readyok"));
    assert_eq!(a_events, b_events);

    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);

    // Both viewers receive the closing note, then end of stream.
    let tail_a = a.read_until(|l| l.contains("engine exited"));
    assert!(tail_a.last().unwrap().contains("code=0"));
    while a.next_line().is_some() {}
    while b.next_line().is_some() {}
    assert!(
        !test_home()
            .join("sessions")
            .join(format!("{pid}.json"))
            .exists(),
        "the session entry is removed when the shim exits"
    );
}

#[test]
fn no_viewer_means_no_server_and_no_session_entry() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log", "--no-viewer"], &[]);
    let pid = shim.child.id();
    shim.send("isready\n");
    shim.read_until(|l| l == "readyok");
    assert!(!test_home()
        .join("sessions")
        .join(format!("{pid}.json"))
        .exists());
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn a_viewer_that_disconnects_does_not_disturb_the_engine() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log"], &[]);
    let info = wait_for_session(shim.child.id());
    {
        let mut v = Viewer::connect(info.port);
        v.next_line().unwrap();
    } // dropped: connection closed abruptly
    for _ in 0..50 {
        shim.send("isready\n");
        shim.read_until(|l| l == "readyok");
    }
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
}

fn from_viewer(text: &'static str) -> impl Fn(&str) -> bool {
    move |l| l.contains("\"dir\":\"viewer>engine\"") && l.contains(&format!("\"line\":\"{text}\""))
}

#[test]
fn commands_typed_in_the_viewer_reach_the_engine_and_are_recorded() {
    let log_dir = temp_dir("manual");
    let mut shim = Shim::spawn(&["--engine", FAKE, "--log-dir", path(&log_dir)], &[]);
    let pid = shim.child.id();
    let info = wait_for_session(pid);
    assert!(info.input, "manual input is on by default");

    let mut v = Viewer::connect(info.port);
    let hello = v.next_line().unwrap();
    assert!(hello.contains("\"input\":true"), "{hello}");

    // The engine answers a command nobody at the host GUI sent. The answer
    // is shown to the viewer, and also reaches the host GUI as normal output.
    v.send_cmd("isready");
    let seen = v.read_until(|l| l.contains("\"dir\":\"engine>gui\"") && l.contains("readyok"));
    assert!(
        seen.iter().any(|l| from_viewer("isready")(l)),
        "recorded as typed in the viewer, not as the GUI: {seen:?}"
    );
    shim.read_until(|l| l == "readyok");

    // The host GUI keeps working alongside.
    shim.send("uci\n");
    shim.read_until(|l| l == "uciok");
    v.read_until(is_line("uciok"));

    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);

    let transcript = std::fs::read_dir(&log_dir)
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .next()
        .unwrap();
    let events: Vec<Event> = transcript
        .lines()
        .map(|l| Event::from_json(l).unwrap())
        .collect();
    let typed: Vec<_> = events
        .iter()
        .filter(|e| e.dir == Dir::ViewerToEngine)
        .collect();
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].line, "isready");
    assert!(!events
        .iter()
        .any(|e| e.dir == Dir::GuiToEngine && e.line == "isready"));
}

#[test]
fn refused_viewer_commands_are_noted_and_the_connection_survives() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log"], &[]);
    let pid = shim.child.id();
    let info = wait_for_session(pid);
    let mut v = Viewer::connect(info.port);
    v.next_line().unwrap();

    v.send_cmd("two\nlines");
    v.send_cmd("bell\u{7}");
    v.send_raw("this is not json\n");
    v.send_raw("{\"cmd\":5}\n");
    v.send_cmd("   ");
    let seen = v.read_until(|l| l.contains("refused") && l.contains("plain text"));
    assert!(
        seen.iter().all(|l| !l.contains("viewer>engine")),
        "{seen:?}"
    );
    // Both bad commands are noted; nothing was typed into the engine.
    v.read_until(|l| l.contains("refused") && l.contains("plain text"));

    v.send_cmd("isready");
    v.read_until(from_viewer("isready"));
    v.read_until(is_line("readyok"));
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn viewer_input_can_be_switched_off() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log", "--no-input"], &[]);
    let pid = shim.child.id();
    let info = wait_for_session(pid);
    assert!(!info.input);
    let mut v = Viewer::connect(info.port);
    assert!(v.next_line().unwrap().contains("\"input\":false"));

    v.send_cmd("isready");
    let seen = v.read_until(|l| l.contains("switched off"));
    assert!(seen.iter().all(|l| !l.contains("viewer>engine")));

    // The engine really did not get it: the next thing it says is an answer
    // to the host GUI's own command.
    shim.send("isready\n");
    let seen = v.read_until(is_line("readyok"));
    assert!(
        seen.iter().all(|l| !l.contains("viewer>engine")),
        "{seen:?}"
    );
    shim.send("quit\n");
    assert_eq!(shim.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn manual_commands_after_the_gui_closed_are_not_delivered() {
    let mut shim = Shim::spawn(&["--engine", FAKE, "--no-log"], &[("FAKE_IGNORE_EOF", "1")]);
    let pid = shim.child.id();
    let info = wait_for_session(pid);
    let mut v = Viewer::connect(info.port);
    v.next_line().unwrap();
    shim.close_stdin();
    v.read_until(|l| l.contains("gui closed its input"));
    v.send_cmd("isready");
    let seen = v.read_until(|l| l.contains("refused") && l.contains("input is closed"));
    assert!(
        seen.iter().all(|l| !l.contains("viewer>engine")),
        "{seen:?}"
    );
    let _ = shim.wait_exit(Duration::from_secs(8));
}
