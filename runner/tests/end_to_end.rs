//! End-to-end: the real runner, the real corpus, a real adapter process.
//!
//! The unit tests drive the checking logic with a scripted adapter, which covers
//! the decisions but not the pipe. These cover the pipe: spawning, the
//! handshake, a request and response crossing a process boundary, and what
//! happens when the far side dies or answers nonsense.

use lumen_conformance::adapter::{Adapter, ChildAdapter};
use lumen_conformance::proto::{Request, Response};
use lumen_conformance::run;
use lumen_conformance::vector;
use std::path::{Path, PathBuf};

const ADAPTER: &str = env!("CARGO_BIN_EXE_lumen-echo-adapter");

fn vectors_dir() -> PathBuf {
    // Tests run with the package root as the working directory.
    PathBuf::from("../vectors")
}

fn load() -> Vec<vector::VectorFile> {
    let sources = vector::load_dir(&vectors_dir()).expect("vectors/ is readable");
    let (files, problems) = vector::parse_all(&sources);
    assert!(problems.is_empty(), "{problems:#?}");
    files
}

fn spawn(vectors: &Path) -> ChildAdapter {
    ChildAdapter::spawn(&format!(
        "\"{ADAPTER}\" --vectors \"{}\"",
        vectors.display()
    ))
    .expect("the reference adapter starts")
}

#[test]
fn the_reference_adapter_passes_the_whole_corpus() {
    let files = load();
    let mut adapter = spawn(&vectors_dir());
    let report = run::run_all(&files, &mut adapter, None);

    assert!(
        report.adapter.contains("lumen-echo-adapter"),
        "{}",
        report.adapter
    );
    assert!(report.total() > 100, "only {} checks ran", report.total());
    assert_eq!(
        report.failed(),
        0,
        "{}",
        lumen_conformance::report::render(&report, false)
    );
}

#[test]
fn every_message_type_in_the_spec_has_vectors() {
    // The suite is only as good as its coverage of the type table, and a
    // message added to the spec without vectors is exactly the drift the repo
    // exists to prevent.
    let files = load();
    let present: Vec<&str> = files.iter().map(|f| f.message.as_str()).collect();
    for message in [
        "L1_HEADER",
        "TICK",
        "SYNC_REQ",
        "SYNC_RESP",
        "ACTIVATE",
        "CHAN",
        "CHAN_CLAIM",
        "CHAN_RELEASE",
        "FRAME",
        "SRC_PUSH",
        "SRC_RENEW",
        "SRC_POP",
        "EVENT",
        "STATE_DIGEST",
        "STATE_PULL",
        "STATE_PUSH",
        "PROG_BEGIN",
        "PROG_CHUNK",
        "PROG_END",
        "FED_HELLO",
        "FED_EVENT",
        "FED_CUE",
        "PROBE_SET",
        "PROBE_DATA",
        "TIMECTL",
        "MALFORMED",
    ] {
        assert!(present.contains(&message), "no vectors for {message}");
    }
}

#[test]
fn the_corpus_carries_the_rules_that_are_easy_to_get_wrong() {
    let files = load();
    let malformed = files
        .iter()
        .find(|f| f.message == "MALFORMED")
        .expect("malformed.json");
    let named = |name: &str| {
        malformed
            .cases
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no case `{name}`"))
    };

    // An unknown type must be ignored. If this ever becomes `reject`, every
    // future minor version of the protocol is a breaking change.
    assert_eq!(named("unknown_message_type").expect, vector::Expect::Ignore);
    // The "stuck red at 3am" rule.
    assert_eq!(
        named("src_push_priority_200_without_expiry").expect,
        vector::Expect::Reject
    );
    assert_eq!(named("bad_magic").expect, vector::Expect::Reject);
    assert_eq!(
        named("unsupported_major_version").expect,
        vector::Expect::Reject
    );
    assert_eq!(
        named("higher_minor_version_is_accepted").expect,
        vector::Expect::Accept
    );
}

#[test]
fn an_adapter_that_cannot_answer_fails_every_case_rather_than_hanging() {
    let empty = std::env::temp_dir().join("lumen-e2e-empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(
        empty.join("nothing.json"),
        r#"{"schema":1,"message":"NONE","description":"an adapter that knows nothing",
             "cases":[{"name":"c","description":"d","datagram":"","expect":"reject",
                       "reason":"empty"}]}"#,
    )
    .unwrap();

    let files = load();
    let mut adapter = spawn(&empty);
    let report = run::run_all(&files, &mut adapter, Some("TICK"));
    assert!(report.total() > 0);
    assert_eq!(report.failed(), report.total());
    let rendered = lumen_conformance::report::render(&report, false);
    assert!(rendered.contains("no vector for datagram"), "{rendered}");

    std::fs::remove_dir_all(&empty).unwrap();
}

#[test]
fn an_adapter_that_exits_at_once_is_reported_not_awaited() {
    // The reference adapter refuses to start against a corpus it cannot read,
    // which gives us a process that dies before the handshake.
    let mut adapter = spawn(Path::new("no-such-directory-6f2a"));
    let e = adapter.request(&Request::Hello).unwrap_err();
    assert!(e.contains("closed its output"), "{e}");
}

#[test]
fn the_adapter_tolerates_comments_and_refuses_nonsense() {
    let mut adapter = spawn(&vectors_dir());
    match adapter.request(&Request::Decode {
        datagram: "00".to_string(),
    }) {
        Ok(Response::Error(why)) => assert!(why.contains("no vector"), "{why}"),
        other => panic!("expected an error response, got {other:?}"),
    }
}

#[test]
fn the_adapter_refuses_a_command_line_it_does_not_understand() {
    // An adapter that started anyway and answered nothing would look like a
    // protocol failure rather than a typo in the invocation.
    for command in [
        format!("\"{ADAPTER}\" --bogus"),
        format!("\"{ADAPTER}\" --vectors"),
    ] {
        let mut adapter = ChildAdapter::spawn(&command).expect("it starts, then exits");
        assert!(adapter.request(&Request::Hello).is_err(), "{command}");
    }
}

#[test]
fn the_self_test_passes_on_the_shipped_corpus() {
    use lumen_conformance::cli::{execute, Options, EXIT_OK};
    let (text, code) = execute(&Options {
        vectors: vectors_dir(),
        self_test_only: true,
        ..Options::default()
    });
    assert_eq!(code, EXIT_OK, "{text}");
    assert!(text.contains("0 problems"), "{text}");
}
