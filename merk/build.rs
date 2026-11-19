use std::{env, process::exit};

const TEST_ENGINES: &str = include_str!("test-engines.txt");
const MANIFEST: &str = include_str!("Cargo.toml");
const TEST_ENGINE_VARIABLE: &str = "MERK_TEST_ENGINE";

fn main() {
    select_test_engine();
}

/// Sets the cfg `merk_test_engine` to the engine the test suite of this build runs
/// on: the one `MERK_TEST_ENGINE` names, or the first of `test-engines.txt`. The cfg is
/// set, with no value and with the name, only if the Cargo feature that
/// compiles that engine is on. Without it the suite has no engine, which is
/// an error if the variable asked for one.
fn select_test_engine() {
    let engines: Vec<(&str, &str)> = TEST_ENGINES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(name), Some(feature), None) => (name, feature),
                _ => {
                    eprintln!("merk/test-engines.txt: `{line}` is not a name and a feature");
                    exit(1);
                }
            }
        })
        .collect();
    for (name, feature) in &engines {
        if !MANIFEST.lines().any(|line| {
            line.split_once('=')
                .is_some_and(|(key, _)| key.trim() == *feature)
        }) {
            eprintln!(
                "merk/test-engines.txt: `{name}` names the feature `{feature}`, which merk has not"
            );
            exit(1);
        }
    }
    let names: Vec<&str> = engines.iter().map(|(name, _)| *name).collect();
    let values: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=test-engines.txt");
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-env-changed={TEST_ENGINE_VARIABLE}");
    println!(
        "cargo::rustc-check-cfg=cfg(merk_test_engine, values(none(), {}))",
        values.join(", ")
    );

    let named = env::var(TEST_ENGINE_VARIABLE).unwrap_or_default();
    let selected = if named.is_empty() {
        engines.first().copied()
    } else {
        engines.iter().copied().find(|(name, _)| *name == named)
    };
    let Some((name, feature)) = selected else {
        eprintln!(
            "{TEST_ENGINE_VARIABLE}={named} names no engine of merk/test-engines.txt, which \
             has: {}",
            names.join(", ")
        );
        exit(1);
    };
    let feature_variable = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
    if env::var_os(feature_variable).is_some() {
        println!("cargo::rustc-cfg=merk_test_engine");
        println!("cargo::rustc-cfg=merk_test_engine=\"{name}\"");
        println!("cargo::rustc-env=MERK_SUITE_ENGINE={name}");
    } else if !named.is_empty() {
        eprintln!(
            "{TEST_ENGINE_VARIABLE}={named} selects an engine that is compiled with the feature \
             `{feature}`, which is off in this build"
        );
        exit(1);
    }
}
