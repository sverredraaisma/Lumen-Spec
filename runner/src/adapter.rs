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
        writeln!(self.stdin, "{}", request.to_line())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("writing to the adapter failed: {e}"))?;
        self.read_response()
    }
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
