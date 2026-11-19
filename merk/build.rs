use std::{env, process::exit};

const TEST_ENGINES: &str = include_str!("test-engines.txt");
const TEST_ENGINE_VARIABLE: &str = "MERK_TEST_ENGINE";

/// Sets the cfg `merk_test_engine` to the engine the test suite of this build
/// runs on: the one `MERK_TEST_ENGINE` names, or the first of
/// `test-engines.txt`.
fn main() {
    let engines: Vec<&str> = TEST_ENGINES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let values: Vec<String> = engines.iter().map(|name| format!("\"{name}\"")).collect();
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=test-engines.txt");
    println!("cargo::rerun-if-env-changed={TEST_ENGINE_VARIABLE}");
    println!(
        "cargo::rustc-check-cfg=cfg(merk_test_engine, values({}))",
        values.join(", ")
    );

    let named = env::var(TEST_ENGINE_VARIABLE).unwrap_or_default();
    let selected = if named.is_empty() {
        engines[0]
    } else if let Some(engine) = engines.iter().find(|engine| **engine == named) {
        engine
    } else {
        eprintln!(
            "{TEST_ENGINE_VARIABLE}={named} names no engine of merk/test-engines.txt, which has: \
             {}",
            engines.join(", ")
        );
        exit(1);
    };
    println!("cargo::rustc-cfg=merk_test_engine=\"{selected}\"");
    println!("cargo::rustc-env=MERK_SUITE_ENGINE={selected}");
}
