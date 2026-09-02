//! Argument parsing and the top-level flow.
//!
//! Parsing is a pure function over a slice of strings so the command line is
//! covered by unit tests like anything else; [`execute`] is the only part that
//! reads the disk or starts a process.

use crate::adapter::ChildAdapter;
use crate::report;
use crate::run;
use crate::vector;
use std::path::PathBuf;

/// Exit code when everything passed.
pub const EXIT_OK: i32 = 0;
/// Exit code when a conforming implementation would have passed and this one
/// did not. Distinct from a usage error so CI can tell them apart.
pub const EXIT_FAILURES: i32 = 1;
/// Exit code for a broken invocation, an unreadable corpus, or a corpus that
/// fails its own schema check.
pub const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "\
lumen-conformance — drive an implementation through the Lumen spec vectors

USAGE:
    lumen-conformance [OPTIONS] [VECTOR_DIR]

ARGS:
    VECTOR_DIR          directory searched recursively for *.json [default: vectors]

OPTIONS:
    --adapter <CMD>     command to spawn and drive over the line protocol
    --self-test         check the vector corpus against its schema and stop
    --filter <TEXT>     only run cases whose `MESSAGE/case` id contains TEXT
    --verbose           list passing checks too
    -h, --help          print this
";

/// A parsed command line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Options {
    pub vectors: PathBuf,
    pub adapter: Option<String>,
    pub self_test_only: bool,
    pub filter: Option<String>,
    pub verbose: bool,
    pub help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            vectors: PathBuf::from("vectors"),
            adapter: None,
            self_test_only: false,
            filter: None,
            verbose: false,
            help: false,
        }
    }
}

impl Options {
    /// Parse arguments, excluding the program name.
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Options, String> {
        let mut options = Options::default();
        let mut positional = None;
        let mut iter = args.iter().map(AsRef::as_ref);
        while let Some(arg) = iter.next() {
            let mut value = |name: &str| {
                iter.next()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match arg {
                "-h" | "--help" => options.help = true,
                "--self-test" => options.self_test_only = true,
                "--verbose" => options.verbose = true,
                "--adapter" => options.adapter = Some(value("--adapter")?),
                "--filter" => options.filter = Some(value("--filter")?),
                other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
                other if positional.is_some() => {
                    return Err(format!("unexpected second argument `{other}`"))
                }
                other => positional = Some(PathBuf::from(other)),
            }
        }
        if let Some(path) = positional {
            options.vectors = path;
        }
        if options.adapter.is_none() && !options.self_test_only && !options.help {
            return Err("nothing to do: pass --adapter <CMD> or --self-test".to_string());
        }
        Ok(options)
    }
}

/// Load the corpus, check it, and — unless `--self-test` — drive an adapter
/// through it. Returns what to print and what to exit with.
///
/// The corpus is always schema-checked first. Driving an adapter through vectors
/// that do not agree with themselves produces failures that are the suite's
/// fault, and an implementer chasing one of those loses an afternoon.
pub fn execute(options: &Options) -> (String, i32) {
    if options.help {
        return (USAGE.to_string(), EXIT_OK);
    }

    let sources = match vector::load_dir(&options.vectors) {
        Ok(sources) if sources.is_empty() => {
            return (
                format!("no *.json vectors under {}\n", options.vectors.display()),
                EXIT_USAGE,
            )
        }
        Ok(sources) => sources,
        Err(e) => {
            return (
                format!("cannot read {}: {e}\n", options.vectors.display()),
                EXIT_USAGE,
            )
        }
    };

    let (files, problems) = vector::parse_all(&sources);
    if options.self_test_only || !problems.is_empty() {
        let code = if problems.is_empty() {
            EXIT_OK
        } else {
            EXIT_USAGE
        };
        return (
            report::render_self_test(&files, &problems, options.verbose),
            code,
        );
    }

    let command = options.adapter.as_deref().unwrap_or_default();
    let mut adapter = match ChildAdapter::spawn(command) {
        Ok(adapter) => adapter,
        Err(e) => return (format!("{e}\n"), EXIT_USAGE),
    };
    let report = run::run_all(&files, &mut adapter, options.filter.as_deref());
    let code = if report.failed() == 0 {
        EXIT_OK
    } else {
        EXIT_FAILURES
    };
    (report::render(&report, options.verbose), code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args)
    }

    #[test]
    fn parses_a_full_command_line() {
        let o = parse(&[
            "--adapter",
            "prog --x",
            "--filter",
            "TICK",
            "--verbose",
            "vectors/codec",
        ])
        .unwrap();
        assert_eq!(o.adapter.as_deref(), Some("prog --x"));
        assert_eq!(o.filter.as_deref(), Some("TICK"));
        assert!(o.verbose);
        assert_eq!(o.vectors, PathBuf::from("vectors/codec"));
        assert!(!o.self_test_only);
    }

    #[test]
    fn self_test_needs_no_adapter_and_defaults_its_directory() {
        let o = parse(&["--self-test"]).unwrap();
        assert!(o.self_test_only);
        assert_eq!(o.vectors, PathBuf::from("vectors"));
        assert_eq!(
            o,
            Options {
                self_test_only: true,
                ..Options::default()
            }
        );
    }

    #[test]
    fn help_short_circuits_the_requirement_to_do_something() {
        for flag in ["-h", "--help"] {
            assert!(parse(&[flag]).unwrap().help);
        }
        let (text, code) = execute(&Options {
            help: true,
            ..Options::default()
        });
        assert!(text.contains("USAGE"));
        assert_eq!(code, EXIT_OK);
    }

    #[test]
    fn an_invocation_with_nothing_to_do_is_a_usage_error() {
        assert!(parse(&[]).unwrap_err().contains("nothing to do"));
        assert!(parse(&["vectors"]).unwrap_err().contains("nothing to do"));
    }

    #[test]
    fn rejects_unknown_options_missing_values_and_extra_positionals() {
        assert!(parse(&["--nope"]).unwrap_err().contains("unknown option"));
        assert!(parse(&["--adapter"]).unwrap_err().contains("needs a value"));
        assert!(parse(&["--filter"]).unwrap_err().contains("needs a value"));
        assert!(parse(&["--self-test", "a", "b"])
            .unwrap_err()
            .contains("unexpected second argument"));
    }

    #[test]
    fn a_missing_or_empty_vector_directory_is_a_usage_error() {
        let (text, code) = execute(&Options {
            vectors: PathBuf::from("no-such-directory-6f2a"),
            self_test_only: true,
            ..Options::default()
        });
        assert!(text.contains("cannot read"), "{text}");
        assert_eq!(code, EXIT_USAGE);

        let empty = std::env::temp_dir().join(format!("lumen-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let (text, code) = execute(&Options {
            vectors: empty.clone(),
            self_test_only: true,
            ..Options::default()
        });
        assert!(text.contains("no *.json vectors"), "{text}");
        assert_eq!(code, EXIT_USAGE);
        std::fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn a_corpus_that_fails_its_own_schema_stops_before_any_adapter_runs() {
        let dir = std::env::temp_dir().join(format!("lumen-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.json"), "{}").unwrap();
        let (text, code) = execute(&Options {
            vectors: dir.clone(),
            adapter: Some("no-such-program-6f2a".to_string()),
            ..Options::default()
        });
        assert!(text.contains("BAD"), "{text}");
        assert_eq!(code, EXIT_USAGE, "the adapter must not have been spawned");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_adapter_that_will_not_start_is_a_usage_error() {
        let (text, code) = execute(&Options {
            vectors: PathBuf::from("../vectors"),
            adapter: Some("no-such-program-6f2a".to_string()),
            ..Options::default()
        });
        assert!(text.contains("could not start"), "{text}");
        assert_eq!(code, EXIT_USAGE);
    }

    #[test]
    fn the_shipped_corpus_passes_its_own_schema_check() {
        // The vectors are the product of this repo. If they do not agree with
        // themselves, nothing downstream is worth running.
        let (text, code) = execute(&Options {
            vectors: PathBuf::from("../vectors"),
            self_test_only: true,
            verbose: true,
            ..Options::default()
        });
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("0 problems"), "{text}");
    }
}
