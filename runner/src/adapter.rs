//! Talking to an adapter process.
//!
//! Every byte of I/O in the conformance path is here, behind [`Adapter`], so the
//! checking logic in [`crate::run`] can be driven by a value in a unit test and
//! never has to spawn anything. That split is not tidiness: a runner whose
//! failure modes can only be reproduced by starting a subprocess is a runner
//! nobody trusts when it disagrees with an implementer.

use crate::proto::{is_noise, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Something that answers protocol requests.
pub trait Adapter {
    /// Send one request and read one response.
    fn request(&mut self, request: &Request) -> Result<Response, String>;
}

/// An adapter running as a child process, spoken to over its stdin and stdout.
#[derive(Debug)]
pub struct ChildAdapter {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    line: String,
}

impl ChildAdapter {
    /// Spawn `command`, which is split on whitespace with double quotes holding
    /// a fragment together — enough for a path with a space in it, which on
    /// Windows is most of them.
    ///
    /// stderr is left attached to the runner's own, so an adapter's diagnostics
    /// reach the operator instead of filling a pipe nobody drains.
    pub fn spawn(command: &str) -> Result<ChildAdapter, String> {
        let parts = split_command(command);
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| "--adapter was empty".to_string())?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("could not start `{program}`: {e}"))?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
        Ok(ChildAdapter {
            child,
            stdin,
            stdout,
            line: String::new(),
        })
    }

    fn read_response(&mut self) -> Result<Response, String> {
        loop {
            self.line.clear();
            let read = self
                .stdout
                .read_line(&mut self.line)
                .map_err(|e| format!("reading from the adapter failed: {e}"))?;
            if read == 0 {
                return Err("the adapter closed its output before answering".to_string());
            }
            if is_noise(&self.line) {
                continue;
            }
            return Response::parse(&self.line);
        }
    }
}

impl Adapter for ChildAdapter {
    fn request(&mut self, request: &Request) -> Result<Response, String> {
        let written =
            writeln!(self.stdin, "{}", request.to_line()).and_then(|()| self.stdin.flush());
        if let Err(e) = written {
            return Err(describe_write_failure(&e));
        }
        self.read_response()
    }
}

/// What to report when writing a request to the adapter fails.
///
/// An adapter that has already exited reports differently depending on the
/// platform: the write into the pipe fails immediately on Unix, and succeeds
/// into a buffer on Windows so that only the following read notices. Both are
/// the same event to whoever reads the report — the adapter is gone — so both
/// say so, and the two-spellings-for-one-event problem does not reach the
/// operator.
///
/// Anything that is not a dead pipe keeps its own wording, because a full disk
/// or a bad handle is a different problem with a different fix.
fn describe_write_failure(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        return "the adapter closed its output before answering".to_string();
    }
    format!("writing to the adapter failed: {e}")
}

impl Drop for ChildAdapter {
    fn drop(&mut self) {
        // Closing stdin is the polite exit signal; killing is the one that
        // works when an adapter has wedged, and a conformance run must not hang
        // a CI job because an implementation under test did.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Split a command line on whitespace, honouring double quotes.
pub fn split_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    #[test]
    fn a_dead_pipe_reads_as_a_dead_adapter_whatever_the_platform() {
        // The branch CI exercises and this machine does not: on Unix the write
        // fails, on Windows the read does. A test that only ran where the
        // developer happens to be would have caught neither.
        let e = Error::new(ErrorKind::BrokenPipe, "broken pipe");
        assert_eq!(
            describe_write_failure(&e),
            "the adapter closed its output before answering"
        );
    }

    #[test]
    fn any_other_write_failure_keeps_its_own_wording() {
        // A full disk or a bad handle is a different problem with a different
        // fix, and collapsing it into "the adapter is gone" would send whoever
        // reads it looking in the wrong place.
        let e = Error::new(ErrorKind::PermissionDenied, "denied");
        let m = describe_write_failure(&e);
        assert!(m.starts_with("writing to the adapter failed"), "{m}");
        assert!(m.contains("denied"), "{m}");
    }

    use super::*;
    use crate::json::Json;

    #[test]
    fn splits_a_plain_command() {
        assert_eq!(split_command("prog a b"), ["prog", "a", "b"]);
        assert_eq!(split_command("  prog   a  "), ["prog", "a"]);
    }

    #[test]
    fn keeps_a_quoted_path_together() {
        assert_eq!(
            split_command(r#""C:\Program Files\a.exe" --vectors "v dir""#),
            [r"C:\Program Files\a.exe", "--vectors", "v dir"]
        );
    }

    #[test]
    fn an_empty_quoted_argument_survives() {
        // Not a curiosity: `--adapter 'prog ""'` is how someone passes an empty
        // argument, and dropping it silently changes the command that runs.
        assert_eq!(split_command(r#"prog "" x"#), ["prog", "", "x"]);
        assert_eq!(split_command("   "), Vec::<String>::new());
    }

    #[test]
    fn spawning_a_command_that_does_not_exist_is_an_error_not_a_panic() {
        let e = ChildAdapter::spawn("no-such-program-hopefully-6f2a").unwrap_err();
        assert!(e.contains("could not start"), "{e}");
        assert!(ChildAdapter::spawn("").unwrap_err().contains("empty"));
    }

    #[test]
    fn the_scripted_adapter_replays_in_order() {
        let mut a = crate::testing::Scripted {
            answers: vec![Ok(Response::Ignore), Ok(Response::Ok(Json::Null))],
            seen: Vec::new(),
        };
        assert_eq!(a.request(&Request::Hello).unwrap(), Response::Ignore);
        assert_eq!(
            a.request(&Request::Hello).unwrap(),
            Response::Ok(Json::Null)
        );
        assert!(a.request(&Request::Hello).is_err());
        assert_eq!(a.seen.len(), 3);
    }
}
