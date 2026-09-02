//! Fixtures shared between the unit test modules.
//!
//! Chiefly [`Scripted`], an adapter that answers from a list. Every interesting
//! failure mode of the runner — a wrong structure, a rejection where an ignore
//! was required, a dead pipe — is reachable by writing three lines of script,
//! which is why none of the checking logic needs a subprocess to test.

use crate::adapter::Adapter;
use crate::json;
use crate::proto::{Request, Response};
use crate::vector::{Case, Expect, VectorFile};

/// An adapter that replays canned answers in order.
pub struct Scripted {
    pub answers: Vec<Result<Response, String>>,
    pub seen: Vec<Request>,
}

impl Adapter for Scripted {
    fn request(&mut self, request: &Request) -> Result<Response, String> {
        self.seen.push(request.clone());
        if self.answers.is_empty() {
            return Err("script exhausted".to_string());
        }
        self.answers.remove(0)
    }
}

/// A [`Scripted`] adapter built from protocol response lines.
pub fn scripted(lines: &[&str]) -> Scripted {
    Scripted {
        answers: lines
            .iter()
            .map(
                |l| Ok(Response::parse(l).unwrap_or_else(|e| panic!("bad script line `{l}`: {e}"))),
            )
            .collect(),
        seen: Vec::new(),
    }
}

/// A case whose datagram is `4c01` and whose value is a small object.
pub fn case_with(expect: Expect) -> Case {
    Case {
        name: "c".to_string(),
        description: "d".to_string(),
        datagram: "4c01".to_string(),
        bytes: vec![0x4C, 0x01],
        expect,
        value: Some(
            json::parse(r#"{"header":{"type":17},"tag":"aa","payload":{"t1":1}}"#).unwrap(),
        ),
        reason: None,
    }
}

/// A file holding the given cases.
pub fn vector_file(cases: Vec<Case>) -> VectorFile {
    VectorFile {
        path: "t.json".to_string(),
        message: "TEST".to_string(),
        description: "d".to_string(),
        cases,
    }
}
