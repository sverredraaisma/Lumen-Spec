//! Driving vectors through an adapter.
//!
//! Nothing here touches the filesystem or a process: it takes parsed vectors and
//! an [`Adapter`] and produces outcomes. The checks themselves ([`check_decode`],
//! [`check_encode`]) are free functions over a case and a response, so the
//! interesting half of the runner is testable by calling it.
//!
//! Every round-trip vector produces **two** outcomes, one per direction. Reported
//! separately on purpose: "TICK fails" is a bug report, "TICK encodes but does
//! not decode" is a diagnosis.

use crate::adapter::Adapter;
use crate::json::Json;
use crate::proto::{Request, Response};
use crate::vector::{Case, Expect, VectorFile};

/// Which half of a codec vector an outcome refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Bytes into the implementation, structure out.
    Decode,
    /// Structure into the implementation, bytes out.
    Encode,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Decode => "decode",
            Direction::Encode => "encode",
        }
    }
}

/// What happened to one case in one direction.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub file: String,
    pub message: String,
    pub case: String,
    pub direction: Direction,
    pub expect: Expect,
    /// `None` means it passed.
    pub failure: Option<String>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }

    /// `message/case/direction`, the identifier a report and a `--filter` share.
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.message, self.case, self.direction.name())
    }
}

/// The result of a whole run.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// What the adapter called itself in the handshake.
    pub adapter: String,
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn total(&self) -> usize {
        self.outcomes.len()
    }

    pub fn failures(&self) -> impl Iterator<Item = &Outcome> {
        self.outcomes.iter().filter(|o| !o.passed())
    }

    pub fn failed(&self) -> usize {
        self.failures().count()
    }

    pub fn passed(&self) -> usize {
        self.total() - self.failed()
    }
}

/// Introduce the runner and read back the adapter's identity.
///
/// A run that skipped this would report a version mismatch as a wall of decode
/// failures, which is a bad first experience for someone writing an adapter.
pub fn handshake(adapter: &mut dyn Adapter) -> Result<String, String> {
    match adapter.request(&Request::Hello)? {
        Response::Ok(value) => Ok(value
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("unnamed adapter")
            .to_string()),
        other => Err(format!(
            "the adapter answered the handshake with `{}`",
            other.summary()
        )),
    }
}

/// Run every case in every file, in order.
///
/// `filter`, when given, keeps only cases whose `message/case` identifier
/// contains it.
pub fn run_all(files: &[VectorFile], adapter: &mut dyn Adapter, filter: Option<&str>) -> Report {
    let mut report = Report {
        adapter: match handshake(adapter) {
            Ok(name) => name,
            Err(e) => format!("<handshake failed: {e}>"),
        },
        outcomes: Vec::new(),
    };
    for file in files {
        for case in &file.cases {
            if filter.is_some_and(|f| !format!("{}/{}", file.message, case.name).contains(f)) {
                continue;
            }
            run_case(file, case, adapter, &mut report.outcomes);
        }
    }
    report
}

fn run_case(file: &VectorFile, case: &Case, adapter: &mut dyn Adapter, out: &mut Vec<Outcome>) {
    let record = |out: &mut Vec<Outcome>, direction, failure| {
        out.push(Outcome {
            file: file.path.clone(),
            message: file.message.clone(),
            case: case.name.clone(),
            direction,
            expect: case.expect,
            failure,
        });
    };

    let request = Request::Decode {
        datagram: case.datagram.clone(),
    };
    let failure = match adapter.request(&request) {
        Ok(response) => check_decode(case, &response).err(),
        Err(transport) => Some(transport),
    };
    record(out, Direction::Decode, failure);

    // Only a round trip pins the encode direction. `accept` cases are legal but
    // non-canonical inputs whose re-encoding is *expected* to differ, and there
    // is nothing to encode for `ignore` and `reject`.
    if case.expect != Expect::RoundTrip {
        return;
    }
    let Some(value) = case.value.clone() else {
        return;
    };
    let failure = match adapter.request(&Request::Encode { value }) {
        Ok(response) => check_encode(case, &response).err(),
        Err(transport) => Some(transport),
    };
    record(out, Direction::Encode, failure);
}

/// Judge an adapter's answer to a `decode` request.
pub fn check_decode(case: &Case, response: &Response) -> Result<(), String> {
    match (case.expect, response) {
        (Expect::RoundTrip | Expect::Accept, Response::Ok(actual)) => match &case.value {
            Some(expected) if expected != actual => Err(format!(
                "decoded structure differs: {}",
                describe_difference(expected, actual)
            )),
            _ => Ok(()),
        },
        (Expect::Ignore, Response::Ignore) => Ok(()),
        (Expect::Reject, Response::Reject(_)) => Ok(()),
        // The two failures worth naming, because they are the ones that look
        // like conformance and are not.
        (Expect::Ignore, Response::Reject(why)) => Err(format!(
            "rejected an input the spec requires be ignored ({why}). Rejecting \
             here turns every future message type into a compatibility break"
        )),
        (Expect::Reject, Response::Ok(_) | Response::Ignore) => Err(format!(
            "accepted an input the spec requires be rejected; answered `{}`",
            response.summary()
        )),
        (expect, other) => Err(format!(
            "expected {}, answered `{}`",
            expect.name(),
            other.summary()
        )),
    }
}

/// Judge an adapter's answer to an `encode` request.
pub fn check_encode(case: &Case, response: &Response) -> Result<(), String> {
    let Response::Ok(value) = response else {
        return Err(format!(
            "encoding failed, answered `{}`",
            response.summary()
        ));
    };
    match value.get("datagram").and_then(Json::as_str) {
        None => Err("`ok` for an encode needs a string field `datagram`".to_string()),
        Some(actual) if actual == case.datagram => Ok(()),
        Some(actual) => Err(format!(
            "re-encoded bytes differ:\n      want {}\n      got  {}{}",
            case.datagram,
            actual,
            first_byte_difference(&case.datagram, actual)
        )),
    }
}

fn first_byte_difference(want: &str, got: &str) -> String {
    let at = want
        .bytes()
        .zip(got.bytes())
        .position(|(a, b)| a != b)
        .map(|nibble| nibble / 2);
    match at {
        Some(byte) => format!("\n      first differing byte: {byte}"),
        None => format!(
            "\n      identical for {} bytes, then the lengths differ",
            want.len().min(got.len()) / 2
        ),
    }
}

/// Name the first place two structures disagree, rather than printing both in
/// full. A `STATE_PUSH` vector is several hundred characters and "they differ"
/// is not a bug report.
pub fn describe_difference(expected: &Json, actual: &Json) -> String {
    match difference_at("", expected, actual) {
        Some(text) => text,
        None => "no difference found (structures compare equal)".to_string(),
    }
}

fn difference_at(path: &str, expected: &Json, actual: &Json) -> Option<String> {
    let here = |what: String| {
        Some(format!(
            "at `{}`: {what}",
            if path.is_empty() { "." } else { path }
        ))
    };
    match (expected, actual) {
        (Json::Object(want), Json::Object(_)) => {
            for (key, value) in want {
                let child = format!("{path}.{key}");
                match actual.get(key) {
                    None => return here(format!("missing field `{key}`")),
                    Some(got) => {
                        if let Some(text) = difference_at(&child, value, got) {
                            return Some(text);
                        }
                    }
                }
            }
            let got = actual.as_object().unwrap_or_default();
            for (key, _) in got {
                if expected.get(key).is_none() {
                    return here(format!("unexpected field `{key}`"));
                }
            }
            None
        }
        (Json::Array(want), Json::Array(got)) => {
            if want.len() != got.len() {
                return here(format!(
                    "expected {} items, found {}",
                    want.len(),
                    got.len()
                ));
            }
            want.iter()
                .zip(got)
                .enumerate()
                .find_map(|(i, (w, g))| difference_at(&format!("{path}[{i}]"), w, g))
        }
        (want, got) if want == got => None,
        (want, got) => here(format!(
            "want {}, got {}",
            want.to_canonical(),
            got.to_canonical()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::testing::{case_with, scripted, vector_file, Scripted};

    fn ok(text: &str) -> Response {
        Response::Ok(json::parse(text).unwrap())
    }

    #[test]
    fn direction_names_itself() {
        assert_eq!(Direction::Decode.name(), "decode");
        assert_eq!(Direction::Encode.name(), "encode");
    }

    #[test]
    fn a_round_trip_case_checks_both_directions() {
        let file = vector_file(vec![case_with(Expect::RoundTrip)]);
        let mut adapter = scripted(&[
            r#"ok {"name":"ref"}"#,
            r#"ok {"header":{"type":17},"tag":"aa","payload":{"t1":1}}"#,
            r#"ok {"datagram":"4c01"}"#,
        ]);
        let report = run_all(&[file], &mut adapter, None);
        assert_eq!(report.adapter, "ref");
        assert_eq!(report.total(), 2);
        assert_eq!(report.passed(), 2);
        assert_eq!(report.failed(), 0);
        assert_eq!(report.outcomes[0].id(), "TEST/c/decode");
        assert_eq!(report.outcomes[1].direction, Direction::Encode);
    }

    #[test]
    fn a_negative_case_checks_only_the_decode_direction() {
        // There is nothing to encode from a datagram that must be refused, and
        // asking would force every adapter to invent an answer.
        for expect in [Expect::Ignore, Expect::Reject, Expect::Accept] {
            let file = vector_file(vec![case_with(expect)]);
            let mut adapter = scripted(&[r#"ok {"name":"ref"}"#, "ignore", "reject no"]);
            let report = run_all(&[file], &mut adapter, None);
            assert_eq!(report.total(), 1, "{expect:?}");
        }
    }

    #[test]
    fn a_transport_failure_is_reported_as_a_failure_not_a_panic() {
        let file = vector_file(vec![case_with(Expect::RoundTrip)]);
        let mut adapter = Scripted {
            answers: vec![
                Ok(ok(r#"{"name":"ref"}"#)),
                Err("pipe closed".to_string()),
                Err("pipe closed".to_string()),
            ],
            seen: Vec::new(),
        };
        let report = run_all(&[file], &mut adapter, None);
        assert_eq!(report.failed(), 2);
        assert!(report
            .failures()
            .all(|o| o.failure.as_deref() == Some("pipe closed")));
    }

    #[test]
    fn a_failed_handshake_is_recorded_and_the_run_continues() {
        // Continuing is deliberate: the failures that follow are the evidence
        // an implementer needs, and stopping would hide all of them.
        let file = vector_file(vec![case_with(Expect::Ignore)]);
        let mut adapter = scripted(&["reject who are you", "ignore"]);
        let report = run_all(&[file], &mut adapter, None);
        assert!(
            report.adapter.contains("handshake failed"),
            "{}",
            report.adapter
        );
        assert_eq!(report.passed(), 1);
    }

    #[test]
    fn a_handshake_without_a_name_still_succeeds() {
        let mut adapter = scripted(&["ok {}"]);
        assert_eq!(handshake(&mut adapter).unwrap(), "unnamed adapter");
        let mut adapter = Scripted {
            answers: vec![Err("no pipe".into())],
            seen: Vec::new(),
        };
        assert_eq!(handshake(&mut adapter), Err("no pipe".to_string()));
    }

    #[test]
    fn the_filter_selects_by_message_and_case_name() {
        let file = vector_file(vec![
            Case {
                name: "alpha".to_string(),
                ..case_with(Expect::Ignore)
            },
            Case {
                name: "beta".to_string(),
                ..case_with(Expect::Ignore)
            },
        ]);
        let mut adapter = scripted(&["ok {}", "ignore"]);
        let report = run_all(&[file.clone()], &mut adapter, Some("beta"));
        assert_eq!(report.total(), 1);
        assert_eq!(report.outcomes[0].case, "beta");

        let mut adapter = scripted(&["ok {}", "ignore", "ignore"]);
        assert_eq!(run_all(&[file], &mut adapter, Some("TEST")).total(), 2);
    }

    #[test]
    fn decode_accepts_a_matching_structure_and_rejects_a_differing_one() {
        let case = case_with(Expect::RoundTrip);
        assert!(check_decode(
            &case,
            &ok(r#"{"header":{"type":17},"tag":"aa","payload":{"t1":1}}"#)
        )
        .is_ok());
        let e = check_decode(
            &case,
            &ok(r#"{"header":{"type":17},"tag":"aa","payload":{"t1":2}}"#),
        )
        .unwrap_err();
        assert!(e.contains("payload.t1"), "{e}");
    }

    #[test]
    fn an_accept_case_without_a_value_only_needs_success() {
        let case = Case {
            value: None,
            ..case_with(Expect::Accept)
        };
        assert!(check_decode(&case, &ok("{}")).is_ok());
    }

    #[test]
    fn rejecting_an_input_that_must_be_ignored_says_why_it_matters() {
        // The single most consequential conformance failure in the suite: it
        // looks healthy and it breaks every future minor version.
        let case = case_with(Expect::Ignore);
        let e = check_decode(&case, &Response::Reject("unknown type".into())).unwrap_err();
        assert!(e.contains("compatibility break"), "{e}");
    }

    #[test]
    fn accepting_an_input_that_must_be_rejected_is_a_failure() {
        let case = case_with(Expect::Reject);
        for answer in [ok("{}"), Response::Ignore] {
            let e = check_decode(&case, &answer).unwrap_err();
            assert!(e.contains("requires be rejected"), "{e}");
        }
        assert!(check_decode(&case, &Response::Reject("no".into())).is_ok());
    }

    #[test]
    fn any_other_mismatch_names_both_sides() {
        let case = case_with(Expect::RoundTrip);
        let e = check_decode(&case, &Response::Ignore).unwrap_err();
        assert!(e.contains("expected round-trip"), "{e}");
        let e = check_decode(&case, &Response::Error("boom".into())).unwrap_err();
        assert!(e.contains("boom"), "{e}");
        let case = case_with(Expect::Ignore);
        assert!(check_decode(&case, &ok("{}"))
            .unwrap_err()
            .contains("expected ignore"));
    }

    #[test]
    fn encode_compares_the_datagram_byte_for_byte() {
        let case = case_with(Expect::RoundTrip);
        assert!(check_encode(&case, &ok(r#"{"datagram":"4c01"}"#)).is_ok());

        let e = check_encode(&case, &ok(r#"{"datagram":"4c02"}"#)).unwrap_err();
        assert!(e.contains("first differing byte: 1"), "{e}");

        let e = check_encode(&case, &ok(r#"{"datagram":"4c0100"}"#)).unwrap_err();
        assert!(e.contains("lengths differ"), "{e}");

        let e = check_encode(&case, &ok("{}")).unwrap_err();
        assert!(e.contains("needs a string field"), "{e}");

        let e = check_encode(&case, &Response::Reject("nope".into())).unwrap_err();
        assert!(e.contains("encoding failed"), "{e}");
    }

    #[test]
    fn a_difference_is_reported_by_path() {
        let cases = [
            (r#"{"a":{"b":1}}"#, r#"{"a":{"b":2}}"#, "a.b"),
            (r#"{"a":1}"#, r#"{}"#, "missing field `a`"),
            (r#"{}"#, r#"{"a":1}"#, "unexpected field `a`"),
            (r#"{"a":[1,2]}"#, r#"{"a":[1]}"#, "expected 2 items"),
            (r#"{"a":[1,2]}"#, r#"{"a":[1,3]}"#, "a[1]"),
            (r#"{"a":1}"#, r#"{"a":"1"}"#, "want 1, got \"1\""),
        ];
        for (want, got, needle) in cases {
            let text = describe_difference(&json::parse(want).unwrap(), &json::parse(got).unwrap());
            assert!(text.contains(needle), "{want} vs {got}: {text}");
        }
        assert!(describe_difference(&Json::Null, &Json::Null).contains("no difference"));
        // A difference at the root has no field path to name.
        assert!(describe_difference(&Json::Null, &Json::Bool(true)).contains("at `.`"));
    }

    #[test]
    fn a_report_counts_what_it_holds() {
        let report = Report::default();
        assert_eq!(
            (report.total(), report.passed(), report.failed()),
            (0, 0, 0)
        );
    }
}
