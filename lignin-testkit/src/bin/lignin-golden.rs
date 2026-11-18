//! Records, replays and dumps the golden trace corpus. See
//! `lignin_testkit::golden`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(lignin_testkit::golden::cli::run(
        &arguments,
        &mut std::io::stdout(),
    ))
}
