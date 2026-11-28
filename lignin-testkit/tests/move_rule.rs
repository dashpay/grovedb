//! The value log's move rule on workloads of large values (PERF-08): how
//! many values, and how many bytes, the commit groups repeat because a
//! cell or a flat entry was carried to another page
//! (`value-log-format.md` O5), beside what the workload writes.
//!
//! `measure_the_move_rule` is ignored: it runs the workloads at the sizes
//! the report gives, and prints one line of JSON for each. The sizes can be
//! set with `MOVE_RULE_MAX_VALUE` and `MOVE_RULE_VALUES`, and LigninDB's
//! settings with `MOVE_RULE_SET` (`KEY=VALUE`, separated by spaces), as
//! `lignin-bench run --set` takes them.
#![cfg(grovedb_engine_seam)]

use grovedb_version::version::GroveVersion;
use lignin_testkit::{
    bench::{run::disk_usage, Engine, EngineSettings, LigninDbEngine},
    workload::{adversarial::LargeValues, Op, Transactions, Workload},
};

const SEED: u64 = 0x5045_5246_3038;

/// What one run of `workload` on LigninDB counted.
fn measure(workload: LargeValues, settings: &EngineSettings) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let grove_version = GroveVersion::latest();
    let mut engine = LigninDbEngine::open_with(dir.path(), settings).unwrap();
    let (mut transitions, mut blocks) = (0u64, 0u64);
    {
        let mut transactions = Transactions::new(engine.grove());
        let ops = Workload::LargeValues(workload).stream(SEED);
        for op in ops.chain([Op::Commit]) {
            match op {
                Op::Commit => blocks += 1,
                Op::Read(_) => {}
                _ => transitions += 1,
            }
            transactions.apply(&op, grove_version).unwrap().unwrap();
        }
    }
    engine.flush().unwrap();
    engine.wait_for_background().unwrap();
    let stats = engine.grove().engine().database().stats();
    let (commits, flush, cleaner) = (&stats.commits, &stats.store.flush, &stats.cleaner);
    let disk = disk_usage(dir.path()).unwrap();
    serde_json::json!({
        "max_value_size": workload.max_value_size,
        "random_values": workload.random_values,
        "transitions": transitions,
        "blocks": blocks,
        "values_logged": flush.values_logged,
        "value_bytes_logged": flush.value_logged_bytes,
        "values_carried": commits.values_carried,
        "value_bytes_carried": commits.value_bytes_carried,
        "values_appended": flush.values_appended,
        "value_payload_bytes": flush.value_bytes,
        "value_log_bytes": flush.value_stored_bytes,
        "regroup_bytes": flush.regroup_stored_bytes,
        "cleaner_value_bytes": cleaner.value_bytes_copied,
        "size_on_disk": disk.bytes,
        "by_extension": disk.by_extension,
    })
}

#[test]
fn carried_values_are_a_part_of_the_values_logged() {
    let workload = LargeValues {
        max_value_size: 16_384,
        random_values: 1_000,
    };
    let measured = measure(workload, &EngineSettings::default());
    let count = |key: &str| measured[key].as_u64().unwrap();
    assert!(count("values_carried") > 0, "{measured}");
    assert!(
        count("values_carried") < count("values_logged"),
        "{measured}"
    );
    assert!(
        count("value_bytes_carried") < count("value_bytes_logged"),
        "{measured}"
    );
}

#[test]
#[ignore = "the sizes of the report"]
fn measure_the_move_rule() {
    let number = |name: &str| std::env::var(name).ok().map(|value| value.parse().unwrap());
    let mut settings = EngineSettings::default();
    for pair in std::env::var("MOVE_RULE_SET")
        .unwrap_or_default()
        .split_whitespace()
    {
        settings.set(pair).unwrap();
    }
    let sizes = match (number("MOVE_RULE_MAX_VALUE"), number("MOVE_RULE_VALUES")) {
        (Some(max_value_size), Some(random_values)) => vec![LargeValues {
            max_value_size,
            random_values,
        }],
        _ => vec![
            LargeValues {
                max_value_size: 16_384,
                random_values: 4_000,
            },
            LargeValues::default(),
        ],
    };
    for workload in sizes {
        println!("MOVE-RULE {}", measure(workload, &settings));
    }
}
