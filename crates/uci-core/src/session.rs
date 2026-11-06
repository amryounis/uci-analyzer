//! The session registry entry a running shim publishes, and the hello line it
//! sends first to every viewer that connects.

use crate::json::{self, Obj, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub pid: u32,
    /// Localhost TCP port of the shim's viewer server.
    pub port: u16,
    pub engine: String,
    pub started_unix_ms: u64,
    /// Transcript file, empty when logging is off.
    pub transcript: String,
    /// Whether the shim accepts commands typed into the viewer.
    pub input: bool,
}

impl SessionInfo {
    /// The registry file content.
    pub fn to_json(&self) -> String {
        Obj::new()
            .num("pid", self.pid)
            .num("port", self.port)
            .str("engine", &self.engine)
            .num("started_unix_ms", self.started_unix_ms)
            .str("transcript", &self.transcript)
            .bool("input", self.input)
            .finish()
    }

    /// The first line the shim sends to a connected viewer.
    pub fn hello_json(&self, version: &str) -> String {
        Obj::new()
            .str("hello", "uci-shim")
            .str("version", version)
            .num("pid", self.pid)
            .str("engine", &self.engine)
            .num("started_unix_ms", self.started_unix_ms)
            .str("transcript", &self.transcript)
            .bool("input", self.input)
            .finish()
    }

    pub fn from_json(src: &str) -> Result<SessionInfo, String> {
        let mut pid = None;
        let mut port = None;
        let mut engine = String::new();
        let mut started = 0u64;
        let mut transcript = String::new();
        let mut input = true;
        for (key, value) in json::parse_flat_object(src)? {
            match (key.as_str(), value) {
                ("pid", Value::Num(n)) => pid = n.parse().ok(),
                ("port", Value::Num(n)) => port = n.parse().ok(),
                ("engine", Value::Str(s)) => engine = s,
                ("started_unix_ms", Value::Num(n)) => started = n.parse().unwrap_or(0),
                ("transcript", Value::Str(s)) => transcript = s,
                ("input", Value::Bool(b)) => input = b,
                _ => {}
            }
        }
        Ok(SessionInfo {
            pid: pid.ok_or("missing pid")?,
            port: port.ok_or("missing port")?,
            engine,
            started_unix_ms: started,
            transcript,
            input,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_entry_roundtrips() {
        let info = SessionInfo {
            pid: 4242,
            port: 51234,
            engine: r"C:\Program Files\SF\sf.exe".into(),
            started_unix_ms: 1_793_000_000_000,
            transcript: r"C:\Users\you\AppData\Local\uci-visualizer\transcripts\a.jsonl".into(),
            input: false,
        };
        assert_eq!(SessionInfo::from_json(&info.to_json()).unwrap(), info);
    }

    #[test]
    fn hello_names_the_shim_and_pid() {
        let info = SessionInfo {
            pid: 7,
            port: 1,
            engine: "e".into(),
            started_unix_ms: 5,
            transcript: String::new(),
            input: true,
        };
        let hello = info.hello_json("0.2.0");
        assert!(hello.starts_with(r#"{"hello":"uci-shim","version":"0.2.0","pid":7"#));
        assert!(hello.ends_with(r#""input":true}"#), "{hello}");
    }

    #[test]
    fn missing_fields_are_errors() {
        assert!(SessionInfo::from_json(r#"{"pid":1}"#).is_err());
        assert!(SessionInfo::from_json("not json").is_err());
    }
}
