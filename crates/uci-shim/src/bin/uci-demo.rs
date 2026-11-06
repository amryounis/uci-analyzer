//! uci-demo: shows the analyzer with made-up traffic, no chess program needed.
//!
//! It starts `uci-viewer` (which opens the window), then plays a short
//! scripted session through `uci-shim` with the pretend engine
//! `uci-fake-engine`, the way a chess program would.
//!
//! All three programs are expected in the same folder as this one.

use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::Duration;

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn sibling(base: &str) -> PathBuf {
    let me = env::current_exe().unwrap_or_default();
    me.parent()
        .map(|d| d.join(exe_name(base)))
        .unwrap_or_else(|| PathBuf::from(exe_name(base)))
}

fn pause(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

fn send(stdin: &mut ChildStdin, line: &str) -> io::Result<()> {
    writeln!(stdin, "{line}")?;
    stdin.flush()
}

/// One pretend game session: what a chess program does when you start an
/// analysis and let it run for a few moves.
fn play_session(shim: &mut Child) -> io::Result<()> {
    let mut stdin = shim.stdin.take().expect("piped stdin");
    send(&mut stdin, "uci")?;
    pause(600);
    send(&mut stdin, "setoption name Hash value 256")?;
    send(&mut stdin, "setoption name Threads value 4")?;
    send(&mut stdin, "setoption name Style value Very Aggressive")?;
    send(&mut stdin, "isready")?;
    pause(400);
    send(&mut stdin, "ucinewgame")?;

    let moves = ["e2e4", "e7e5", "g1f3", "b8c6", "f1b5", "a7a6"];
    for n in 1..=moves.len() {
        let played = moves[..n].join(" ");
        send(&mut stdin, &format!("position startpos moves {played}"))?;
        send(
            &mut stdin,
            "go wtime 180000 btime 175000 winc 2000 binc 2000",
        )?;
        pause(900);
        if n == 3 {
            // Something a chess program might send that is not standard UCI.
            send(&mut stdin, "register later")?;
        }
    }

    // The user asks for an open-ended analysis of the current position.
    send(
        &mut stdin,
        "position startpos moves e2e4 e7e5 g1f3 b8c6 f1b5 a7a6 b5a4",
    )?;
    send(&mut stdin, "isready")?;
    pause(300);
    send(&mut stdin, "go infinite")?;
    pause(7000);
    send(&mut stdin, "stop")?;
    pause(500);
    send(&mut stdin, "quit")?;
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "uci-demo [--sessions N] [--no-viewer]\n\n\
             Plays N pretend sessions (default 2) through uci-shim and shows them\n\
             in uci-viewer. Needs uci-shim, uci-viewer and uci-fake-engine in the\n\
             same folder as uci-demo."
        );
        return;
    }
    let sessions: usize = args
        .iter()
        .position(|a| a == "--sessions")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let start_viewer = !args.iter().any(|a| a == "--no-viewer");

    let shim_exe = sibling("uci-shim");
    let fake_exe = sibling("uci-fake-engine");
    let viewer_exe = sibling("uci-viewer");
    for (what, path) in [("uci-shim", &shim_exe), ("uci-fake-engine", &fake_exe)] {
        if !path.exists() {
            eprintln!("uci-demo: cannot find {what} at {}", path.display());
            std::process::exit(1);
        }
    }

    let mut viewer = None;
    if start_viewer {
        if viewer_exe.exists() {
            match Command::new(&viewer_exe).spawn() {
                Ok(child) => {
                    viewer = Some(child);
                    // Give the window a moment to come up.
                    pause(2500);
                }
                Err(e) => eprintln!("uci-demo: could not start the viewer: {e}"),
            }
        } else {
            eprintln!(
                "uci-demo: uci-viewer not found next to this program; \
                 start it yourself in another window."
            );
        }
    }

    for n in 1..=sessions {
        println!("session {n} of {sessions} ...");
        let mut shim = match Command::new(&shim_exe)
            .arg("--engine")
            .arg(&fake_exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("uci-demo: could not start uci-shim: {e}");
                break;
            }
        };
        if let Err(e) = play_session(&mut shim) {
            eprintln!("uci-demo: lost the shim: {e}");
        }
        let _ = shim.wait();
        pause(1500);
    }

    println!("\nDone. The sessions stay in the viewer, and are saved as transcripts.");
    if let Some(mut v) = viewer {
        print!("Press Enter to close the viewer. ");
        let _ = io::stdout().flush();
        let _ = io::stdin().lock().read_line(&mut String::new());
        let _ = v.kill();
        let _ = v.wait();
    }
}
