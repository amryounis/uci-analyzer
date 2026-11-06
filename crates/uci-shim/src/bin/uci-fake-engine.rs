//! uci-fake-engine: a scripted UCI engine for tests and demos. It plays no
//! chess; it answers like an engine so you can watch a UCI conversation in the
//! viewer without installing anything.
//!
//! During `go` it keeps sending `info` lines (depth climbing, a wobbling score)
//! until `stop`, like a real engine analysing a position.
//!
//! Behaviour switches (environment variables):
//!   FAKE_EXIT_CODE=n   exit code used on `quit` (default 0)
//!   FAKE_IGNORE_EOF=1  keep running when stdin closes (tests the shim's kill)
//!
//! Extra commands: `echorawline` replies with the exact bytes received (to prove
//! the shim forwards unmodified), `crash` writes to stderr and exits with 7.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

const UCI_REPLY: &[&str] = &[
    "id name FakeFish 0.1",
    "id author uci-visualizer",
    "option name Hash type spin default 16 min 1 max 33554432",
    "option name Threads type spin default 1 min 1 max 1024",
    "option name MultiPV type spin default 1 min 1 max 256",
    "option name Ponder type check default false",
    "option name UCI_Chess960 type check default false",
    "option name Style type combo default Normal var Solid var Normal var Very Aggressive",
    "option name Clear Hash type button",
    "option name EvalFile type string default <empty>",
    "uciok",
];

const LINES: &[&str] = &[
    "e2e4 e7e5 g1f3 b8c6 f1b5 a7a6 b5a4 g8f6",
    "d2d4 d7d5 c2c4 e7e6 b1c3 g8f6 c1g5 f8e7",
    "g1f3 d7d5 g2g3 g8f6 f1g2 e7e6 e1g1 f8e7",
];

const TICK: Duration = Duration::from_millis(40);

struct Search {
    ticks: u32,
    /// `None` runs until `stop`.
    ticks_left: Option<u32>,
}

fn main() {
    let exit_code: i32 = std::env::var("FAKE_EXIT_CODE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let ignore_eof = std::env::var_os("FAKE_IGNORE_EOF").is_some();

    let (tx, rx) = mpsc::channel::<Option<String>>();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut raw = String::new();
            match input.read_line(&mut raw) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
                Ok(_) => {
                    if tx.send(Some(raw)).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut search: Option<Search> = None;

    macro_rules! say {
        ($($arg:tt)*) => {{
            if writeln!(out, $($arg)*).and_then(|_| out.flush()).is_err() {
                std::process::exit(1);
            }
        }};
    }

    loop {
        let received = if search.is_some() {
            rx.recv_timeout(TICK)
        } else {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        let raw = match received {
            Ok(Some(raw)) => raw,
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                if ignore_eof {
                    loop {
                        thread::sleep(Duration::from_secs(1));
                    }
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                // One search tick: a new info line, maybe the finish.
                if let Some(s) = search.as_mut() {
                    s.ticks += 1;
                    let t = s.ticks;
                    let depth = if t <= 12 { t } else { 12 + (t - 12) / 5 }.min(34);
                    let wobble = ((t * 7) % 23) as i32 - 11;
                    let cp = 18 + depth as i32 * 2 + wobble;
                    let line = LINES[(t as usize / 9) % LINES.len()];
                    say!(
                        "info depth {depth} seldepth {} multipv 1 score cp {cp} nodes {} nps {} hashfull {} tbhits 0 time {} pv {line}",
                        depth + 3 + t % 4,
                        t * 21_000,
                        520_000 + (t * 13) % 40_000,
                        (t / 4).min(990),
                        t * 40,
                    );
                    if t % 17 == 0 {
                        say!("info string tablebases 0 positions found");
                    }
                    if let Some(left) = s.ticks_left.as_mut() {
                        *left -= 1;
                        if *left == 0 {
                            search = None;
                            say!("bestmove e2e4 ponder e7e5");
                        }
                    }
                }
                continue;
            }
        };

        let command = raw.trim();
        match command.split_whitespace().next().unwrap_or("") {
            "uci" => {
                for line in UCI_REPLY {
                    say!("{line}");
                }
            }
            "isready" => say!("readyok"),
            "go" => {
                let infinite = command.split_whitespace().any(|t| t == "infinite");
                search = Some(Search {
                    ticks: 0,
                    ticks_left: if infinite { None } else { Some(8) },
                });
            }
            "stop" => {
                if search.take().is_some() {
                    say!("bestmove e2e4");
                }
            }
            "echorawline" => say!("rawline {raw:?}"),
            "crash" => {
                eprintln!("fake engine crashing: simulated failure");
                std::process::exit(7);
            }
            "quit" => std::process::exit(exit_code),
            // setoption, position, ucinewgame, debug and unknown commands are
            // silently accepted, as the UCI spec asks.
            _ => {}
        }
    }
}
