//! The acceptance test of the cost model (BASE-06): the storage rules of
//! `docs/lignindb/cost-model.md`, applied to a logical state, predict every
//! `OperationCost` of the golden corpus.
//!
//! GroveDB cannot run on the model inside this workspace before the
//! `Engine` seam is cut, so the test runs
//! `lignin-testkit/cost-model-storage/check.sh`, which builds the workspace
//! a second time with the storage crate replaced by the model and replays
//! the corpus there. That build takes minutes, so the test runs on request:
//!
//! ```text
//! cargo test -p lignin-testkit --test cost_model -- --ignored
//! ```

use std::process::Command;

/// The names of the variants of the enum `name` in the Rust source `file`.
fn variants(file: &str, name: &str) -> Vec<String> {
    let opening = format!("pub enum {name} {{");
    let body = file.split_once(&opening).map(|(_, rest)| rest);
    let body = body.and_then(|rest| rest.split_once("\n}"));
    let (body, _) = body.unwrap_or_else(|| panic!("no enum {name}"));
    let named = |line: &str| {
        let line = line.trim_start();
        let name: String = line
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        line.starts_with(char::is_uppercase).then_some(name)
    };
    body.lines().filter_map(named).collect()
}

/// GroveDB is built against the model's storage crate as it is against the
/// real one, and matches on the errors of both: an enum of the stand-in
/// that lacks a variant fails that build far from its cause.
#[test]
fn the_errors_of_the_stand_in_have_the_variants_of_the_storage_crate() {
    let real = include_str!("../../storage/src/error.rs");
    let stand_in = include_str!("../cost-model-storage/src/error.rs");
    for name in ["Error", "CommitError", "WriteScopeError"] {
        let expected = variants(real, name);
        assert!(!expected.is_empty(), "no variant of {name} was read");
        assert_eq!(
            variants(stand_in, name),
            expected,
            "`{name}` of lignin-testkit/cost-model-storage/src/error.rs against storage/src/error.rs"
        );
    }
}

#[test]
#[ignore = "builds the workspace a second time; run it on request"]
fn the_cost_model_predicts_every_cost_of_the_corpus() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/cost-model-storage/check.sh");
    let output = Command::new(script)
        .arg("replay")
        .output()
        .unwrap_or_else(|error| panic!("{script}: {error}"));
    let report = String::from_utf8_lossy(&output.stdout);
    let summary = report
        .lines()
        .find(|line| line.starts_with("golden corpus:"))
        .unwrap_or_else(|| {
            panic!(
                "no replay was reported:\n{report}\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert!(output.status.success(), "{report}");
    assert!(summary.ends_with(", 0 not"), "{summary}");
}
