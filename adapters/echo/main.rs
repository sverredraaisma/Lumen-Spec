//! The reference adapter.
//!
//! # What this is, and what it deliberately is not
//!
//! It answers protocol requests out of the vector corpus itself: a `decode` is a
//! lookup by datagram, an `encode` is a lookup by decoded structure. It contains
//! **no codec**. That is a choice, and it is worth being clear about why,
//! because the obvious alternative looks better and is not.
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
//! It is also the file to copy when writing one of those. The request loop below
//! is the whole of the line protocol; replace the two lookups with calls into an
//! implementation and the adapter is finished.

use lumen_conformance::json::Json;
use lumen_conformance::proto::{is_noise, Request, Response};
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
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => fail(&format!("reading stdin failed: {e}")),
        };
        if is_noise(&line) {
            continue;
        }
        let response = match Request::parse(&line) {
            Ok(request) => corpus.answer(&request),
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

/// Everything the adapter knows: what each datagram decodes to, and what each
/// structure encodes to.
#[derive(Default)]
struct Corpus {
    decode: HashMap<String, Response>,
    encode: HashMap<String, String>,
}

impl Corpus {
    fn answer(&self, request: &Request) -> Response {
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
        }
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
        for case in &file.cases {
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
