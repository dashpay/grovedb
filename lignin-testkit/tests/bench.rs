//! The benchmark harness (BASE-03) end to end, at a small size.
//!
//! The acceptance runs at the reference size are made with the
//! `lignin-bench` binary; this checks that a run produces every row of the
//! prediction table, that its count rows come out the same in a second run,
//! and that the report reads back and compares.

use grovedb_version::version::GroveVersion;
use lignin_testkit::bench::{
    cli, compare, run, ColdMethod, EngineChoice, Report, RowKind, RunOptions, Settings,
};

fn small(scratch: std::path::PathBuf) -> Settings {
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
        perf: true,
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

#[test]
fn a_run_measures_every_row_and_count_rows_repeat() {
    let scratch = tempfile::tempdir().unwrap();
    let first = run(&options(scratch.path().join("first"))).unwrap();
    let second = run(&options(scratch.path().join("second"))).unwrap();

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
        assert!(row.value > 0.0, "{id} in {phase} is {}", row.value);
        assert!(!row.source.is_empty());
        assert_eq!(row.ratio_to_rocksdb, Some(1.0));
        if row.kind == RowKind::Time {
            assert_eq!(row.value, row.summary.unwrap().median);
        }
    }

    let create = engine.row("wall_per_transition", "create").unwrap();
    assert_eq!(create.samples.len(), 10, "one sample per block");
    assert!(create.recorded.is_some() && create.ratio_to_recorded.is_some());
    if first.method.perf_instructions {
        assert!(create.instructions_per_sample.unwrap() > 1_000.0);
    }
    if first.method.thread_cpu_time {
        assert!(create.cpu_per_sample.unwrap() > 0.0);
    }
    if first.method.cold_method == ColdMethod::ReopenAndDropFileCache {
        let cold = engine.row("point_get", "cold").unwrap();
        assert!(cold.storage_read_bytes_per_sample.is_some());
    }

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

    let agreements = compare(&first, &second);
    for agreement in agreements.iter().filter(|agreement| {
        agreement.kind == RowKind::Count
            && !agreement.id.contains("physical")
            && !agreement.id.contains("flush")
            && !agreement.id.contains("compaction")
            && !agreement.id.contains("process_write")
            && !agreement.id.starts_with("size_on_disk")
    }) {
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
        "1000".to_string(),
    ])
    .unwrap();
    assert!(comparison.contains("within +100000.0%"));
    assert!(comparison.contains("time rows by instructions"));
    assert!(!comparison.contains("APART"));
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
