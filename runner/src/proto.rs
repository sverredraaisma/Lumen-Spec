//! The adapter line protocol: one request per line, one response per line.
//!
//! Line-oriented and text, so an adapter can be written in anything with a
//! standard library, and so a failing exchange can be pasted into a bug report
//! and replayed by hand. The full grammar is in `adapters/README.md`; this
//! module is the normative implementation of it.

use crate::json::{self, Json};

/// Protocol revision the runner speaks. Sent in the handshake so a mismatch is
/// a clear message rather than a puzzling parse failure ten lines later.
///
/// Revision 2 adds `reset` and `event`, the two verbs behavioural vectors need.
/// It is a version bump rather than a silent extension because the addition is
/// not detectable any other way: a revision-1 adapter handed a `reset` answers
/// `error unknown request verb`, which is indistinguishable from a real bug in
/// an adapter that meant to support it. Declaring [`Kind`]s in the handshake
/// turns that into "this adapter does codec vectors only", which is a fact
/// about the adapter rather than a failure.
pub const PROTOCOL: u64 = 2;

/// A class of vector an adapter is able to run.
///
/// Named on the wire so the runner can skip what an adapter cannot do instead
/// of reporting it as broken. A codec-only adapter is a perfectly good adapter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Codec,
    Behavioural,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Codec => "codec",
            Kind::Behavioural => "behavioural",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "codec" => Some(Kind::Codec),
            "behavioural" => Some(Kind::Behavioural),
            _ => None,
        }
    }
}

/// What the runner asks an adapter to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Identify yourself. Always first.
    Hello,
    /// Decode this datagram.
    Decode { datagram: String },
    /// Encode this structure back to a datagram.
    Encode { value: Json },
    /// Discard all state and build `machine` in the given starting condition.
    ///
    /// Named rather than implicit because a behavioural vector has to start
    /// somewhere reproducible, and "whatever the last vector left behind" is
    /// not that. The runner sends one before every scenario.
    Reset { machine: String, state: Json },
    /// Deliver `event` at `at_us` and answer with the actions it produced.
    ///
    /// Delivery and read-back are one exchange, not two. A separate `actions`
    /// verb would buy nothing — the actions of one event are known the moment
    /// it returns — and would cost every adapter a queue to hold them in,
    /// which is state the sans-IO contract does not otherwise need.
    Event { at_us: u64, event: Json },
}

impl Request {
    /// The verb this request uses on the wire.
    pub fn verb(&self) -> &'static str {
        match self {
            Request::Hello => "hello",
            Request::Decode { .. } => "decode",
            Request::Encode { .. } => "encode",
            Request::Reset { .. } => "reset",
            Request::Event { .. } => "event",
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
            Request::Reset { machine, state } => Json::Object(vec![
                ("machine".to_string(), Json::String(machine.clone())),
                ("state".to_string(), state.clone()),
            ]),
            Request::Event { at_us, event } => Json::Object(vec![
                ("at_us".to_string(), Json::Number(at_us.to_string())),
                ("event".to_string(), event.clone()),
            ]),
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
            "reset" => match (
                value.get("machine").and_then(Json::as_str),
                value.get("state"),
            ) {
                (Some(machine), Some(state)) => Ok(Request::Reset {
                    machine: machine.to_string(),
                    state: state.clone(),
                }),
                _ => Err("`reset` needs a string `machine` and an object `state`".to_string()),
            },
            "event" => match (
                value.get("at_us").and_then(Json::as_u64),
                value.get("event"),
            ) {
                (Some(at_us), Some(event)) => Ok(Request::Event {
                    at_us,
                    event: event.clone(),
                }),
                _ => Err("`event` needs an integer `at_us` and an object `event`".to_string()),
            },
            other => Err(format!("unknown request verb `{other}`")),
        }
    }
}

/// The vector kinds an adapter claimed in its handshake.
///
/// An adapter that says nothing is a revision-1 adapter, and those all do codec
/// vectors and nothing else. Assuming that is what lets the codec adapters
/// written before this revision keep working untouched.
pub fn kinds_from_hello(value: &Json) -> Vec<Kind> {
    match value.get("kinds").and_then(Json::as_array) {
        None => vec![Kind::Codec],
        Some(items) => items
            .iter()
            .filter_map(|item| item.as_str().and_then(Kind::parse))
            .collect(),
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
            Request::Reset {
                machine: "node".to_string(),
                state: obj(&[("capacity", Json::Number("1000".into()))]),
            },
            Request::Event {
                at_us: 3_000_000,
                event: obj(&[("event", Json::String("tick".into()))]),
            },
        ];
        for request in requests {
            let line = request.to_line();
            assert_eq!(Request::parse(&line).unwrap(), request, "{line}");
        }
    }

    #[test]
    fn the_handshake_carries_the_protocol_revision() {
        assert_eq!(Request::Hello.to_line(), r#"hello {"protocol":2}"#);
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
    fn the_behavioural_verbs_spell_themselves_the_way_the_documentation_does() {
        assert_eq!(
            Request::Reset {
                machine: "sources".into(),
                state: obj(&[("budget", Json::Number("100".into()))]),
            }
            .to_line(),
            r#"reset {"machine":"sources","state":{"budget":100}}"#
        );
        assert_eq!(
            Request::Event {
                at_us: 5,
                event: obj(&[("event", Json::String("tick".into()))]),
            }
            .to_line(),
            r#"event {"at_us":5,"event":{"event":"tick"}}"#
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
            "reset {}",
            r#"reset {"machine":"node"}"#,
            r#"reset {"machine":7,"state":{}}"#,
            "event {}",
            r#"event {"at_us":1}"#,
            r#"event {"at_us":-1,"event":{}}"#,
        ] {
            assert!(Request::parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn an_adapter_that_names_no_kinds_is_taken_for_a_codec_adapter() {
        // The compatibility rule the version bump rests on: every adapter
        // written against revision 1 does codec vectors and says nothing.
        assert_eq!(kinds_from_hello(&obj(&[])), vec![Kind::Codec]);
        let both = obj(&[(
            "kinds",
            Json::Array(vec![
                Json::String("codec".into()),
                Json::String("behavioural".into()),
            ]),
        )]);
        assert_eq!(
            kinds_from_hello(&both),
            vec![Kind::Codec, Kind::Behavioural]
        );
        // A kind this runner does not know is dropped rather than fatal: a
        // newer adapter must be able to advertise more than we ask for.
        let unknown = obj(&[("kinds", Json::Array(vec![Json::String("psychic".into())]))]);
        assert!(kinds_from_hello(&unknown).is_empty());
    }

    #[test]
    fn every_kind_names_itself_and_parses_back() {
        for kind in [Kind::Codec, Kind::Behavioural] {
            assert_eq!(Kind::parse(kind.name()), Some(kind));
        }
        assert_eq!(Kind::parse("nonsense"), None);
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
