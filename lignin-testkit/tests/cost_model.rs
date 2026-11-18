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
