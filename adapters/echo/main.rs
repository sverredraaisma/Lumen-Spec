//! The reference adapter.
//!
//! # What this is, and what it deliberately is not
//!
//! It answers protocol requests out of the vector corpus itself: a `decode` is a
//! lookup by datagram, an `encode` is a lookup by decoded structure, and a
//! behavioural replay hands the corpus's own expectations back. It contains
//! **no codec and no state machines**. That is a choice, and it is worth being
//! clear about why, because the obvious alternative looks better and is not.
//!
//! The obvious alternative is to link `lumen-proto` from `lumen-core` and let
//! the reference adapter be a real implementation. Two things forbid it. The
//! dependency would point the wrong way — `lumen-spec` is the repo every other
//! repo implements, and a spec that builds against one implementation is that
//! implementation's documentation, not a specification. And a second codec
//! living here would become the de facto normative one: when the prose and the
//! code disagreed, everyone would read the code.
//!
//! So this adapter is a **fixture for the runner**, not an implementation under
//! test. Pointed at the corpus it must report a clean run, which is what proves
//! the runner, the line protocol and the vector loader work end to end; it can
//! never prove anything about the protocol, and a clean run from it says nothing
//! about whether the vectors are right. That job belongs to `--self-test`, which
//! checks the corpus against its own schema and against the L1 header, and to
//! the real adapters in the implementation repos.
//!
//! The behavioural half makes the point unusually plainly: an expectation may
//! be a *bound* rather than a value, and this fixture answers with the smallest
//! thing that satisfies it. An adapter that can do that is obviously not an
//! implementation of anything.
//!
//! It is also the file to copy when writing one of those. The request loop below
//! is the whole of the line protocol; replace the lookups with calls into an
//! implementation and the adapter is finished.

use lumen_conformance::json::Json;
use lumen_conformance::matcher;
use lumen_conformance::proto::{is_noise, Request, Response};
use lumen_conformance::scenario::Scenario;
use lumen_conformance::vector::{self, Expect};
use std::collections::HashMap;
use std::io::{BufRead, Write};

fn main() {
    let mut dir = String::from("vectors");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--vectors" => match args.next() {
                Some(value) => dir = value,
                None => fail("--vectors needs a value"),
            },
            other => fail(&format!("unknown option `{other}`")),
        }
    }

    let corpus = match load(&dir) {
        Ok(corpus) => corpus,
        Err(e) => fail(&e),
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut replay = Replay::default();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => fail(&format!("reading stdin failed: {e}")),
        };
        if is_noise(&line) {
            continue;
        }
        let response = match Request::parse(&line) {
            Ok(request) => corpus.answer(&request, &mut replay),
            Err(e) => Response::Error(e),
        };
        // Flush every line: the runner is blocked waiting for it, so a buffered
        // answer is a deadlock rather than a slow one.
        if writeln!(stdout, "{}", response.to_line())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return;
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("lumen-echo-adapter: {message}");
    std::process::exit(2);
}

/// Everything the adapter knows: what each datagram decodes to, what each
/// structure encodes to, and every scenario it can replay.
#[derive(Default)]
struct Corpus {
    decode: HashMap<String, Response>,
    encode: HashMap<String, String>,
    scenarios: Vec<Scenario>,
}

/// Where a scenario replay has got to.
///
/// A `reset` narrows the corpus to the scenarios that start this way, and each
/// `event` narrows it further to those that were expecting that event next.
/// Narrowing rather than picking one up front is what keeps two scenarios with
/// the same initial state from being confused for each other — which they
/// would be, because the fixture cannot see the vector file it is answering.
#[derive(Default)]
struct Replay {
    candidates: Vec<usize>,
    step: usize,
}

impl Corpus {
    fn answer(&self, request: &Request, replay: &mut Replay) -> Response {
        match request {
            Request::Hello => Response::Ok(Json::Object(vec![
                (
                    "name".to_string(),
                    Json::String(format!(
                        "lumen-echo-adapter {} (reference fixture)",
                        env!("CARGO_PKG_VERSION")
                    )),
                ),
                (
                    "protocol".to_string(),
                    Json::Number(lumen_conformance::proto::PROTOCOL.to_string()),
                ),
                (
                    "kinds".to_string(),
                    Json::Array(vec![
                        Json::String("codec".to_string()),
                        Json::String("behavioural".to_string()),
                    ]),
                ),
            ])),
            Request::Decode { datagram } => match self.decode.get(datagram) {
                Some(response) => response.clone(),
                None => Response::Error(format!("no vector for datagram `{datagram}`")),
            },
            Request::Encode { value } => match self.encode.get(&value.to_canonical()) {
                Some(hex) => Response::Ok(Json::Object(vec![(
                    "datagram".to_string(),
                    Json::String(hex.clone()),
                )])),
                None => Response::Error("no vector for that structure".to_string()),
            },
            Request::Reset { machine, state } => {
                replay.step = 0;
                replay.candidates = self
                    .scenarios
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        s.machine == *machine
                            && s.initial_state.to_canonical() == state.to_canonical()
                    })
                    .map(|(i, _)| i)
                    .collect();
                if replay.candidates.is_empty() {
                    return Response::Error(format!("no scenario starts `{machine}` this way"));
                }
                Response::Ok(Json::Object(Vec::new()))
            }
            Request::Event { at_us, event } => self.replay_event(*at_us, event, replay),
        }
    }

    fn replay_event(&self, at_us: u64, event: &Json, replay: &mut Replay) -> Response {
        let step = replay.step;
        replay.candidates.retain(|i| {
            self.scenarios[*i]
                .steps
                .get(step)
                .is_some_and(|s| s.at_us == at_us && s.event.to_canonical() == event.to_canonical())
        });
        let Some(first) = replay.candidates.first() else {
            return Response::Error(format!(
                "no scenario has `{}` at {at_us} us as step {}",
                event.to_compact(),
                step + 1
            ));
        };
        replay.step += 1;
        // The expectation may be a bound rather than a value, so the fixture
        // answers with the smallest thing that satisfies it. That it can do
        // this at all is the clearest statement of what it is: a proof that the
        // plumbing works, never a proof that a vector is right.
        let actions: Vec<Json> = self.scenarios[*first].steps[step]
            .expect
            .iter()
            .map(matcher::witness)
            .collect();
        Response::Ok(Json::Object(vec![(
            "actions".to_string(),
            Json::Array(actions),
        )]))
    }
}

fn load(dir: &str) -> Result<Corpus, String> {
    let sources = vector::load_dir(std::path::Path::new(dir))
        .map_err(|e| format!("cannot read {dir}: {e}"))?;
    let (files, problems) = vector::parse_all(&sources);
    if !problems.is_empty() {
        return Err(format!(
            "the corpus does not pass its own schema check:\n  {}",
            problems.join("\n  ")
        ));
    }

    let mut corpus = Corpus::default();
    for file in &files {
        if let Some(scenario) = file.scenario() {
            corpus.scenarios.push(scenario.clone());
        }
        for case in file.cases() {
            let response = match (case.expect, &case.value) {
                (Expect::Ignore, _) => Response::Ignore,
                (Expect::Reject, _) => Response::Reject(
                    case.reason
                        .clone()
                        .unwrap_or_else(|| "malformed".to_string()),
                ),
                (_, Some(value)) => Response::Ok(value.clone()),
                (_, None) => Response::Ok(Json::Object(Vec::new())),
            };
            corpus.decode.insert(case.datagram.clone(), response);
            if case.expect == Expect::RoundTrip {
                if let Some(value) = &case.value {
                    corpus
                        .encode
                        .insert(value.to_canonical(), case.datagram.clone());
                }
            }
        }
    }
    Ok(corpus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_conformance::scenario::{Scenario, Step};

    // Same recipe `vector.rs`'s own tests use: a header whose bytes agree with
    // the JSON so `check_against_bytes` (run by `parse_all` inside `load`)
    // does not reject the fixture.
    fn datagram(payload: &[u8]) -> String {
        let mut bytes = vec![0u8; vector::HEADER_LEN];
        bytes[0] = vector::MAGIC;
        bytes[1] = 0x01;
        bytes[2] = 0x11;
        bytes[22..24].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&[0xAA; vector::TAG_LEN]);
        lumen_conformance::hex::encode(&bytes)
    }

    fn header_json(payload_len: usize) -> String {
        format!(
            r#"{{"magic":76,"version_major":0,"version_minor":1,"type":17,"flags":0,
                 "mesh_prefix":"0000","sender_prefix":"00000000","sequence":0,
                 "show_time_us":0,"payload_len":{payload_len}}}"#
        )
    }

    fn tag() -> String {
        "aa".repeat(vector::TAG_LEN)
    }

    // --- Corpus::answer, tested directly against a corpus built by hand rather
    // --- than through `load`, so each branch of the dispatch is exercised on
    // --- its own.

    #[test]
    fn hello_names_the_fixture_and_claims_both_kinds() {
        let corpus = Corpus::default();
        let mut replay = Replay::default();
        let response = corpus.answer(&Request::Hello, &mut replay);
        let Response::Ok(Json::Object(fields)) = response else {
            panic!("expected an ok object, got {response:?}");
        };
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        assert!(
            get("name")
                .and_then(Json::as_str)
                .is_some_and(|n| n.contains("lumen-echo-adapter")),
            "{fields:?}"
        );
        assert_eq!(
            get("protocol"),
            Some(&Json::Number(
                lumen_conformance::proto::PROTOCOL.to_string()
            ))
        );
        assert_eq!(
            get("kinds"),
            Some(&Json::Array(vec![
                Json::String("codec".to_string()),
                Json::String("behavioural".to_string()),
            ]))
        );
    }

    #[test]
    fn decode_looks_up_the_datagram_verbatim_and_reports_a_miss_by_name() {
        let mut corpus = Corpus::default();
        corpus
            .decode
            .insert("aa".to_string(), Response::Ok(Json::Null));
        let mut replay = Replay::default();

        assert_eq!(
            corpus.answer(
                &Request::Decode {
                    datagram: "aa".to_string()
                },
                &mut replay
            ),
            Response::Ok(Json::Null)
        );

        let miss = corpus.answer(
            &Request::Decode {
                datagram: "bb".to_string(),
            },
            &mut replay,
        );
        match miss {
            Response::Error(why) => assert!(why.contains("no vector for datagram `bb`"), "{why}"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn encode_looks_up_by_canonical_structure_and_reports_a_miss() {
        let mut corpus = Corpus::default();
        let value = Json::Object(vec![("t1".to_string(), Json::Number("1".to_string()))]);
        corpus.encode.insert(value.to_canonical(), "aa".to_string());
        let mut replay = Replay::default();

        assert_eq!(
            corpus.answer(
                &Request::Encode {
                    value: value.clone()
                },
                &mut replay
            ),
            Response::Ok(Json::Object(vec![(
                "datagram".to_string(),
                Json::String("aa".to_string())
            )]))
        );

        let miss = corpus.answer(&Request::Encode { value: Json::Null }, &mut replay);
        match miss {
            Response::Error(why) => assert!(why.contains("no vector for that structure"), "{why}"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    fn scenario(machine: &str, name: &str) -> Scenario {
        Scenario {
            machine: machine.to_string(),
            name: name.to_string(),
            initial_state: Json::Object(vec![(
                "capacity".to_string(),
                Json::Number("1".to_string()),
            )]),
            steps: vec![
                Step {
                    at_us: 1000,
                    event: lumen_conformance::json::parse(r#"{"event":"tick"}"#).unwrap(),
                    expect: vec![lumen_conformance::json::parse(
                        r#"{"action":"send","delay":{"$between":[5,9]}}"#,
                    )
                    .unwrap()],
                    description: None,
                },
                Step {
                    at_us: 2000,
                    event: lumen_conformance::json::parse(r#"{"event":"tock"}"#).unwrap(),
                    expect: vec![],
                    description: None,
                },
            ],
        }
    }

    #[test]
    fn reset_narrows_to_scenarios_that_start_that_way_or_errors_by_name() {
        let corpus = Corpus {
            scenarios: vec![scenario("node", "s1")],
            ..Corpus::default()
        };
        let mut replay = Replay::default();

        let ok = corpus.answer(
            &Request::Reset {
                machine: "node".to_string(),
                state: corpus.scenarios[0].initial_state.clone(),
            },
            &mut replay,
        );
        assert_eq!(ok, Response::Ok(Json::Object(Vec::new())));
        assert_eq!(replay.candidates, vec![0]);
        assert_eq!(replay.step, 0);

        let miss = corpus.answer(
            &Request::Reset {
                machine: "no-such-machine".to_string(),
                state: Json::Object(Vec::new()),
            },
            &mut replay,
        );
        match miss {
            Response::Error(why) => {
                assert!(
                    why.contains("no scenario starts `no-such-machine` this way"),
                    "{why}"
                );
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn event_replay_narrows_by_step_and_witnesses_a_matcher() {
        let corpus = Corpus {
            scenarios: vec![scenario("node", "s1")],
            ..Corpus::default()
        };
        let mut replay = Replay::default();
        corpus.answer(
            &Request::Reset {
                machine: "node".to_string(),
                state: corpus.scenarios[0].initial_state.clone(),
            },
            &mut replay,
        );

        // The first step's bound resolves to the smallest witness, not the bound
        // itself — the whole reason the fixture cannot stand in for a real
        // implementation.
        let response = corpus.answer(
            &Request::Event {
                at_us: 1000,
                event: lumen_conformance::json::parse(r#"{"event":"tick"}"#).unwrap(),
            },
            &mut replay,
        );
        assert_eq!(
            response,
            Response::Ok(Json::Object(vec![(
                "actions".to_string(),
                Json::Array(vec![lumen_conformance::json::parse(
                    r#"{"action":"send","delay":5}"#
                )
                .unwrap()])
            )]))
        );
        assert_eq!(replay.step, 1);

        // The second step expects nothing at all.
        let response = corpus.answer(
            &Request::Event {
                at_us: 2000,
                event: lumen_conformance::json::parse(r#"{"event":"tock"}"#).unwrap(),
            },
            &mut replay,
        );
        assert_eq!(
            response,
            Response::Ok(Json::Object(vec![(
                "actions".to_string(),
                Json::Array(Vec::new())
            )]))
        );
        assert_eq!(replay.step, 2);
    }

    #[test]
    fn an_event_no_scenario_expects_next_is_reported_by_time_and_step() {
        let corpus = Corpus {
            scenarios: vec![scenario("node", "s1")],
            ..Corpus::default()
        };
        let mut replay = Replay::default();
        corpus.answer(
            &Request::Reset {
                machine: "node".to_string(),
                state: corpus.scenarios[0].initial_state.clone(),
            },
            &mut replay,
        );

        let response = corpus.answer(
            &Request::Event {
                at_us: 999,
                event: lumen_conformance::json::parse(r#"{"event":"tick"}"#).unwrap(),
            },
            &mut replay,
        );
        match response {
            Response::Error(why) => {
                assert!(why.contains("no scenario has"), "{why}");
                assert!(why.contains("999"), "{why}");
                assert!(why.contains("step 1"), "{why}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
        // A step that fails to narrow leaves the candidate list empty, so the
        // *next* event reports the same way rather than resurrecting a
        // scenario the mismatch already ruled out.
        assert!(replay.candidates.is_empty());
    }

    // --- `load`: the pipeline that turns a vector directory into a `Corpus`.

    fn write(dir: &std::path::Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-echo-adapter-test-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_reports_when_the_directory_cannot_be_read() {
        match load("no-such-directory-for-the-echo-adapter-tests") {
            Err(err) => assert!(
                err.starts_with("cannot read no-such-directory-for-the-echo-adapter-tests"),
                "{err}"
            ),
            Ok(_) => panic!("expected an error"),
        }
    }

    #[test]
    fn load_reports_a_corpus_that_fails_its_own_schema_check() {
        let dir = temp_dir("bad-schema");
        write(&dir, "bad.json", r#"{"schema":1,"message":"T","cases":[]}"#);
        match load(dir.to_str().unwrap()) {
            Err(err) => assert!(
                err.contains("the corpus does not pass its own schema check"),
                "{err}"
            ),
            Ok(_) => panic!("expected an error"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_builds_a_corpus_from_every_case_and_every_scenario_in_the_tree() {
        let dir = temp_dir("good");
        let rt = datagram(&[1]);
        let acc = datagram(&[1, 2]);
        write(
            &dir,
            "codec.json",
            &format!(
                r#"{{"schema":1,"message":"TEST","description":"d","cases":[
                    {{"name":"rt","description":"d","datagram":"{rt}",
                      "value":{{"header":{h1},"tag":"{t}","payload":{{"t1":1}}}}}},
                    {{"name":"acc","description":"d","datagram":"{acc}","expect":"accept",
                      "value":{{"header":{h2},"tag":"{t}","payload":{{"t1":2}}}}}},
                    {{"name":"ign","description":"d","datagram":"aa","expect":"ignore"}},
                    {{"name":"rej_reason","description":"d","datagram":"bb","expect":"reject",
                      "reason":"bad magic"}},
                    {{"name":"rej_default","description":"d","datagram":"cc","expect":"reject"}}
                ]}}"#,
                h1 = header_json(1),
                h2 = header_json(2),
                t = tag(),
            ),
        );
        write(
            &dir,
            "behavioural.json",
            r#"{"schema":1,"kind":"behavioural","description":"d","machine":"node","name":"s1",
                "initial_state":{},"steps":[
                    {"at_us":1000,"event":{"event":"tick"},"expect":[{"action":"send"}]}
                ]}"#,
        );

        let corpus = load(dir.to_str().unwrap()).expect("a well-formed corpus loads");
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(corpus.scenarios.len(), 1);
        assert_eq!(corpus.scenarios[0].machine, "node");

        // Round trip: both directions are indexed.
        assert!(corpus.decode.contains_key(&rt));
        let rt_value = Json::Object(vec![
            (
                "header".to_string(),
                lumen_conformance::json::parse(&header_json(1)).unwrap(),
            ),
            ("tag".to_string(), Json::String(tag())),
            (
                "payload".to_string(),
                Json::Object(vec![("t1".to_string(), Json::Number("1".to_string()))]),
            ),
        ]);
        assert_eq!(corpus.encode.get(&rt_value.to_canonical()), Some(&rt));

        // Accept: decode only, never registered for encode.
        assert!(corpus.decode.contains_key(&acc));
        let acc_value = Json::Object(vec![
            (
                "header".to_string(),
                lumen_conformance::json::parse(&header_json(2)).unwrap(),
            ),
            ("tag".to_string(), Json::String(tag())),
            (
                "payload".to_string(),
                Json::Object(vec![("t1".to_string(), Json::Number("2".to_string()))]),
            ),
        ]);
        assert!(!corpus.encode.contains_key(&acc_value.to_canonical()));

        // Ignore: answered with `ignore`, no value carried at all.
        assert_eq!(corpus.decode.get("aa"), Some(&Response::Ignore));

        // Reject with a stated reason, and reject with none: defaults to
        // "malformed" so the fixture never answers a reject with an empty line.
        assert_eq!(
            corpus.decode.get("bb"),
            Some(&Response::Reject("bad magic".to_string()))
        );
        assert_eq!(
            corpus.decode.get("cc"),
            Some(&Response::Reject("malformed".to_string()))
        );
    }
}
