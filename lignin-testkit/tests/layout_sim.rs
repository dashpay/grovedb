//! Acceptance tests for the Merkle host model (TREE-01).
//!
//! `the_reference_workload_meets_the_layout_targets` is the task's "done
//! when". It loads the reference workload, one million documents, so it is
//! ignored by default and wants a release build:
//!
//! ```text
//! cargo test --release -p lignin-testkit --test layout_sim -- --ignored --nocapture the_reference_workload
//! ```
//!
//! The other tests run the same simulation at a small size on every
//! workload, check the observer against what RocksDB stores, and recount
//! every layout from the mirror. Every run, the large ones included, ends
//! with both checks.

use std::thread;

use grovedb::{Element, GroveDb};
use grovedb_version::version::GroveVersion;
use lignin_testkit::{
    layout_sim::{simulate, ChangeSet, Config, Model, Observer, Report, Run},
    workload::{
        adversarial::{DeepNesting, DeleteHeavy, LargeValues, LongKeys, Moves, TinySubtrees},
        apply::apply,
        drive::{Documents, IndexRekey},
        Features, Op, Rng, Workload, Write,
    },
};

const SEED: u64 = 1;

/// Deep nesting recurses once per level inside GroveDB.
const STACK_BYTES: usize = 64 << 20;

fn on_a_big_stack<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    thread::scope(|scope| {
        thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, body)
            .expect("spawn")
            .join()
            .expect("the simulation does not panic")
    })
}

fn open() -> (tempfile::TempDir, GroveDb) {
    let dir = match std::env::var_os("LAYOUT_SIM_DIR") {
        Some(parent) => tempfile::tempdir_in(parent),
        None => tempfile::tempdir(),
    }
    .expect("temp dir");
    let db = GroveDb::open(dir.path()).expect("open");
    (dir, db)
}

fn page_sizes() -> Vec<Config> {
    [4096, 8192, 16_384, 32_768, 65_536]
        .into_iter()
        .map(Config::with_page_size)
        .collect()
}

/// Runs a workload through the simulator with every layout recounted every
/// `check_every` transitions and the mirror compared with what RocksDB
/// stores at the end of every block.
fn run_checked(
    name: &str,
    ops: impl Iterator<Item = Op> + Send,
    configs: &[Config],
    check_every: u64,
) -> Vec<Report> {
    on_a_big_stack(|| {
        let (_dir, db) = open();
        simulate(
            name,
            &db,
            ops,
            GroveVersion::latest(),
            configs,
            Run {
                sample_every: 50,
                check_every,
                verify_mirror: true,
            },
            |_, observer, _| assert_eq!(observer.unfollowed(), 0, "{name}"),
        )
    })
}

/// Applies one write in `transaction` and returns what the observer saw.
fn write(
    db: &GroveDb,
    transaction: &grovedb::Transaction,
    observer: &mut Observer,
    write: Write,
) -> ChangeSet {
    let grove_version = GroveVersion::latest();
    apply(db, &Op::Write(write), Some(transaction), grove_version)
        .unwrap()
        .expect("the write is accepted");
    observer.observe(db, transaction)
}

fn insert(path: &[&[u8]], key: &[u8], element: Element) -> Write {
    Write::Insert {
        path: path.iter().map(|segment| segment.to_vec()).collect(),
        key: key.to_vec(),
        element,
    }
}

#[test]
fn the_mirror_check_finds_a_write_the_observer_did_not_see() {
    let (_dir, db) = open();
    let transaction = db.start_transaction();
    let mut observer = Observer::new(GroveVersion::latest());
    let grove_version = GroveVersion::latest();
    write(
        &db,
        &transaction,
        &mut observer,
        insert(&[], b"tree", Element::empty_tree()),
    );
    for key in [b"b", b"d", b"f"] {
        write(
            &db,
            &transaction,
            &mut observer,
            insert(&[b"tree"], key, Element::new_item(vec![1; 8])),
        );
    }
    observer.verify(&db, &transaction).expect("in step");

    let unseen = |observer: &Observer, write: Write| {
        apply(&db, &Op::Write(write), Some(&transaction), grove_version)
            .unwrap()
            .expect("the write is accepted");
        observer
            .verify(&db, &transaction)
            .expect_err("the mirror is behind")
    };
    // A key the scan meets before any record it knows.
    let problem = unseen(&observer, insert(&[], b"a", Element::new_item(vec![2; 8])));
    assert_eq!(
        problem,
        "the Merk at Some([]), key [61]: storage has it, the mirror does not"
    );
    assert!(!observer.observe(&db, &transaction).is_empty());
    observer.verify(&db, &transaction).expect("in step again");

    // A write below changes every record up to the root.
    let problem = unseen(
        &observer,
        Write::Delete {
            path: vec![b"tree".to_vec()],
            key: b"d".to_vec(),
        },
    );
    assert!(
        problem.ends_with("its record is not the one the mirror read"),
        "{problem}"
    );
    assert!(!observer.observe(&db, &transaction).is_empty());

    // The last record of the root Merk goes: nothing is left to compare.
    for key in [&b"a"[..], b"tree"] {
        let write = Write::Delete {
            path: Vec::new(),
            key: key.to_vec(),
        };
        if key == b"a" {
            apply(&db, &Op::Write(write), Some(&transaction), grove_version)
                .unwrap()
                .expect("the write is accepted");
            observer.observe(&db, &transaction);
        } else {
            let problem = unseen(&observer, write);
            assert_eq!(
                problem,
                "the mirror of the Merk at Some([]) has 1 keys, storage has 0"
            );
        }
    }
}

#[test]
fn an_emptied_merk_is_a_delete_or_a_clear_and_a_deleted_tree_one_drop() {
    let (_dir, db) = open();
    let transaction = db.start_transaction();
    let mut observer = Observer::new(GroveVersion::latest());
    let mut step = |write_op: Write| write(&db, &transaction, &mut observer, write_op);
    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };

    step(insert(&[], b"outer", Element::empty_tree()));
    let created = step(insert(&[b"outer"], b"only", Element::new_item(vec![1])));
    assert_eq!(
        created.events.iter().filter(|event| event.created).count(),
        1
    );

    // The last key of a tree is deleted: one node removed, its page written.
    let emptied = step(Write::Delete {
        path: path(&[b"outer"]),
        key: b"only".to_vec(),
    });
    assert_eq!((emptied.removed.len(), emptied.dropped.len()), (1, 0));
    assert_eq!(emptied.roots_changed.len(), 1);

    // A tree of several keys is cleared: its nodes go as with a drop, and
    // the tree nested in it is one Merk dropped with it.
    for key in [b"a", b"b", b"c"] {
        step(insert(&[b"outer"], key, Element::new_item(vec![2])));
    }
    step(insert(&[b"outer"], b"inner", Element::empty_tree()));
    step(insert(
        &[b"outer", b"inner"],
        b"leaf",
        Element::new_item(vec![3]),
    ));
    let cleared = step(Write::ClearSubtree {
        path: path(&[b"outer"]),
    });
    assert_eq!((cleared.removed.len(), cleared.dropped.len()), (0, 5));
    assert_eq!((cleared.merks_dropped, cleared.subtrees_dropped), (1, 0));

    // A tree with a tree inside is deleted: its host lets go of one subtree.
    step(insert(&[b"outer"], b"inner", Element::empty_tree()));
    step(insert(
        &[b"outer", b"inner"],
        b"leaf",
        Element::new_item(vec![3]),
    ));
    let deleted = step(Write::Delete {
        path: Vec::new(),
        key: b"outer".to_vec(),
    });
    assert_eq!((deleted.removed.len(), deleted.dropped.len()), (1, 2));
    assert_eq!((deleted.merks_dropped, deleted.subtrees_dropped), (2, 1));
    assert_eq!(observer.unfollowed(), 0);
    observer.verify(&db, &transaction).expect("in step");
    assert_eq!(observer.grove().live_nodes(), 0);
}

#[test]
fn a_small_documents_workload_is_laid_out_consistently() {
    let workload = Documents::default();
    let reports = run_checked("documents", workload.stream(SEED), &page_sizes(), 1);
    for report in &reports {
        println!("{report}");
        assert_eq!(report.end.stats.overfull_left, 0);
        assert!(report.end.cells > 0);
    }
}

#[test]
fn every_workload_is_laid_out_consistently() {
    let only = std::env::var("WORKLOAD_ONLY").ok();
    for workload in Workload::suite(Features::for_grove_version(GroveVersion::latest())) {
        let name = workload.name();
        if only.as_deref().is_some_and(|only| only != name) {
            continue;
        }
        let configs = [Config::with_page_size(4096), Config::with_page_size(32_768)];
        let reports = run_checked(name, workload.stream(SEED), &configs, 16);
        for report in &reports {
            println!("{report}");
            assert_eq!(report.end.stats.overfull_left, 0, "{name}");
        }
    }
}

/// The "done when" of TREE-01: on the reference workload the proposed
/// layout has at least 15 nodes per page, is at least 70% full and dirties
/// at most 8 pages per transition.
///
/// A page counts as dirtied by a transition when the transition changes a
/// cell, a link or a hash in it, and a directory entry written counts as a
/// page. The bound is checked for the whole load and again for its last
/// tenth, when the trees are largest.
#[test]
#[ignore = "loads one million documents; run it in release mode"]
fn the_reference_workload_meets_the_layout_targets() {
    let workload = Documents::reference();
    let config = Config::proposed();
    let reports = run_big("documents", workload.stream(SEED), &[config], 100_000);
    let report = &reports[0];
    println!("{report}");
    print_samples(report);

    let stats = &report.end.stats;
    assert_eq!(
        stats.transitions,
        workload.documents + 7,
        "seven setup writes"
    );
    assert_eq!(stats.overfull_left, 0);
    assert!(
        report.nodes_per_page() >= 15.0,
        "{:.1} nodes per page",
        report.nodes_per_page()
    );
    assert!(report.fill() >= 0.70, "fill {:.3}", report.fill());
    assert!(
        report.dirty_per_transition() <= 8.0,
        "{:.2} pages dirtied per transition",
        report.dirty_per_transition()
    );
    let last = report.last_tenth();
    assert!(last.transitions >= workload.documents / 10);
    assert!(
        last.dirty_per_transition() <= 8.0,
        "{:.2} pages dirtied per transition in the last tenth",
        last.dirty_per_transition()
    );
    println!(
        "last tenth: {:.2} pages dirtied, {:.1} Merk records and {:.0} B per transition",
        last.dirty_per_transition(),
        last.records as f64 / last.transitions as f64,
        last.record_bytes as f64 / last.transitions as f64,
    );
}

fn print_samples(report: &Report) {
    let mut before = None;
    for sample in &report.samples {
        let interval = match &before {
            Some(before) => sample.stats.since(before),
            None => sample.stats.clone(),
        };
        let transitions = interval.transitions.max(1) as f64;
        println!(
            "    at {:>8} transitions: {:>8} nodes, {:>7} pages, {:.1} nodes per page, fill \
             {:.1}%; in the interval {:.2} pages dirtied, {:.1} records, {:.3} cells carried",
            sample.stats.transitions,
            sample.cells,
            sample.pages,
            sample.nodes_per_page(),
            sample.fill * 100.0,
            interval.dirty_per_transition(),
            interval.records as f64 / transitions,
            interval.moved().cells as f64 / transitions,
        );
        before = Some(sample.stats.clone());
    }
}

/// The sizes of the workloads a sweep runs, read from the environment so
/// one test serves a quick look and the full reference run.
fn documents_from_env() -> Documents {
    let documents = number_from_env("LAYOUT_SIM_DOCUMENTS").unwrap_or(1_000_000);
    let owners = number_from_env("LAYOUT_SIM_OWNERS").unwrap_or((documents / 100).max(1));
    Documents {
        documents,
        owners,
        ..Documents::reference()
    }
}

fn number_from_env(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
}

fn configs_from_env() -> Vec<Config> {
    let Ok(spec) = std::env::var("LAYOUT_SIM_CONFIGS") else {
        return vec![Config::proposed()];
    };
    // "page:largest fragment %:seed limit %:merge below %:inline:small
    // divisor:colocate:rotation:delete", comma separated; later fields
    // optional.
    spec.split(',')
        .map(|entry| {
            let mut fields = entry.split(':');
            let mut config =
                Config::with_page_size(fields.next().expect("a page size").parse().expect("u32"));
            if let Some(percent) = fields.next() {
                config.largest_fragment_percent = percent.parse().expect("u32");
            }
            if let Some(limit) = fields.next() {
                config.seed_limit_percent = limit.parse().expect("u32");
            }
            if let Some(percent) = fields.next() {
                config.merge_below_percent = percent.parse().expect("u32");
            }
            if let Some(inline) = fields.next() {
                config.inline_max = inline.parse().expect("u32");
            }
            if let Some(divisor) = fields.next() {
                config.small_fragment_divisor = divisor.parse().expect("u32");
            }
            if let Some(colocate) = fields.next() {
                config.colocate = colocate.parse().expect("bool");
            }
            if let Some(carry) = fields.next() {
                config.carry_on_rotation = carry.parse().expect("bool");
            }
            if let Some(carry) = fields.next() {
                config.carry_on_delete = carry.parse().expect("bool");
            }
            config
        })
        .collect()
}

fn run_big(
    name: &str,
    ops: impl Iterator<Item = Op> + Send,
    configs: &[Config],
    sample_every: u64,
) -> Vec<Report> {
    on_a_big_stack(|| {
        let (_dir, db) = open();
        let started = std::time::Instant::now();
        simulate(
            name,
            &db,
            ops,
            GroveVersion::latest(),
            configs,
            Run {
                sample_every,
                check_every: sample_every,
                verify_mirror: false,
            },
            |transitions, observer, models: &[Model]| {
                eprintln!(
                    "{name}: {transitions} transitions, {} nodes, {} Merks, {:.0} s",
                    observer.grove().live_nodes(),
                    observer.grove().live_merks(),
                    started.elapsed().as_secs_f64(),
                );
                for model in models {
                    eprintln!(
                        "    page {:>5}/{}/{}: {:>7} pages, fill {:.1}%, dirtied {:.2}",
                        model.config().page_size,
                        model.config().largest_fragment_percent,
                        model.config().seed_limit_percent,
                        model.pages(),
                        model.fill() * 100.0,
                        model.stats().dirty_per_transition(),
                    );
                }
            },
        )
    })
}

/// A sweep over layouts on the documents workload. The size and the layouts
/// come from `LAYOUT_SIM_DOCUMENTS` and `LAYOUT_SIM_CONFIGS`.
#[test]
#[ignore = "a parameter sweep, sized by the environment"]
fn sweep_documents() {
    let workload = documents_from_env();
    let sample_every = (workload.documents / 10).max(1);
    for report in run_big(
        "documents",
        workload.stream(SEED),
        &configs_from_env(),
        sample_every,
    ) {
        println!("{report}");
        print_samples(&report);
    }
}

/// The adversarial workloads at a size that stresses a layout.
#[test]
#[ignore = "larger adversarial workloads"]
fn sweep_adversarial() {
    let features = Features::for_grove_version(GroveVersion::latest());
    let workloads = [
        Workload::DeepNesting(DeepNesting {
            depth: 127,
            chains: 8,
            leaves: 64,
        }),
        Workload::TinySubtrees(TinySubtrees {
            subtrees: 20_000,
            features,
        }),
        Workload::DeleteHeavy(DeleteHeavy {
            keys: 20_000,
            rounds: 4,
            features,
        }),
        Workload::LongKeys(LongKeys {
            keys: 20_000,
            nesting: 8,
        }),
        Workload::LargeValues(LargeValues {
            max_value_size: 16_384,
            random_values: 4_000,
        }),
        Workload::Moves(Moves {
            rounds: 50,
            features,
        }),
        Workload::IndexRekey(IndexRekey {
            documents: 20_000,
            values: 400,
            twins: 40,
            moves_per_batch: 8,
            documents_after: 2_000,
            features,
        }),
    ];
    let only = std::env::var("WORKLOAD_ONLY").ok();
    for workload in workloads {
        let name = workload.name();
        if only.as_deref().is_some_and(|only| only != name) {
            continue;
        }
        for report in run_big(name, workload.stream(SEED), &configs_from_env(), 2_000) {
            println!("{report}");
            let peak = report
                .samples
                .iter()
                .max_by_key(|sample| sample.cells)
                .expect("a sample");
            println!(
                "  at its largest {} nodes in {} pages: {:.1} nodes per page, fill {:.1}%\n",
                peak.cells,
                peak.pages,
                peak.nodes_per_page(),
                peak.fill * 100.0,
            );
        }
    }
}

/// One tree of `keys` random keys, then `rounds` rounds that each delete a
/// random key and insert a new one: a tree that stays the same size while
/// Merk keeps rotating it.
fn churn(keys: u64, rounds: u64, value_len: u32) -> impl Iterator<Item = Op> + Send {
    let path = vec![b"churn".to_vec()];
    let mut rng = Rng::derive(SEED, 0xC4_02, 0);
    let mut live: Vec<[u8; 32]> = Vec::new();
    let mut ops = vec![Op::Write(Write::Insert {
        path: Vec::new(),
        key: b"churn".to_vec(),
        element: Element::empty_tree(),
    })];
    let insert = |rng: &mut Rng, live: &mut Vec<[u8; 32]>, ops: &mut Vec<Op>| {
        let key = rng.id();
        live.push(key);
        ops.push(Op::Write(Write::Insert {
            path: path.clone(),
            key: key.to_vec(),
            element: Element::new_item(rng.bytes(value_len)),
        }));
    };
    for index in 0..keys {
        insert(&mut rng, &mut live, &mut ops);
        if index % 20 == 19 {
            ops.push(Op::Commit);
        }
    }
    for round in 0..rounds {
        let victim = live.swap_remove(rng.below(live.len() as u64) as usize);
        ops.push(Op::Write(Write::Delete {
            path: path.clone(),
            key: victim.to_vec(),
        }));
        insert(&mut rng, &mut live, &mut ops);
        if round % 10 == 9 {
            ops.push(Op::Commit);
        }
    }
    ops.push(Op::Commit);
    ops.into_iter()
}

/// A tree of constant size under steady deletes and inserts, to see whether
/// a layout wears out when nothing grows.
#[test]
#[ignore = "a long steady-state run"]
fn sweep_churn() {
    let keys = number_from_env("LAYOUT_SIM_KEYS").unwrap_or(50_000);
    let rounds = keys * 4;
    let value_len = 400;
    let sample_every = keys;
    for report in run_big(
        "churn",
        churn(keys, rounds, value_len),
        &configs_from_env(),
        sample_every,
    ) {
        println!("{report}");
        print_samples(&report);
    }
}
