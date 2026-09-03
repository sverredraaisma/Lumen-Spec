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
            .cases()
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
fn the_shipped_behavioural_corpus_covers_the_scenarios_the_spec_names() {
    // `docs/wire-format.md` lists the scenarios worth shipping from the start.
    // The two it names that are absent — the three-way partition and channel
    // preemption — have no behaviour behind them yet; `vectors/README.md` says
    // so rather than shipping a vector nothing can pass.
    let files = load();
    let scenarios: Vec<&str> = files
        .iter()
        .filter_map(|f| f.scenario().map(|s| s.name.as_str()))
        .collect();
    for name in [
        "sync_cold_start_converges",
        "sync_discards_a_slow_sample",
        "master_vanishes_mid_show",
        "equal_capacity_lower_uuid_wins",
        "equal_capacity_higher_uuid_loses",
        "leader_does_not_yield_to_a_worse_candidate",
        "leader_yields_after_three_better_ticks",
        "source_push_after_its_expiry_is_refused",
        "source_above_the_ambient_floor_must_expire",
        "source_stack_falls_back_as_each_source_expires",
        "source_admission_drops_the_least_important",
        "source_pop_fades_out_before_it_is_gone",
        "channel_equal_priority_does_not_preempt",
        "channel_a_lapsed_lease_reopens_it",
        "channel_a_release_from_a_stranger_is_ignored",
        "channel_a_reordered_packet_from_the_owner_is_dropped",
        "channel_decays_toward_its_default_when_the_producer_dies",
        "channel_with_no_hold_never_goes_stale",
        "gateway_clamps_priority_and_clips_pixels",
        "gateway_never_accepts_a_program",
        "gateway_refuses_a_binding_it_cannot_honour",
        "gateway_an_empty_pixel_range_is_not_a_binding",
        "zone_geometry_never_selects_a_synthetic_device",
        "zone_geometry_skips_a_synthetic_device_but_naming_it_does_not",
        "zone_naming_a_device_selects_it_however_it_is_mapped",
        "zone_an_explicit_set_minus_a_geometric_exclusion",
        "zone_a_named_device_can_be_narrowed_to_a_range_of_leds",
    ] {
        assert!(scenarios.contains(&name), "no behavioural vector `{name}`");
    }

    // Every machine a vector names has to be one an adapter can be expected to
    // have. A typo here would be reported as "the adapter cannot build this
    // initial state", which sends the reader to the wrong repo.
    for file in &files {
        if let Some(scenario) = file.scenario() {
            assert!(
                matches!(
                    scenario.machine.as_str(),
                    "node" | "sources" | "channel" | "gateway" | "zone"
                ),
                "{} names an unknown machine `{}`",
                file.path,
                scenario.machine
            );
            assert!(!scenario.steps.is_empty(), "{} has no steps", file.path);
        }
    }
}

#[test]
fn a_behavioural_vector_asserts_that_nothing_else_happens() {
    // The property the exhaustive comparison exists for. If no vector in the
    // corpus ever expected an exact list, an implementation could emit a
    // spurious action anywhere and still pass everything.
    let files = load();
    let steps: usize = files.iter().filter_map(VectorFileExt::scenario_steps).sum();
    assert!(steps > 60, "only {steps} behavioural steps in the corpus");
}

trait VectorFileExt {
    fn scenario_steps(&self) -> Option<usize>;
}

impl VectorFileExt for vector::VectorFile {
    fn scenario_steps(&self) -> Option<usize> {
        self.scenario().map(|s| s.steps.len())
    }
}

#[test]
fn the_reference_adapter_refuses_a_scenario_it_has_no_vector_for() {
    // The fixture answers from the corpus, so a `reset` for something it has
    // never seen has to say so rather than inventing an empty machine and
    // reporting a wall of action mismatches.
    let mut adapter = spawn(&vectors_dir());
    let request = Request::Reset {
        machine: "no-such-machine".to_string(),
        state: lumen_conformance::json::parse("{}").unwrap(),
    };
    match adapter.request(&request) {
        Ok(Response::Error(why)) => assert!(why.contains("no scenario"), "{why}"),
        other => panic!("expected an error response, got {other:?}"),
    }

    // And an event that no scenario has at that point in its sequence.
    let request = Request::Event {
        at_us: 999_999_999,
        event: lumen_conformance::json::parse(r#"{"event":"tick"}"#).unwrap(),
    };
    match adapter.request(&request) {
        Ok(Response::Error(why)) => assert!(why.contains("no scenario has"), "{why}"),
        other => panic!("expected an error response, got {other:?}"),
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

#[test]
fn execute_drives_a_real_adapter_end_to_end_and_reports_a_clean_run() {
    // `cli::tests` covers `execute`'s parsing and its usage-error exits with a
    // scripted / never-spawned adapter, but never the success path — spawning,
    // running the whole corpus, and rendering a passing tally — because that
    // needs a real adapter process, which unit tests deliberately never start.
    // This is the one place that walks the entire `main` behind the binary.
    use lumen_conformance::cli::{execute, Options, EXIT_OK};
    let (text, code) = execute(&Options {
        vectors: vectors_dir(),
        adapter: Some(format!(
            "\"{ADAPTER}\" --vectors \"{}\"",
            vectors_dir().display()
        )),
        ..Options::default()
    });
    assert_eq!(code, EXIT_OK, "{text}");
    assert!(text.contains("0 failed"), "{text}");
}

#[test]
fn execute_reports_exit_failures_when_a_real_run_has_failing_checks() {
    // The mirror of the test above: `execute` must translate a nonzero failure
    // count from a real run into `EXIT_FAILURES`, not just `EXIT_OK`/usage.
    use lumen_conformance::cli::{execute, Options, EXIT_FAILURES};
    let empty = std::env::temp_dir().join("lumen-e2e-execute-failures");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(
        empty.join("nothing.json"),
        r#"{"schema":1,"message":"NONE","description":"an adapter that knows nothing",
             "cases":[{"name":"c","description":"d","datagram":"","expect":"reject",
                       "reason":"empty"}]}"#,
    )
    .unwrap();

    let (text, code) = execute(&Options {
        vectors: vectors_dir(),
        adapter: Some(format!("\"{ADAPTER}\" --vectors \"{}\"", empty.display())),
        filter: Some("TICK".to_string()),
        ..Options::default()
    });
    assert_eq!(code, EXIT_FAILURES, "{text}");
    assert!(!text.contains("0 failed"), "{text}");

    std::fs::remove_dir_all(&empty).unwrap();
}
