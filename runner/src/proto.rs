//! The adapter line protocol: one request per line, one response per line.
//!
//! Line-oriented and text, so an adapter can be written in anything with a
//! standard library, and so a failing exchange can be pasted into a bug report
//! and replayed by hand. The full grammar is in `adapters/README.md`; this
//! module is the normative implementation of it.

use crate::json::{self, Json};

/// Protocol revision the runner speaks. Sent in the handshake so a mismatch is
/// a clear message rather than a puzzling parse failure ten lines later.
pub const PROTOCOL: u64 = 1;

/// What the runner asks an adapter to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Identify yourself. Always first.
    Hello,
    /// Decode this datagram.
    Decode { datagram: String },
    /// Encode this structure back to a datagram.
    Encode { value: Json },
}

impl Request {
    /// The verb this request uses on the wire.
    pub fn verb(&self) -> &'static str {
        match self {
            Request::Hello => "hello",
            Request::Decode { .. } => "decode",
            Request::Encode { .. } => "encode",
        }
    }

    /// Render as a protocol line, without its trailing newline.
    pub fn to_line(&self) -> String {
        let body = match self {
            Request::Hello => Json::Object(vec![(
                "protocol".to_string(),
                Json::Number(PROTOCOL.to_string()),
            )]),
            Request::Decode { datagram } => Json::Object(vec![(
                "datagram".to_string(),
                Json::String(datagram.clone()),
            )]),
            Request::Encode { value } => value.clone(),
        };
        format!("{} {}", self.verb(), body.to_compact())
    }

    /// Read a request line. Adapters need this as much as the runner does, and
    /// a second parser on the far side is a second place for the grammar to
    /// drift.
    pub fn parse(line: &str) -> Result<Request, String> {
        let (verb, body) = split_line(line)?;
        let value = json::parse(body).map_err(|e| format!("`{verb}` body is not JSON: {e}"))?;
        match verb {
            "hello" => Ok(Request::Hello),
            "decode" => match value.get("datagram").and_then(Json::as_str) {
                Some(hex) => Ok(Request::Decode {
                    datagram: hex.to_string(),
                }),
                None => Err("`decode` needs a string field `datagram`".to_string()),
            },
            "encode" => Ok(Request::Encode { value }),
            other => Err(format!("unknown request verb `{other}`")),
        }
    }
}

/// What an adapter answers.
///
/// The three outcomes are the protocol's own vocabulary, not a transport
/// detail: `ignore` and `reject` mean genuinely different things to a receiver,
/// and collapsing them is the mistake the malformed vectors exist to catch.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    /// Success, carrying the result.
    Ok(Json),
    /// The datagram was dropped silently. No error was raised.
    Ignore,
    /// The datagram was refused. The text is for humans only.
    Reject(String),
    /// The adapter itself failed. Never a conforming answer to anything.
    Error(String),
}

impl Response {
    pub fn to_line(&self) -> String {
        match self {
            Response::Ok(value) => format!("ok {}", value.to_compact()),
            Response::Ignore => "ignore".to_string(),
            Response::Reject(why) => format!("reject {why}"),
            Response::Error(why) => format!("error {why}"),
        }
    }

    pub fn parse(line: &str) -> Result<Response, String> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (verb, rest) = match line.split_once(' ') {
            Some((verb, rest)) => (verb, rest),
            None => (line, ""),
        };
        match verb {
            "ok" => json::parse(rest)
                .map(Response::Ok)
                .map_err(|e| format!("`ok` body is not JSON: {e}")),
            // A bare word is the whole message for these two: an adapter must
            // not have to invent a reason to say it dropped something.
            "ignore" => Ok(Response::Ignore),
            "reject" => Ok(Response::Reject(rest.to_string())),
            "error" => Ok(Response::Error(rest.to_string())),
            other => Err(format!("unknown response verb `{other}`")),
        }
    }

    /// Short form for a report line.
    pub fn summary(&self) -> String {
        match self {
            Response::Ok(v) => format!("ok {}", v.to_canonical()),
            Response::Ignore => "ignore".to_string(),
            Response::Reject(why) => format!("reject {why}"),
            Response::Error(why) => format!("error {why}"),
        }
    }
}

fn split_line(line: &str) -> Result<(&str, &str), String> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.split_once(' ') {
        Some((verb, rest)) => Ok((verb, rest.trim_start())),
        None => Err(format!("`{line}` has no body")),
    }
}

/// True for a line the far side should skip: blank, or a `#` comment.
///
/// Adapters print diagnostics; a protocol that could not tolerate that would
/// push every implementer into inventing a side channel.
pub fn is_noise(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(pairs: &[(&str, Json)]) -> Json {
        Json::Object(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn requests_round_trip_through_their_line_form() {
        let requests = [
            Request::Hello,
            Request::Decode {
                datagram: "4c01".to_string(),
            },
            Request::Encode {
                value: obj(&[("header", obj(&[("type", Json::Number("17".into()))]))]),
            },
        ];
        for request in requests {
            let line = request.to_line();
            assert_eq!(Request::parse(&line).unwrap(), request, "{line}");
        }
    }

    #[test]
    fn the_handshake_carries_the_protocol_revision() {
        assert_eq!(Request::Hello.to_line(), r#"hello {"protocol":1}"#);
        assert_eq!(Request::Hello.verb(), "hello");
        assert_eq!(
            Request::Decode {
                datagram: "ab".into()
            }
            .to_line(),
            r#"decode {"datagram":"ab"}"#
        );
    }

    #[test]
    fn rejects_request_lines_that_are_not_the_grammar() {
        for bad in [
            "hello",
            "decode",
            "decode not-json",
            "decode {}",
            r#"decode {"datagram":7}"#,
            r#"frobnicate {"a":1}"#,
        ] {
            assert!(Request::parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn responses_round_trip_through_their_line_form() {
        let responses = [
            Response::Ok(obj(&[("datagram", Json::String("4c".into()))])),
            Response::Ignore,
            Response::Reject("bad magic".to_string()),
            Response::Error("boom".to_string()),
        ];
        for response in responses {
            let line = response.to_line();
            assert_eq!(Response::parse(&line).unwrap(), response, "{line}");
        }
    }

    #[test]
    fn a_bare_ignore_needs_no_body() {
        assert_eq!(Response::parse("ignore").unwrap(), Response::Ignore);
        assert_eq!(Response::parse("ignore\r\n").unwrap(), Response::Ignore);
        assert_eq!(
            Response::parse("reject").unwrap(),
            Response::Reject(String::new())
        );
    }

    #[test]
    fn rejects_response_lines_that_are_not_the_grammar() {
        assert!(Response::parse("ok").is_err());
        assert!(Response::parse("ok {").is_err());
        assert!(Response::parse("weird thing").is_err());
    }

    #[test]
    fn summaries_are_stable_regardless_of_key_order() {
        let a = Response::Ok(obj(&[("b", Json::Null), ("a", Json::Null)]));
        let b = Response::Ok(obj(&[("a", Json::Null), ("b", Json::Null)]));
        assert_eq!(a.summary(), b.summary());
        assert_eq!(Response::Ignore.summary(), "ignore");
        assert_eq!(Response::Reject("x".into()).summary(), "reject x");
        assert_eq!(Response::Error("x".into()).summary(), "error x");
    }

    #[test]
    fn blank_lines_and_comments_are_noise() {
        assert!(is_noise(""));
        assert!(is_noise("   \r\n"));
        assert!(is_noise("# adapter says hello"));
        assert!(!is_noise("ignore"));
    }
}
