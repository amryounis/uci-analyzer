//! Loading an engine from the viewer.
//!
//! The viewer starts `uci-shim` with the chosen engine and plays the part of
//! the chess program itself: it holds the shim's input open, so the engine
//! stays up until it is told to quit or the viewer closes. The session then
//! shows up like any other, and commands typed in the page reach it through
//! the shim's normal live connection.

use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use uci_core::json::{self, Value};
use uci_core::{paths, split_args};

/// Most recently loaded engines remembered for the list in the dialog.
const MAX_RECENT: usize = 12;
const MAX_ARGS: usize = 4 * 1024;
const MAX_PATH: usize = 1024;

static ALLOWED: AtomicBool = AtomicBool::new(true);

/// `--no-launch` turns loading (and browsing) off for the whole run.
pub fn set_allowed(on: bool) {
    ALLOWED.store(on, Ordering::Relaxed);
}

pub fn allowed() -> bool {
    ALLOWED.load(Ordering::Relaxed)
}

/// The file dialog only exists on Windows.
pub fn can_browse() -> bool {
    allowed() && cfg!(windows)
}

/// A pasted path often arrives wrapped in quotes ("Copy as path").
pub fn clean_path(text: &str) -> String {
    let t = text.trim();
    let t = t
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(t);
    t.trim().to_string()
}

fn plain_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control)
}

/// What the page sends: `{"engine":"C:\\...\\stockfish.exe","args":""}`.
pub fn parse_request(body: &[u8]) -> Result<(PathBuf, String), &'static str> {
    let text = std::str::from_utf8(body).map_err(|_| "not text")?;
    let fields = json::parse_flat_object(text).map_err(|_| "not a JSON object")?;
    let (mut engine, mut args) = (None, String::new());
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("engine", Value::Str(s)) => engine = Some(s),
            ("args", Value::Str(s)) => args = s.trim().to_string(),
            _ => {}
        }
    }
    let engine = clean_path(&engine.ok_or("missing engine")?);
    if engine.is_empty() {
        return Err("type or paste the path of the engine");
    }
    if !plain_text(&engine, MAX_PATH) {
        return Err("that is not a usable path");
    }
    if !plain_text(&args, MAX_ARGS) {
        return Err("the arguments must be one line of plain text");
    }
    Ok((PathBuf::from(engine), args))
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// `uci-shim` next to this program, or the file named by `UCI_SHIM_EXE`.
fn shim_path() -> PathBuf {
    if let Some(p) = env::var_os("UCI_SHIM_EXE") {
        return PathBuf::from(p);
    }
    let me = env::current_exe().unwrap_or_default();
    me.parent()
        .map(|d| d.join(exe_name("uci-shim")))
        .unwrap_or_else(|| PathBuf::from(exe_name("uci-shim")))
}

/// Why this file cannot be an engine, in words for the person typing it.
pub fn check_engine(path: &Path) -> Result<(), String> {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(()),
        Ok(_) => Err(format!(
            "{} is a folder, not a program. Choose the engine's .exe file.",
            path.display()
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(format!("There is no file at {}.", path.display()))
        }
        Err(e) => Err(format!("Cannot open {}: {e}", path.display())),
    }
}

/// The command line used to run the engine through the shim. The user's own
/// `uci-shim.cfg` is deliberately ignored: it describes the engine the chess
/// program uses, not this one.
fn shim_command(shim: &Path, engine: &Path, args: &str) -> Command {
    let mut cmd = Command::new(shim);
    cmd.arg("--config")
        .arg(paths::data_dir().join("launched-engines.cfg"))
        .arg("--engine")
        .arg(engine)
        .arg("--")
        .args(split_args(args));
    cmd
}

/// Starts the engine and returns the shim's process id, which is also the
/// session's id in the viewer.
pub fn launch(engine: &Path, args: &str) -> Result<u32, String> {
    if !allowed() {
        return Err("Loading engines is switched off (--no-launch).".to_string());
    }
    check_engine(engine)?;
    let shim = shim_path();
    if !shim.is_file() {
        return Err(format!(
            "Cannot find {} next to the viewer. Build everything with cargo build --release.",
            shim.display()
        ));
    }
    let mut cmd = shim_command(&shim, engine, args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", shim.display()))?;
    let pid = child.id();
    let stdin = child.stdin.take();
    // Keeping the shim's input open is what keeps the engine running. When
    // the shim exits (the engine quit) the thread ends and frees both.
    thread::spawn(move || {
        let _held_open = stdin;
        let _ = child.wait();
    });
    remember(engine, args);
    Ok(pid)
}

// ------------------------------------------------------------------ recent

fn recent_file() -> PathBuf {
    paths::data_dir().join("engines.txt")
}

/// One engine per line: path, a tab, then the arguments.
pub fn recent() -> Vec<(String, String)> {
    let text = fs::read_to_string(recent_file()).unwrap_or_default();
    parse_recent(&text)
}

fn parse_recent(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (path, args) = l.split_once('\t').unwrap_or((l, ""));
            let path = path.trim();
            (!path.is_empty()).then(|| (path.to_string(), args.trim().to_string()))
        })
        .take(MAX_RECENT)
        .collect()
}

/// Puts an engine first in the remembered list, without duplicates.
fn merge_recent(list: Vec<(String, String)>, path: &str, args: &str) -> VecDeque<(String, String)> {
    let mut out: VecDeque<(String, String)> = list
        .into_iter()
        .filter(|(p, a)| !(p == path && a == args))
        .collect();
    out.push_front((path.to_string(), args.to_string()));
    out.truncate(MAX_RECENT);
    out
}

fn remember(engine: &Path, args: &str) {
    let list = merge_recent(recent(), &engine.to_string_lossy(), args);
    let mut text = String::new();
    for (p, a) in &list {
        text.push_str(&format!("{p}\t{a}\n"));
    }
    let file = recent_file();
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(file, text);
}

// ------------------------------------------------------------------ browse

/// Windows only: the standard "open file" dialog, so the path need not be
/// typed. `Ok(None)` means the dialog was cancelled.
pub fn browse() -> Result<Option<String>, String> {
    if !can_browse() {
        return Err("The file dialog is only available on Windows. Paste the path instead.".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8;\
            Add-Type -AssemblyName System.Windows.Forms;\
            $o=New-Object System.Windows.Forms.Form;$o.TopMost=$true;\
            $d=New-Object System.Windows.Forms.OpenFileDialog;\
            $d.Title='Choose a UCI engine';\
            $d.Filter='Programs (*.exe)|*.exe|All files (*.*)|*.*';\
            if($d.ShowDialog($o) -eq 'OK'){[Console]::Out.Write($d.FileName)}";
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("Could not open the file dialog: {e}"))?;
        let chosen = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!chosen.is_empty()).then_some(chosen))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_paths_lose_their_quotes() {
        assert_eq!(
            clean_path(r#"  "C:\Engines\sf.exe"  "#),
            r"C:\Engines\sf.exe"
        );
        assert_eq!(clean_path("/usr/bin/engine"), "/usr/bin/engine");
        assert_eq!(clean_path(r#""""#), "");
    }

    #[test]
    fn requests_need_a_plain_engine_path() {
        let ok = parse_request(br#"{"engine":"\"C:\\E\\sf.exe\"","args":" --x 1 "}"#).unwrap();
        assert_eq!(ok.0, PathBuf::from(r"C:\E\sf.exe"));
        assert_eq!(ok.1, "--x 1");
        assert!(parse_request(br#"{"args":""}"#).is_err());
        assert!(parse_request(br#"{"engine":"  "}"#).is_err());
        assert!(parse_request(br#"{"engine":"a\nb"}"#).is_err());
        assert!(parse_request(br#"{"engine":"a","args":"x\ny"}"#).is_err());
        assert!(parse_request(b"[1]").is_err());
        assert!(parse_request(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn missing_files_and_folders_are_explained() {
        let dir = env::temp_dir();
        assert!(check_engine(&dir).unwrap_err().contains("folder"));
        let gone = dir.join("no-such-engine-here.exe");
        assert!(check_engine(&gone).unwrap_err().contains("no file"));
    }

    #[test]
    fn the_shim_gets_the_engine_and_split_arguments() {
        let cmd = shim_command(
            Path::new("uci-shim"),
            Path::new("/e/engine"),
            r#"--net "a b" -t 2"#,
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|a| a == "--engine").unwrap();
        assert_eq!(args[at + 1], "/e/engine");
        assert_eq!(&args[at + 2..], ["--", "--net", "a b", "-t", "2"]);
        assert!(args.contains(&"--config".to_string()));
    }

    #[test]
    fn recent_engines_are_unique_newest_first_and_bounded() {
        let list = parse_recent("a\t\nb\t-x\n\n  \nc");
        assert_eq!(list.len(), 3);
        assert_eq!(list[2], ("c".to_string(), String::new()));
        let merged = merge_recent(list, "b", "-x");
        assert_eq!(merged[0].0, "b");
        assert_eq!(merged.len(), 3);
        let mut many = Vec::new();
        for i in 0..30 {
            many = merge_recent(many.into_iter().collect(), &format!("e{i}"), "")
                .into_iter()
                .collect();
        }
        assert_eq!(many.len(), MAX_RECENT);
        assert_eq!(many[0].0, "e29");
    }
}
