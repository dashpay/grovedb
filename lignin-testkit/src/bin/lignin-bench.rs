//! Runs the LigninDB benchmark suite; see `lignin_testkit::bench::cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match lignin_testkit::bench::cli::main_with(&args) {
        Ok(output) => print!("{output}"),
        Err(error) => {
            eprintln!("lignin-bench: {error}");
            std::process::exit(1);
        }
    }
}
