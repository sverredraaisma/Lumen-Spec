//! A JSON parser and writer, hand-written, covering exactly the subset the
//! vectors use.
//!
//! Pulling in `serde` would buy derives this crate has no use for — every value
//! here is walked dynamically, because the runner must not know what fields a
//! vector contains. It would also put a dependency tree between a new
//! implementation and the ability to run the suite, and the suite is supposed to
//! be the easy part of adopting the protocol. Roughly three hundred lines is a
//! fair price for `cargo run` working on a machine with no network.
//!
//! Numbers are kept as text rather than converted to `f64`. `show_time_us` is a
//! `u64` and the protocol uses its top values; a parser that rounded them would
//! quietly turn a conformance suite into a lie.

use std::fmt::Write as _;

/// A parsed JSON value.
#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    /// Canonical text of a number: integers are normalised so `1` and `1e0`
    /// compare equal, anything else keeps the literal it was written with.
    Number(String),
    String(String),
    Array(Vec<Json>),
    /// Insertion-ordered so a re-serialised vector reads like the file it came
    /// from. Key order is never significant to equality.
    Object(Vec<(String, Json)>),
}

/// Where and why parsing stopped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct JsonError {
    pub message: String,
    pub offset: usize,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl PartialEq for Json {
    fn eq(&self, other: &Json) -> bool {
        match (self, other) {
            (Json::Null, Json::Null) => true,
            (Json::Bool(a), Json::Bool(b)) => a == b,
            (Json::Number(a), Json::Number(b)) => a == b,
            (Json::String(a), Json::String(b)) => a == b,
            (Json::Array(a), Json::Array(b)) => a == b,
            // Key order carries no meaning in JSON, and an adapter has no reason
            // to reproduce the order a vector file happens to use.
            (Json::Object(a), Json::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| {
                        other.get(k).is_some_and(|o| o == v) && b.iter().any(|(k2, _)| k2 == k)
                    })
            }
            _ => false,
        }
    }
}

impl Json {
    /// Field lookup on an object; `None` for any other kind of value.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(fields) => Some(fields),
            _ => None,
        }
    }

    /// A non-negative integer, or `None` if the value is not one that fits.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The name of this kind of value, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "bool",
            Json::Number(_) => "number",
            Json::String(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
        }
    }

    /// Compact JSON, keys in insertion order. This is what goes on a protocol
    /// line.
    pub fn to_compact(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, false);
        out
    }

    /// Compact JSON with object keys sorted, so two structurally equal values
    /// produce the same string. Used as a map key and in failure diffs, where
    /// stable output is worth more than fidelity to the source order.
    pub fn to_canonical(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, true);
        out
    }

    fn write(&self, out: &mut String, sorted: bool) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(text) => out.push_str(text),
            Json::String(s) => write_string(out, s),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out, sorted);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                let mut order: Vec<&(String, Json)> = fields.iter().collect();
                if sorted {
                    order.sort_by(|a, b| a.0.cmp(&b.0));
                }
                for (i, (k, v)) in order.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(out, k);
                    out.push(':');
                    v.write(out, sorted);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse a complete JSON document. Trailing content other than whitespace is an
/// error, which is what catches two values concatenated onto one protocol line.
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(p.err("trailing content after the value"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, message: &str) -> JsonError {
        JsonError {
            message: message.to_string(),
            offset: self.pos,
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, lit: &str) -> Result<(), JsonError> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(self.err(&format!("expected `{lit}`")))
        }
    }

    fn value(&mut self) -> Result<Json, JsonError> {
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.eat("true").map(|()| Json::Bool(true)),
            Some(b'f') => self.eat("false").map(|()| Json::Bool(false)),
            Some(b'n') => self.eat("null").map(|()| Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(c) => Err(self.err(&format!("unexpected byte `{}`", c as char))),
        }
    }

    fn object(&mut self) -> Result<Json, JsonError> {
        self.pos += 1; // '{'
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected a key string"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.err("expected `:`"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(fields));
                }
                _ => return Err(self.err("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self) -> Result<Json, JsonError> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.err("expected `,` or `]`")),
            }
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.pos += 1; // '"'
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or_else(|| self.err("unterminated string"))?;
            self.pos += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.peek().ok_or_else(|| self.err("unterminated escape"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        other => {
                            return Err(self.err(&format!("unknown escape `\\{}`", other as char)))
                        }
                    }
                }
                c if c < 0x20 => return Err(self.err("raw control character in a string")),
                // The input is a `&str`, so a multi-byte sequence is already
                // known-good UTF-8; copy it across verbatim.
                c if c < 0x80 => out.push(c as char),
                _ => {
                    let start = self.pos - 1;
                    while self.peek().is_some_and(|b| b & 0xC0 == 0x80) {
                        self.pos += 1;
                    }
                    out.push_str(
                        std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("\u{fffd}"),
                    );
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, JsonError> {
        let unit = self.hex4()?;
        // Surrogate pairs: the vectors do not need them, but a JSON writer on
        // the far side of an adapter may well emit them, and silently producing
        // U+FFFD would turn a passing implementation into a failing one.
        if (0xD800..0xDC00).contains(&unit) {
            self.eat("\\u")?;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.err("expected a low surrogate"));
            }
            let combined = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
            return char::from_u32(combined).ok_or_else(|| self.err("invalid surrogate pair"));
        }
        char::from_u32(unit).ok_or_else(|| self.err("invalid \\u escape"))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.err("truncated \\u escape"));
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| self.err("invalid \\u escape"))?;
        let value = u32::from_str_radix(text, 16).map_err(|_| self.err("invalid \\u escape"))?;
        self.pos += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.err("invalid number"))?;
        if text.is_empty() || text == "-" {
            return Err(self.err("invalid number"));
        }
        // Normalise integers so `7`, `+7` and `7e0` cannot compare unequal. A
        // non-integer keeps its literal: the protocol has no float fields, so
        // there is nothing to normalise it against.
        Ok(Json::Number(match text.parse::<i128>() {
            Ok(n) => n.to_string(),
            Err(_) => text.to_string(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Json {
        parse(text).expect(text)
    }

    #[test]
    fn parses_every_scalar() {
        assert_eq!(ok("null"), Json::Null);
        assert_eq!(ok(" true "), Json::Bool(true));
        assert_eq!(ok("false"), Json::Bool(false));
        assert_eq!(ok("\"hi\""), Json::String("hi".into()));
        assert_eq!(ok("-12"), Json::Number("-12".into()));
    }

    #[test]
    fn integers_beyond_f64_survive_exactly() {
        // The reason this parser exists: show_time_us uses the top of the u64
        // range, and 18446744073709551615 is not representable as an f64.
        let v = ok("{\"show_time_us\":18446744073709551615}");
        assert_eq!(v.get("show_time_us").unwrap().as_u64(), Some(u64::MAX));
        assert_eq!(v.to_compact(), "{\"show_time_us\":18446744073709551615}");
    }

    #[test]
    fn integer_spellings_normalise_to_one_form() {
        assert_eq!(ok("7"), ok("7"));
        assert_eq!(ok("-0"), Json::Number("0".into()));
        // A fraction has no canonical integer form and keeps its literal.
        assert_eq!(ok("1.5"), Json::Number("1.5".into()));
        assert_ne!(ok("1.5"), ok("1.50"));
    }

    #[test]
    fn parses_nested_containers() {
        let v = ok(r#"{"a":[1,{"b":null}],"c":{}}"#);
        assert_eq!(v.get("a").unwrap().as_array().unwrap().len(), 2);
        assert_eq!(v.get("c").unwrap().as_object().unwrap().len(), 0);
        assert_eq!(ok("[]").as_array().unwrap().len(), 0);
        assert!(v.get("missing").is_none());
    }

    #[test]
    fn object_equality_ignores_key_order_but_not_content() {
        assert_eq!(ok(r#"{"a":1,"b":2}"#), ok(r#"{"b":2,"a":1}"#));
        assert_ne!(ok(r#"{"a":1}"#), ok(r#"{"a":2}"#));
        assert_ne!(ok(r#"{"a":1}"#), ok(r#"{"a":1,"b":2}"#));
        // Same length, disjoint keys: the `any` arm is what catches this.
        assert_ne!(ok(r#"{"a":1}"#), ok(r#"{"b":1}"#));
        assert_ne!(ok("1"), ok("\"1\""));
        assert_ne!(ok("[1]"), ok("[2]"));
    }

    #[test]
    fn string_escapes_round_trip() {
        let v = ok(r#""a\"b\\c\/d\be\ff\ng\rh\ti""#);
        assert_eq!(v.as_str().unwrap(), "a\"b\\c/d\u{8}e\u{c}f\ng\rh\ti");
        assert_eq!(ok(r#""\u0041\u00e9""#).as_str().unwrap(), "Aé");
        // Surrogate pair for U+1F600.
        assert_eq!(ok(r#""\ud83d\ude00""#).as_str().unwrap(), "😀");
    }

    #[test]
    fn multibyte_utf8_passes_through_unchanged() {
        let v = ok("\"bewegingéé\"");
        assert_eq!(v.as_str().unwrap(), "bewegingéé");
        assert_eq!(v.to_compact(), "\"bewegingéé\"");
    }

    #[test]
    fn serialising_escapes_what_must_be_escaped() {
        let v = Json::String("q\"b\\s\nr\rt\tc\u{1}".into());
        assert_eq!(v.to_compact(), r#""q\"b\\s\nr\rt\tc\u0001""#);
        assert_eq!(parse(&v.to_compact()).unwrap(), v);
        assert_eq!(Json::Null.to_compact(), "null");
        assert_eq!(Json::Bool(false).to_compact(), "false");
    }

    #[test]
    fn canonical_form_sorts_keys_at_every_depth() {
        let v = ok(r#"{"b":1,"a":{"z":[{"y":1,"x":2}],"w":true}}"#);
        assert_eq!(
            v.to_canonical(),
            r#"{"a":{"w":true,"z":[{"x":2,"y":1}]},"b":1}"#
        );
        // Insertion order is what `to_compact` preserves.
        assert_eq!(ok(r#"{"b":1,"a":2}"#).to_compact(), r#"{"b":1,"a":2}"#);
    }

    #[test]
    fn accessors_return_none_for_the_wrong_kind() {
        assert!(ok("1").get("a").is_none());
        assert!(ok("1").as_str().is_none());
        assert!(ok("1").as_array().is_none());
        assert!(ok("1").as_object().is_none());
        assert!(ok("\"x\"").as_u64().is_none());
        assert!(ok("-1").as_u64().is_none());
    }

    #[test]
    fn kind_names_every_variant() {
        for (text, name) in [
            ("null", "null"),
            ("true", "bool"),
            ("1", "number"),
            ("\"s\"", "string"),
            ("[]", "array"),
            ("{}", "object"),
        ] {
            assert_eq!(ok(text).kind(), name);
        }
    }

    #[test]
    fn every_syntax_error_is_reported_with_an_offset() {
        for bad in [
            "",
            "{",
            "{\"a\"}",
            "{\"a\":1",
            "{1:2}",
            "{\"a\":1,}",
            "[",
            "[1",
            "[1,]",
            "\"unterminated",
            "\"\\",
            "\"\\q\"",
            "\"\u{1}\"",
            "\"\\u00\"",
            "\"\\ud83d\"",
            "\"\\ud83dxx\"",
            "\"\\ud83d\\u0041\"",
            "\"\\udfff\"",
            "tru",
            "nul",
            "fals",
            "-",
            "@",
            "1 2",
        ] {
            assert!(parse(bad).is_err(), "`{bad}` should not parse");
        }
        let e = parse("  @").unwrap_err();
        assert_eq!(e.offset, 2);
        assert!(e.to_string().contains("at byte 2"), "{e}");
    }

    #[test]
    fn a_lone_high_surrogate_without_a_pair_is_an_error_not_a_replacement_char() {
        // Silently substituting U+FFFD would turn a conforming adapter into a
        // failing one for reasons nobody could see in the diff.
        assert!(parse(r#""\ud800""#).is_err());
    }
}
