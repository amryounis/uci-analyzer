//! A very small JSON reader and writer, just enough for transcripts, the
//! session registry and the viewer's stream. Keeps the shim dependency-free.

/// A scalar JSON value as read from a flat object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    /// The number exactly as written; parse it as the type you need.
    Num(String),
    Bool(bool),
    /// `null`.
    Other,
}

pub fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    escape_into(&mut out, text);
    out
}

/// Builds one JSON object a field at a time.
pub struct Obj {
    buf: String,
    first: bool,
}

impl Default for Obj {
    fn default() -> Self {
        Self::new()
    }
}

impl Obj {
    pub fn new() -> Obj {
        Obj {
            buf: String::from("{"),
            first: true,
        }
    }

    fn key(&mut self, key: &str) {
        if !self.first {
            self.buf.push(',');
        }
        self.first = false;
        self.buf.push('"');
        self.buf.push_str(key);
        self.buf.push_str("\":");
    }

    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        self.buf.push('"');
        escape_into(&mut self.buf, value);
        self.buf.push('"');
        self
    }

    pub fn opt_str(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        match value {
            Some(v) => self.str(key, v),
            None => self.raw(key, "null"),
        }
    }

    pub fn num<T: std::fmt::Display>(&mut self, key: &str, value: T) -> &mut Self {
        self.key(key);
        self.buf.push_str(&value.to_string());
        self
    }

    pub fn opt_num<T: std::fmt::Display>(&mut self, key: &str, value: Option<T>) -> &mut Self {
        match value {
            Some(v) => self.num(key, v),
            None => self.raw(key, "null"),
        }
    }

    pub fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.raw(key, if value { "true" } else { "false" })
    }

    pub fn opt_bool(&mut self, key: &str, value: Option<bool>) -> &mut Self {
        match value {
            Some(v) => self.bool(key, v),
            None => self.raw(key, "null"),
        }
    }

    pub fn str_array(&mut self, key: &str, values: &[String]) -> &mut Self {
        self.key(key);
        self.buf.push('[');
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                self.buf.push(',');
            }
            self.buf.push('"');
            escape_into(&mut self.buf, v);
            self.buf.push('"');
        }
        self.buf.push(']');
        self
    }

    /// Adds a field whose value is already valid JSON.
    pub fn raw(&mut self, key: &str, json: &str) -> &mut Self {
        self.key(key);
        self.buf.push_str(json);
        self
    }

    pub fn finish(&mut self) -> String {
        let mut out = std::mem::take(&mut self.buf);
        out.push('}');
        self.first = true;
        self.buf.push('{');
        out
    }
}

/// Parses one flat JSON object whose values are strings, numbers, `true`,
/// `false` or `null`. Nested objects and arrays are an error.
pub fn parse_flat_object(src: &str) -> Result<Vec<(String, Value)>, String> {
    let mut reader = Reader {
        chars: src.chars().peekable(),
    };
    let fields = reader.object()?;
    reader.skip_ws();
    if reader.chars.peek().is_some() {
        return Err("trailing characters after object".into());
    }
    Ok(fields)
}

struct Reader<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Reader<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn expect(&mut self, want: char) -> Result<(), String> {
        self.skip_ws();
        match self.chars.next() {
            Some(c) if c == want => Ok(()),
            other => Err(format!("expected '{want}', found {other:?}")),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..4 {
            let c = self.chars.next().ok_or("short \\u escape")?;
            value = value * 16 + c.to_digit(16).ok_or("bad hex digit in \\u escape")?;
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.chars.next() {
                None => return Err("unterminated string".into()),
                Some('"') => return Ok(out),
                Some('\\') => match self.chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let hi = self.hex4()?;
                        if (0xD800..0xDC00).contains(&hi) {
                            let low_follows =
                                self.chars.next() == Some('\\') && self.chars.next() == Some('u');
                            let lo = if low_follows { self.hex4()? } else { 0 };
                            if (0xDC00..0xE000).contains(&lo) {
                                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            } else {
                                out.push('\u{FFFD}');
                            }
                        } else {
                            out.push(char::from_u32(hi).unwrap_or('\u{FFFD}'));
                        }
                    }
                    other => return Err(format!("bad escape: {other:?}")),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.chars.peek().copied() {
            Some('"') => Ok(Value::Str(self.string()?)),
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let mut text = String::new();
                while let Some(&c) = self.chars.peek() {
                    if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E') {
                        text.push(c);
                        self.chars.next();
                    } else {
                        break;
                    }
                }
                Ok(Value::Num(text))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let mut word = String::new();
                while let Some(&c) = self.chars.peek() {
                    if c.is_ascii_alphabetic() {
                        word.push(c);
                        self.chars.next();
                    } else {
                        break;
                    }
                }
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Other),
                    _ => Err(format!("bad literal: {word}")),
                }
            }
            other => Err(format!("unsupported value starting with {other:?}")),
        }
    }

    fn object(&mut self) -> Result<Vec<(String, Value)>, String> {
        self.expect('{')?;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.chars.peek() == Some(&'}') {
            self.chars.next();
            return Ok(fields);
        }
        loop {
            let key = self.string()?;
            self.expect(':')?;
            let value = self.value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some('}') => return Ok(fields),
                other => return Err(format!("expected ',' or '}}', found {other:?}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_writes_valid_objects() {
        let json = Obj::new()
            .str("a", "x\"y")
            .num("b", 5)
            .opt_num::<u32>("c", None)
            .bool("d", true)
            .opt_str("e", Some("é"))
            .str_array("f", &["m1".to_string(), "m2".to_string()])
            .raw("g", "{\"k\":1}")
            .finish();
        assert_eq!(
            json,
            r#"{"a":"x\"y","b":5,"c":null,"d":true,"e":"é","f":["m1","m2"],"g":{"k":1}}"#
        );
    }

    #[test]
    fn builder_can_be_reused_after_finish() {
        let mut obj = Obj::new();
        assert_eq!(obj.num("a", 1).finish(), r#"{"a":1}"#);
        assert_eq!(obj.num("b", 2).finish(), r#"{"b":2}"#);
    }

    #[test]
    fn flat_objects_parse() {
        let fields =
            parse_flat_object(r#" {"pid": 42, "engine":"C:\\e\\sf.exe", "ok": true} "#).unwrap();
        assert_eq!(fields[0], ("pid".to_string(), Value::Num("42".to_string())));
        assert_eq!(
            fields[1],
            (
                "engine".to_string(),
                Value::Str("C:\\e\\sf.exe".to_string())
            )
        );
        assert_eq!(fields[2].1, Value::Bool(true));
        let nulls = parse_flat_object(r#"{"a":null,"b":false}"#).unwrap();
        assert_eq!(nulls[0].1, Value::Other);
        assert_eq!(nulls[1].1, Value::Bool(false));
    }

    #[test]
    fn nested_values_are_rejected() {
        assert!(parse_flat_object(r#"{"a":[1]}"#).is_err());
        assert!(parse_flat_object(r#"{"a":{"b":1}}"#).is_err());
    }
}
