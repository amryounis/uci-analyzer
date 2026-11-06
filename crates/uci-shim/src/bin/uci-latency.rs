//! uci-latency: measures `isready` -> `readyok` round-trip time.
//!
//! Usage:
//!   uci-latency [--count N] [--warmup N] -- <engine> [engine args...]
//!   uci-latency [--count N] [--warmup N] --shim <path-to-uci-shim> -- <engine> [args...]
//!
//! With `--shim` it measures the engine directly and then through the shim
//! (transcript logging on, written to a temp folder) and prints the added
//! latency. This is the NFR-01 check from the requirements.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Stats {
    samples: Vec<f64>,
}

impl Stats {
    fn percentile(&self, p: f64) -> f64 {
        let last = self.samples.len() - 1;
        self.samples[((last as f64) * p).round() as usize]
    }

    fn line(&self, label: &str) -> String {
        format!(
            "{label:<12} n={:<6} min {:>7.0} us   median {:>7.0} us   p95 {:>7.0} us   p99 {:>7.0} us   max {:>7.0} us",
            self.samples.len(),
            self.samples[0],
            self.percentile(0.5),
            self.percentile(0.95),
            self.percentile(0.99),
            self.samples[self.samples.len() - 1],
        )
    }
}

fn measure(program: &str, args: &[String], count: usize, warmup: usize) -> Result<Stats, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {program}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut reader = BufReader::new(child.stdout.take().ok_or("no stdout")?);

    let mut send = |text: &str| -> Result<(), String> {
        stdin
            .write_all(text.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("write failed: {e}"))
    };
    let mut wait_for = |word: &str| -> Result<(), String> {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return Err(format!("engine closed its output before sending {word}")),
                Ok(_) if line.trim() == word => return Ok(()),
                Ok(_) => {}
                Err(e) => return Err(format!("read failed: {e}")),
            }
        }
    };

    send("uci\n")?;
    wait_for("uciok")?;
    let mut samples = Vec::with_capacity(count);
    for i in 0..warmup + count {
        let started = Instant::now();
        send("isready\n")?;
        wait_for("readyok")?;
        if i >= warmup {
            samples.push(started.elapsed().as_secs_f64() * 1e6);
        }
    }
    send("quit\n")?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();

    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok(Stats { samples })
}

fn usage() -> ! {
    eprintln!(
        "usage: uci-latency [--count N] [--warmup N] [--shim <uci-shim path>] -- <engine> [args...]"
    );
    std::process::exit(2);
}

fn main() {
    let mut count = 1000usize;
    let mut warmup = 50usize;
    let mut shim: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--count" => {
                count = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--warmup" => {
                warmup = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--shim" => shim = Some(PathBuf::from(it.next().unwrap_or_else(|| usage()))),
            "--" => {
                rest = it.by_ref().collect();
                break;
            }
            _ => usage(),
        }
    }
    if rest.is_empty() || count == 0 {
        usage();
    }
    let engine = rest.remove(0);
    let engine_args = rest;

    let direct = measure(&engine, &engine_args, count, warmup).unwrap_or_else(|e| {
        eprintln!("direct run failed: {e}");
        std::process::exit(1);
    });
    println!("{}", direct.line("direct"));

    if let Some(shim) = shim {
        let log_dir = std::env::temp_dir().join("uci-latency-logs");
        let mut args = vec![
            "--engine".to_string(),
            engine,
            "--log-dir".to_string(),
            log_dir.to_string_lossy().into_owned(),
            "--".to_string(),
        ];
        args.extend(engine_args);
        let shimmed = measure(&shim.to_string_lossy(), &args, count, warmup).unwrap_or_else(|e| {
            eprintln!("shim run failed: {e}");
            std::process::exit(1);
        });
        println!("{}", shimmed.line("via shim"));
        println!(
            "added latency (median): {:+.0} us, (p99): {:+.0} us   transcripts: {}",
            shimmed.percentile(0.5) - direct.percentile(0.5),
            shimmed.percentile(0.99) - direct.percentile(0.99),
            log_dir.display()
        );
    }
}
