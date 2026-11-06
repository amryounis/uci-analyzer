//! Tolerant parser for UCI lines, in both directions.
//!
//! The parser never fails: a line it cannot understand becomes
//! `Message::Unknown` carrying the original text, so nothing is ever hidden
//! from the viewer. Callers should keep the raw line next to the parsed form.

use std::str::FromStr;

/// The value part of an `info ... score` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKind {
    /// Centipawns from the engine's point of view.
    Cp(i32),
    /// Mate in N moves; negative means the engine is being mated.
    Mate(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreBound {
    Exact,
    Lower,
    Upper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    pub kind: ScoreKind,
    pub bound: ScoreBound,
}

/// Fields of an engine `info` line. Absent fields are `None` / empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub depth: Option<u32>,
    pub seldepth: Option<u32>,
    pub multipv: Option<u32>,
    pub score: Option<Score>,
    pub nodes: Option<u64>,
    pub nps: Option<u64>,
    pub hashfull: Option<u32>,
    pub tbhits: Option<u64>,
    pub time_ms: Option<u64>,
    pub cpuload: Option<u32>,
    pub currmove: Option<String>,
    pub currmovenumber: Option<u32>,
    pub pv: Vec<String>,
    pub string: Option<String>,
}

/// Arguments of a `go` command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoParams {
    pub searchmoves: Vec<String>,
    pub ponder: bool,
    pub wtime: Option<i64>,
    pub btime: Option<i64>,
    pub winc: Option<i64>,
    pub binc: Option<i64>,
    pub movestogo: Option<u32>,
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub mate: Option<u32>,
    pub movetime: Option<u64>,
    pub infinite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionKind {
    Check {
        default: Option<bool>,
    },
    Spin {
        default: Option<i64>,
        min: Option<i64>,
        max: Option<i64>,
    },
    Combo {
        default: Option<String>,
        vars: Vec<String>,
    },
    Button,
    String {
        default: Option<String>,
    },
    /// A `type` the parser does not know; the text is kept as sent.
    Unknown(String),
}

/// An engine `option name ... type ...` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionDecl {
    pub name: String,
    pub kind: OptionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    // GUI -> engine
    Uci,
    Debug(bool),
    IsReady,
    SetOption {
        name: String,
        value: Option<String>,
    },
    UciNewGame,
    /// `fen: None` means `startpos`.
    Position {
        fen: Option<String>,
        moves: Vec<String>,
    },
    Go(GoParams),
    Stop,
    PonderHit,
    Quit,
    // Engine -> GUI
    Id {
        field: String,
        value: String,
    },
    UciOk,
    ReadyOk,
    BestMove {
        best: String,
        ponder: Option<String>,
    },
    Info(Info),
    Option(OptionDecl),
    /// Anything else, kept verbatim (trimmed).
    Unknown(String),
}

const INFO_KEYWORDS: &[&str] = &[
    "depth",
    "seldepth",
    "time",
    "nodes",
    "pv",
    "multipv",
    "score",
    "currmove",
    "currmovenumber",
    "hashfull",
    "nps",
    "tbhits",
    "sbhits",
    "cpuload",
    "string",
    "refutation",
    "currline",
];

const GO_KEYWORDS: &[&str] = &[
    "searchmoves",
    "ponder",
    "wtime",
    "btime",
    "winc",
    "binc",
    "movestogo",
    "depth",
    "nodes",
    "mate",
    "movetime",
    "infinite",
];

fn take<T: FromStr>(rest: &[&str], i: &mut usize) -> Option<T> {
    let value = rest.get(*i)?.parse::<T>().ok()?;
    *i += 1;
    Some(value)
}

fn skip_until(rest: &[&str], i: &mut usize, keywords: &[&str]) {
    while *i < rest.len() && !keywords.contains(&rest[*i]) {
        *i += 1;
    }
}

fn collect_until(rest: &[&str], i: &mut usize, keywords: &[&str]) -> Vec<String> {
    let start = *i;
    skip_until(rest, i, keywords);
    rest[start..*i].iter().map(|s| (*s).to_string()).collect()
}

fn parse_info(rest: &[&str]) -> Info {
    let mut info = Info::default();
    let mut i = 0;
    while i < rest.len() {
        let token = rest[i];
        i += 1;
        match token {
            "depth" => {
                if let Some(v) = take(rest, &mut i) {
                    info.depth = Some(v);
                }
            }
            "seldepth" => {
                if let Some(v) = take(rest, &mut i) {
                    info.seldepth = Some(v);
                }
            }
            "multipv" => {
                if let Some(v) = take(rest, &mut i) {
                    info.multipv = Some(v);
                }
            }
            "time" => {
                if let Some(v) = take(rest, &mut i) {
                    info.time_ms = Some(v);
                }
            }
            "nodes" => {
                if let Some(v) = take(rest, &mut i) {
                    info.nodes = Some(v);
                }
            }
            "nps" => {
                if let Some(v) = take(rest, &mut i) {
                    info.nps = Some(v);
                }
            }
            "hashfull" => {
                if let Some(v) = take(rest, &mut i) {
                    info.hashfull = Some(v);
                }
            }
            "tbhits" => {
                if let Some(v) = take(rest, &mut i) {
                    info.tbhits = Some(v);
                }
            }
            "cpuload" => {
                if let Some(v) = take(rest, &mut i) {
                    info.cpuload = Some(v);
                }
            }
            "currmovenumber" => {
                if let Some(v) = take(rest, &mut i) {
                    info.currmovenumber = Some(v);
                }
            }
            "currmove" => {
                if let Some(v) = rest.get(i) {
                    info.currmove = Some((*v).to_string());
                    i += 1;
                }
            }
            "pv" => info.pv = collect_until(rest, &mut i, INFO_KEYWORDS),
            "string" => {
                info.string = Some(rest[i..].join(" "));
                i = rest.len();
            }
            "score" => {
                let mut kind = None;
                let mut bound = ScoreBound::Exact;
                while i < rest.len() {
                    match rest[i] {
                        "cp" => {
                            i += 1;
                            if let Some(v) = take(rest, &mut i) {
                                kind = Some(ScoreKind::Cp(v));
                            }
                        }
                        "mate" => {
                            i += 1;
                            if let Some(v) = take(rest, &mut i) {
                                kind = Some(ScoreKind::Mate(v));
                            }
                        }
                        "lowerbound" => {
                            i += 1;
                            bound = ScoreBound::Lower;
                        }
                        "upperbound" => {
                            i += 1;
                            bound = ScoreBound::Upper;
                        }
                        _ => break,
                    }
                }
                if let Some(kind) = kind {
                    info.score = Some(Score { kind, bound });
                }
            }
            // Fields we recognise but do not model: skip their values.
            "sbhits" => {
                let _: Option<u64> = take(rest, &mut i);
            }
            "refutation" | "currline" => skip_until(rest, &mut i, INFO_KEYWORDS),
            _ => {}
        }
    }
    info
}

fn parse_go(rest: &[&str]) -> GoParams {
    let mut go = GoParams::default();
    let mut i = 0;
    while i < rest.len() {
        let token = rest[i];
        i += 1;
        match token {
            "searchmoves" => go.searchmoves = collect_until(rest, &mut i, GO_KEYWORDS),
            "ponder" => go.ponder = true,
            "infinite" => go.infinite = true,
            "wtime" => {
                if let Some(v) = take(rest, &mut i) {
                    go.wtime = Some(v);
                }
            }
            "btime" => {
                if let Some(v) = take(rest, &mut i) {
                    go.btime = Some(v);
                }
            }
            "winc" => {
                if let Some(v) = take(rest, &mut i) {
                    go.winc = Some(v);
                }
            }
            "binc" => {
                if let Some(v) = take(rest, &mut i) {
                    go.binc = Some(v);
                }
            }
            "movestogo" => {
                if let Some(v) = take(rest, &mut i) {
                    go.movestogo = Some(v);
                }
            }
            "depth" => {
                if let Some(v) = take(rest, &mut i) {
                    go.depth = Some(v);
                }
            }
            "nodes" => {
                if let Some(v) = take(rest, &mut i) {
                    go.nodes = Some(v);
                }
            }
            "mate" => {
                if let Some(v) = take(rest, &mut i) {
                    go.mate = Some(v);
                }
            }
            "movetime" => {
                if let Some(v) = take(rest, &mut i) {
                    go.movetime = Some(v);
                }
            }
            _ => {}
        }
    }
    go
}

fn parse_position(rest: &[&str]) -> Option<Message> {
    let mut i;
    let fen = match *rest.first()? {
        "startpos" => {
            i = 1;
            None
        }
        "fen" => {
            i = 1;
            let start = i;
            while i < rest.len() && rest[i] != "moves" {
                i += 1;
            }
            if i == start {
                return None;
            }
            Some(rest[start..i].join(" "))
        }
        _ => return None,
    };
    let moves = if rest.get(i) == Some(&"moves") {
        rest[i + 1..].iter().map(|s| (*s).to_string()).collect()
    } else {
        Vec::new()
    };
    Some(Message::Position { fen, moves })
}

fn parse_setoption(rest: &[&str]) -> Option<Message> {
    if rest.first() != Some(&"name") {
        return None;
    }
    let mut i = 1;
    while i < rest.len() && rest[i] != "value" {
        i += 1;
    }
    let name = rest[1..i].join(" ");
    if name.is_empty() {
        return None;
    }
    let value = if i < rest.len() {
        Some(rest[i + 1..].join(" "))
    } else {
        None
    };
    Some(Message::SetOption { name, value })
}

/// Parses the text after `option`. Known limitation: a `string` default or a
/// combo choice that itself contains the words `default`, `min`, `max` or
/// `var` is split at that word.
fn parse_option(rest: &[&str]) -> Option<OptionDecl> {
    const ATTRS: &[&str] = &["default", "min", "max", "var"];
    if rest.first() != Some(&"name") {
        return None;
    }
    let mut i = 1;
    while i < rest.len() && rest[i] != "type" {
        i += 1;
    }
    if i >= rest.len() {
        return None;
    }
    let name = rest[1..i].join(" ");
    if name.is_empty() {
        return None;
    }
    i += 1;
    let ty = *rest.get(i)?;
    i += 1;

    let (mut default, mut min, mut max, mut vars) = (None, None, None, Vec::new());
    while i < rest.len() {
        let keyword = rest[i];
        i += 1;
        let start = i;
        skip_until(rest, &mut i, ATTRS);
        let value = rest[start..i].join(" ");
        match keyword {
            "default" => default = Some(value),
            "min" => min = Some(value),
            "max" => max = Some(value),
            "var" => vars.push(value),
            _ => {}
        }
    }

    let kind = match ty {
        "check" => OptionKind::Check {
            default: match default.as_deref() {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            },
        },
        "spin" => OptionKind::Spin {
            default: default.and_then(|v| v.parse().ok()),
            min: min.and_then(|v| v.parse().ok()),
            max: max.and_then(|v| v.parse().ok()),
        },
        "combo" => OptionKind::Combo { default, vars },
        "button" => OptionKind::Button,
        "string" => OptionKind::String { default },
        other => OptionKind::Unknown(other.to_string()),
    };
    Some(OptionDecl { name, kind })
}

/// Parses one UCI line (either direction). Never fails.
pub fn parse_line(line: &str) -> Message {
    let trimmed = line.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let Some((&first, rest)) = tokens.split_first() else {
        return Message::Unknown(String::new());
    };
    let parsed = match first {
        "uci" => Some(Message::Uci),
        "debug" => match rest.first().copied() {
            Some("on") => Some(Message::Debug(true)),
            Some("off") => Some(Message::Debug(false)),
            _ => None,
        },
        "isready" => Some(Message::IsReady),
        "ucinewgame" => Some(Message::UciNewGame),
        "stop" => Some(Message::Stop),
        "ponderhit" => Some(Message::PonderHit),
        "quit" => Some(Message::Quit),
        "uciok" => Some(Message::UciOk),
        "readyok" => Some(Message::ReadyOk),
        "setoption" => parse_setoption(rest),
        "position" => parse_position(rest),
        "go" => Some(Message::Go(parse_go(rest))),
        "id" => match rest.split_first() {
            Some((&field, value)) if field == "name" || field == "author" => Some(Message::Id {
                field: field.to_string(),
                value: value.join(" "),
            }),
            _ => None,
        },
        "bestmove" => rest.first().map(|best| Message::BestMove {
            best: (*best).to_string(),
            ponder: if rest.get(1) == Some(&"ponder") {
                rest.get(2).map(|p| (*p).to_string())
            } else {
                None
            },
        }),
        "info" => Some(Message::Info(parse_info(rest))),
        "option" => parse_option(rest).map(Message::Option),
        _ => None,
    };
    parsed.unwrap_or_else(|| Message::Unknown(trimmed.to_string()))
}

// ---------------------------------------------------------------------------
// Human-readable descriptions (used by uci-replay, reusable by the viewer)
// ---------------------------------------------------------------------------

fn human_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.2}G", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

impl Score {
    pub fn describe(&self) -> String {
        let base = match self.kind {
            ScoreKind::Cp(cp) => format!("{:+.2}", f64::from(cp) / 100.0),
            ScoreKind::Mate(n) => format!("#{n}"),
        };
        match self.bound {
            ScoreBound::Exact => base,
            ScoreBound::Lower => format!("{base} (lower bound)"),
            ScoreBound::Upper => format!("{base} (upper bound)"),
        }
    }
}

impl Info {
    pub fn describe(&self) -> String {
        const PV_SHOWN: usize = 12;
        let mut parts: Vec<String> = Vec::new();
        match (self.depth, self.seldepth) {
            (Some(d), Some(s)) => parts.push(format!("depth {d}/{s}")),
            (Some(d), None) => parts.push(format!("depth {d}")),
            _ => {}
        }
        if let Some(m) = self.multipv {
            parts.push(format!("multipv {m}"));
        }
        if let Some(score) = &self.score {
            parts.push(score.describe());
        }
        if let Some(n) = self.nodes {
            parts.push(format!("{} nodes", human_count(n)));
        }
        if let Some(n) = self.nps {
            parts.push(format!("{} nps", human_count(n)));
        }
        if let Some(h) = self.hashfull {
            parts.push(format!("hash {:.1}%", f64::from(h) / 10.0));
        }
        if let Some(n) = self.tbhits {
            parts.push(format!("{} tbhits", human_count(n)));
        }
        if let Some(t) = self.time_ms {
            parts.push(format!("{t} ms"));
        }
        if let Some(c) = self.cpuload {
            parts.push(format!("cpu {:.1}%", f64::from(c) / 10.0));
        }
        if let Some(mv) = &self.currmove {
            match self.currmovenumber {
                Some(n) => parts.push(format!("currmove {mv} (#{n})")),
                None => parts.push(format!("currmove {mv}")),
            }
        }
        if !self.pv.is_empty() {
            let shown: Vec<&str> = self.pv.iter().take(PV_SHOWN).map(String::as_str).collect();
            let more = if self.pv.len() > PV_SHOWN { " ..." } else { "" };
            parts.push(format!("pv {}{more}", shown.join(" ")));
        }
        if let Some(s) = &self.string {
            parts.push(format!("string: {s}"));
        }
        if parts.is_empty() {
            "info".to_string()
        } else {
            format!("info {}", parts.join(" | "))
        }
    }
}

impl GoParams {
    pub fn describe(&self) -> String {
        let mut parts = vec!["go".to_string()];
        if self.ponder {
            parts.push("ponder".into());
        }
        if self.infinite {
            parts.push("infinite".into());
        }
        let numbers: [(&str, Option<String>); 9] = [
            ("wtime", self.wtime.map(|v| v.to_string())),
            ("btime", self.btime.map(|v| v.to_string())),
            ("winc", self.winc.map(|v| v.to_string())),
            ("binc", self.binc.map(|v| v.to_string())),
            ("movestogo", self.movestogo.map(|v| v.to_string())),
            ("depth", self.depth.map(|v| v.to_string())),
            ("nodes", self.nodes.map(|v| v.to_string())),
            ("mate", self.mate.map(|v| v.to_string())),
            ("movetime", self.movetime.map(|v| v.to_string())),
        ];
        for (name, value) in numbers {
            if let Some(value) = value {
                parts.push(format!("{name} {value}"));
            }
        }
        if !self.searchmoves.is_empty() {
            parts.push(format!("searchmoves {}", self.searchmoves.join(" ")));
        }
        parts.join(" ")
    }
}

impl OptionDecl {
    pub fn describe(&self) -> String {
        let detail = match &self.kind {
            OptionKind::Check { default } => match default {
                Some(d) => format!("check, default {d}"),
                None => "check".to_string(),
            },
            OptionKind::Spin { default, min, max } => {
                let mut s = String::from("spin");
                if let Some(d) = default {
                    s.push_str(&format!(", default {d}"));
                }
                match (min, max) {
                    (Some(lo), Some(hi)) => s.push_str(&format!(", range {lo}..{hi}")),
                    (Some(lo), None) => s.push_str(&format!(", min {lo}")),
                    (None, Some(hi)) => s.push_str(&format!(", max {hi}")),
                    (None, None) => {}
                }
                s
            }
            OptionKind::Combo { default, vars } => {
                let mut s = String::from("combo");
                if let Some(d) = default {
                    s.push_str(&format!(", default {d}"));
                }
                if !vars.is_empty() {
                    s.push_str(&format!(", choices {}", vars.join(" | ")));
                }
                s
            }
            OptionKind::Button => "button".to_string(),
            OptionKind::String { default } => match default {
                Some(d) => format!("string, default {d}"),
                None => "string".to_string(),
            },
            OptionKind::Unknown(t) => format!("unknown type '{t}'"),
        };
        format!("option {}: {detail}", self.name)
    }
}

impl Message {
    pub fn describe(&self) -> String {
        match self {
            Message::Uci => "uci".into(),
            Message::Debug(on) => format!("debug {}", if *on { "on" } else { "off" }),
            Message::IsReady => "isready".into(),
            Message::SetOption { name, value } => match value {
                Some(v) => format!("setoption {name} = {v}"),
                None => format!("setoption {name}"),
            },
            Message::UciNewGame => "ucinewgame".into(),
            Message::Position { fen, moves } => {
                let base = match fen {
                    Some(f) => format!("position fen {f}"),
                    None => "position startpos".to_string(),
                };
                if moves.is_empty() {
                    base
                } else {
                    format!("{base} + {} moves", moves.len())
                }
            }
            Message::Go(go) => go.describe(),
            Message::Stop => "stop".into(),
            Message::PonderHit => "ponderhit".into(),
            Message::Quit => "quit".into(),
            Message::Id { field, value } => format!("id {field} {value}"),
            Message::UciOk => "uciok".into(),
            Message::ReadyOk => "readyok".into(),
            Message::BestMove { best, ponder } => match ponder {
                Some(p) => format!("bestmove {best} (ponder {p})"),
                None => format!("bestmove {best}"),
            },
            Message::Info(info) => info.describe(),
            Message::Option(decl) => decl.describe(),
            Message::Unknown(text) => text.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(line: &str) -> Info {
        match parse_line(line) {
            Message::Info(i) => i,
            other => panic!("expected info, got {other:?}"),
        }
    }

    fn option(line: &str) -> OptionDecl {
        match parse_line(line) {
            Message::Option(o) => o,
            other => panic!("expected option, got {other:?}"),
        }
    }

    #[test]
    fn full_info_line() {
        let i = info(
            "info depth 18 seldepth 24 multipv 1 score cp 35 nodes 1234567 nps 850123 \
             hashfull 120 tbhits 0 time 1452 pv e2e4 e7e5 g1f3",
        );
        assert_eq!(i.depth, Some(18));
        assert_eq!(i.seldepth, Some(24));
        assert_eq!(i.multipv, Some(1));
        assert_eq!(
            i.score,
            Some(Score {
                kind: ScoreKind::Cp(35),
                bound: ScoreBound::Exact
            })
        );
        assert_eq!(i.nodes, Some(1_234_567));
        assert_eq!(i.nps, Some(850_123));
        assert_eq!(i.hashfull, Some(120));
        assert_eq!(i.tbhits, Some(0));
        assert_eq!(i.time_ms, Some(1452));
        assert_eq!(i.pv, vec!["e2e4", "e7e5", "g1f3"]);
    }

    #[test]
    fn mate_score_with_bound() {
        let i = info("info depth 5 score mate -3 lowerbound nodes 99");
        assert_eq!(
            i.score,
            Some(Score {
                kind: ScoreKind::Mate(-3),
                bound: ScoreBound::Lower
            })
        );
        assert_eq!(i.nodes, Some(99));
    }

    #[test]
    fn info_string_takes_rest_of_line() {
        let i = info("info string NNUE evaluation using nn-abc.nnue depth 3");
        assert_eq!(
            i.string.as_deref(),
            Some("NNUE evaluation using nn-abc.nnue depth 3")
        );
        assert_eq!(i.depth, None);
    }

    #[test]
    fn currmove_and_refutation_are_tolerated() {
        let i = info("info currmove e2e4 currmovenumber 1 refutation d1h5 g6h5 depth 7");
        assert_eq!(i.currmove.as_deref(), Some("e2e4"));
        assert_eq!(i.currmovenumber, Some(1));
        assert_eq!(i.depth, Some(7));
    }

    #[test]
    fn malformed_numbers_do_not_panic() {
        let i = info("info depth abc nodes");
        assert_eq!(i.depth, None);
        assert_eq!(i.nodes, None);
    }

    #[test]
    fn option_spin_and_check() {
        let o = option("option name Hash type spin default 16 min 1 max 33554432");
        assert_eq!(o.name, "Hash");
        assert_eq!(
            o.kind,
            OptionKind::Spin {
                default: Some(16),
                min: Some(1),
                max: Some(33_554_432)
            }
        );
        let o = option("option name Ponder type check default false");
        assert_eq!(
            o.kind,
            OptionKind::Check {
                default: Some(false)
            }
        );
    }

    #[test]
    fn option_names_with_spaces() {
        let o = option("option name Clear Hash type button");
        assert_eq!(o.name, "Clear Hash");
        assert_eq!(o.kind, OptionKind::Button);
        let o = option("option name Debug Log File type string default <empty>");
        assert_eq!(o.name, "Debug Log File");
        assert_eq!(
            o.kind,
            OptionKind::String {
                default: Some("<empty>".into())
            }
        );
    }

    #[test]
    fn option_combo_with_spaced_choices() {
        let o = option(
            "option name Style type combo default Normal var Solid var Very Aggressive var Normal",
        );
        assert_eq!(
            o.kind,
            OptionKind::Combo {
                default: Some("Normal".into()),
                vars: vec!["Solid".into(), "Very Aggressive".into(), "Normal".into()]
            }
        );
    }

    #[test]
    fn option_without_type_is_unknown() {
        assert!(matches!(
            parse_line("option name Broken"),
            Message::Unknown(_)
        ));
    }

    #[test]
    fn go_variants() {
        let Message::Go(g) =
            parse_line("go wtime 60000 btime 59000 winc 1000 binc 1000 movestogo 20")
        else {
            panic!()
        };
        assert_eq!(g.wtime, Some(60_000));
        assert_eq!(g.btime, Some(59_000));
        assert_eq!(g.winc, Some(1000));
        assert_eq!(g.movestogo, Some(20));
        assert!(!g.infinite);

        let Message::Go(g) = parse_line("go infinite") else {
            panic!()
        };
        assert!(g.infinite);

        let Message::Go(g) = parse_line("go searchmoves e2e4 d2d4 depth 12") else {
            panic!()
        };
        assert_eq!(g.searchmoves, vec!["e2e4", "d2d4"]);
        assert_eq!(g.depth, Some(12));
    }

    #[test]
    fn negative_clock_is_accepted() {
        let Message::Go(g) = parse_line("go wtime -50 btime 100") else {
            panic!()
        };
        assert_eq!(g.wtime, Some(-50));
    }

    #[test]
    fn position_variants() {
        assert_eq!(
            parse_line("position startpos moves e2e4 e7e5"),
            Message::Position {
                fen: None,
                moves: vec!["e2e4".into(), "e7e5".into()]
            }
        );
        assert_eq!(
            parse_line(
                "position fen rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1 moves e2e4"
            ),
            Message::Position {
                fen: Some("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".into()),
                moves: vec!["e2e4".into()]
            }
        );
        assert!(matches!(parse_line("position"), Message::Unknown(_)));
    }

    #[test]
    fn setoption_variants() {
        assert_eq!(
            parse_line("setoption name Hash value 128"),
            Message::SetOption {
                name: "Hash".into(),
                value: Some("128".into())
            }
        );
        assert_eq!(
            parse_line("setoption name Clear Hash"),
            Message::SetOption {
                name: "Clear Hash".into(),
                value: None
            }
        );
        assert_eq!(
            parse_line(r"setoption name Debug Log File value C:\a b\log.txt"),
            Message::SetOption {
                name: "Debug Log File".into(),
                value: Some(r"C:\a b\log.txt".into())
            }
        );
    }

    #[test]
    fn simple_commands_and_ids() {
        assert_eq!(parse_line("uci"), Message::Uci);
        assert_eq!(parse_line("  isready \r\n"), Message::IsReady);
        assert_eq!(parse_line("uciok"), Message::UciOk);
        assert_eq!(parse_line("debug on"), Message::Debug(true));
        assert_eq!(
            parse_line("id name Stockfish 18"),
            Message::Id {
                field: "name".into(),
                value: "Stockfish 18".into()
            }
        );
        assert_eq!(
            parse_line("bestmove e2e4 ponder e7e5"),
            Message::BestMove {
                best: "e2e4".into(),
                ponder: Some("e7e5".into())
            }
        );
        assert_eq!(
            parse_line("bestmove (none)"),
            Message::BestMove {
                best: "(none)".into(),
                ponder: None
            }
        );
    }

    #[test]
    fn unknown_lines_are_kept_verbatim() {
        assert_eq!(
            parse_line("  Stockfish 18 by the Stockfish developers  \n"),
            Message::Unknown("Stockfish 18 by the Stockfish developers".into())
        );
        assert_eq!(parse_line(""), Message::Unknown(String::new()));
    }

    #[test]
    fn descriptions_are_readable() {
        let d = parse_line(
            "info depth 18 seldepth 24 multipv 1 score cp 35 nodes 1234567 nps 850123 hashfull 120 pv e2e4 e7e5",
        )
        .describe();
        assert_eq!(
            d,
            "info depth 18/24 | multipv 1 | +0.35 | 1.23M nodes | 850.1k nps | hash 12.0% | pv e2e4 e7e5"
        );
        assert_eq!(
            parse_line("info depth 3 score mate -2").describe(),
            "info depth 3 | #-2"
        );
        assert_eq!(
            parse_line("option name Hash type spin default 16 min 1 max 1024").describe(),
            "option Hash: spin, default 16, range 1..1024"
        );
        assert_eq!(
            parse_line("go wtime 1000 btime 2000 depth 9").describe(),
            "go wtime 1000 btime 2000 depth 9"
        );
        assert_eq!(
            parse_line("setoption name Hash value 64").describe(),
            "setoption Hash = 64"
        );
    }
}
