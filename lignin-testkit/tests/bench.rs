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
    cli, compare,
    machine::{Perf, StorageReads},
    run, EngineChoice, Report, RowKind, RunOptions, Settings,
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
        churn: 0,
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
        engine: Default::default(),
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
const MEASURED_ROWS: [&str; 13] = [
    "bytes_logged_per_transition",
    "physical_bytes_per_transition",
    "physical_bytes_to_flushed",
    "flush_bytes_per_transition",
    "compaction_bytes_per_transition",
    "process_write_bytes_per_transition",
    "size_on_disk",
    "point_get_pages_asked",
    "point_get_pages_missed",
    "point_query_proof_pages_asked",
    "point_query_proof_pages_missed",
    "references_proof_pages_asked",
    "references_proof_pages_missed",
];

/// Whether `id` is a row of what the system counted of the process's
/// reads, over a write phase or over the timed reads of one kind.
fn is_system_read_count(id: &str) -> bool {
    ["process_read_bytes", "process_read_calls", "device_read"]
        .iter()
        .any(|count| id.contains(count))
}

/// Whether `id` is a row of what a write phase left on disk: the size at
/// its end, and the dead shares of its start and end.
fn is_space_of_a_phase(id: &str) -> bool {
    id == "size_on_disk_at_end" || id.ends_with("_at_start") || id.ends_with("_at_end")
}

/// Whether `id` is a row of the pages a write phase asked its pool for.
fn is_page_count_of_a_phase(id: &str) -> bool {
    ["page_reads_per_transition", "pages_asked_per_transition"].contains(&id)
}

fn kind_of(id: &str) -> RowKind {
    if TIME_ROWS.contains(&id) {
        RowKind::Time
    } else if MEASURED_ROWS.contains(&id)
        || is_detail(id)
        || is_system_read_count(id)
        || is_page_count_of_a_phase(id)
        || is_space_of_a_phase(id)
    {
        RowKind::Measured
    } else {
        RowKind::Count
    }
}

/// Whether `id` is the row of a detail of LigninDB's background work.
#[cfg(grovedb_engine_seam)]
fn is_detail(id: &str) -> bool {
    use lignin_testkit::bench::lignindb::{DETAIL_IDS, GAUGE_IDS};
    GAUGE_IDS.contains(&id)
        || id
            .strip_suffix("_per_transition")
            .or_else(|| id.strip_suffix("_to_flushed"))
            .is_some_and(|detail| DETAIL_IDS.contains(&detail))
}

#[cfg(not(grovedb_engine_seam))]
fn is_detail(_id: &str) -> bool {
    false
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
        ("point_get", "settled"),
        ("point_query_proof", "settled"),
        ("references_proof", "settled"),
        ("point_proof_bytes", "settled"),
        ("references_proof_bytes", "settled"),
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
        let flushed = |id: &str| {
            let rows = &report.engines[0].rows;
            rows.iter()
                .any(|row| row.id == id && row.phase == "flushed")
        };
        assert!(flushed("physical_bytes_to_flushed"));
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
        for phase in ["warm", "settled"] {
            let row = lignindb.row(id, phase).unwrap();
            assert_eq!(row.value, rocksdb.row(id, phase).unwrap().value, "{id}");
        }
    }
    for detail in lignin_testkit::bench::lignindb::DETAIL_IDS {
        let id = format!("{detail}_to_flushed");
        assert!(lignindb.row(&id, "flushed").is_some(), "no row {id}");
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

/// The warm rows are measured again over the databases a run kept: the
/// same grove, so the same proofs, and nothing but the warm rows, in the
/// phase of the settled state, which a run measures too.
#[test]
fn the_warm_reads_are_measured_again_over_a_kept_database() {
    let scratch = tempfile::tempdir().unwrap();
    let kept = scratch.path().join("db");
    let out = scratch.path().join("run.json");
    let sizes = "--documents 400 --owners 2 --creates 0 --updates 0 --deletes 0 --point-reads 4 \
                 --reference-reads 2 --warmup-reads 3 --label test --commit 0123";
    let engines = if cfg!(grovedb_engine_seam) {
        "--engine rocksdb --engine lignindb"
    } else {
        "--engine rocksdb"
    };
    let args = |command: &str, out: &std::path::Path, rest: &str| -> Vec<String> {
        format!(
            "{command} --scratch {} --out {} {engines} {sizes} {rest}",
            kept.display(),
            out.display()
        )
        .split_whitespace()
        .map(str::to_string)
        .collect()
    };

    // Nothing is kept yet.
    let refused = cli::main_with(&args("reads", &out, "")).unwrap_err();
    assert!(
        refused.to_string().starts_with("no kept database at "),
        "{refused}"
    );

    cli::main_with(&args(
        "run",
        &out,
        "--keep --cold-reads 1 --checkpoints 1 --reopens 1",
    ))
    .unwrap();
    let run = Report::from_json(&std::fs::read_to_string(&out).unwrap()).unwrap();

    let out = scratch.path().join("reads.json");
    let printed = cli::main_with(&args("reads", &out, "")).unwrap();
    assert!(printed.contains("references_proof"), "{printed}");
    let reads = Report::from_json(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(reads.engines.len(), run.engines.len());
    for (read, ran) in reads.engines.iter().zip(&run.engines) {
        assert_eq!(read.engine, ran.engine);
        assert_eq!(read.root_hash_after_load, ran.root_hash_after_load);
        assert_eq!(read.root_hash_at_end, ran.root_hash_after_load);
        // An engine that counts the pages its pool is asked for has two
        // rows more for each kind of read: LigninDB does, RocksDB does not.
        let (pages, ids): (Vec<&str>, Vec<&str>) = (read.rows.iter())
            .map(|row| row.id.as_str())
            .filter(|id| !is_system_read_count(id))
            .partition(|id| id.contains("_pages_"));
        // What the system counts of the process's reads is a row for each
        // kind of read, where the system counts it.
        let counted = StorageReads::now();
        for kind in ["point_get", "point_query_proof", "references_proof"] {
            for (count, given) in [
                ("process_read_bytes", counted.process_bytes.is_some()),
                ("process_read_calls", counted.process_calls.is_some()),
                ("device_reads", counted.device_reads.is_some()),
                ("device_read_bytes", counted.device_bytes.is_some()),
            ] {
                let row = read.row(&format!("{kind}_{count}"), "settled");
                assert_eq!(row.is_some(), given, "{kind}_{count}");
            }
        }
        assert_eq!(
            ids,
            [
                "point_get",
                "point_query_proof",
                "point_proof_bytes",
                "references_proof",
                "references_proof_bytes"
            ]
        );
        match read.engine.as_str() {
            "rocksdb" => assert!(pages.is_empty(), "{pages:?}"),
            _ => {
                assert_eq!(
                    pages,
                    [
                        "point_get_pages_asked",
                        "point_get_pages_missed",
                        "point_query_proof_pages_asked",
                        "point_query_proof_pages_missed",
                        "references_proof_pages_asked",
                        "references_proof_pages_missed"
                    ]
                );
                // Every read asks the pool for a page, and the pool
                // misses no more pages than it is asked for. (What the
                // rows are made of is held by a test beside them.)
                for kind in ["point_get", "point_query_proof", "references_proof"] {
                    let asked = read.row(&format!("{kind}_pages_asked"), "settled");
                    let missed = read.row(&format!("{kind}_pages_missed"), "settled");
                    let (asked, missed) = (asked.unwrap().value, missed.unwrap().value);
                    assert!(asked >= 1.0 && missed <= asked, "{kind}: {asked}, {missed}");
                }
            }
        }
        assert!(read.rows.iter().all(|row| row.phase == "settled"));
        // The run has the settled rows beside the ones after the load.
        for row in &read.rows {
            assert!(ran.row(&row.id, "settled").is_some(), "{}", row.id);
            assert!(ran.row(&row.id, "warm").is_some(), "{}", row.id);
        }
        for (id, samples) in [
            ("point_get", 4),
            ("point_query_proof", 4),
            ("references_proof", 2),
        ] {
            let row = read.row(id, "settled").unwrap();
            assert_eq!(row.samples.len(), samples, "{id}");
            assert!(row.value > 0.0, "{id}");
        }
        // The reads are those of the run's settled rows: the same seed
        // and the same numbers of reads ask for the same documents and
        // owners in the same order, so every proof is as long. They are
        // not the reads of the rows after the load, which are a stream
        // of their own.
        for id in ["point_proof_bytes", "references_proof_bytes"] {
            assert_eq!(
                read.row(id, "settled").unwrap().samples,
                ran.row(id, "settled").unwrap().samples,
                "{id}"
            );
        }
        assert!(read.phases.is_empty());
        // It has no size on disk, where the run has both.
        assert_eq!(
            (&read.disk_after_load, &read.disk_after_flush),
            (&None, &None)
        );
        assert!(ran.disk_after_load.is_some() && ran.disk_after_flush.is_some());
    }
    let written = std::fs::read_to_string(&out).unwrap();
    for size in ["disk_after_load", "disk_after_flush"] {
        assert!(written.contains(&format!("\"{size}\": null")), "{size}");
    }
    // The report says what was measured: no write after the load, no cold
    // read, no checkpoint and no reopen, whatever the options named.
    let said = (&reads.workload, &reads.method);
    assert_eq!(
        (said.0.measured_creates, said.0.updates, said.0.deletes),
        (0, 0, 0)
    );
    assert_eq!(
        (said.1.cold_reads, said.1.checkpoints, said.1.reopens),
        (0, 0, 0)
    );
    assert_eq!((said.1.point_reads, said.1.reference_reads), (4, 2));
    // The run that kept the databases states its own.
    assert_eq!(run.method.reopens, 1);
    // The second engine's rows carry their ratio to RocksDB's.
    if let Some(second) = reads.engines.get(1) {
        assert!(second
            .row("point_get", "settled")
            .unwrap()
            .ratio_to_rocksdb
            .is_some());
    }
    // The kept databases are still there for the next call, and a call
    // whose options name writes, cold reads, checkpoints and reopens
    // reports none all the same.
    let rest = "--creates 20 --updates 20 --deletes 20 --cold-reads 3 --checkpoints 2 --reopens 2";
    cli::main_with(&args("reads", &out, rest)).unwrap();
    let again = Report::from_json(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        (&again.workload, &again.method),
        (&reads.workload, &reads.method)
    );
}

/// The writes at size are measured over a copy of the databases a run
/// kept. The stream goes on where the load ended, so the grove at the end
/// is the one a whole run with the same writes ends with, GroveDB charges
/// every phase what it charges it there, and the kept databases are as
/// they were, for the next call.
#[test]
fn the_writes_at_size_are_measured_over_a_copy_of_a_kept_database() {
    let scratch = tempfile::tempdir().unwrap();
    let kept = scratch.path().join("kept");
    let whole = scratch.path().join("whole");
    let sizes = "--documents 400 --owners 2 --point-reads 4 --reference-reads 2 --warmup-reads 3 \
                 --cold-reads 1 --checkpoints 1 --reopens 1 --label test --commit 0123";
    let writes = "--creates 40 --updates 20 --deletes 20";
    let engines = if cfg!(grovedb_engine_seam) {
        "--engine rocksdb --engine lignindb"
    } else {
        "--engine rocksdb"
    };
    let call = |command: &str, dir: &std::path::Path, name: &str, rest: &str| -> Report {
        let out = scratch.path().join(name);
        let args: Vec<String> = format!(
            "{command} --scratch {} --out {} {engines} {sizes} {rest}",
            dir.display(),
            out.display()
        )
        .split_whitespace()
        .map(str::to_string)
        .collect();
        cli::main_with(&args).unwrap();
        Report::from_json(&std::fs::read_to_string(&out).unwrap()).unwrap()
    };

    // Nothing is kept yet.
    let args: Vec<String> = format!("writes --scratch {} {engines} {sizes}", kept.display())
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let refused = cli::main_with(&args).unwrap_err();
    assert!(
        refused.to_string().starts_with("no kept database at "),
        "{refused}"
    );

    let loaded = call(
        "run",
        &kept,
        "kept.json",
        "--keep --creates 0 --updates 0 --deletes 0",
    );
    let ran = call("run", &whole, "whole.json", writes);

    // While one call has the copy of an engine, a second is refused: it
    // would remove the copy under the first.
    let lock = std::fs::File::create(kept.join("rocksdb-writes.lock")).unwrap();
    lock.try_lock().unwrap();
    // What stands for the first call's copy is not touched by the second.
    let copy = kept.join("rocksdb-writes");
    std::fs::create_dir(&copy).unwrap();
    std::fs::write(copy.join("in-use"), b"the first call's").unwrap();
    let args: Vec<String> = format!("writes --scratch {} {engines} {sizes}", kept.display())
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let refused = cli::main_with(&args).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("another `writes` call is measuring over the copy of rocksdb"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read(copy.join("in-use")).unwrap(),
        b"the first call's"
    );
    drop(lock);

    let wrote = call("writes", &kept, "writes.json", writes);
    assert_eq!(wrote.engines.len(), ran.engines.len());
    for ((wrote, ran), loaded) in wrote.engines.iter().zip(&ran.engines).zip(&loaded.engines) {
        assert_eq!(wrote.engine, ran.engine);
        assert_eq!(wrote.root_hash_after_load, loaded.root_hash_after_load);
        assert_eq!(wrote.root_hash_after_load, ran.root_hash_after_load);
        assert_eq!(wrote.root_hash_at_end, ran.root_hash_at_end);
        let phases: Vec<(&str, u64)> = (wrote.phases.iter())
            .map(|totals| (totals.phase.as_str(), totals.transitions))
            .collect();
        assert_eq!(phases, [("create", 40), ("update", 20), ("delete", 20)]);
        assert!(wrote
            .rows
            .iter()
            .all(|row| ["create", "update", "delete"].contains(&row.phase.as_str())));
        for (phase, _) in phases {
            // The same operations on the same grove: the same charges.
            for id in [
                "logical_bytes_per_transition",
                "seeks_per_transition",
                "hashes_per_transition",
                "loaded_bytes_per_transition",
            ] {
                let (here, there) = (wrote.row(id, phase), ran.row(id, phase));
                assert_eq!(here.unwrap().value, there.unwrap().value, "{id} {phase}");
            }
            assert!(wrote.row("wall_per_transition", phase).unwrap().value > 0.0);
            // An engine that counts the pages its pool is asked for has
            // them for a transition: LigninDB does, RocksDB does not.
            let pages = wrote.row("page_reads_per_transition", phase);
            let asked = wrote.row("pages_asked_per_transition", phase);
            match wrote.engine.as_str() {
                "rocksdb" => assert!(pages.is_none() && asked.is_none()),
                _ => assert!(asked.unwrap().value >= pages.unwrap().value),
            }
        }
        assert_eq!(
            (&wrote.disk_after_load, &wrote.disk_after_flush),
            (&None, &None)
        );
        // The copy is gone, and the kept database is still there.
        assert!(kept.join(&wrote.engine).is_dir());
        assert!(!kept.join(format!("{}-writes", wrote.engine)).exists());
    }
    // The report says what was measured: the writes, and no timed read,
    // no cold read, no checkpoint and no reopen, whatever the options
    // named.
    let said = (&wrote.workload, &wrote.method);
    assert_eq!(
        (said.0.measured_creates, said.0.updates, said.0.deletes),
        (40, 20, 20)
    );
    assert_eq!(
        (
            said.1.point_reads,
            said.1.reference_reads,
            said.1.cold_reads
        ),
        (0, 0, 0)
    );
    assert_eq!((said.1.checkpoints, said.1.reopens), (0, 0));
    // The second engine's rows carry their ratio to RocksDB's.
    if let Some(second) = wrote.engines.get(1) {
        assert!(second
            .row("wall_per_transition", "create")
            .unwrap()
            .ratio_to_rocksdb
            .is_some());
    }

    // A second call starts from the same kept state and ends on the same
    // grove; with `--keep` its copy stays, and the next call replaces it.
    let again = call("writes", &kept, "again.json", &format!("{writes} --keep"));
    for (again, wrote) in again.engines.iter().zip(&wrote.engines) {
        assert_eq!(again.root_hash_after_load, wrote.root_hash_after_load);
        assert_eq!(again.root_hash_at_end, wrote.root_hash_at_end);
        assert!(kept.join(format!("{}-writes", again.engine)).is_dir());
    }
    let third = call(
        "writes",
        &kept,
        "third.json",
        "--creates 20 --updates 0 --deletes 0",
    );
    for (third, wrote) in third.engines.iter().zip(&wrote.engines) {
        assert_eq!(third.root_hash_after_load, wrote.root_hash_after_load);
        assert_ne!(third.root_hash_at_end, wrote.root_hash_at_end);
        let phases: Vec<&str> = (third.phases.iter())
            .map(|totals| totals.phase.as_str())
            .collect();
        assert_eq!(phases, ["create"]);
        assert!(!kept.join(format!("{}-writes", third.engine)).exists());
    }

    // The churn is the first of the creates as a phase of its own: the
    // same stream, so the same grove at the end. Each phase says the size
    // at its end, and LigninDB's the dead shares at its start and end.
    let churned = call(
        "writes",
        &kept,
        "churned.json",
        "--churn 20 --creates 20 --updates 20 --deletes 20",
    );
    assert_eq!(churned.workload.churn, 20);
    let names = |report: &Report| -> Vec<String> {
        report
            .engines
            .iter()
            .map(|engine| engine.engine.clone())
            .collect()
    };
    assert_eq!(names(&churned), names(&wrote));
    for (churned, wrote) in churned.engines.iter().zip(&wrote.engines) {
        assert_eq!(churned.root_hash_at_end, wrote.root_hash_at_end);
        let phases: Vec<(&str, u64)> = (churned.phases.iter())
            .map(|totals| (totals.phase.as_str(), totals.transitions))
            .collect();
        assert_eq!(
            phases,
            [
                ("churn", 20),
                ("create", 20),
                ("update", 20),
                ("delete", 20)
            ]
        );
        for (phase, _) in phases {
            let size = churned.row("size_on_disk_at_end", phase).unwrap();
            assert!(size.value > 0.0, "{phase}");
            for at in ["start", "end"] {
                let value = |id: &str| {
                    churned
                        .row(&format!("{id}_at_{at}"), phase)
                        .map(|row| row.value)
                };
                match churned.engine.as_str() {
                    "rocksdb" => assert_eq!(value("value_log_dead_share"), None),
                    _ => {
                        for share in ["segment_dead_share", "value_log_dead_share"] {
                            let share = value(share).unwrap();
                            assert!((0.0..=1.0).contains(&share), "{phase} {at}");
                        }
                        assert!(value("segments_size").unwrap() > 0.0, "{phase} {at}");
                        assert!(value("value_log_size").unwrap() > 0.0, "{phase} {at}");
                    }
                }
            }
        }
    }
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

/// Two batches of many operations, small here: the trees under twelve
/// parents moved to other keys in one batch, and a batch that replaces,
/// deletes and puts items. Both engines are charged the same for each and
/// end at the same root hash.
#[cfg(grovedb_engine_seam)]
#[test]
fn large_batches_are_applied_alike_on_both_engines() {
    use lignin_testkit::bench::{
        batches::{large_batches, Sizes},
        BenchError, LigninDbEngine, RocksDbEngine,
    };

    let sizes = Sizes {
        moves: 120,
        parents: 12,
        mixed: 150,
    };
    let scratch = tempfile::tempdir().unwrap();
    let version = GroveVersion::latest();
    let rocksdb =
        large_batches::<RocksDbEngine>(&scratch.path().join("rocksdb"), sizes, version).unwrap();
    let lignindb =
        large_batches::<LigninDbEngine>(&scratch.path().join("lignindb"), sizes, version).unwrap();

    let named: Vec<_> = (rocksdb.iter())
        .map(|batch| (batch.name, batch.operations))
        .collect();
    assert_eq!(named, [("moves", 120), ("mixed", 150)]);
    for (on_rocksdb, on_lignindb) in rocksdb.iter().zip(&lignindb) {
        let name = on_rocksdb.name;
        assert_eq!(on_rocksdb.operations, on_lignindb.operations, "{name}");
        assert_eq!(on_rocksdb.cost, on_lignindb.cost, "{name}");
        assert_eq!(on_rocksdb.root_hash, on_lignindb.root_hash, "{name}");
        assert!(on_rocksdb.cost.seek_count > 0, "{name}");
    }
    assert_eq!(lignindb.len(), 2);
    assert_ne!(rocksdb[0].root_hash, rocksdb[1].root_hash);

    let none = Sizes {
        parents: 0,
        ..sizes
    };
    let refused = large_batches::<RocksDbEngine>(&scratch.path().join("none"), none, version);
    assert!(matches!(refused, Err(BenchError::Check(_))), "{refused:?}");
}

/// The same at the size a caller makes such batches: ten thousand moves
/// under twelve parents and ten thousand mixed operations, five times on
/// each engine in turn. It prints the times of each batch, and holds the
/// two engines to the same charge and the same root for each batch that
/// both report. What else the small test asserts is not asserted here.
#[cfg(grovedb_engine_seam)]
#[test]
#[ignore = "a measurement: it prints, and wants a release build"]
fn batches_of_ten_thousand_operations_on_both_engines() {
    use lignin_testkit::bench::{
        batches::{large_batches, Sizes},
        LigninDbEngine, RocksDbEngine,
    };

    let sizes = Sizes {
        moves: 10_000,
        parents: 12,
        mixed: 10_000,
    };
    let version = GroveVersion::latest();
    let ms = |time: std::time::Duration| time.as_secs_f64() * 1e3;
    for round in 1..=5 {
        let scratch = tempfile::tempdir().unwrap();
        let rocksdb =
            large_batches::<RocksDbEngine>(&scratch.path().join("rocksdb"), sizes, version)
                .unwrap();
        let lignindb =
            large_batches::<LigninDbEngine>(&scratch.path().join("lignindb"), sizes, version)
                .unwrap();
        for (on_rocksdb, on_lignindb) in rocksdb.iter().zip(&lignindb) {
            assert_eq!(on_rocksdb.cost, on_lignindb.cost, "{}", on_rocksdb.name);
            assert_eq!(on_rocksdb.root_hash, on_lignindb.root_hash);
            for (engine, batch) in [("rocksdb", on_rocksdb), ("lignindb", on_lignindb)] {
                println!(
                    "large batch: round {round} {engine:8} {:5} {} operations: apply {:.3} ms, commit {:.3} ms",
                    batch.name,
                    batch.operations,
                    ms(batch.apply),
                    ms(batch.commit)
                );
            }
        }
    }
}

/// A run of LigninDB with settings, its blocks paced: the report names
/// the settings, the details of the background work are rows that add up
/// to the rows they split, and the four gauges are rows of each write
/// phase.
/// A setting in a run that measures no engine which takes one is refused
/// before anything is measured.
#[cfg(grovedb_engine_seam)]
#[test]
fn a_run_of_lignindb_with_settings_splits_its_bytes() {
    let scratch = tempfile::tempdir().unwrap();
    let mut options = options(scratch.path().join("set"));
    for pair in ["pace=block", "deferred=4"] {
        options.settings.engine.set(pair).unwrap();
    }
    let refused = run(&options).unwrap_err().to_string();
    assert_eq!(refused, "--set: no engine of this run takes a setting");
    assert!(!options.settings.scratch.exists(), "nothing was begun");
    options.engines = vec![EngineChoice::LigninDb];
    let report = run(&options).unwrap();
    assert_eq!(report.method.engine_settings, ["pace=block", "deferred=4"]);
    let engine = &report.engines[0];
    let value = |id: &str, phase: &str| {
        engine
            .row(id, phase)
            .unwrap_or_else(|| panic!("no row {id} in {phase}"))
            .value
    };
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    for phase in ["load", "create", "update", "delete"] {
        let split = |rows: &[&str]| rows.iter().map(|id| value(id, phase)).sum::<f64>();
        let logged = split(&[
            "log_group_bytes_per_transition",
            "log_page_step_bytes_per_transition",
            "log_mark_bytes_per_transition",
        ]);
        assert!(
            close(logged, value("bytes_logged_per_transition", phase)),
            "the log in {phase}"
        );
        let flushed = split(&[
            "segment_bytes_per_transition",
            "value_log_bytes_per_transition",
            "regroup_bytes_per_transition",
            "table_checkpoint_bytes_per_transition",
        ]);
        assert!(
            close(flushed, value("flush_bytes_per_transition", phase)),
            "write-back in {phase}"
        );
        let cleaned = split(&[
            "cleaner_block_bytes_per_transition",
            "cleaner_value_bytes_per_transition",
        ]);
        assert!(
            close(cleaned, value("compaction_bytes_per_transition", phase)),
            "the cleaner in {phase}"
        );
        let physical = split(&[
            "bytes_logged_per_transition",
            "flush_bytes_per_transition",
            "compaction_bytes_per_transition",
        ]);
        assert!(close(
            physical,
            value("physical_bytes_per_transition", phase)
        ));
        assert!(value("log_held_mean", phase) <= value("log_held_max", phase));
        assert!(
            value("page_versions_waiting_mean", phase) <= value("page_versions_waiting_max", phase)
        );
    }
    assert!(value("segment_bytes_per_transition", "load") > 0.0);
    // A turn leaves four pages at most. The load changes more pages
    // than that in a block, so after some commit at least four wait.
    assert!(value("page_versions_waiting_max", "load") >= 4.0);
    assert!(value("log_held_max", "load") >= value("log_held_mean", "load"));
    assert!(value("log_held_mean", "load") > 0.0);
    assert!(value("base_blocks_per_transition", "load") > 0.0);
    for row in &engine.rows {
        assert_eq!(row.kind, kind_of(&row.id), "{} in {}", row.id, row.phase);
        assert_eq!(row.ratio_to_rocksdb, None, "RocksDB is not in the run");
    }

    let table = cli::table(&report);
    assert!(
        table.contains("\nsettings: pace=block deferred=4\n"),
        "{table}"
    );
    assert!(table.contains("\nlignindb  load     segment_bytes_per_transition"));
}
