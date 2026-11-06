//! uci-replay: prints a uci-shim transcript with direction, timestamps and
//! parsed UCI lines.
//!
//!   uci-replay <transcript.jsonl> [--raw] [--no-color] [--only a,b,c] [--quiet]
//!
//! Direction tags: GUI>ENG (host GUI to engine), ENG>GUI (engine to host GUI),
//! ENG!ERR (engine stderr), SHIM (notes from the shim itself).

use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::process::ExitCode;

use uci_core::{parse_line, Dir, Event};

const USAGE: &str = "\
usage: uci-replay <transcript.jsonl> [options]

options:
    --raw         show the exact text of each line instead of the parsed summary
    --only a,b,c  show only gui/engine lines whose first word is one of these
                  (for example: --only info,bestmove). Stderr and shim notes
                  are always shown.
    --no-color    plain text output
    --quiet       skip the summary at the end
    -h, --help    show this help";

#[cfg(windows)]
fn enable_ansi() -> bool {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    // SAFETY: plain Win32 calls on the process's own stdout handle.
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle as isize == -1 {
            return false;
        }
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_ansi() -> bool {
    true
}

struct Options {
    path: String,
    raw: bool,
    only: Option<Vec<String>>,
    color: bool,
    quiet: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut path = None;
    let mut raw = false;
    let mut only = None;
    let mut no_color = false;
    let mut quiet = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--raw" => raw = true,
            "--no-color" => no_color = true,
            "--quiet" => quiet = true,
            "--only" => {
                let list = it.next().ok_or("--only needs a comma-separated list")?;
                only = Some(
                    list.split(',')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other => {
                if path.replace(other.to_string()).is_some() {
                    return Err("only one transcript file at a time".into());
                }
            }
        }
    }
    let color = !no_color
        && std::env::var_os("NO_COLOR").is_none()
        && io::stdout().is_terminal()
        && enable_ansi();
    Ok(Options {
        path: path.ok_or("no transcript file given")?,
        raw,
        only,
        color,
        quiet,
    })
}

fn tag(dir: Dir) -> (&'static str, &'static str) {
    match dir {
        Dir::GuiToEngine => ("GUI>ENG", "36"),
        Dir::ViewerToEngine => ("YOU>ENG", "35"),
        Dir::EngineToGui => ("ENG>GUI", "32"),
        Dir::EngineStderr => ("ENG!ERR", "31"),
        Dir::Shim => ("SHIM   ", "2"),
    }
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("uci-replay: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let file = match File::open(&opts.path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("uci-replay: cannot open {}: {e}", opts.path);
            return ExitCode::from(1);
        }
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut counts = [0usize; 5];
    let mut bad_lines = 0usize;
    let mut last_t = 0u64;
    let mut total = 0usize;

    for (index, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else {
            bad_lines += 1;
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let event = match Event::from_json(&line) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("uci-replay: line {}: {e}", index + 1);
                bad_lines += 1;
                continue;
            }
        };
        total += 1;
        last_t = last_t.max(event.t_ms);
        counts[match event.dir {
            Dir::GuiToEngine => 0,
            Dir::EngineToGui => 1,
            Dir::EngineStderr => 2,
            Dir::Shim => 3,
            Dir::ViewerToEngine => 4,
        }] += 1;

        let is_uci_line = matches!(
            event.dir,
            Dir::GuiToEngine | Dir::ViewerToEngine | Dir::EngineToGui
        );
        if let (Some(only), true) = (&opts.only, is_uci_line) {
            let first = event
                .line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase();
            if !only.contains(&first) {
                continue;
            }
        }

        let text = if opts.raw || !is_uci_line {
            event.line.clone()
        } else {
            parse_line(&event.line).describe()
        };
        let (label, color) = tag(event.dir);
        let time = format!("{:>8.3}s", event.t_ms as f64 / 1000.0);
        let written = if opts.color {
            writeln!(out, "{time}  \x1b[{color}m{label}\x1b[0m  {text}")
        } else {
            writeln!(out, "{time}  {label}  {text}")
        };
        if written.is_err() {
            // The reader (for example `| head`) went away.
            return ExitCode::SUCCESS;
        }
    }

    if !opts.quiet {
        let _ = writeln!(
            out,
            "\n{total} events over {:.3}s: gui>engine {}, engine>gui {}, engine stderr {}, shim notes {}{}{}",
            last_t as f64 / 1000.0,
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            if counts[4] > 0 {
                format!(", typed in viewer {}", counts[4])
            } else {
                String::new()
            },
            if bad_lines > 0 {
                format!("; {bad_lines} unreadable lines skipped")
            } else {
                String::new()
            }
        );
    }
    ExitCode::SUCCESS
}
