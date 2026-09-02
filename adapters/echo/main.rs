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
