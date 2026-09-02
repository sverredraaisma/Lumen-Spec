//! Comparing what a vector expects against what an implementation produced.
//!
//! Codec vectors compare for plain structural equality: bytes have exactly one
//! right answer. Behavioural vectors do not, and pretending otherwise is how a
//! suite ends up asserting one implementation's arithmetic. `SetTimer` is the
//! clearest case — `lumen-device` documents it as "a hint, not a contract:
//! waking late is a quality problem, waking early is free" — so a vector that
//! demanded an exact deadline would fail every implementation that computed a
//! perfectly conforming different one.
//!
//! So an expected value may be a **matcher** instead of a literal: an object
//! whose single key begins with `$`.
//!
//! | Matcher | Matches |
//! |---|---|
//! | `{"$any": true}` | any value, as long as the field is present |
//! | `{"$between": [lo, hi]}` | an integer in `lo..=hi`, inclusive |
//! | `{"$starts_with": "4c01"}` | a string with that prefix |
//!
//! Three, and deliberately no more. Each one exists because the spec constrains
//! a *bound* rather than a value: a timer deadline, a clock correction, the
//! leading bytes of a datagram that pin its type and sender while leaving the
//! sequence number and timestamp free. A fourth matcher would almost certainly
//! be someone encoding a rule the prose does not state.
//!
//! The `$` prefix is what keeps a matcher distinguishable from a payload field.
//! Every field name in the wire format is a snake_case identifier, so no literal
//! object can be mistaken for one, and [`check`] rejects an unknown `$name`
//! rather than silently treating it as a field.

use crate::json::Json;

/// Name the first place `expected` and `actual` disagree, or `None` if they
/// match. Matchers in `expected` are honoured; `actual` is compared as data.
///
/// Names one place rather than printing both structures in full: an action list
/// is several hundred characters and "they differ" is not a bug report.
pub fn difference(expected: &Json, actual: &Json) -> Option<String> {
    difference_at("", expected, actual)
}

/// [`difference`], but always producing a sentence — for a caller that has
/// already decided the two are unequal.
pub fn describe(expected: &Json, actual: &Json) -> String {
    difference(expected, actual)
        .unwrap_or_else(|| "no difference found (structures compare equal)".to_string())
}

fn difference_at(path: &str, expected: &Json, actual: &Json) -> Option<String> {
    let here = |what: String| {
        Some(format!(
            "at `{}`: {what}",
            if path.is_empty() { "." } else { path }
        ))
    };
    if let Some((name, argument)) = as_matcher(expected) {
        return match_one(name, argument, actual)
            .err()
            .map(|what| format!("at `{}`: {what}", if path.is_empty() { "." } else { path }));
    }
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

/// The matcher name and its argument, for an object that is one.
fn as_matcher(value: &Json) -> Option<(&str, &Json)> {
    match value.as_object() {
        Some([(key, argument)]) if key.starts_with('$') => Some((key.as_str(), argument)),
        _ => None,
    }
}

fn match_one(name: &str, argument: &Json, actual: &Json) -> Result<(), String> {
    match name {
        "$any" => Ok(()),
        "$between" => {
            let (lo, hi) = range(argument).ok_or_else(|| {
                format!("`$between` needs two integers, found {}", argument.kind())
            })?;
            match integer(actual) {
                Some(n) if (lo..=hi).contains(&n) => Ok(()),
                Some(n) => Err(format!("want an integer in {lo}..={hi}, got {n}")),
                None => Err(format!(
                    "want an integer in {lo}..={hi}, got {}",
                    actual.to_canonical()
                )),
            }
        }
        "$starts_with" => {
            let prefix = argument.as_str().ok_or_else(|| {
                format!("`$starts_with` needs a string, found {}", argument.kind())
            })?;
            match actual.as_str() {
                Some(text) if text.starts_with(prefix) => Ok(()),
                Some(text) => Err(format!("want a string starting `{prefix}`, got `{text}`")),
                None => Err(format!(
                    "want a string starting `{prefix}`, got {}",
                    actual.to_canonical()
                )),
            }
        }
        other => Err(format!("unknown matcher `{other}`")),
    }
}

/// Every problem with the matchers inside `value`, reported by path.
///
/// Run by `--self-test`, so a mistyped matcher is caught by the corpus check
/// rather than showing up as a mysterious failure against somebody's adapter.
pub fn check(value: &Json) -> Vec<String> {
    let mut problems = Vec::new();
    check_at("", value, &mut problems);
    problems
}

fn check_at(path: &str, value: &Json, problems: &mut Vec<String>) {
    let at = |what: String| format!("at `{}`: {what}", if path.is_empty() { "." } else { path });
    if let Some((name, argument)) = as_matcher(value) {
        match name {
            "$any" => {}
            "$between" => match range(argument) {
                Some((lo, hi)) if lo <= hi => {}
                Some((lo, hi)) => problems.push(at(format!("`$between` has {lo} above {hi}"))),
                None => problems.push(at("`$between` needs two integers".to_string())),
            },
            "$starts_with" => {
                if argument.as_str().is_none() {
                    problems.push(at("`$starts_with` needs a string".to_string()));
                }
            }
            other => problems.push(at(format!(
                "unknown matcher `{other}`; the matchers are $any, $between and $starts_with"
            ))),
        }
        return;
    }
    match value {
        Json::Object(fields) => {
            for (key, child) in fields {
                if key.starts_with('$') {
                    // A `$` key that is not a lone matcher is a matcher someone
                    // wrote next to a sibling field, which silently degrades to
                    // an equality check on an object nothing will ever produce.
                    problems.push(at(format!(
                        "`{key}` looks like a matcher but shares its object with other fields"
                    )));
                }
                check_at(&format!("{path}.{key}"), child, problems);
            }
        }
        Json::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                check_at(&format!("{path}[{i}]"), child, problems);
            }
        }
        _ => {}
    }
}

fn range(argument: &Json) -> Option<(i128, i128)> {
    match argument.as_array() {
        Some([lo, hi]) => Some((integer(lo)?, integer(hi)?)),
        _ => None,
    }
}

/// A JSON number as an integer. `None` for anything else, including a real —
/// every quantity the protocol carries is an integer.
fn integer(value: &Json) -> Option<i128> {
    match value {
        Json::Number(text) => text.parse().ok(),
        _ => None,
    }
}

/// Replace every matcher in `value` with a value that satisfies it.
///
/// Only the reference adapter needs this: it answers from the corpus, and a
/// corpus entry may be a bound rather than a value. Resolving to the smallest
/// witness keeps that fixture honest about what it is — a proof that the
/// plumbing works, never a proof that the vectors are right.
pub fn witness(value: &Json) -> Json {
    if let Some((name, argument)) = as_matcher(value) {
        return match name {
            "$between" => range(argument)
                .map(|(lo, _)| Json::Number(lo.to_string()))
                .unwrap_or(Json::Null),
            "$starts_with" => Json::String(argument.as_str().unwrap_or_default().to_string()),
            // `$any`, and anything `check` would have rejected.
            _ => Json::Null,
        };
    }
    match value {
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), witness(v)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(witness).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn j(text: &str) -> Json {
        json::parse(text).unwrap()
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
            let text = describe(&j(want), &j(got));
            assert!(text.contains(needle), "{want} vs {got}: {text}");
        }
        assert!(describe(&Json::Null, &Json::Null).contains("no difference"));
        // A difference at the root has no field path to name.
        assert!(describe(&Json::Null, &Json::Bool(true)).contains("at `.`"));
    }

    #[test]
    fn any_accepts_whatever_is_there_but_still_requires_the_field() {
        let expected = j(r#"{"in_us":{"$any":true}}"#);
        for actual in [r#"{"in_us":1}"#, r#"{"in_us":null}"#, r#"{"in_us":"x"}"#] {
            assert_eq!(difference(&expected, &j(actual)), None, "{actual}");
        }
        let e = describe(&expected, &j("{}"));
        assert!(e.contains("missing field `in_us`"), "{e}");
    }

    #[test]
    fn between_bounds_an_integer_inclusively() {
        // The rule SetTimer needs: a deadline is a bound, not a value, so a
        // vector that pinned one would fail conforming implementations.
        let expected = j(r#"{"in_us":{"$between":[1000,2000]}}"#);
        for ok in ["1000", "1500", "2000"] {
            assert_eq!(
                difference(&expected, &j(&format!(r#"{{"in_us":{ok}}}"#))),
                None
            );
        }
        let e = describe(&expected, &j(r#"{"in_us":2001}"#));
        assert!(e.contains("1000..=2000"), "{e}");
        assert!(e.contains("at `.in_us`"), "{e}");

        let e = describe(&expected, &j(r#"{"in_us":"soon"}"#));
        assert!(e.contains("got \"soon\""), "{e}");
        // Negative offsets are ordinary: the clock can be behind as easily as
        // ahead.
        let expected = j(r#"{"offset_us":{"$between":[-500,500]}}"#);
        assert_eq!(difference(&expected, &j(r#"{"offset_us":-12}"#)), None);
    }

    #[test]
    fn starts_with_pins_a_prefix_of_a_string() {
        let expected = j(r#"{"datagram":{"$starts_with":"4c0110"}}"#);
        assert_eq!(
            difference(&expected, &j(r#"{"datagram":"4c011000ff"}"#)),
            None
        );
        let e = describe(&expected, &j(r#"{"datagram":"4c0111"}"#));
        assert!(e.contains("starting `4c0110`"), "{e}");
        let e = describe(&expected, &j(r#"{"datagram":7}"#));
        assert!(e.contains("got 7"), "{e}");
    }

    #[test]
    fn a_malformed_matcher_fails_the_comparison_rather_than_passing_it() {
        // Belt and braces: `check` catches these in the corpus, but a matcher
        // that silently matched everything would be the worst possible bug in
        // a conformance suite.
        for bad in [
            r#"{"a":{"$between":7}}"#,
            r#"{"a":{"$starts_with":7}}"#,
            r#"{"a":{"$nope":1}}"#,
        ] {
            let e = describe(&j(bad), &j(r#"{"a":1}"#));
            assert!(e.contains("at `.a`"), "{bad}: {e}");
        }
    }

    #[test]
    fn check_finds_every_malformed_matcher_by_path() {
        let cases = [
            (r#"{"a":{"$between":[5,1]}}"#, "5 above 1"),
            (r#"{"a":{"$between":["x","y"]}}"#, "two integers"),
            (r#"{"a":{"$starts_with":7}}"#, "needs a string"),
            (r#"{"a":{"$nope":1}}"#, "unknown matcher"),
            (r#"{"a":{"$any":1,"b":2}}"#, "shares its object"),
            (r#"[{"$nope":1}]"#, "[0]"),
        ];
        for (text, needle) in cases {
            let problems = check(&j(text));
            assert!(
                problems.iter().any(|p| p.contains(needle)),
                "{text}: {problems:?}"
            );
        }
        assert!(check(&j(r#"{"a":[1,{"$between":[1,2]}],"b":{"$any":true}}"#)).is_empty());
    }

    #[test]
    fn a_witness_satisfies_the_matcher_it_replaces() {
        let expected = j(
            r#"{"in_us":{"$between":[1000,2000]},"d":{"$starts_with":"4c"},
                "x":{"$any":true},"list":[{"$between":[1,9]}],"plain":3}"#,
        );
        let stand_in = witness(&expected);
        assert_eq!(difference(&expected, &stand_in), None, "{stand_in:?}");
        assert_eq!(stand_in.get("in_us").and_then(Json::as_u64), Some(1000));
        assert_eq!(stand_in.get("d").and_then(Json::as_str), Some("4c"));
        // A matcher `check` would have rejected still produces *something*,
        // because the fixture must not panic on a corpus it was handed.
        assert_eq!(witness(&j(r#"{"$nope":1}"#)), Json::Null);
        assert_eq!(witness(&j(r#"{"$between":7}"#)), Json::Null);
    }
}
