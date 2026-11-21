//! Fuzzes two engines against each other. See `lignin_testkit::diff`.

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(grovedb_engine_seam)]
    {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        ExitCode::from(lignin_testkit::diff::cli::run_command(
            &arguments,
            &mut std::io::stdout(),
        ))
    }
    // Against the engine the golden corpus is pinned to there is one engine
    // and nothing to compare it with.
    #[cfg(not(grovedb_engine_seam))]
    {
        eprintln!("lignin-diff needs a GroveDB with the Engine seam");
        ExitCode::from(2)
    }
}
