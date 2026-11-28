//! The `lignin` command line: see the documentation of the crate. The
//! library forbids unsafe code; this file holds none.

fn main() {
    let status = lignin_tools::main(
        std::env::args_os(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(status);
}
