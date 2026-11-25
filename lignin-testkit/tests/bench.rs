//! The benchmark harness (BASE-03) end to end, at a small size.
//!
//! The acceptance runs at the reference size are made with the
//! `lignin-bench` binary; this checks that a run produces every row of the
//! prediction table, that its count rows come out the same in a second run,
//! and that the report reads back and compares.
//!
//! A run of these tests asks for instruction counts where `perf` counts
//! them on the machine, and measures without them where it counts none:
//! there a run that asks for them is refused.

use std::sync::OnceLock;

use grovedb_version::version::GroveVersion;
use lignin_testkit::bench::{
    cli, compare, machine::Perf, run, EngineChoice, Report, RowKind, RunOptions, Settings,
};

/// Whether `perf` counts the instructions of a thread here: what the probe
/// says by which a run that asks for instruction counts is refused where
/// `perf` counts none. It is asked once for the tests of this file.
fn perf_counts() -> bool {
    static COUNTS: OnceLock<bool> = OnceLock::new();
    *COUNTS.get_or_init(|| {
        let scratch = tempfile::tempdir().unwrap();
        Perf::works(scratch.path())
    })
}

/// Prints the instructions of every row of `report` that has a count.
fn print_counts(report: &Report) {
    for engine in &report.engines {
        for row in &engine.rows {
            if let Some(instructions) = row.instructions_per_sample {
                let (name, phase, id) = (&engine.engine, &row.phase, &row.id);
                println!("{name} {phase} {id}: {instructions:.0} instructions a sample");
            }
        }
    }
}

fn small(scratch: std::path::PathBuf) -> Settings {
    let perf = perf_counts();
    if !perf {
        println!("`perf` counts no instructions here: this run measures without them");
    }
    Settings {
        seed: 3,
        documents: 2_000,
        owners: 10,
        measured_creates: 200,
        updates: 100,
        deletes: 100,
        point_reads: 20,
        reference_reads: 5,
        warmup_reads: 6,
        cold_reads: 2,
        checkpoints: 2,
        reopens: 2,
        perf,
        scratch,
        keep: false,
    }
}

fn options(scratch: std::path::PathBuf) -> RunOptions {
    RunOptions {
        settings: small(scratch),
        engines: vec![EngineChoice::RocksDb],
        machine_label: Some("test".to_string()),
        commit: None,
        grove_version: GroveVersion::latest(),
    }
}

/// The rows that are times.
const TIME_ROWS: [&str; 8] = [
    "bulk_load",
    "wall_per_transition",
    "block_commit",
    "checkpoint",
    "reopen",
    "point_get",
    "point_query_proof",
    "references_proof",
];

/// The rows whose values follow the schedule: where a log file starts,
/// when background work ran, what the rest of the process wrote.
const MEASURED_ROWS: [&str; 6] = [
    "bytes_logged_per_transition",
    "physical_bytes_per_transition",
    "flush_bytes_per_transition",
    "compaction_bytes_per_transition",
    "process_write_bytes_per_transition",
    "size_on_disk",
];

fn kind_of(id: &str) -> RowKind {
    if TIME_ROWS.contains(&id) {
        RowKind::Time
    } else if MEASURED_ROWS.contains(&id) {
        RowKind::Measured
    } else {
        RowKind::Count
    }
}

/// The id and phase of every count row of `report`.
fn count_rows(report: &Report) -> std::collections::BTreeSet<(String, String)> {
    report.engines[0]
        .rows
        .iter()
        .filter(|row| row.kind == RowKind::Count)
        .map(|row| (row.id.clone(), row.phase.clone()))
        .collect()
}

#[test]
fn a_run_measures_every_row_and_count_rows_repeat() {
    let scratch = tempfile::tempdir().unwrap();
    let first = run(&options(scratch.path().join("first"))).unwrap();
    let second = run(&options(scratch.path().join("second"))).unwrap();
    assert_eq!(first.method.perf_instructions, perf_counts());
    print_counts(&first);

    assert_eq!(first.engines.len(), 1);
    let engine = &first.engines[0];
    assert_eq!(engine.engine, "rocksdb");
    assert_eq!(engine.root_hash_after_load.len(), 64);
    assert_ne!(engine.root_hash_after_load, engine.root_hash_at_end);
    assert_eq!(engine.root_hash_at_end, second.engines[0].root_hash_at_end);
    assert!(
        !scratch.path().join("first").join("rocksdb").exists(),
        "the database is removed"
    );

    for (id, phase) in [
        ("logical_bytes_per_transition", "load"),
        ("node_records_per_transition", "load"),
        ("bytes_handed_per_transition", "load"),
        ("bytes_logged_per_transition", "load"),
        ("physical_bytes_per_transition", "load"),
        ("keyed_lookups_per_transition", "load"),
        ("hashes_per_transition", "load"),
        ("bulk_load", "load"),
        ("size_on_disk", "at-size"),
        ("checkpoint", "at-size"),
        ("checkpoint", "flushed"),
        ("size_on_disk", "flushed"),
        ("reopen", "flushed"),
        ("reopen", "cold"),
        ("seeks_per_transition", "load"),
        ("point_get", "warm"),
        ("point_query_proof", "warm"),
        ("references_proof", "warm"),
        ("point_proof_bytes", "warm"),
        ("references_proof_bytes", "warm"),
        ("point_get", "cold"),
        ("point_query_proof", "cold"),
        ("references_proof", "cold"),
        ("wall_per_transition", "create"),
        ("block_commit", "create"),
        ("wall_per_transition", "update"),
        ("wall_per_transition", "delete"),
        ("bytes_logged_per_transition", "delete"),
    ] {
        let row = engine
            .row(id, phase)
            .unwrap_or_else(|| panic!("no row {id} in {phase}"));
        assert!(!row.source.is_empty());
        if row.kind == RowKind::Time {
            assert_eq!(row.value, row.summary.unwrap().median);
            assert!(row.value >= 0.0, "{id} in {phase} is {}", row.value);
            assert_eq!(row.ratio_to_rocksdb, (row.value != 0.0).then_some(1.0));
        } else {
            assert!(row.value > 0.0, "{id} in {phase} is {}", row.value);
            assert_eq!(row.ratio_to_rocksdb, Some(1.0));
        }
    }

    let create = engine.row("wall_per_transition", "create").unwrap();
    assert_eq!(create.samples.len(), 10, "one sample per block");
    assert!(create.recorded.is_some() && create.ratio_to_recorded.is_some());

    let records = engine
        .row("node_records_per_transition", "load")
        .unwrap()
        .value;
    assert!(
        (15.0..80.0).contains(&records),
        "{records} node records per transition"
    );
    let handed = engine
        .row("bytes_handed_per_transition", "load")
        .unwrap()
        .value;
    let logged = engine
        .row("bytes_logged_per_transition", "load")
        .unwrap()
        .value;
    assert!(
        logged > handed && logged < handed * 1.1,
        "{logged} vs {handed}"
    );
    let logical = engine
        .row("logical_bytes_per_transition", "load")
        .unwrap()
        .value;
    assert!((500.0..600.0).contains(&logical), "{logical} logical bytes");
    let reference_proof = engine.row("references_proof_bytes", "warm").unwrap();
    assert!(reference_proof.value > 100.0 * 400.0);

    let value_share = engine.row("node_record_value_share", "load").unwrap().value;
    let shares: f64 = [
        "node_record_value_share",
        "node_record_key_share",
        "node_record_node_hash_share",
        "node_record_link_hash_share",
        "node_record_other_share",
    ]
    .iter()
    .map(|id| engine.row(id, "load").unwrap().value)
    .sum();
    assert!((shares - 1.0).abs() < 1e-9, "shares sum to {shares}");
    assert!(
        (0.2..0.8).contains(&value_share),
        "{value_share} of node bytes are values"
    );

    let load = &engine.phases[0];
    let anatomy = load.node_anatomy.unwrap();
    assert_eq!(anatomy.records, load.counters.node_records);
    assert_eq!(anatomy.undecoded, 0);
    assert_eq!(
        (load.phase.as_str(), load.transitions, load.blocks),
        ("load", 2_000, 100)
    );
    assert_eq!(load.counters.log_groups, 100, "one write batch per block");

    for report in [&first, &second] {
        for row in &report.engines[0].rows {
            assert_eq!(row.kind, kind_of(&row.id), "{} in {}", row.id, row.phase);
        }
    }
    let counted = count_rows(&first);
    assert_eq!(
        counted,
        count_rows(&second),
        "both runs have the same count rows"
    );

    let agreements = compare(&first, &second);
    let counts: Vec<_> = agreements
        .iter()
        .filter(|agreement| agreement.kind == RowKind::Count)
        .collect();
    assert_eq!(counts.len(), counted.len());
    for agreement in &counts {
        assert!(
            agreement.within(0.0),
            "{} in {} changed between runs: {} then {}",
            agreement.id,
            agreement.phase,
            agreement.first,
            agreement.second
        );
    }

    let json = first.to_json();
    assert_eq!(Report::from_json(&json).unwrap(), first);
    let dir = scratch.path();
    std::fs::write(dir.join("first.json"), &json).unwrap();
    std::fs::write(dir.join("second.json"), second.to_json()).unwrap();
    let path = |name: &str| dir.join(name).display().to_string();
    let table = cli::main_with(&["table".to_string(), path("first.json")]).unwrap();
    assert!(table.contains("node_records_per_transition"));
    let comparison = cli::main_with(&[
        "compare".to_string(),
        path("first.json"),
        path("second.json"),
        "--tolerance".to_string(),
        "0".to_string(),
    ])
    .unwrap();
    let n = counts.len();
    assert!(
        comparison.contains(&format!("within +0.0%: {n} of {n} count rows, ")),
        "{comparison}"
    );
    assert!(comparison.contains("measured rows"));
    assert!(comparison.contains("time rows by instructions"));
}

/// The rows that come from GroveDB's charges and from what the workload
/// writes: the same on every engine.
#[cfg(grovedb_engine_seam)]
const CHARGED_ROWS: [&str; 4] = [
    "logical_bytes_per_transition",
    "seeks_per_transition",
    "hashes_per_transition",
    "loaded_bytes_per_transition",
];

#[cfg(grovedb_engine_seam)]
#[test]
fn a_run_of_both_engines_gives_one_grove_and_the_same_charges() {
    let scratch = tempfile::tempdir().unwrap();
    let mut options = options(scratch.path().join("both"));
    options.engines.push(EngineChoice::LigninDb);
    let report = run(&options).unwrap();
    print_counts(&report);

    let [rocksdb, lignindb] = &report.engines[..] else {
        panic!("{} engines were measured", report.engines.len());
    };
    assert_eq!(
        (rocksdb.engine.as_str(), lignindb.engine.as_str()),
        ("rocksdb", "lignindb")
    );
    assert_eq!(rocksdb.root_hash_after_load, lignindb.root_hash_after_load);
    assert_eq!(rocksdb.root_hash_at_end, lignindb.root_hash_at_end);
    assert!(
        !scratch.path().join("both").join("lignindb").exists(),
        "the database is removed"
    );

    for phase in ["load", "create", "update", "delete"] {
        for id in CHARGED_ROWS {
            let row = lignindb
                .row(id, phase)
                .unwrap_or_else(|| panic!("no row {id} in {phase}"));
            let base = rocksdb.row(id, phase).unwrap();
            assert_eq!(row.value, base.value, "{id} in {phase}");
            assert_eq!(row.ratio_to_rocksdb, (base.value != 0.0).then_some(1.0));
        }
        let lookups = lignindb.row("keyed_lookups_per_transition", phase).unwrap();
        assert!(
            lookups.source.contains("searches by key"),
            "{}",
            lookups.source
        );
        let wall = lignindb.row("wall_per_transition", phase).unwrap();
        assert!(wall.value > 0.0 && wall.ratio_to_rocksdb.is_some());
        assert!(lignindb.row("node_record_value_share", phase).is_none());
    }
    for id in ["point_proof_bytes", "references_proof_bytes"] {
        let row = lignindb.row(id, "warm").unwrap();
        assert_eq!(row.value, rocksdb.row(id, "warm").unwrap().value, "{id}");
    }
    for (id, phase) in [
        ("point_get", "warm"),
        ("point_query_proof", "warm"),
        ("references_proof", "warm"),
        ("point_get", "cold"),
        ("checkpoint", "flushed"),
        ("reopen", "flushed"),
        ("size_on_disk", "flushed"),
        ("block_commit", "create"),
    ] {
        let row = lignindb
            .row(id, phase)
            .unwrap_or_else(|| panic!("no row {id} in {phase}"));
        assert!(row.value > 0.0, "{id} in {phase} is {}", row.value);
    }

    let load = &lignindb.phases[0];
    assert_eq!(
        (load.phase.as_str(), load.transitions, load.blocks),
        ("load", 2_000, 100)
    );
    assert_eq!(load.counters.log_groups, 100, "one log group per block");
    assert!(load.node_anatomy.is_none());
    assert!(load.counters.keyed_lookups.is_some() && load.counters.hashes.is_some());
    assert!(load.counters.page_reads.is_some() && load.counters.cache_hits.is_some());
    for row in &lignindb.rows {
        assert_eq!(row.kind, kind_of(&row.id), "{} in {}", row.id, row.phase);
    }
}

#[test]
fn the_command_writes_a_report() {
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("report.json");
    let line = format!(
        "run --scratch {} --out {} --documents 400 --owners 2 --creates 20 --updates 0 \
         --deletes 0 --point-reads 2 --reference-reads 1 --warmup-reads 0 --cold-reads 1 \
         --checkpoints 1 --reopens 1 --label test --commit 0123",
        scratch.path().join("db").display(),
        out.display()
    );
    let args: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    let printed = cli::main_with(&args).unwrap();
    assert!(printed.contains("bulk_load"));
    let report = Report::from_json(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report.commit.as_deref(), Some("0123"));
    assert_eq!(report.machine.label.as_deref(), Some("test"));
    assert!(report.engines[0]
        .row("wall_per_transition", "update")
        .is_none());

    let to_stdout: Vec<String> = args
        .iter()
        .take_while(|arg| *arg != "--out")
        .chain(args.iter().skip_while(|arg| *arg != "--out").skip(2))
        .cloned()
        .collect();
    let json = cli::main_with(&to_stdout).unwrap();
    assert!(Report::from_json(&json).is_ok());

    let unwritable = args
        .iter()
        .map(|arg| {
            if *arg == out.display().to_string() {
                "/nonexistent/dir/report.json".to_string()
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>();
    assert!(cli::main_with(&unwritable)
        .unwrap_err()
        .to_string()
        .starts_with("writing /nonexistent/dir/report.json"));
}

#[test]
fn a_run_that_asks_for_instructions_is_refused_where_perf_counts_none() {
    if perf_counts() {
        println!("`perf` counts instructions here: no run is refused for asking");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let mut options = options(scratch.path().join("asked"));
    options.settings.perf = true;
    let error = run(&options).unwrap_err();
    assert!(error.to_string().starts_with("--perf: "), "{error}");
    assert!(
        !scratch.path().join("asked").join("rocksdb").exists(),
        "nothing was measured"
    );
}

#[test]
fn a_run_without_enough_documents_per_owner_is_refused() {
    let scratch = tempfile::tempdir().unwrap();
    let mut options = options(scratch.path().to_path_buf());
    options.settings.documents = 40;
    options.settings.owners = 20;
    let error = run(&options).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no owner has 100 documents to read through references"
    );
    options.settings.deletes = 1_000;
    assert_eq!(
        run(&options).unwrap_err().to_string(),
        "more deletes than documents"
    );
}
