//! Transcript format: JSON Lines, one event per line.
//!
//! ```text
//! {"t":12,"dir":"gui>engine","line":"isready"}
//! ```
//!
//! * `t`    milliseconds since the shim started
//! * `dir`  `gui>engine`, `engine>gui`, `engine>stderr` or `shim` (shim notes)
//! * `line` the text of the line without its line ending
//!
//! Readers ignore unknown keys, so fields can be added later without breaking
//! old transcripts. The same line format is what the shim streams to a viewer.

use crate::json::{self, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    GuiToEngine,
    /// A command typed into the viewer by the person watching, not sent by
    /// the host GUI.
    ViewerToEngine,
    EngineToGui,
    EngineStderr,
    Shim,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::GuiToEngine => "gui>engine",
            Dir::ViewerToEngine => "viewer>engine",
            Dir::EngineToGui => "engine>gui",
            Dir::EngineStderr => "engine>stderr",
            Dir::Shim => "shim",
        }
    }

    pub fn parse(s: &str) -> Option<Dir> {
        match s {
            "gui>engine" => Some(Dir::GuiToEngine),
            "viewer>engine" => Some(Dir::ViewerToEngine),
            "engine>gui" => Some(Dir::EngineToGui),
            "engine>stderr" => Some(Dir::EngineStderr),
            "shim" => Some(Dir::Shim),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub t_ms: u64,
    pub dir: Dir,
    pub line: String,
}

impl Event {
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(self.line.len() + 48);
        out.push_str("{\"t\":");
        out.push_str(&self.t_ms.to_string());
        out.push_str(",\"dir\":\"");
        out.push_str(self.dir.as_str());
        out.push_str("\",\"line\":\"");
        json::escape_into(&mut out, &self.line);
        out.push_str("\"}");
        out
    }

    pub fn from_json(src: &str) -> Result<Event, String> {
        let mut t_ms = None;
        let mut dir = None;
        let mut line = None;
        for (key, value) in json::parse_flat_object(src)? {
            match (key.as_str(), value) {
                ("t", Value::Num(n)) => {
                    t_ms = Some(n.parse::<u64>().map_err(|_| format!("bad t: {n}"))?)
                }
                ("dir", Value::Str(s)) => {
                    dir = Some(Dir::parse(&s).ok_or_else(|| format!("unknown dir: {s}"))?)
                }
                ("line", Value::Str(s)) => line = Some(s),
                _ => {}
            }
        }
        Ok(Event {
            t_ms: t_ms.ok_or("missing t")?,
            dir: dir.ok_or("missing dir")?,
            line: line.ok_or("missing line")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(line: &str) -> Event {
        Event {
            t_ms: 1234,
            dir: Dir::GuiToEngine,
            line: line.to_string(),
        }
    }

    #[test]
    fn writes_the_documented_shape() {
        assert_eq!(
            event("isready").to_json(),
            r#"{"t":1234,"dir":"gui>engine","line":"isready"}"#
        );
    }

    #[test]
    fn roundtrips_awkward_text() {
        for text in [
            "",
            "plain",
            "quote \" and backslash \\ done",
            "tab\tnewline\ncarriage\rreturn",
            "control \u{1} \u{1f} chars",
            "unicode caf\u{e9} \u{4e2d}\u{6587} \u{1f600}",
            r"C:\Program Files\Engine\engine.exe",
        ] {
            let json = event(text).to_json();
            assert!(!json.contains('\n'), "one event must stay on one line");
            assert_eq!(Event::from_json(&json).unwrap(), event(text), "{text:?}");
        }
    }

    #[test]
    fn all_directions_roundtrip() {
        for dir in [
            Dir::GuiToEngine,
            Dir::ViewerToEngine,
            Dir::EngineToGui,
            Dir::EngineStderr,
            Dir::Shim,
        ] {
            let e = Event {
                t_ms: 7,
                dir,
                line: "x".into(),
            };
            assert_eq!(Event::from_json(&e.to_json()).unwrap(), e);
        }
    }

    #[test]
    fn reader_tolerates_whitespace_order_and_extra_keys() {
        let json = r#" { "line" : "uci" , "extra": [1] , "t" : 5 , "dir" : "engine>gui" } "#;
        // arrays are not supported values: this must be an error, not a panic
        assert!(Event::from_json(json).is_err());

        let json =
            r#" { "line" : "uci" , "extra": true , "t" : 5 , "ver": 2, "dir" : "engine>gui" } "#;
        assert_eq!(
            Event::from_json(json).unwrap(),
            Event {
                t_ms: 5,
                dir: Dir::EngineToGui,
                line: "uci".into()
            }
        );
    }

    #[test]
    fn reads_surrogate_pairs() {
        let json = r#"{"t":0,"dir":"shim","line":"\ud83d\ude00"}"#;
        assert_eq!(Event::from_json(json).unwrap().line, "\u{1f600}");
    }

    #[test]
    fn rejects_bad_input_without_panicking() {
        for bad in [
            "",
            "{",
            "[]",
            r#"{"t":1}"#,
            r#"{"t":1,"dir":"sideways","line":"x"}"#,
            r#"{"t":-1,"dir":"shim","line":"x"}"#,
            r#"{"t":1,"dir":"shim","line":"unterminated}"#,
            r#"{"t":1,"dir":"shim","line":"x"} trailing"#,
            r#"{"t":1,"dir":"shim","line":"bad \q escape"}"#,
        ] {
            assert!(Event::from_json(bad).is_err(), "{bad:?}");
        }
    }
}
