//! Turning a [`Report`] into something a person can act on.
//!
//! Separate from [`crate::run`] so that what the runner decides and what it
//! prints can change independently — and so the decision is testable without
//! matching on prose.

use crate::run::Report;
use crate::vector::VectorFile;

/// Render a run.
///
/// Failures carry the vector file path, because the first question after a
/// failure is always "show me the vector".
pub fn render(report: &Report, verbose: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("adapter: {}\n\n", report.adapter));

    for outcome in &report.outcomes {
        match &outcome.failure {
            Some(why) => {
                out.push_str(&format!("FAIL {}\n", outcome.id()));
                for line in why.lines() {
                    out.push_str(&format!("     {line}\n"));
                }
                out.push_str(&format!("     in {}\n", outcome.file));
            }
            None if verbose => out.push_str(&format!("pass {}\n", outcome.id())),
            None => {}
        }
    }

    for line in &report.skipped {
        out.push_str(&format!("skip {line}\n"));
    }

    if report.failed() > 0 {
        out.push('\n');
    }
    out.push_str(&format!(
        "{} checks: {} passed, {} failed",
        report.total(),
        report.passed(),
        report.failed()
    ));
    // Only when there are any. A line reading "0 skipped" on every clean run
    // trains people to stop reading the tally, which is the one line that has
    // to be read.
    if !report.skipped.is_empty() {
        out.push_str(&format!(", {} skipped", report.skipped.len()));
    }
    out.push('\n');
    out
}

/// Render the outcome of `--self-test`: the corpus checked against its own
/// schema, with no implementation involved.
pub fn render_self_test(files: &[VectorFile], problems: &[String], verbose: bool) -> String {
    let mut out = String::new();
    if verbose {
        for file in files {
            out.push_str(&format!(
                "ok   {:<24} {:>3} cases  {}\n",
                file.message,
                file.len(),
                file.path
            ));
        }
    }
    for problem in problems {
        out.push_str(&format!("BAD  {problem}\n"));
    }
    let cases: usize = files.iter().map(|f| f.len()).sum();
    out.push_str(&format!(
        "{} vector files, {cases} cases, {} problems\n",
        files.len(),
        problems.len()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{Direction, Outcome};
    use crate::testing::vector_file;
    use crate::vector::Expect;

    fn outcome(failure: Option<&str>) -> Outcome {
        Outcome {
            file: "vectors/codec/tick.json".to_string(),
            message: "TICK".to_string(),
            case: "gps".to_string(),
            direction: Direction::Decode,
            expect: Some(Expect::RoundTrip),
            failure: failure.map(str::to_string),
        }
    }

    #[test]
    fn a_clean_run_prints_only_the_tally() {
        let report = Report {
            adapter: "ref 0.1".to_string(),
            outcomes: vec![outcome(None), outcome(None)],
            skipped: Vec::new(),
        };
        let text = render(&report, false);
        assert!(text.starts_with("adapter: ref 0.1\n\n"));
        assert!(text.ends_with("2 checks: 2 passed, 0 failed\n"));
        assert!(!text.contains("FAIL"));
    }

    #[test]
    fn verbose_names_every_check() {
        let report = Report {
            adapter: "ref".to_string(),
            outcomes: vec![outcome(None)],
            skipped: Vec::new(),
        };
        assert!(render(&report, true).contains("pass TICK/gps/decode"));
    }

    #[test]
    fn a_failure_carries_its_reason_and_its_file() {
        let report = Report {
            adapter: "ref".to_string(),
            outcomes: vec![outcome(Some("want 1\ngot 2"))],
            skipped: Vec::new(),
        };
        let text = render(&report, false);
        assert!(text.contains("FAIL TICK/gps/decode"));
        // Multi-line reasons stay indented under their heading.
        assert!(text.contains("     want 1\n     got 2\n"));
        assert!(text.contains("in vectors/codec/tick.json"));
        assert!(text.contains("1 checks: 0 passed, 1 failed"));
    }

    #[test]
    fn a_skipped_vector_is_named_and_counted_separately() {
        // A run that quietly checked half the corpus and printed "0 failed"
        // would be worse than one that failed honestly.
        let report = Report {
            adapter: "codec only".to_string(),
            outcomes: vec![outcome(None)],
            skipped: vec!["node/cold_start — the adapter does not run behavioural vectors".into()],
        };
        let text = render(&report, false);
        assert!(text.contains("skip node/cold_start"), "{text}");
        assert!(
            text.ends_with(
                "1 checks: 1 passed, 0 failed, 1 skipped
"
            ),
            "{text}"
        );
    }

    #[test]
    fn the_self_test_report_counts_files_cases_and_problems() {
        let files = vec![vector_file(vec![])];
        let text = render_self_test(&files, &["t.json: broken".to_string()], true);
        assert!(text.contains("ok   TEST"));
        assert!(text.contains("BAD  t.json: broken"));
        assert!(text.contains("1 vector files, 0 cases, 1 problems"));
        assert!(!render_self_test(&files, &[], false).contains("ok   TEST"));
    }
}
