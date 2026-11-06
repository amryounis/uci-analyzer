//! uci-viewer: a local web page that shows UCI traffic live, like a protocol
//! analyzer. It connects to running `uci-shim` processes, or opens a saved
//! transcript, and serves the page and the data on localhost only.

mod api;
mod http;
mod launch;

use std::net::{Ipv4Addr, TcpListener};
use std::process::{Command, Stdio};
use std::thread;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PORT: u16 = 7878;

const HELP: &str = "\
uci-viewer - live protocol-analyzer view of UCI traffic

Usage: uci-viewer [options]

Options:
  --port <n>          Port to serve on (default 7878; the next free one is
                      used if it is taken; 0 lets the system choose)
  --no-open           Do not open a window, just print the address
  --no-launch         Turn off the Load engine button (the viewer then only
                      watches, and cannot start programs)
  --browser <name>    'app' (default): a standalone Edge/Chrome app window
                      'default': a tab in your default browser
  --help, --version
";

struct Options {
    port: u16,
    open: bool,
    app_window: bool,
    launch: bool,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        port: DEFAULT_PORT,
        open: true,
        app_window: true,
        launch: true,
    };
    let mut i = 0;
    while i < args.len() {
        let value = |i: usize| {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--port" => {
                opts.port = value(i)?
                    .parse()
                    .map_err(|_| "--port needs a number from 0 to 65535".to_string())?;
                i += 1;
            }
            "--no-open" => opts.open = false,
            "--no-launch" => opts.launch = false,
            "--browser" => {
                match value(i)?.as_str() {
                    "app" => opts.app_window = true,
                    "default" => opts.app_window = false,
                    other => return Err(format!("unknown browser mode '{other}'")),
                }
                i += 1;
            }
            "--help" | "-h" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("uci-viewer {VERSION}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option '{other}' (try --help)")),
        }
        i += 1;
    }
    Ok(opts)
}

fn bind(start: u16) -> std::io::Result<TcpListener> {
    if start == 0 {
        return TcpListener::bind((Ipv4Addr::LOCALHOST, 0));
    }
    let mut last = None;
    for port in start..start.saturating_add(20) {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(l) => return Ok(l),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no port available")))
}

/// Best effort: a failure to open a window is not fatal, the address is
/// printed anyway.
fn open_window(url: &str, app_window: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let quiet = |mut c: Command| {
            c.creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
        };
        if app_window {
            for browser in ["msedge", "chrome"] {
                let mut c = Command::new("cmd");
                c.args(["/C", "start", "", browser, &format!("--app={url}")]);
                if quiet(c) {
                    return;
                }
            }
        }
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        quiet(c);
    }
    #[cfg(not(windows))]
    {
        let _ = app_window;
        let _ = Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("uci-viewer: {e}");
            std::process::exit(2);
        }
    };
    launch::set_allowed(opts.launch);
    let listener = match bind(opts.port) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("uci-viewer: cannot listen on localhost: {e}");
            std::process::exit(1);
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}/");
    println!("uci-viewer {VERSION} listening on {url}");

    if opts.open {
        open_window(&url, opts.app_window);
    }

    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        thread::spawn(move || {
            let _ = conn.set_nodelay(true);
            match http::read_request(&conn) {
                Ok(Some(req)) => {
                    let _ = api::route(&req, &mut conn, port);
                }
                Ok(None) => {}
                Err(_) => {
                    let _ = http::respond_text(&mut conn, 400, "bad request");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults() {
        let o = parse_args(&[]).unwrap();
        assert_eq!(o.port, 7878);
        assert!(o.open && o.app_window && o.launch);
    }

    #[test]
    fn flags() {
        let o = parse_args(&args(&[
            "--port",
            "0",
            "--no-open",
            "--no-launch",
            "--browser",
            "default",
        ]))
        .unwrap();
        assert_eq!(o.port, 0);
        assert!(!o.open && !o.app_window && !o.launch);
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(parse_args(&args(&["--port"])).is_err());
        assert!(parse_args(&args(&["--port", "x"])).is_err());
        assert!(parse_args(&args(&["--browser", "ie"])).is_err());
        assert!(parse_args(&args(&["--wat"])).is_err());
    }
}
