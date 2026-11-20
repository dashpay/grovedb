//! Crashes the page store at every write boundary of random workloads. See
//! `lignin_testkit::crash`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(lignin_testkit::crash::cli::run_command(
        &arguments,
        &mut std::io::stdout(),
    ))
}
