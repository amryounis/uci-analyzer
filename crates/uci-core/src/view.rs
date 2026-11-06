//! JSON for the viewer: each transcript event plus its decoded UCI message.

use crate::json::Obj;
use crate::parse::{
    parse_line, GoParams, Info, Message, OptionDecl, OptionKind, ScoreBound, ScoreKind,
};
use crate::transcript::{Dir, Event};

fn info_json(info: &Info) -> String {
    let score = match &info.score {
        None => "null".to_string(),
        Some(score) => {
            let mut o = Obj::new();
            match score.kind {
                ScoreKind::Cp(v) => o.str("kind", "cp").num("value", v),
                ScoreKind::Mate(v) => o.str("kind", "mate").num("value", v),
            };
            o.str(
                "bound",
                match score.bound {
                    ScoreBound::Exact => "exact",
                    ScoreBound::Lower => "lower",
                    ScoreBound::Upper => "upper",
                },
            );
            o.finish()
        }
    };
    Obj::new()
        .str("type", "info")
        .opt_num("depth", info.depth)
        .opt_num("seldepth", info.seldepth)
        .opt_num("multipv", info.multipv)
        .raw("score", &score)
        .opt_num("nodes", info.nodes)
        .opt_num("nps", info.nps)
        .opt_num("hashfull", info.hashfull)
        .opt_num("tbhits", info.tbhits)
        .opt_num("time_ms", info.time_ms)
        .opt_num("cpuload", info.cpuload)
        .opt_str("currmove", info.currmove.as_deref())
        .opt_num("currmovenumber", info.currmovenumber)
        .str_array("pv", &info.pv)
        .opt_str("string", info.string.as_deref())
        .finish()
}

fn go_json(go: &GoParams) -> String {
    Obj::new()
        .str("type", "go")
        .str_array("searchmoves", &go.searchmoves)
        .bool("ponder", go.ponder)
        .opt_num("wtime", go.wtime)
        .opt_num("btime", go.btime)
        .opt_num("winc", go.winc)
        .opt_num("binc", go.binc)
        .opt_num("movestogo", go.movestogo)
        .opt_num("depth", go.depth)
        .opt_num("nodes", go.nodes)
        .opt_num("mate", go.mate)
        .opt_num("movetime", go.movetime)
        .bool("infinite", go.infinite)
        .finish()
}

fn option_json(decl: &OptionDecl) -> String {
    let mut o = Obj::new();
    o.str("type", "option").str("name", &decl.name);
    match &decl.kind {
        OptionKind::Check { default } => {
            o.str("kind", "check").opt_bool("default", *default);
        }
        OptionKind::Spin { default, min, max } => {
            o.str("kind", "spin")
                .opt_num("default", *default)
                .opt_num("min", *min)
                .opt_num("max", *max);
        }
        OptionKind::Combo { default, vars } => {
            o.str("kind", "combo")
                .opt_str("default", default.as_deref())
                .str_array("vars", vars);
        }
        OptionKind::Button => {
            o.str("kind", "button");
        }
        OptionKind::String { default } => {
            o.str("kind", "string")
                .opt_str("default", default.as_deref());
        }
        OptionKind::Unknown(t) => {
            o.str("kind", "unknown").str("raw_type", t);
        }
    }
    o.finish()
}

impl Message {
    /// The decoded message as a JSON object with a `type` field.
    pub fn to_json(&self) -> String {
        let simple = |name: &str| Obj::new().str("type", name).finish();
        match self {
            Message::Uci => simple("uci"),
            Message::Debug(on) => Obj::new().str("type", "debug").bool("on", *on).finish(),
            Message::IsReady => simple("isready"),
            Message::SetOption { name, value } => Obj::new()
                .str("type", "setoption")
                .str("name", name)
                .opt_str("value", value.as_deref())
                .finish(),
            Message::UciNewGame => simple("ucinewgame"),
            Message::Position { fen, moves } => Obj::new()
                .str("type", "position")
                .opt_str("fen", fen.as_deref())
                .str_array("moves", moves)
                .finish(),
            Message::Go(go) => go_json(go),
            Message::Stop => simple("stop"),
            Message::PonderHit => simple("ponderhit"),
            Message::Quit => simple("quit"),
            Message::Id { field, value } => Obj::new()
                .str("type", "id")
                .str("field", field)
                .str("value", value)
                .finish(),
            Message::UciOk => simple("uciok"),
            Message::ReadyOk => simple("readyok"),
            Message::BestMove { best, ponder } => Obj::new()
                .str("type", "bestmove")
                .str("best", best)
                .opt_str("ponder", ponder.as_deref())
                .finish(),
            Message::Info(info) => info_json(info),
            Message::Option(decl) => option_json(decl),
            Message::Unknown(_) => simple("unknown"),
        }
    }
}

/// One event as the viewer receives it:
/// `{"i":0,"t":12,"dir":"gui>engine","line":"isready","desc":"isready","msg":{"type":"isready"}}`.
/// `msg` is `null` for stderr and shim notes.
pub fn view_json(index: u64, event: &Event) -> String {
    let (desc, msg) = match event.dir {
        Dir::GuiToEngine | Dir::ViewerToEngine | Dir::EngineToGui => {
            let message = parse_line(&event.line);
            (message.describe(), message.to_json())
        }
        Dir::EngineStderr | Dir::Shim => (event.line.clone(), "null".to_string()),
    };
    Obj::new()
        .num("i", index)
        .num("t", event.t_ms)
        .str("dir", event.dir.as_str())
        .str("line", &event.line)
        .str("desc", &desc)
        .raw("msg", &msg)
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(dir: Dir, line: &str) -> String {
        view_json(
            3,
            &Event {
                t_ms: 250,
                dir,
                line: line.to_string(),
            },
        )
    }

    #[test]
    fn info_lines_become_structured() {
        let json = view(
            Dir::EngineToGui,
            "info depth 18 seldepth 24 multipv 1 score cp 35 nodes 1234567 nps 850123 hashfull 120 pv e2e4 e7e5",
        );
        assert!(json.starts_with(r#"{"i":3,"t":250,"dir":"engine>gui","line":"info depth 18"#));
        assert!(json.contains(r#""msg":{"type":"info","depth":18,"seldepth":24,"multipv":1,"score":{"kind":"cp","value":35,"bound":"exact"},"nodes":1234567,"nps":850123,"hashfull":120,"tbhits":null"#));
        assert!(json.contains(r#""pv":["e2e4","e7e5"]"#));
        assert!(json.contains(r#""desc":"info depth 18/24 | multipv 1 | +0.35"#));
    }

    #[test]
    fn options_and_commands_decode() {
        let json = view(
            Dir::EngineToGui,
            "option name Style type combo default Normal var Solid var Very Aggressive",
        );
        assert!(json.contains(
            r#""msg":{"type":"option","name":"Style","kind":"combo","default":"Normal","vars":["Solid","Very Aggressive"]}"#
        ));
        let json = view(Dir::GuiToEngine, "setoption name Hash value 128");
        assert!(json.contains(r#""msg":{"type":"setoption","name":"Hash","value":"128"}"#));
        let json = view(Dir::GuiToEngine, "go infinite");
        assert!(json.contains(r#""type":"go""#) && json.contains(r#""infinite":true"#));
    }

    #[test]
    fn stderr_and_shim_notes_have_no_message() {
        assert!(view(Dir::EngineStderr, "boom").ends_with(r#""msg":null}"#));
        assert!(view(Dir::Shim, "engine exited").ends_with(r#""msg":null}"#));
    }

    #[test]
    fn unknown_lines_keep_their_text() {
        let json = view(Dir::EngineToGui, "Stockfish 18 by the developers");
        assert!(json.contains(r#""line":"Stockfish 18 by the developers""#));
        assert!(json.contains(r#""msg":{"type":"unknown"}"#));
    }

    #[test]
    fn every_view_line_is_valid_single_line_json_shape() {
        for line in [
            "uci",
            "bestmove e2e4 ponder e7e5",
            "info string tab\tand \"quotes\"",
            "position startpos moves e2e4",
            "id name Stock\u{e9}fish",
        ] {
            let json = view(Dir::EngineToGui, line);
            assert!(!json.contains('\n'));
            assert!(json.starts_with('{') && json.ends_with('}'));
        }
    }
}
