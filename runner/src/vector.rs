//! The vector file model, and the schema check behind `--self-test`.
//!
//! Vectors are data. The runner reads `expect`, `datagram` and `value` and
//! nothing else — it never looks at `message`, never switches on a type code,
//! and has no table of payload fields. That is what makes "adding a vector must
//! not require touching the runner" true rather than aspirational.
//!
//! The one exception is the L1 header, which `--self-test` cross-checks against
//! the datagram bytes. It is the same twenty-four bytes for every message in the
//! protocol, so knowing it costs the runner no per-message knowledge, and it
//! catches the mistake a vector author actually makes: editing the decoded
//! structure and forgetting the hex, or the reverse.
//!
//! # Two kinds in one tree
//!
//! A file's `kind` selects the body: `"codec"` (the default, so every file
//! written before behavioural vectors existed still reads) gives [`Case`]s, and
//! `"behavioural"` gives a [`Scenario`]. They live in one tree and load through
//! one function because an implementer runs one command and gets one verdict;
//! splitting them would mean two corpora that can be at two different revisions.

use crate::hex;
use crate::json::{self, Json};
use crate::scenario::Scenario;
use std::path::{Path, PathBuf};

/// Bytes of L1 header on every datagram.
pub const HEADER_LEN: usize = 24;
/// Bytes of trailing AEAD tag on every datagram.
pub const TAG_LEN: usize = 16;
/// First byte of every datagram. ASCII `L`.
pub const MAGIC: u8 = 0x4C;

/// What a conforming implementation must do with a case's datagram.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expect {
    /// Bytes decode to `value`, and `value` re-encodes to exactly those bytes.
    ///
    /// The default, and the only outcome that pins both directions. A vector
    /// that checks one direction lets an encoder and a decoder drift together.
    RoundTrip,
    /// Bytes decode successfully; re-encoding is not required to reproduce them.
    ///
    /// For inputs that are legal but not canonical — a dirty reserved byte, a
    /// field appended by a newer minor version. Re-encoding normalises them, and
    /// that is correct behaviour, so demanding byte equality would be wrong.
    Accept,
    /// Dropped silently, with no error surfaced. Unknown message types.
    Ignore,
    /// Refused, with an error. Malformed input.
    Reject,
}

impl Expect {
    fn parse(text: &str) -> Option<Expect> {
        match text {
            "accept" => Some(Expect::Accept),
            "ignore" => Some(Expect::Ignore),
            "reject" => Some(Expect::Reject),
            _ => None,
        }
    }

    /// The word used in the vector files and in reports.
    pub fn name(self) -> &'static str {
        match self {
            Expect::RoundTrip => "round-trip",
            Expect::Accept => "accept",
            Expect::Ignore => "ignore",
            Expect::Reject => "reject",
        }
    }
}

/// One vector: a datagram and what must happen to it.
#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,
    pub description: String,
    /// The datagram as written in the file: lowercase hex, no separators.
    pub datagram: String,
    pub bytes: Vec<u8>,
    pub expect: Expect,
    /// The decoded structure. Required for a round trip, optional otherwise.
    pub value: Option<Json>,
    /// Prose explaining a negative case. Never checked against an adapter — an
    /// implementation is free to word its errors however it likes.
    pub reason: Option<String>,
}

/// The body of a vector file, according to its `kind`.
#[derive(Clone, Debug)]
pub enum Vectors {
    /// `kind: "codec"` — bytes against structure, both directions.
    Codec(Vec<Case>),
    /// `kind: "behavioural"` — events in, actions out.
    Behavioural(Box<Scenario>),
}

/// One vector file: the cases for one message type, or one scenario.
#[derive(Clone, Debug)]
pub struct VectorFile {
    pub path: String,
    /// The message name for a codec file, the machine name for a behavioural
    /// one. Documentation and the first half of a check's id; **the runner
    /// never switches on it**.
    pub message: String,
    pub description: String,
    pub vectors: Vectors,
}

/// The schema version these files are written against.
pub const SCHEMA: u64 = 1;

impl VectorFile {
    /// The codec cases, or nothing for a behavioural file.
    pub fn cases(&self) -> &[Case] {
        match &self.vectors {
            Vectors::Codec(cases) => cases,
            Vectors::Behavioural(_) => &[],
        }
    }

    /// The scenario, or nothing for a codec file.
    pub fn scenario(&self) -> Option<&Scenario> {
        match &self.vectors {
            Vectors::Codec(_) => None,
            Vectors::Behavioural(scenario) => Some(scenario),
        }
    }

    /// How many vectors this file holds, for the self-test tally. A scenario is
    /// one, because it is checked as one thing: a divergence at step four says
    /// nothing about step five, which never ran.
    pub fn len(&self) -> usize {
        match &self.vectors {
            Vectors::Codec(cases) => cases.len(),
            Vectors::Behavioural(_) => 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Parse and validate one file. Returns every problem found rather than the
    /// first, because a vector author fixing them one round trip at a time is
    /// how a suite stops being edited.
    pub fn parse(path: &str, text: &str) -> Result<VectorFile, Vec<String>> {
        let doc = json::parse(text).map_err(|e| vec![format!("{path}: {e}")])?;
        let mut problems = Vec::new();
        let at = |what: &str| format!("{path}: {what}");

        if doc.as_object().is_none() {
            return Err(vec![at("the top level must be an object")]);
        }
        match doc.get("schema").and_then(Json::as_u64) {
            Some(SCHEMA) => {}
            Some(other) => problems.push(at(&format!(
                "schema {other} is not supported; this runner reads schema {SCHEMA}"
            ))),
            None => problems.push(at("missing integer field `schema`")),
        }
        let description = string_field(&doc, "description").unwrap_or_else(|| {
            problems.push(at("missing string field `description`"));
            String::new()
        });

        // `kind` is absent on every file written before behavioural vectors
        // existed, and defaulting it to codec is what keeps those readable
        // rather than making the addition a corpus-wide edit.
        match doc
            .get("kind")
            .map(|k| k.as_str().unwrap_or("<not a string>"))
        {
            None | Some("codec") => {}
            Some("behavioural") => {
                return finish_behavioural(path, &doc, description, problems);
            }
            Some(other) => {
                problems.push(at(&format!(
                    "`kind` must be `codec` or `behavioural`; found `{other}`"
                )));
                return Err(problems);
            }
        }

        let message = string_field(&doc, "message").unwrap_or_else(|| {
            problems.push(at("missing string field `message`"));
            String::new()
        });

        let mut cases = Vec::new();
        match doc.get("cases").and_then(Json::as_array) {
            None => problems.push(at("missing array field `cases`")),
            Some([]) => problems.push(at("`cases` is empty; a file with no cases tests nothing")),
            Some(items) => {
                for (i, item) in items.iter().enumerate() {
                    match parse_case(item) {
                        Ok(case) => cases.push(case),
                        Err(errs) => {
                            let label = string_field(item, "name")
                                .unwrap_or_else(|| format!("case #{}", i + 1));
                            problems.extend(errs.into_iter().map(|e| at(&format!("{label}: {e}"))));
                        }
                    }
                }
            }
        }

        for (i, case) in cases.iter().enumerate() {
            if cases[..i].iter().any(|c| c.name == case.name) {
                problems.push(at(&format!("duplicate case name `{}`", case.name)));
            }
            for problem in check_against_bytes(case) {
                problems.push(at(&format!("{}: {problem}", case.name)));
            }
        }

        if problems.is_empty() {
            Ok(VectorFile {
                path: path.to_string(),
                message,
                description,
                vectors: Vectors::Codec(cases),
            })
        } else {
            Err(problems)
        }
    }
}

/// Finish parsing a `kind: "behavioural"` file.
///
/// Split out rather than inlined so the codec path reads as it did: the two
/// bodies share only the envelope — `schema`, `kind` and `description` — and
/// nothing else about them is common.
fn finish_behavioural(
    path: &str,
    doc: &Json,
    description: String,
    mut problems: Vec<String>,
) -> Result<VectorFile, Vec<String>> {
    match Scenario::parse(doc) {
        Ok(scenario) if problems.is_empty() => Ok(VectorFile {
            path: path.to_string(),
            message: scenario.machine.clone(),
            description,
            vectors: Vectors::Behavioural(Box::new(scenario)),
        }),
        Ok(_) => Err(problems),
        Err(errs) => {
            problems.extend(errs.into_iter().map(|e| format!("{path}: {e}")));
            Err(problems)
        }
    }
}

fn string_field(value: &Json, key: &str) -> Option<String> {
    value.get(key).and_then(Json::as_str).map(str::to_string)
}

fn parse_case(item: &Json) -> Result<Case, Vec<String>> {
    let mut problems = Vec::new();
    if item.as_object().is_none() {
        return Err(vec![format!(
            "a case must be an object, found {}",
            item.kind()
        )]);
    }
    let name = string_field(item, "name").unwrap_or_else(|| {
        problems.push("missing string field `name`".to_string());
        String::new()
    });
    if name.is_empty() && item.get("name").is_some() {
        problems.push("`name` must not be empty".to_string());
    }
    let description = string_field(item, "description").unwrap_or_else(|| {
        // Every case has to say what it is for. A vector nobody can explain is
        // one nobody dares delete when it starts failing.
        problems.push("missing string field `description`".to_string());
        String::new()
    });

    let datagram = string_field(item, "datagram").unwrap_or_else(|| {
        problems.push("missing string field `datagram`".to_string());
        String::new()
    });
    let bytes = match hex::decode(&datagram) {
        Ok(bytes) => bytes,
        Err(e) => {
            problems.push(format!("`datagram` is not valid hex: {e}"));
            Vec::new()
        }
    };

    let expect = match item.get("expect") {
        None => Expect::RoundTrip,
        Some(Json::String(text)) => match Expect::parse(text) {
            Some(e) => e,
            None => {
                problems.push(format!(
                    "`expect` must be accept, ignore or reject; found `{text}`"
                ));
                Expect::Reject
            }
        },
        Some(other) => {
            problems.push(format!("`expect` must be a string, found {}", other.kind()));
            Expect::Reject
        }
    };

    let value = item.get("value").cloned();
    match (&value, expect) {
        (None, Expect::RoundTrip) => problems.push(
            "a round-trip case needs `value`; without it only one direction is checked, \
             which is what lets an encoder and a decoder drift together"
                .to_string(),
        ),
        (Some(v), _) => problems.extend(check_value_shape(v)),
        _ => {}
    }
    if value.is_some() && matches!(expect, Expect::Ignore) {
        problems.push("an `ignore` case cannot have a `value`: there is nothing to decode".into());
    }

    if problems.is_empty() {
        Ok(Case {
            name,
            description,
            datagram,
            bytes,
            expect,
            value,
            reason: string_field(item, "reason"),
        })
    } else {
        Err(problems)
    }
}

fn check_value_shape(value: &Json) -> Vec<String> {
    let mut problems = Vec::new();
    if value.as_object().is_none() {
        return vec![format!("`value` must be an object, found {}", value.kind())];
    }
    for key in ["header", "payload"] {
        match value.get(key) {
            Some(v) if v.as_object().is_some() => {}
            Some(v) => problems.push(format!(
                "`value.{key}` must be an object, found {}",
                v.kind()
            )),
            None => problems.push(format!("`value` is missing `{key}`")),
        }
    }
    match value.get("tag").and_then(Json::as_str) {
        Some(tag) if tag.len() == TAG_LEN * 2 && hex::decode(tag).is_ok() => {}
        Some(tag) => problems.push(format!(
            "`value.tag` must be {} lowercase hex digits, found `{tag}`",
            TAG_LEN * 2
        )),
        None => problems.push("`value` is missing the string field `tag`".to_string()),
    }
    problems
}

/// One header field, at the offset the wire format gives it.
struct HeaderField {
    name: &'static str,
    offset: usize,
    len: usize,
    /// Little-endian integer when true, opaque bytes rendered as hex when false.
    integer: bool,
}

const HEADER_FIELDS: &[HeaderField] = &[
    HeaderField {
        name: "type",
        offset: 2,
        len: 1,
        integer: true,
    },
    HeaderField {
        name: "flags",
        offset: 3,
        len: 1,
        integer: true,
    },
    HeaderField {
        name: "mesh_prefix",
        offset: 4,
        len: 2,
        integer: false,
    },
    HeaderField {
        name: "sender_prefix",
        offset: 6,
        len: 4,
        integer: false,
    },
    HeaderField {
        name: "sequence",
        offset: 10,
        len: 4,
        integer: true,
    },
    HeaderField {
        name: "show_time_us",
        offset: 14,
        len: 8,
        integer: true,
    },
    HeaderField {
        name: "payload_len",
        offset: 22,
        len: 2,
        integer: true,
    },
];

/// Cross-check a case's declared structure against its datagram bytes, as far
/// as can be done without a codec: the header, the tag, and the total length.
fn check_against_bytes(case: &Case) -> Vec<String> {
    let Some(value) = &case.value else {
        return Vec::new();
    };
    let Some(header) = value.get("header") else {
        return Vec::new();
    };
    let bytes = &case.bytes;
    if bytes.len() < HEADER_LEN + TAG_LEN {
        return vec![format!(
            "`datagram` is {} bytes; a case with a `value` must be at least the \
             {HEADER_LEN}-byte header plus the {TAG_LEN}-byte tag",
            bytes.len()
        )];
    }

    let mut problems = Vec::new();
    if bytes[0] != MAGIC {
        problems.push(format!(
            "`datagram` starts with {:#04x}, not the magic byte {MAGIC:#04x}",
            bytes[0]
        ));
    }
    expect_int(&mut problems, header, "magic", u64::from(bytes[0]));
    expect_int(
        &mut problems,
        header,
        "version_major",
        u64::from(bytes[1] >> 4),
    );
    expect_int(
        &mut problems,
        header,
        "version_minor",
        u64::from(bytes[1] & 0x0F),
    );

    for field in HEADER_FIELDS {
        let raw = &bytes[field.offset..field.offset + field.len];
        if field.integer {
            let mut n = 0u64;
            for (i, b) in raw.iter().enumerate() {
                n |= u64::from(*b) << (8 * i);
            }
            expect_int(&mut problems, header, field.name, n);
        } else {
            expect_hex(&mut problems, header, field.name, raw);
        }
    }

    // payload_len is the field a hand-edited vector gets wrong most often,
    // because it is the only one that is a function of the rest of the file.
    let declared = header
        .get("payload_len")
        .and_then(Json::as_u64)
        .unwrap_or_default() as usize;
    let available = bytes.len() - HEADER_LEN - TAG_LEN;
    if declared != available {
        problems.push(format!(
            "`value.header.payload_len` is {declared} but the datagram carries \
             {available} payload bytes"
        ));
    }

    if let Some(tag) = value.get("tag").and_then(Json::as_str) {
        let actual = hex::encode(&bytes[bytes.len() - TAG_LEN..]);
        if tag != actual {
            problems.push(format!(
                "`value.tag` is `{tag}` but the datagram ends with `{actual}`"
            ));
        }
    }

    problems
}

fn expect_int(problems: &mut Vec<String>, header: &Json, name: &str, actual: u64) {
    match header.get(name).and_then(Json::as_u64) {
        Some(declared) if declared == actual => {}
        Some(declared) => problems.push(format!(
            "`value.header.{name}` is {declared} but the datagram says {actual}"
        )),
        None => problems.push(format!("`value.header` is missing the integer `{name}`")),
    }
}

fn expect_hex(problems: &mut Vec<String>, header: &Json, name: &str, actual: &[u8]) {
    let actual = hex::encode(actual);
    match header.get(name).and_then(Json::as_str) {
        Some(declared) if declared == actual => {}
        Some(declared) => problems.push(format!(
            "`value.header.{name}` is `{declared}` but the datagram says `{actual}`"
        )),
        None => problems.push(format!("`value.header` is missing the hex string `{name}`")),
    }
}

/// Read every `.json` file under `dir`, recursively, sorted by path.
///
/// The only filesystem access in the crate outside the adapter transport. Order
/// is deterministic so a report can be diffed between runs.
pub fn load_dir(dir: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut paths = Vec::new();
    collect(dir, &mut paths)?;
    paths.sort();
    paths
        .into_iter()
        .map(|p| std::fs::read_to_string(&p).map(|text| (p, text)))
        .collect()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse everything that was loaded, keeping the good files and collecting the
/// complaints about the rest.
pub fn parse_all(sources: &[(PathBuf, String)]) -> (Vec<VectorFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut problems = Vec::new();
    for (path, text) in sources {
        match VectorFile::parse(&path.display().to_string(), text) {
            Ok(file) => files.push(file),
            Err(errs) => problems.extend(errs),
        }
    }
    (files, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal well-formed datagram: header, one payload byte, tag.
    fn datagram(payload: &[u8], patch: &[(usize, u8)]) -> String {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[0] = MAGIC;
        bytes[1] = 0x01;
        bytes[2] = 0x11;
        bytes[22..24].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&[0xAA; TAG_LEN]);
        for (i, b) in patch {
            bytes[*i] = *b;
        }
        hex::encode(&bytes)
    }

    fn header_json(payload_len: usize) -> String {
        format!(
            r#"{{"magic":76,"version_major":0,"version_minor":1,"type":17,"flags":0,
                 "mesh_prefix":"0000","sender_prefix":"00000000","sequence":0,
                 "show_time_us":0,"payload_len":{payload_len}}}"#
        )
    }

    fn file_with(case_body: &str) -> Result<VectorFile, Vec<String>> {
        let text =
            format!(r#"{{"schema":1,"message":"TEST","description":"d","cases":[{case_body}]}}"#);
        VectorFile::parse("t.json", &text)
    }

    fn round_trip_case() -> String {
        format!(
            r#"{{"name":"c","description":"d","datagram":"{}",
                 "value":{{"header":{},"tag":"{}","payload":{{"t1":1}}}}}}"#,
            datagram(&[7], &[]),
            header_json(1),
            "aa".repeat(TAG_LEN)
        )
    }

    #[test]
    fn parses_a_well_formed_file() {
        let file = file_with(&round_trip_case()).unwrap();
        assert_eq!(file.message, "TEST");
        assert_eq!(file.description, "d");
        assert_eq!(file.path, "t.json");
        assert_eq!(file.cases().len(), 1);
        let case = &file.cases()[0];
        assert_eq!(case.expect, Expect::RoundTrip);
        assert_eq!(case.bytes.len(), HEADER_LEN + 1 + TAG_LEN);
        assert!(case.value.is_some());
        assert!(case.reason.is_none());
    }

    #[test]
    fn every_expectation_word_parses_and_names_itself() {
        for (word, expect) in [
            ("accept", Expect::Accept),
            ("ignore", Expect::Ignore),
            ("reject", Expect::Reject),
        ] {
            assert_eq!(Expect::parse(word), Some(expect));
            assert_eq!(expect.name(), word);
        }
        assert_eq!(Expect::parse("maybe"), None);
        assert_eq!(Expect::RoundTrip.name(), "round-trip");
    }

    #[test]
    fn a_negative_case_needs_no_value() {
        let file = file_with(&format!(
            r#"{{"name":"c","description":"d","datagram":"{}","expect":"reject","reason":"why"}}"#,
            datagram(&[7], &[(0, b'X')])
        ))
        .unwrap();
        assert_eq!(file.cases()[0].expect, Expect::Reject);
        assert_eq!(file.cases()[0].reason.as_deref(), Some("why"));
        assert!(file.cases()[0].value.is_none());
    }

    #[test]
    fn a_round_trip_case_without_a_value_is_refused() {
        // The rule the whole codec suite rests on: one direction is not enough.
        let errs = file_with(r#"{"name":"c","description":"d","datagram":""}"#).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("drift together")),
            "{errs:?}"
        );
    }

    #[test]
    fn an_ignore_case_may_not_carry_a_value() {
        let errs = file_with(&format!(
            r#"{{"name":"c","description":"d","datagram":"{}","expect":"ignore",
                 "value":{{"header":{},"tag":"{}","payload":{{}}}}}}"#,
            datagram(&[7], &[]),
            header_json(1),
            "aa".repeat(TAG_LEN)
        ))
        .unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("nothing to decode")),
            "{errs:?}"
        );
    }

    #[test]
    fn top_level_problems_are_all_reported_together() {
        let errs = VectorFile::parse("t.json", "{}").unwrap_err();
        assert_eq!(errs.len(), 4, "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("`schema`")));
        assert!(errs.iter().any(|e| e.contains("`message`")));
        assert!(errs.iter().any(|e| e.contains("`description`")));
        assert!(errs.iter().any(|e| e.contains("`cases`")));
    }

    #[test]
    fn rejects_a_schema_version_it_does_not_understand() {
        let errs = VectorFile::parse(
            "t.json",
            r#"{"schema":99,"message":"m","description":"d","cases":[]}"#,
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.contains("schema 99")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("tests nothing")), "{errs:?}");
    }

    #[test]
    fn rejects_malformed_json_and_non_object_documents() {
        assert!(VectorFile::parse("t.json", "{").is_err());
        let errs = VectorFile::parse("t.json", "[]").unwrap_err();
        assert!(errs[0].contains("top level"), "{errs:?}");
    }

    #[test]
    fn a_case_must_be_an_object_with_the_required_strings() {
        let errs = file_with("42").unwrap_err();
        assert!(errs[0].contains("must be an object"), "{errs:?}");

        let errs = file_with("{}").unwrap_err();
        assert!(errs.iter().any(|e| e.contains("case #1")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("`name`")));
        assert!(errs.iter().any(|e| e.contains("`description`")));
        assert!(errs.iter().any(|e| e.contains("`datagram`")));

        let errs = file_with(r#"{"name":"","description":"d","datagram":"","expect":"reject"}"#)
            .unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("must not be empty")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_a_datagram_that_is_not_hex() {
        let errs = file_with(r#"{"name":"c","description":"d","datagram":"zz","expect":"reject"}"#)
            .unwrap_err();
        assert!(errs.iter().any(|e| e.contains("not valid hex")), "{errs:?}");
    }

    #[test]
    fn rejects_an_unknown_or_mistyped_expectation() {
        for body in [
            r#"{"name":"c","description":"d","datagram":"","expect":"maybe"}"#,
            r#"{"name":"c","description":"d","datagram":"","expect":7}"#,
        ] {
            let errs = file_with(body).unwrap_err();
            assert!(errs.iter().any(|e| e.contains("`expect`")), "{errs:?}");
        }
    }

    #[test]
    fn checks_the_shape_of_a_value() {
        let cases = [
            (r#""value":7"#, "must be an object"),
            (r#""value":{}"#, "missing `header`"),
            (
                r#""value":{"header":1,"payload":{},"tag":"x"}"#,
                "must be an object",
            ),
            (
                r#""value":{"header":{},"payload":{}}"#,
                "missing the string field `tag`",
            ),
            (
                r#""value":{"header":{},"payload":{},"tag":"ab"}"#,
                "lowercase hex digits",
            ),
        ];
        for (fragment, needle) in cases {
            let errs = file_with(&format!(
                r#"{{"name":"c","description":"d","datagram":"","expect":"accept",{fragment}}}"#
            ))
            .unwrap_err();
            assert!(
                errs.iter().any(|e| e.contains(needle)),
                "{fragment}: {errs:?}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_case_names() {
        let body = format!("{},{}", round_trip_case(), round_trip_case());
        let errs = file_with(&body).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("duplicate case name")),
            "{errs:?}"
        );
    }

    #[test]
    fn catches_a_header_field_edited_without_the_hex() {
        // The mistake this whole check exists for: the structure says one thing
        // and the bytes say another, and both directions of the vector agree
        // with each other and disagree with the spec.
        for (field, wrong) in [
            ("\"type\":17", "\"type\":18"),
            ("\"flags\":0", "\"flags\":4"),
            ("\"sequence\":0", "\"sequence\":9"),
            ("\"show_time_us\":0", "\"show_time_us\":9"),
            ("\"version_minor\":1", "\"version_minor\":2"),
            ("\"magic\":76", "\"magic\":77"),
            ("\"mesh_prefix\":\"0000\"", "\"mesh_prefix\":\"0001\""),
        ] {
            let body = round_trip_case().replace(field, wrong);
            let errs = file_with(&body).unwrap_err();
            assert!(
                errs.iter().any(|e| e.contains("but the datagram")),
                "{wrong}: {errs:?}"
            );
        }
    }

    #[test]
    fn catches_a_payload_len_that_disagrees_with_the_bytes() {
        let body = round_trip_case().replace("\"payload_len\":1", "\"payload_len\":2");
        let errs = file_with(&body).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("payload bytes")), "{errs:?}");
    }

    #[test]
    fn catches_a_tag_that_is_not_the_one_on_the_wire() {
        let body = round_trip_case().replace(
            &format!("\"tag\":\"{}\"", "aa".repeat(TAG_LEN)),
            &format!("\"tag\":\"{}\"", "bb".repeat(TAG_LEN)),
        );
        let errs = file_with(&body).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("ends with")), "{errs:?}");
    }

    #[test]
    fn catches_a_missing_header_field() {
        let body = round_trip_case().replace("\"flags\":0,", "");
        let errs = file_with(&body).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("missing the integer `flags`")),
            "{errs:?}"
        );
        let body = round_trip_case().replace("\"mesh_prefix\":\"0000\",", "");
        let errs = file_with(&body).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("missing the hex string `mesh_prefix`")),
            "{errs:?}"
        );
    }

    #[test]
    fn catches_a_value_on_a_datagram_too_short_to_hold_a_header() {
        let body = format!(
            r#"{{"name":"c","description":"d","datagram":"4c01","expect":"accept",
                 "value":{{"header":{},"tag":"{}","payload":{{}}}}}}"#,
            header_json(0),
            "aa".repeat(TAG_LEN)
        );
        let errs = file_with(&body).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("at least")), "{errs:?}");
    }

    #[test]
    fn a_bad_magic_byte_is_reported_even_when_declared_faithfully() {
        let body = round_trip_case()
            .replace(&datagram(&[7], &[]), &datagram(&[7], &[(0, 0x58)]))
            .replace("\"magic\":76", "\"magic\":88");
        let errs = file_with(&body).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("magic byte")), "{errs:?}");
    }

    #[test]
    fn a_behavioural_file_parses_into_a_scenario_and_counts_as_one_vector() {
        let text = r#"{"schema":1,"kind":"behavioural","machine":"node","name":"s",
                       "description":"d","initial_state":{"capacity":1},
                       "steps":[{"at_us":0,"event":{"event":"tick"},"expect":[]}]}"#;
        let file = VectorFile::parse("s.json", text).unwrap();
        // The machine name takes the `message` slot, so a check id reads
        // `node/s/behaviour` the way a codec one reads `TICK/gps/decode`.
        assert_eq!(file.message, "node");
        assert_eq!(file.description, "d");
        assert!(file.cases().is_empty());
        assert_eq!(file.scenario().map(|s| s.steps.len()), Some(1));
        assert_eq!(file.len(), 1);
        assert!(!file.is_empty());
    }

    #[test]
    fn a_codec_file_carries_no_scenario_and_may_say_its_kind_explicitly() {
        let text = format!(
            r#"{{"schema":1,"kind":"codec","message":"TEST","description":"d","cases":[{}]}}"#,
            round_trip_case()
        );
        let file = VectorFile::parse("t.json", &text).unwrap();
        assert!(file.scenario().is_none());
        assert_eq!(file.len(), 1);
    }

    #[test]
    fn rejects_a_kind_it_does_not_understand() {
        for kind in ["\"psychic\"", "7"] {
            let text = format!(
                r#"{{"schema":1,"kind":{kind},"message":"T","description":"d","cases":[]}}"#
            );
            let errs = VectorFile::parse("t.json", &text).unwrap_err();
            assert!(
                errs.iter().any(|e| e.contains("`kind` must be")),
                "{errs:?}"
            );
        }
    }

    #[test]
    fn a_behavioural_file_reports_envelope_and_body_problems_together() {
        // Envelope problems are found before the body is even looked at, and
        // both have to reach the author in one pass.
        let errs = VectorFile::parse("s.json", r#"{"kind":"behavioural"}"#).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("`schema`")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("`description`")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("`machine`")), "{errs:?}");
        assert!(errs.iter().all(|e| e.starts_with("s.json: ")), "{errs:?}");

        // A body that parses cleanly does not rescue a broken envelope.
        let text = r#"{"kind":"behavioural","machine":"node","name":"s","description":"d",
                       "initial_state":{},"steps":[{"at_us":0,"event":{"event":"t"},
                                                    "expect":[]}]}"#;
        let errs = VectorFile::parse("s.json", text).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("`schema`"), "{errs:?}");
    }

    #[test]
    fn loads_and_parses_a_directory_tree() {
        let dir = std::env::temp_dir().join(format!("lumen-vec-{}", std::process::id()));
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            dir.join("good.json"),
            format!(
                r#"{{"schema":1,"message":"A","description":"d","cases":[{}]}}"#,
                round_trip_case()
            ),
        )
        .unwrap();
        std::fs::write(nested.join("bad.json"), "{}").unwrap();
        std::fs::write(dir.join("ignored.txt"), "not a vector").unwrap();

        let sources = load_dir(&dir).unwrap();
        assert_eq!(sources.len(), 2, "only .json files, found at any depth");
        let (files, problems) = parse_all(&sources);
        assert_eq!(files.len(), 1);
        assert!(!problems.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            load_dir(&dir).is_err(),
            "a missing directory is an io error"
        );
    }
}
