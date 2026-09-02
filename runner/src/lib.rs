//! The shared conformance runner.
//!
//! One runner, never one per implementation: divergent runners produce divergent
//! notions of passing, which is the exact failure the suite exists to prevent.
//! Adding an implementation means writing an adapter ([`adapter`]), never
//! touching this crate.
//!
//! # Shape
//!
//! ```text
//! vectors/**/*.json  --[vector]-->  VectorFile
//!                                       |
//!                                    [run] --[proto]--> adapter process
//!                                       |
//!                                    Report --[report]--> stdout
//! ```
//!
//! [`vector`] is data only, [`adapter`] is I/O only, and [`run`] is the logic in
//! between with neither. A vector file never names a Rust type and the runner
//! never names a message type; the two meet only through JSON the runner
//! forwards without understanding.
//!
//! # Zero dependencies
//!
//! Including for JSON — see [`json`]. The suite is what an implementer runs
//! first, often on an unfamiliar machine, and `cargo run` working without a
//! network is worth more than the few hundred lines it costs.

#![forbid(unsafe_code)]

pub mod adapter;
pub mod cli;
pub mod hex;
pub mod json;
pub mod matcher;
pub mod proto;
pub mod report;
pub mod run;
pub mod scenario;
pub mod vector;

#[cfg(test)]
pub(crate) mod testing;
