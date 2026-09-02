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
use crate::matcher;
use crate::proto::{self, Kind, Request, Response};
use crate::scenario::{Scenario, Step};
use crate::vector::{Case, Expect, VectorFile, Vectors};

/// Which check an outcome refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Bytes into the implementation, structure out.
    Decode,
    /// Structure into the implementation, bytes out.
    Encode,
    /// Events into the implementation, actions out.
    ///
    /// A whole scenario, not a step. A scenario is checked as one thing because
    /// state accumulates: once step four diverges, step five is being run
    /// against a machine in a state the vector never described, and whatever it
    /// then reports is noise.
    Behaviour,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Decode => "decode",
            Direction::Encode => "encode",
            Direction::Behaviour => "behaviour",
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
    /// The required codec outcome; `None` for a behavioural scenario, which has
    /// no single word for what must happen.
    pub expect: Option<Expect>,
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
    /// Vectors the adapter did not claim to be able to run, one line each.
    ///
    /// Skipped rather than failed, and counted separately rather than hidden: a
    /// codec-only adapter is a perfectly good adapter, but a run that quietly
    /// checked half the corpus and printed "0 failed" would be worse than one
    /// that failed honestly.
    pub skipped: Vec<String>,
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

/// Introduce the runner and read back the adapter's identity and its kinds.
///
/// A run that skipped this would report a version mismatch as a wall of decode
/// failures, which is a bad first experience for someone writing an adapter.
pub fn handshake(adapter: &mut dyn Adapter) -> Result<(String, Vec<Kind>), String> {
    match adapter.request(&Request::Hello)? {
        Response::Ok(value) => Ok((
            value
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("unnamed adapter")
                .to_string(),
            proto::kinds_from_hello(&value),
        )),
        other => Err(format!(
            "the adapter answered the handshake with `{}`",
            other.summary()
        )),
    }
}

/// Run every vector in every file, in order.
///
/// `filter`, when given, keeps only vectors whose `message/case` identifier
/// contains it.
pub fn run_all(files: &[VectorFile], adapter: &mut dyn Adapter, filter: Option<&str>) -> Report {
    let mut report = Report::default();
    let kinds = match handshake(adapter) {
        Ok((name, kinds)) => {
            report.adapter = name;
            kinds
        }
        Err(e) => {
            report.adapter = format!("<handshake failed: {e}>");
            // Carry on with the revision-1 assumption. The failures that follow
            // are the evidence an implementer needs, and stopping would hide
            // all of them.
            vec![Kind::Codec]
        }
    };

    for file in files {
        match &file.vectors {
            Vectors::Codec(cases) => {
                for case in cases {
                    if excluded(filter, &file.message, &case.name) {
                        continue;
                    }
                    if !kinds.contains(&Kind::Codec) {
                        report
                            .skipped
                            .push(skip_line(file, &case.name, Kind::Codec));
                        continue;
                    }
                    run_case(file, case, adapter, &mut report.outcomes);
                }
            }
            Vectors::Behavioural(scenario) => {
                if excluded(filter, &file.message, &scenario.name) {
                    continue;
                }
                if !kinds.contains(&Kind::Behavioural) {
                    report
                        .skipped
                        .push(skip_line(file, &scenario.name, Kind::Behavioural));
                    continue;
                }
                run_scenario(file, scenario, adapter, &mut report.outcomes);
            }
        }
    }
    report
}

fn excluded(filter: Option<&str>, message: &str, name: &str) -> bool {
    filter.is_some_and(|f| !format!("{message}/{name}").contains(f))
}

fn skip_line(file: &VectorFile, name: &str, kind: Kind) -> String {
    format!(
        "{}/{name} — the adapter does not run {} vectors ({})",
        file.message,
        kind.name(),
        file.path
    )
}

fn run_case(file: &VectorFile, case: &Case, adapter: &mut dyn Adapter, out: &mut Vec<Outcome>) {
    let record = |out: &mut Vec<Outcome>, direction, failure| {
        out.push(Outcome {
            file: file.path.clone(),
            message: file.message.clone(),
            case: case.name.clone(),
            direction,
            expect: Some(case.expect),
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

/// Reset the machine, then walk the steps, stopping at the first divergence.
fn run_scenario(
    file: &VectorFile,
    scenario: &Scenario,
    adapter: &mut dyn Adapter,
    out: &mut Vec<Outcome>,
) {
    let reset = Request::Reset {
        machine: scenario.machine.clone(),
        state: scenario.initial_state.clone(),
    };
    let mut failure = match adapter.request(&reset) {
        Ok(Response::Ok(_)) => None,
        Ok(other) => Some(format!(
            "`reset {}` answered `{}`; an adapter that cannot build this \
             initial state cannot run the scenario",
            scenario.machine,
            other.summary()
        )),
        Err(transport) => Some(transport),
    };

    for (i, step) in scenario.steps.iter().enumerate() {
        if failure.is_some() {
            break;
        }
        let request = Request::Event {
            at_us: step.at_us,
            event: step.event.clone(),
        };
        failure = match adapter.request(&request) {
            Ok(response) => check_actions(step, &response)
                .err()
                .map(|why| describe_step(i, step, &why)),
            Err(transport) => Some(describe_step(i, step, &transport)),
        };
    }

    out.push(Outcome {
        file: file.path.clone(),
        message: file.message.clone(),
        case: scenario.name.clone(),
        direction: Direction::Behaviour,
        expect: None,
        failure,
    });
}

/// Locate a failure in the scenario, by step number, time and event.
///
/// All three, because each answers a different first question: which line of
/// the file, what the machine had been through by then, and what was actually
/// delivered.
fn describe_step(index: usize, step: &Step, why: &str) -> String {
    let tag = step
        .event
        .get("event")
        .and_then(Json::as_str)
        .unwrap_or("?");
    let mut text = format!("step {} — `{tag}` at {} us: {why}", index + 1, step.at_us);
    if let Some(description) = &step.description {
        text.push_str(&format!("\n  the step says: {description}"));
    }
    text
}

/// Judge an adapter's answer to an `event` request.
///
/// The comparison is **ordered and exhaustive**: the actions must be exactly
/// these, in exactly this order. Both halves are deliberate.
///
/// Exhaustive, because a check that only looked for the actions it wanted would
/// pass an implementation that also emitted a spurious `sync_lost` in the
/// middle of a show — and a spurious action is a real defect, not a stylistic
/// one. `expect: []` is how a vector says "and nothing at all happens here",
/// which is the strongest thing a behavioural vector can assert.
///
/// Ordered, because the sans-IO contract hands the shell a *list* and the shell
/// executes it in order. Two `Send`s in one batch leave in the order they were
/// given, and `RoleChanged` before the `TICK` that role now owes is the causal
/// order a shell reads. Ordering is also the rule an implementer can check
/// against without guessing; "some permutation is acceptable" would need the
/// spec to say which permutations, and it does not.
///
/// What is *not* pinned is the value of any field a vector writes as a matcher.
/// That is where the deliberate looseness lives — see [`crate::matcher`].
pub fn check_actions(step: &Step, response: &Response) -> Result<(), String> {
    let Response::Ok(value) = response else {
        return Err(format!("answered `{}`", response.summary()));
    };
    let Some(actual) = value.get("actions").and_then(Json::as_array) else {
        return Err("`ok` for an `event` needs an array field `actions`".to_string());
    };
    let expected = Json::Array(step.expect.clone());
    let actual = Json::Array(actual.to_vec());
    match matcher::difference(&expected, &actual) {
        None => Ok(()),
        Some(difference) => Err(format!(
            "actions differ: {difference}\n  want {}\n  got  {}",
            expected.to_compact(),
            actual.to_compact()
        )),
    }
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
///
/// Codec values never contain matchers, so this is [`matcher::describe`] with
/// nothing extra — one comparator, so the two halves of the suite can never
/// grow two notions of "the same".
pub fn describe_difference(expected: &Json, actual: &Json) -> String {
    matcher::describe(expected, actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::testing::{case_with, scenario_file, scripted, step, vector_file, Scripted};

    fn ok(text: &str) -> Response {
        Response::Ok(json::parse(text).unwrap())
    }

    /// A handshake answer that claims both kinds of vector.
    const BOTH: &str = r#"ok {"name":"ref","kinds":["codec","behavioural"]}"#;

    #[test]
    fn direction_names_itself() {
        assert_eq!(Direction::Decode.name(), "decode");
        assert_eq!(Direction::Encode.name(), "encode");
        assert_eq!(Direction::Behaviour.name(), "behaviour");
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
        let (name, kinds) = handshake(&mut adapter).unwrap();
        assert_eq!(name, "unnamed adapter");
        assert_eq!(kinds, vec![Kind::Codec]);
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
    fn a_report_counts_what_it_holds() {
        let report = Report::default();
        assert_eq!(
            (report.total(), report.passed(), report.failed()),
            (0, 0, 0)
        );
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn describe_difference_still_names_the_first_disagreement() {
        // The comparator moved into `matcher`, which is where it is exercised
        // properly; this is the codec half's door to it.
        let text = describe_difference(
            &json::parse(r#"{"a":{"b":1}}"#).unwrap(),
            &json::parse(r#"{"a":{"b":2}}"#).unwrap(),
        );
        assert!(text.contains("a.b"), "{text}");
    }

    #[test]
    fn a_scenario_resets_then_walks_its_steps_and_reports_one_outcome() {
        let file = scenario_file(vec![
            step(0, r#"[{"action":"set_timer","in_us":1000}]"#),
            step(5, "[]"),
        ]);
        let mut adapter = scripted(&[
            BOTH,
            "ok {}",
            r#"ok {"actions":[{"action":"set_timer","in_us":1000}]}"#,
            r#"ok {"actions":[]}"#,
        ]);
        let report = run_all(&[file], &mut adapter, None);
        assert_eq!(
            report.total(),
            1,
            "a scenario is one check, not one per step"
        );
        assert_eq!(report.failed(), 0);
        assert_eq!(report.outcomes[0].id(), "node/s/behaviour");
        assert_eq!(report.outcomes[0].expect, None);
        assert_eq!(
            adapter.seen[1],
            Request::Reset {
                machine: "node".to_string(),
                state: json::parse(r#"{"capacity":1}"#).unwrap(),
            }
        );
        assert_eq!(
            adapter.seen[3],
            Request::Event {
                at_us: 5,
                event: json::parse(r#"{"event":"tick"}"#).unwrap(),
            }
        );
    }

    #[test]
    fn a_scenario_stops_at_the_first_step_that_diverges() {
        // Past a divergence the machine is in a state the vector never
        // described, so whatever the later steps report is noise.
        let file = scenario_file(vec![
            step(0, "[]"),
            step(1, r#"[{"action":"sync_lost"}]"#),
            step(2, "[]"),
        ]);
        let mut adapter = scripted(&[
            BOTH,
            "ok {}",
            r#"ok {"actions":[]}"#,
            r#"ok {"actions":[{"action":"sync_acquired"}]}"#,
        ]);
        let report = run_all(&[file], &mut adapter, None);
        assert_eq!(report.failed(), 1);
        let why = report.outcomes[0].failure.clone().unwrap();
        assert!(why.starts_with("step 2 — `tick` at 1 us:"), "{why}");
        assert!(why.contains("actions differ"), "{why}");
        // Four requests, never a fifth: the third step was not delivered.
        assert_eq!(adapter.seen.len(), 4);
    }

    #[test]
    fn a_step_that_carries_prose_repeats_it_in_the_failure() {
        let mut steps = vec![step(0, "[]")];
        steps[0].description = Some("nothing may happen before the first tick".to_string());
        let file = scenario_file(steps);
        let mut adapter = scripted(&[BOTH, "ok {}", r#"ok {"actions":[{"action":"x"}]}"#]);
        let report = run_all(&[file], &mut adapter, None);
        let why = report.outcomes[0].failure.clone().unwrap();
        assert!(why.contains("the step says: nothing may happen"), "{why}");
    }

    #[test]
    fn a_reset_the_adapter_cannot_honour_fails_before_any_event_is_delivered() {
        for answer in ["reject no such machine", "ignore", "error boom"] {
            let file = scenario_file(vec![step(0, "[]")]);
            let mut adapter = scripted(&[BOTH, answer]);
            let report = run_all(&[file], &mut adapter, None);
            let why = report.outcomes[0].failure.clone().unwrap();
            assert!(why.contains("`reset node` answered"), "{answer}: {why}");
            assert_eq!(adapter.seen.len(), 2, "no event was delivered");
        }
    }

    #[test]
    fn a_transport_failure_mid_scenario_is_a_failure_not_a_panic() {
        let file = scenario_file(vec![step(0, "[]")]);
        let mut adapter = Scripted {
            answers: vec![
                Ok(Response::parse(BOTH).unwrap()),
                Ok(ok("{}")),
                Err("pipe closed".to_string()),
            ],
            seen: Vec::new(),
        };
        let report = run_all(&[file], &mut adapter, None);
        assert!(report.outcomes[0]
            .failure
            .as_deref()
            .is_some_and(|w| w.contains("pipe closed")));

        // And on the reset itself, which happens before any step exists to name.
        let file = scenario_file(vec![step(0, "[]")]);
        let mut adapter = Scripted {
            answers: vec![Ok(Response::parse(BOTH).unwrap()), Err("gone".to_string())],
            seen: Vec::new(),
        };
        let report = run_all(&[file], &mut adapter, None);
        assert_eq!(report.outcomes[0].failure.as_deref(), Some("gone"));
    }

    #[test]
    fn actions_are_compared_in_order_and_exhaustively() {
        let want = step(
            0,
            r#"[{"action":"role","role":"leader"},{"action":"set_timer"}]"#,
        );
        assert!(check_actions(
            &want,
            &ok(r#"{"actions":[{"action":"role","role":"leader"},{"action":"set_timer"}]}"#)
        )
        .is_ok());

        // Reordered. The shell executes the list in order, so this is not the
        // same behaviour.
        let e = check_actions(
            &want,
            &ok(r#"{"actions":[{"action":"set_timer"},{"action":"role","role":"leader"}]}"#),
        )
        .unwrap_err();
        assert!(e.contains("[0]"), "{e}");

        // One action too many. A spurious action is a real defect, not a
        // stylistic one, so the comparison is exhaustive.
        let e = check_actions(
            &want,
            &ok(
                r#"{"actions":[{"action":"role","role":"leader"},{"action":"set_timer"},
                              {"action":"sync_lost"}]}"#,
            ),
        )
        .unwrap_err();
        assert!(e.contains("expected 2 items, found 3"), "{e}");
        assert!(e.contains("want ["), "{e}");
        assert!(e.contains("got  ["), "{e}");
    }

    #[test]
    fn a_step_expecting_nothing_refuses_any_action_at_all() {
        // The strongest assertion a behavioural vector can make, and the reason
        // `expect: []` has to be written out rather than omitted.
        let quiet = step(0, "[]");
        assert!(check_actions(&quiet, &ok(r#"{"actions":[]}"#)).is_ok());
        assert!(check_actions(&quiet, &ok(r#"{"actions":[{"action":"x"}]}"#)).is_err());
    }

    #[test]
    fn a_bound_in_a_vector_accepts_any_conforming_deadline() {
        // SetTimer is a hint, not a contract. Pinning one would fail every
        // implementation that computed a different, equally conforming answer.
        let want = step(
            0,
            r#"[{"action":"set_timer","in_us":{"$between":[1,3000000]}}]"#,
        );
        for in_us in ["1", "1000", "3000000"] {
            let line = format!(r#"{{"actions":[{{"action":"set_timer","in_us":{in_us}}}]}}"#);
            assert!(check_actions(&want, &ok(&line)).is_ok(), "{in_us}");
        }
        let e = check_actions(
            &want,
            &ok(r#"{"actions":[{"action":"set_timer","in_us":3000001}]}"#),
        )
        .unwrap_err();
        assert!(e.contains("1..=3000000"), "{e}");
    }

    #[test]
    fn an_event_answered_with_anything_but_an_action_list_is_a_failure() {
        let want = step(0, "[]");
        for answer in [
            Response::Ignore,
            Response::Reject("no".into()),
            Response::Error("boom".into()),
        ] {
            assert!(check_actions(&want, &answer).is_err(), "{answer:?}");
        }
        let e = check_actions(&want, &ok("{}")).unwrap_err();
        assert!(e.contains("array field `actions`"), "{e}");
        let e = check_actions(&want, &ok(r#"{"actions":7}"#)).unwrap_err();
        assert!(e.contains("array field `actions`"), "{e}");
    }

    #[test]
    fn an_adapter_that_does_not_do_a_kind_has_those_vectors_skipped_not_failed() {
        // A codec-only adapter is a perfectly good adapter. Failing it for the
        // half it never claimed would make the report useless.
        let files = vec![
            vector_file(vec![case_with(Expect::Ignore)]),
            scenario_file(vec![step(0, "[]")]),
        ];
        let mut adapter = scripted(&[r#"ok {"name":"codec only"}"#, "ignore"]);
        let report = run_all(&files, &mut adapter, None);
        assert_eq!(report.total(), 1);
        assert_eq!(report.failed(), 0);
        assert_eq!(report.skipped.len(), 1);
        assert!(
            report.skipped[0].contains("behavioural"),
            "{:?}",
            report.skipped
        );

        // And the other way round, which is what a behaviour-only adapter gets.
        let mut adapter = scripted(&[
            r#"ok {"name":"behaviour only","kinds":["behavioural"]}"#,
            "ok {}",
            r#"ok {"actions":[]}"#,
        ]);
        let report = run_all(&files, &mut adapter, None);
        assert_eq!(report.total(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].contains("codec"), "{:?}", report.skipped);
    }

    #[test]
    fn the_filter_reaches_scenarios_by_machine_and_name() {
        let files = vec![scenario_file(vec![step(0, "[]")])];
        let mut adapter = scripted(&[BOTH]);
        assert_eq!(run_all(&files, &mut adapter, Some("sources")).total(), 0);
        let mut adapter = scripted(&[BOTH, "ok {}", r#"ok {"actions":[]}"#]);
        assert_eq!(run_all(&files, &mut adapter, Some("node/s")).total(), 1);
    }
}
