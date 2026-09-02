//! `lumen-conformance` — the one shared runner.
//!
//! A shim: everything it does lives in the library, where it can be tested
//! without a process boundary.

use lumen_conformance::cli::{self, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match Options::parse(&args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("lumen-conformance: {e}\n\n{}", cli::USAGE);
            std::process::exit(cli::EXIT_USAGE);
        }
    };
    let (output, code) = cli::execute(&options);
    print!("{output}");
    std::process::exit(code);
}
