//! The behavioural vector model, and the schema check behind `--self-test`.
//!
//! A behavioural vector is one state machine, an initial state, and a list of
//! steps: at each step an event goes in at a stated time and a list of actions
//! must come out. That is the sans-IO contract every implementation core is
//! written to — `on_event(now, event) -> Vec<Action>` — expressed as data.
//!
//! Like the codec half, the runner reads the *shape* and never the meaning. It
//! does not know what a `node` is, what a `tick` does, or that `set_timer` has
//! an `in_us`; it forwards `initial_state` and each `event` to the adapter
//! untouched and compares what comes back. Adding a machine, an event or an
//! action is a change to `vectors/` and to adapters, never to this crate.

use crate::json::Json;
use crate::matcher;

/// One event and everything it must produce.
#[derive(Clone, Debug)]
pub struct Step {
    /// Show time at which the event is delivered, in microseconds.
    pub at_us: u64,
    /// Forwarded to the adapter verbatim.
    pub event: Json,
    /// The actions the implementation must emit, in order, and nothing else.
    pub expect: Vec<Json>,
    /// Optional prose for a step that is doing something subtle.
    pub description: Option<String>,
}

/// One behavioural vector.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Which state machine to reset. Opaque to the runner.
    pub machine: String,
    pub name: String,
    /// Forwarded to the adapter verbatim as the machine's starting condition.
    pub initial_state: Json,
    pub steps: Vec<Step>,
}

impl Scenario {
    /// Parse the body of a `"kind": "behavioural"` file.
    ///
    /// Returns every problem rather than the first, for the same reason the
    /// codec parser does: an author fixing them one round trip at a time is how
    /// a suite stops being edited.
    pub fn parse(doc: &Json) -> Result<Scenario, Vec<String>> {
        let mut problems = Vec::new();
        let machine = required_string(doc, "machine", &mut problems);
        let name = required_string(doc, "name", &mut problems);

        let initial_state = match doc.get("initial_state") {
            Some(state) if state.as_object().is_some() => state.clone(),
            Some(other) => {
                problems.push(format!(
                    "`initial_state` must be an object, found {}",
                    other.kind()
                ));
                Json::Object(Vec::new())
            }
            None => {
                problems.push("missing object field `initial_state`".to_string());
                Json::Object(Vec::new())
            }
        };

        let mut steps = Vec::new();
        match doc.get("steps").and_then(Json::as_array) {
            None => problems.push("missing array field `steps`".to_string()),
            Some([]) => problems
                .push("`steps` is empty; a scenario with no events tests nothing".to_string()),
            Some(items) => {
                for (i, item) in items.iter().enumerate() {
                    match parse_step(item) {
                        Ok(step) => steps.push(step),
                        Err(errs) => problems
                            .extend(errs.into_iter().map(|e| format!("step {}: {e}", i + 1))),
                    }
                }
            }
        }

        // Time must not run backwards. An implementation is entitled to assume
        // `now_us` is monotonic — the show clock slews and never steps — so a
        // vector that rewound it would be testing undefined behaviour.
        for (i, pair) in steps.windows(2).enumerate() {
            if pair[1].at_us < pair[0].at_us {
                problems.push(format!(
                    "step {}: `at_us` {} is before step {}'s {}; time never runs backwards",
                    i + 2,
                    pair[1].at_us,
                    i + 1,
                    pair[0].at_us
                ));
            }
        }

        if problems.is_empty() {
            Ok(Scenario {
                machine,
                name,
                initial_state,
                steps,
            })
        } else {
            Err(problems)
        }
    }
}

fn parse_step(item: &Json) -> Result<Step, Vec<String>> {
    let mut problems = Vec::new();
    if item.as_object().is_none() {
        return Err(vec![format!(
            "a step must be an object, found {}",
            item.kind()
        )]);
    }

    let at_us = match item.get("at_us") {
        Some(value) => value.as_u64().unwrap_or_else(|| {
            problems.push(format!(
                "`at_us` must be a non-negative integer, found {}",
                value.to_canonical()
            ));
            0
        }),
        None => {
            problems.push("missing integer field `at_us`".to_string());
            0
        }
    };

    let event = match item.get("event") {
        Some(event) if event.get("event").and_then(Json::as_str).is_some() => event.clone(),
        Some(event) if event.as_object().is_some() => {
            // The tag is what an adapter switches on. Without it the adapter
            // would have to guess from the field names, which is exactly the
            // sort of implicit contract the line protocol exists to avoid.
            problems.push("`event` needs a string field `event` naming the kind".to_string());
            event.clone()
        }
        Some(other) => {
            problems.push(format!("`event` must be an object, found {}", other.kind()));
            Json::Object(Vec::new())
        }
        None => {
            problems.push("missing object field `event`".to_string());
            Json::Object(Vec::new())
        }
    };

    let mut expect = Vec::new();
    match item.get("expect") {
        // Required even when empty. An absent list would read as "I did not
        // check this step", and a step nobody checks is a step an
        // implementation may do anything at.
        None => problems.push(
            "missing array field `expect`; write `[]` for a step that must produce nothing"
                .to_string(),
        ),
        Some(value) => match value.as_array() {
            None => problems.push(format!("`expect` must be an array, found {}", value.kind())),
            Some(actions) => {
                for (i, action) in actions.iter().enumerate() {
                    if action.get("action").and_then(Json::as_str).is_none() {
                        problems.push(format!(
                            "expect[{i}]: an action needs a string field `action` naming it"
                        ));
                    }
                    for problem in matcher::check(action) {
                        problems.push(format!("expect[{i}]: {problem}"));
                    }
                    expect.push(action.clone());
                }
            }
        },
    }

    if problems.is_empty() {
        Ok(Step {
            at_us,
            event,
            expect,
            description: item
                .get("description")
                .and_then(Json::as_str)
                .map(str::to_string),
        })
    } else {
        Err(problems)
    }
}

fn required_string(doc: &Json, key: &str, problems: &mut Vec<String>) -> String {
    match doc.get(key).and_then(Json::as_str) {
        Some("") | None => {
            problems.push(format!("missing non-empty string field `{key}`"));
            String::new()
        }
        Some(text) => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn scenario_with(steps: &str) -> Result<Scenario, Vec<String>> {
        let text = format!(
            r#"{{"machine":"node","name":"s","initial_state":{{"capacity":1}},
                 "steps":{steps}}}"#
        );
        Scenario::parse(&json::parse(&text).unwrap())
    }

    const STEP: &str = r#"{"at_us":0,"event":{"event":"tick"},
                           "expect":[{"action":"set_timer","in_us":{"$between":[1,9]}}]}"#;

    #[test]
    fn parses_a_well_formed_scenario() {
        let s = scenario_with(&format!("[{STEP}]")).unwrap();
        assert_eq!(s.machine, "node");
        assert_eq!(s.name, "s");
        assert_eq!(
            s.initial_state.get("capacity").and_then(Json::as_u64),
            Some(1)
        );
        assert_eq!(s.steps.len(), 1);
        assert_eq!(s.steps[0].at_us, 0);
        assert_eq!(s.steps[0].expect.len(), 1);
        assert!(s.steps[0].description.is_none());
    }

    #[test]
    fn a_step_may_carry_its_own_prose() {
        let s = scenario_with(
            r#"[{"at_us":0,"event":{"event":"tick"},"expect":[],"description":"why"}]"#,
        )
        .unwrap();
        assert_eq!(s.steps[0].description.as_deref(), Some("why"));
        assert!(s.steps[0].expect.is_empty());
    }

    #[test]
    fn top_level_problems_are_all_reported_together() {
        let errs = Scenario::parse(&json::parse("{}").unwrap()).unwrap_err();
        assert_eq!(errs.len(), 4, "{errs:?}");
        for needle in ["`machine`", "`name`", "`initial_state`", "`steps`"] {
            assert!(
                errs.iter().any(|e| e.contains(needle)),
                "{needle}: {errs:?}"
            );
        }
    }

    #[test]
    fn rejects_mistyped_top_level_fields() {
        let errs = Scenario::parse(
            &json::parse(r#"{"machine":"","name":"n","initial_state":7,"steps":[]}"#).unwrap(),
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.contains("`machine`")), "{errs:?}");
        assert!(
            errs.iter().any(|e| e.contains("must be an object")),
            "{errs:?}"
        );
        assert!(errs.iter().any(|e| e.contains("tests nothing")), "{errs:?}");
    }

    #[test]
    fn a_step_must_be_an_object_with_a_time_an_event_and_an_expectation() {
        assert!(scenario_with("[7]").unwrap_err()[0].contains("must be an object"));

        let errs = scenario_with("[{}]").unwrap_err();
        assert!(errs.iter().all(|e| e.starts_with("step 1: ")), "{errs:?}");
        for needle in ["`at_us`", "`event`", "`expect`"] {
            assert!(
                errs.iter().any(|e| e.contains(needle)),
                "{needle}: {errs:?}"
            );
        }
    }

    #[test]
    fn rejects_a_step_whose_fields_are_the_wrong_shape() {
        let cases = [
            (
                r#"{"at_us":-1,"event":{"event":"t"},"expect":[]}"#,
                "non-negative",
            ),
            (
                r#"{"at_us":0,"event":7,"expect":[]}"#,
                "`event` must be an object",
            ),
            (
                r#"{"at_us":0,"event":{"a":1},"expect":[]}"#,
                "naming the kind",
            ),
            (
                r#"{"at_us":0,"event":{"event":"t"},"expect":7}"#,
                "must be an array",
            ),
            (
                r#"{"at_us":0,"event":{"event":"t"},"expect":[{"in_us":1}]}"#,
                "naming it",
            ),
            (
                r#"{"at_us":0,"event":{"event":"t"},"expect":[{"action":"a","x":{"$nope":1}}]}"#,
                "unknown matcher",
            ),
        ];
        for (step, needle) in cases {
            let errs = scenario_with(&format!("[{step}]")).unwrap_err();
            assert!(errs.iter().any(|e| e.contains(needle)), "{step}: {errs:?}");
        }
    }

    #[test]
    fn an_expectation_of_nothing_is_written_out_rather_than_omitted() {
        // The distinction the schema is protecting: `[]` means "this event
        // produces no actions", and an absent list would mean "nobody looked".
        let errs = scenario_with(r#"[{"at_us":0,"event":{"event":"tick"}}]"#).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("write `[]`")), "{errs:?}");
    }

    #[test]
    fn refuses_a_scenario_whose_clock_runs_backwards() {
        let errs = scenario_with(
            r#"[{"at_us":10,"event":{"event":"tick"},"expect":[]},
                {"at_us":9,"event":{"event":"tick"},"expect":[]}]"#,
        )
        .unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("never runs backwards")),
            "{errs:?}"
        );
        // Equal times are fine: two events can land in the same microsecond.
        assert!(scenario_with(
            r#"[{"at_us":10,"event":{"event":"tick"},"expect":[]},
                {"at_us":10,"event":{"event":"tick"},"expect":[]}]"#
        )
        .is_ok());
    }
}
