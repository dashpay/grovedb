//! Tests of [`EitherEngine`]: that it answers as the engine it holds, how
//! the engine is chosen at the open, that a directory of the other engine
//! is refused and left as it was, and what the type refuses of the other
//! engine's.

use std::{
    collections::BTreeMap,
    fmt::Debug,
    fs, io,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::{
    store::{NodeChange, NodeChangeSet, NodeStore},
    tree::kv::ValueDefinedCostType,
};
use grovedb_storage::{
    error::WriteScopeError, flat::PendingPrefixDrops, Error as StorageError, FlatStore, RawIterator,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_io::{
    sim::{Fault, FaultRule, OpKind, OpSet, SimConfig, SimVfs},
    Create, DirEntry, OpenMode, StdVfs, SyncItem, Vfs, VfsFile, VfsLock,
};

use super::{
    lignindb::Options, EitherBatch, EitherEngine, EitherWriteGroup, EngineChoice, EngineKind,
    OneOf, WHERE_NONE_IS_SAID,
};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::{Engine, EngineBatch, SubtreeHandle, WriteGroup},
    operations::delete::DeleteOptions,
    query_result_type::QueryResultType,
    reference_path::ReferencePathType,
    BackwardsReferences, Element, Error, GroveDb, LigninEngine, PathQuery, Query, RocksDbEngine,
};

const ROOT: &[&[u8]] = &[];
const TREE: &[&[u8]] = &[b"tree"];
const KINDS: [EngineKind; 2] = [EngineKind::RocksDb, EngineKind::Lignin];
const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;

type Either = GroveDb<EitherEngine>;

fn choice(kind: EngineKind) -> EngineChoice {
    match kind {
        EngineKind::RocksDb => EngineChoice::RocksDb,
        EngineKind::Lignin => EngineChoice::Lignin(Options::default()),
    }
}

fn open(dir: &Path, kind: EngineKind) -> Either {
    let engine = EitherEngine::open_with(dir, &choice(kind)).expect("a database of the choice");
    assert_eq!(engine.kind(), kind);
    GroveDb::from_engine(engine)
}

fn item(n: u8) -> Element {
    Element::new_item(vec![n; 40])
}

/// A database of `kind` at `dir` with a tree and an item in it, closed.
fn filled(dir: &Path, kind: EngineKind, v: &GroveVersion) {
    let db = open(dir, kind);
    (db.insert(ROOT, b"tree", Element::empty_tree(), None, None, v)
        .unwrap())
    .expect("a tree");
    (db.insert(TREE, b"k", item(7), None, None, v).unwrap()).expect("an item");
    db.flush().expect("a flush");
}

fn holds_the_item(db: &Either, v: &GroveVersion) -> bool {
    db.get(TREE, b"k", None, v).unwrap().ok() == Some(item(7))
}

/// What is at a path below a directory.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir,
    /// A file, with its bytes.
    File(Vec<u8>),
    /// A link, with where it leads. It is not followed.
    Link(PathBuf),
}

/// Every entry below `dir` by its path, with what it is.
fn files(dir: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut found = BTreeMap::new();
    let mut left = vec![dir.to_path_buf()];
    while let Some(next) = left.pop() {
        for entry in fs::read_dir(&next).expect("a listing") {
            let path = entry.expect("an entry").path();
            let kind = fs::symlink_metadata(&path).expect("what the entry is");
            let entry = if kind.file_type().is_symlink() {
                Entry::Link(fs::read_link(&path).expect("where a link leads"))
            } else if kind.is_dir() {
                left.push(path.clone());
                Entry::Dir
            } else {
                Entry::File(fs::read(&path).expect("the bytes of a file"))
            };
            found.insert(path, entry);
        }
    }
    found
}

/// The names of the entries of `dir`.
fn names(dir: &Path) -> Vec<String> {
    let name = |entry: std::io::Result<fs::DirEntry>| {
        let name = entry.expect("an entry").file_name();
        name.to_string_lossy().into_owned()
    };
    let mut names: Vec<_> = fs::read_dir(dir).expect("a listing").map(name).collect();
    names.sort();
    names
}

/// What a refusal of the type answered, which is charged nothing.
fn uncharged<T>(answer: CostResult<T, StorageError>) -> Result<T, StorageError> {
    assert_eq!(answer.cost, OperationCost::default());
    answer.unwrap()
}

/// The words of a refusal of the type itself.
fn refusal<T>(refused: Result<T, impl Into<Error>>) -> String {
    match refused.map(drop).map_err(Into::into) {
        Err(Error::NotSupported(words)) => words,
        other => panic!("no refusal in words: {other:?}"),
    }
}

/// The name of a call, what it cost, and what it answered.
type Answer = (&'static str, OperationCost, String);

fn take<T: Debug>(
    answers: &mut Vec<Answer>,
    name: &'static str,
    answer: CostResult<T, Error>,
) -> Option<T> {
    let cost = answer.cost.clone();
    let value = answer.unwrap();
    answers.push((name, cost, format!("{value:?}")));
    value.ok()
}

fn plain<T: Debug>(answers: &mut Vec<Answer>, name: &'static str, answer: Result<T, Error>) {
    answers.push((name, OperationCost::default(), format!("{answer:?}")));
}

/// The calls of [`history`] that are refused on every engine and every
/// grove version.
const REFUSED: [&str; 5] = [
    "get what is absent",
    "get what was rolled back to the savepoint",
    "get what was rolled back",
    "get through a snapshot what was put beside it",
    "insert through a snapshot",
];

/// Writes, reads, transactions, a batch, a move, deletes, a query with its
/// proof, aux records and an append-only tree. Each call is taken in with
/// its answer and its cost, and each call that writes is followed by the
/// root hash as the transaction it wrote through reads it.
fn history<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut answers = Vec::new();
    let a = &mut answers;
    macro_rules! wrote {
        ($name:expr, $call:expr) => {
            wrote!($name, $call, None)
        };
        ($name:expr, $call:expr, $through:expr) => {{
            let written = take(a, $name, $call);
            take(a, "root", db.root_hash($through, v));
            written
        }};
    }
    let sums: &[&[u8]] = &[b"sums"];
    let going: &[&[u8]] = &[b"going"];
    let flat: &[&[u8]] = &[b"flat"];
    let trees: [(&[u8], Element); 5] = [
        (b"tree", Element::empty_tree()),
        (b"sums", Element::empty_sum_tree()),
        (b"going", Element::empty_tree()),
        (b"flat", Element::empty_tree()),
        (b"mmr", Element::empty_mmr_tree()),
    ];
    for (name, element) in trees {
        wrote!(
            "insert a tree",
            db.insert(ROOT, name, element, None, None, v)
        );
    }
    for n in 0..12u8 {
        let key = [b'k', b'a' + n];
        let sum = Element::new_sum_item(i64::from(n) - 3);
        wrote!(
            "insert an item",
            db.insert(TREE, &key, item(n), None, None, v)
        );
        wrote!(
            "insert a sum item",
            db.insert(sums, &key, sum, None, None, v)
        );
        wrote!(
            "insert below",
            db.insert(going, &key, item(n), None, None, v)
        );
        wrote!(
            "insert to drop",
            db.insert(flat, &key, item(n), None, None, v)
        );
    }
    let to = ReferencePathType::AbsolutePathReference(vec![b"tree".to_vec(), b"ka".to_vec()]);
    let reference = Element::new_reference(to);
    wrote!(
        "insert a reference",
        db.insert(TREE, b"ref", reference, None, None, v)
    );

    take(a, "get", db.get(TREE, b"kd", None, v));
    take(a, "get a reference", db.get(TREE, b"ref", None, v));
    take(a, "get what is absent", db.get(TREE, b"none", None, v));
    take(a, "has", db.has_raw(TREE, b"ke", None, v));
    take(a, "get a sum tree", db.get(ROOT, b"sums", None, v));

    let tx = db.start_transaction();
    let through = Some(&tx);
    let put = db.insert(TREE, b"t1", item(101), None, through, v);
    wrote!("insert in a transaction", put, through);
    plain(a, "set a savepoint", db.set_savepoint(&tx));
    let put = db.insert(TREE, b"t2", item(102), None, through, v);
    wrote!("insert after the savepoint", put, through);
    plain(
        a,
        "roll back to the savepoint",
        db.rollback_to_savepoint(&tx),
    );
    take(a, "root after the savepoint", db.root_hash(through, v));
    let got = db.get(TREE, b"t2", through, v);
    take(a, "get what was rolled back to the savepoint", got);
    take(a, "root beside the transaction", db.root_hash(None, v));
    wrote!("commit", db.commit_transaction(tx));
    take(a, "get what was committed", db.get(TREE, b"t1", None, v));

    let tx = db.start_transaction();
    let through = Some(&tx);
    let put = db.insert(TREE, b"t3", item(103), None, through, v);
    wrote!("insert to roll back", put, through);
    plain(a, "roll back", db.rollback_transaction(&tx));
    take(a, "root after the rollback", db.root_hash(through, v));
    take(
        a,
        "get what was rolled back",
        db.get(TREE, b"t3", through, v),
    );
    drop(tx);

    let snapshot = db.start_snapshot_read_transaction();
    let through = Some(&snapshot);
    wrote!(
        "insert beside a snapshot",
        db.insert(TREE, b"s1", item(104), None, None, v)
    );
    take(a, "root through the snapshot", db.root_hash(through, v));
    let got = db.get(TREE, b"s1", through, v);
    take(a, "get through a snapshot what was put beside it", got);
    take(a, "get through a snapshot", db.get(TREE, b"kd", through, v));
    let put = db.insert(TREE, b"s2", item(105), None, through, v);
    wrote!("insert through a snapshot", put, through);
    drop(snapshot);

    let (at_root, in_tree, in_made) = (vec![], vec![b"tree".to_vec()], vec![b"made".to_vec()]);
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(in_tree.clone(), b"kb".to_vec(), item(201)),
        QualifiedGroveDbOp::delete_op(in_tree.clone(), b"kc".to_vec()),
        QualifiedGroveDbOp::insert_or_replace_op(at_root, b"made".to_vec(), Element::empty_tree()),
        QualifiedGroveDbOp::insert_or_replace_op(in_made, b"in".to_vec(), item(202)),
    ];
    wrote!("a batch", db.apply_batch(ops, None, None, v));
    let ops = vec![QualifiedGroveDbOp::move_op(
        vec![],
        b"made".to_vec(),
        b"moved".to_vec(),
    )];
    wrote!("a move of a tree", db.apply_batch(ops, None, None, v));

    wrote!("delete an item", db.delete(TREE, b"kf", None, None, v));
    let with_what_it_holds = Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    });
    let deleted = db.delete(ROOT, b"going", with_what_it_holds, None, v);
    wrote!("delete a tree with what it holds", deleted);
    let dropped = db.drop_flat_subtree(ROOT, b"flat", BackwardsReferences::DontCheck, None, v);
    wrote!("a flat drop", dropped);
    plain(a, "the drain", db.flush_pending_prefix_drops(v));
    take(a, "root after the drain", db.root_hash(None, v));

    let mut all = Query::new();
    all.insert_all();
    let all = PathQuery::new_unsized(in_tree, all);
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    let rows = db.query_raw(&all, true, true, true, pairs, None, v);
    let rows = rows.map_ok(|(found, skipped)| {
        let rows = found.elements.iter().map(ToString::to_string);
        (rows.collect::<Vec<_>>(), skipped)
    });
    take(a, "query", rows);
    if let Some(proof) = take(a, "prove", db.prove_query(&all, None, v)) {
        plain(a, "verify", GroveDb::verify_query(&proof, &all, v));
    }

    wrote!(
        "put an aux record",
        db.put_aux(b"aux", b"value", None, None)
    );
    take(a, "get an aux record", db.get_aux(b"aux", None));
    wrote!("delete an aux record", db.delete_aux(b"aux", None, None));

    for n in 0..5u8 {
        wrote!(
            "append",
            db.mmr_tree_append(ROOT, b"mmr", vec![n; 20], None, v)
        );
    }
    take(
        a,
        "the root of what was appended",
        db.mmr_tree_root_hash(ROOT, b"mmr", None, v),
    );
    take(
        a,
        "an appended value",
        db.mmr_tree_get_value(ROOT, b"mmr", 1, None, v),
    );

    // Every issue, in the order of its path: a map has none of its own.
    let issues = db.verify_grovedb(None, true, true, v).map(|issues| {
        let mut issues: Vec<_> = issues.into_iter().collect();
        issues.sort();
        issues
    });
    plain(a, "verify the grove", issues);
    plain(a, "flush", db.flush());
    take(a, "root after the flush", db.root_hash(None, v));
    answers
}

#[test]
fn every_answer_and_cost_is_that_of_the_engine_it_holds() {
    let latest = GroveVersion::latest().protocol_version;
    for v in GROVE_VERSIONS.iter() {
        for kind in KINDS {
            let (dir_alone, dir_through) = (
                tempfile::tempdir().expect("a directory"),
                tempfile::tempdir().expect("a directory"),
            );
            let alone = match kind {
                EngineKind::RocksDb => {
                    let db = GroveDb::<RocksDbEngine>::open_with_engine(dir_alone.path());
                    history(&db.expect("a database on RocksDB"), v)
                }
                EngineKind::Lignin => {
                    let db = GroveDb::<LigninEngine>::open_with_engine(dir_alone.path());
                    history(&db.expect("a database on LigninDB"), v)
                }
            };
            let through = history(&open(dir_through.path(), kind), v);
            assert_eq!(alone.len(), through.len());
            for (step, (alone, through)) in alone.iter().zip(&through).enumerate() {
                assert_eq!(
                    alone, through,
                    "call {step} holding {kind} on grove version {}",
                    v.protocol_version
                );
            }
            let refused: Vec<_> = (through.iter())
                .filter(|(.., answer)| !answer.starts_with("Ok"))
                .map(|(name, ..)| *name)
                .collect();
            eprintln!(
                "holding {kind} on grove version {}: {} calls, refused {refused:?}",
                v.protocol_version,
                through.len()
            );
            // What the history is to have done, where every call of it is
            // one that the grove version has.
            if v.protocol_version == latest {
                assert_eq!(refused, REFUSED, "holding {kind}");
            }
        }
    }
}

#[test]
fn a_directory_of_the_other_engine_is_refused_and_left_byte_for_byte_as_it_was() {
    let v = GroveVersion::latest();
    for made in KINDS {
        let other = made.other();
        let dir = tempfile::tempdir().expect("a directory");
        filled(dir.path(), made, v);
        let before = files(dir.path());
        let holds_bytes = |entry: &Entry| matches!(entry, Entry::File(bytes) if !bytes.is_empty());
        assert!(before.values().any(holds_bytes));

        // After each refusal every entry is what it was: its path, its
        // kind, and its bytes or where it leads.
        let refused_and_as_before = |call: &str, words: String| {
            assert!(words.contains(&format!("a database of {made}")), "{words}");
            assert!(words.contains(&dir.path().display().to_string()), "{words}");
            assert!(words.contains(&other.to_string()), "{words}");
            assert_eq!(
                files(dir.path()),
                before,
                "a directory of {made} after {call}"
            );
        };
        let opened = EitherEngine::open_with(dir.path(), &choice(other));
        refused_and_as_before("an open", refusal(opened));
        let opened = EitherEngine::open_checkpoint_with(dir.path(), &choice(other));
        refused_and_as_before("an open of a checkpoint", refusal(opened));
        let removed = EitherEngine::delete_checkpoint_of(other, dir.path());
        refused_and_as_before("a removal", refusal(removed));

        // It is still the database it was: by what its entries say, by the
        // right choice, and by an open that names no engine.
        assert_eq!(
            EitherEngine::found_at(dir.path()).expect("a look"),
            Some(made)
        );
        assert_eq!(files(dir.path()), before, "a look at a directory of {made}");
        assert!(holds_the_item(&open(dir.path(), made), v));
        let db = Either::open_with_engine(dir.path()).expect("the database it holds");
        assert_eq!(db.engine().kind(), made);
        assert!(holds_the_item(&db, v));
    }
}

#[test]
fn a_directory_is_told_by_the_names_each_engine_gives_its_entries() {
    let v = GroveVersion::latest();
    let found = |dir: &Path| EitherEngine::found_at(dir).expect("a look");

    // A directory that is not there, an empty one and one with a lock file
    // hold no database.
    let dir = tempfile::tempdir().expect("a directory");
    assert_eq!(found(&dir.path().join("none")), None);
    assert_eq!(found(dir.path()), None);
    fs::write(dir.path().join("LOCK"), b"").expect("a file");
    fs::write(dir.path().join("a marker"), b"of the caller").expect("a file");
    assert_eq!(found(dir.path()), None);

    // Each name alone says its engine. LigninDB refuses a directory by
    // the names that are RocksDB's here.
    for name in [
        "CURRENT",
        "IDENTITY",
        "MANIFEST-000005",
        "OPTIONS-000007",
        "OPTIONS-of a caller",
        "000008.sst",
    ] {
        let dir = tempfile::tempdir().expect("a directory");
        fs::write(dir.path().join(name), b"MANIFEST-000005\n").expect("a file");
        assert_eq!(found(dir.path()), Some(EngineKind::RocksDb), "{name}");
        let by_lignin = LigninEngine::open(dir.path()).expect_err("a directory of RocksDB");
        assert!(
            by_lignin.to_string().contains("RocksDB"),
            "{name}: {by_lignin}"
        );
        assert_eq!(names(dir.path()), [name]);
    }
    for name in [
        "MANIFEST",
        "MANIFEST.deleting",
        "wal",
        "seg",
        "val",
        "table",
    ] {
        let dir = tempfile::tempdir().expect("a directory");
        fs::create_dir(dir.path().join(name)).expect("an entry");
        assert_eq!(found(dir.path()), Some(EngineKind::Lignin), "{name}");
    }

    // A database that each engine makes, written, flushed, with a
    // checkpoint taken of it, has an entry that says so, and none of a
    // name that is the other engine's.
    for kind in KINDS {
        let dir = tempfile::tempdir().expect("a directory");
        let (live, checkpoint) = (dir.path().join("live"), dir.path().join("checkpoint"));
        filled(&live, kind, v);
        open(&live, kind)
            .create_checkpoint(&checkpoint)
            .expect("a checkpoint");
        for dir in [&live, &checkpoint] {
            assert_eq!(found(dir), Some(kind));
            let said: Vec<_> = (names(dir).iter())
                .filter_map(|name| super::engine_of_entry(name.as_ref()))
                .collect();
            assert!(
                !said.is_empty() && said.iter().all(|said| *said == kind),
                "{said:?}"
            );
            // Every directory LigninDB keeps in its database is one the
            // look knows.
            let kept = |name: &String| dir.join(name).is_dir();
            let unknown: Vec<_> = (names(dir).into_iter())
                .filter(|name| kind == EngineKind::Lignin && kept(name))
                .filter(|name| !super::LIGNIN_DIRECTORIES.contains(&name.as_str()))
                .collect();
            assert!(unknown.is_empty(), "{unknown:?}");
        }
    }
}

#[test]
fn a_directory_with_entries_of_both_engines_is_opened_as_neither() {
    let v = GroveVersion::latest();
    for made in KINDS {
        let dir = tempfile::tempdir().expect("a directory");
        filled(dir.path(), made, v);
        let planted = match made {
            EngineKind::RocksDb => "MANIFEST",
            EngineKind::Lignin => "CURRENT",
        };
        fs::write(dir.path().join(planted), b"of the other engine").expect("a file");
        let before = files(dir.path());
        // Each call is refused with two entries named, in the clause of
        // the words between its brackets: the planted one, once, and an
        // entry of the database that says the engine that made it. Every
        // entry is what it was after the call.
        let refused_and_as_before = |words: String| {
            assert!(words.contains("entries of both engines"), "{words}");
            let clause = words.rsplit_once('(').expect("a clause").1;
            let clause = clause.split_once(')').expect("its end").0;
            let (first, second) = clause.split_once(" and ").expect("two entries");
            let named = |part: &str| {
                let (name, of) = part.split_once("` is ").expect("a name and its engine");
                let name = name.strip_prefix('`').expect("a quoted name").to_owned();
                (name, of.strip_suffix("'s").expect("an engine").to_owned())
            };
            let mut pair = [named(first), named(second)];
            pair.sort_by_key(|(name, _)| name != planted);
            let [(of_planted, its_engine), (of_made, engine)] = pair;
            assert_eq!(
                (of_planted.as_str(), its_engine),
                (planted, made.other().to_string())
            );
            assert_ne!(of_made, planted, "{words}");
            assert!(names(dir.path()).contains(&of_made), "{words}");
            assert_eq!(
                super::engine_of_entry(of_made.as_ref()),
                Some(made),
                "{words}"
            );
            assert_eq!(engine, made.to_string(), "{words}");
            assert_eq!(files(dir.path()), before);
        };
        refused_and_as_before(refusal(EitherEngine::found_at(dir.path())));
        refused_and_as_before(refusal(EitherEngine::open(dir.path())));
        refused_and_as_before(refusal(EitherEngine::open_checkpoint(dir.path())));
        refused_and_as_before(refusal(EitherEngine::delete_checkpoint(dir.path())));
        for kind in KINDS {
            let opened = EitherEngine::open_with(dir.path(), &choice(kind));
            refused_and_as_before(refusal(opened));
            let opened = EitherEngine::open_checkpoint_with(dir.path(), &choice(kind));
            refused_and_as_before(refusal(opened));
            let removed = EitherEngine::delete_checkpoint_of(kind, dir.path());
            refused_and_as_before(refusal(removed));
        }
    }
}

/// A simulated file system that has the directory `dir`, with every
/// directory above it.
fn sim_with(dir: &Path, seed: u64) -> SimVfs {
    let sim = SimVfs::new(SimConfig::new(seed));
    let mut made = PathBuf::new();
    for part in dir.components() {
        made.push(part);
        // The root is there, and so is what an earlier turn made.
        let _ = sim.create_dir(&made);
    }
    assert!(sim.read_dir(dir).expect("the directory").is_empty());
    sim
}

#[test]
fn a_path_that_cannot_be_listed_is_an_error_and_no_database_is_made() {
    let dir = tempfile::tempdir().expect("a directory");
    let file = dir.path().join("a file");
    fs::write(&file, b"no directory").expect("a file");
    let before = files(dir.path());
    let unlisted = |refused: Result<(), Error>| match refused {
        Err(Error::StorageError(StorageError::StorageError(words))) => {
            assert!(words.contains("could not be listed"), "{words}")
        }
        other => panic!("{other:?}"),
    };
    unlisted(EitherEngine::found_at(&file).map(drop).map_err(Into::into));
    for kind in KINDS {
        let opened = EitherEngine::open_with(&file, &choice(kind));
        unlisted(opened.map(drop).map_err(Into::into));
        assert_eq!(files(dir.path()), before);
    }

    // A choice of LigninDB lists the directory on the file system of its
    // options: a listing that fails there is the error, and the directory
    // there holds nothing afterwards.
    let grove = dir.path().join("grove");
    let sim = sim_with(&grove, 3);
    let on_sim = EngineChoice::Lignin(Options {
        vfs: Arc::new(sim.clone()),
        ..Options::default()
    });
    let listing = OpSet::of(&[OpKind::ReadDir]);
    sim.add_rule(FaultRule::new(Fault::Eio).on(listing).forever());
    let opened = EitherEngine::open_with(&grove, &on_sim);
    unlisted(opened.map(drop).map_err(Into::into));
    sim.clear_rules();
    let listed = sim.read_dir(&grove).expect("a listing");
    assert!(listed.is_empty());
    assert_eq!(files(dir.path()), before);
}

#[test]
fn a_choice_of_lignindb_looks_at_the_file_system_of_its_options() {
    let v = GroveVersion::latest();
    // The paths are below a directory of the host that this test made,
    // and that holds nothing.
    let host = tempfile::tempdir().expect("a directory");
    let (other, grove) = (host.path().join("other"), host.path().join("grove"));
    let sim = sim_with(&other, 5);
    let on_sim = EngineChoice::Lignin(Options {
        vfs: Arc::new(sim.clone()),
        ..Options::default()
    });
    // A directory there that has an entry of RocksDB's is refused, and has
    // that entry and no other afterwards.
    let made = sim.open(&other.join("CURRENT"), OpenMode::Write(Create::New));
    drop(made.expect("a file"));
    let words = refusal(EitherEngine::open_with(&other, &on_sim));
    assert!(words.contains("a database of RocksDB"), "{words}");
    let listed = sim.read_dir(&other).expect("a listing");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "CURRENT");

    // A directory that is not there gets a database of LigninDB, there
    // and not on the file system of the process.
    let engine = EitherEngine::open_with(&grove, &on_sim).expect("a database");
    assert_eq!(engine.kind(), EngineKind::Lignin);
    let db = GroveDb::from_engine(engine);
    (db.insert(ROOT, b"tree", Element::empty_tree(), None, None, v)
        .unwrap())
    .expect("a tree");
    assert!(sim.read_dir(&grove).is_ok_and(|listed| !listed.is_empty()));
    assert!(files(host.path()).is_empty());
}

#[test]
fn an_open_of_lignindb_is_given_the_options_of_the_choice() {
    let dir = tempfile::tempdir().expect("a directory");
    let none = dir.path().join("none");
    let make_none = EngineChoice::Lignin(Options {
        create_if_missing: false,
        ..Options::default()
    });
    let refused = EitherEngine::open_with(&none, &make_none).expect_err("no database");
    assert!(refused.to_string().contains("lignindb"), "{refused}");
    assert!(!none.exists());

    let (cache_size, reserve) = (48 << 20, 1 << 20);
    let sized = EngineChoice::Lignin(Options {
        cache_size,
        reserve,
        ..Options::default()
    });
    assert_ne!(cache_size, Options::default().cache_size);
    let live = dir.path().join("live");
    let engine = EitherEngine::open_with(&live, &sized).expect("a database");
    let stats = engine
        .lignin()
        .expect("LigninDB is held")
        .database()
        .stats();
    assert_eq!(stats.memory.capacity, cache_size);
    assert_eq!(stats.reserve.len, reserve);
    assert!(engine.rocksdb().is_none());

    // A checkpoint is opened with the options of the choice, and never
    // made: a choice that says to make a database makes none there.
    let checkpoint = dir.path().join("checkpoint");
    engine.create_checkpoint(&checkpoint).expect("a checkpoint");
    drop(engine);
    let opened = EitherEngine::open_checkpoint_with(&checkpoint, &sized).expect("the checkpoint");
    let stats = opened
        .lignin()
        .expect("LigninDB is held")
        .database()
        .stats();
    assert_eq!(stats.memory.capacity, cache_size);
    assert_eq!(stats.reserve.len, reserve);
    for kind in KINDS {
        let made = EitherEngine::open_checkpoint_with(&none, &choice(kind));
        assert!(made.is_err(), "a checkpoint of {kind} where there is none");
        assert!(!none.exists(), "a checkpoint of {kind} where there is none");
    }

    // The sync policy, by the name this module gives it: with a sync at
    // every commit, the file system is asked for one sync or more for each
    // write that is made with no transaction.
    let v = GroveVersion::latest();
    let synced = dir.path().join("synced");
    let sim = sim_with(&synced, 9);
    let every_commit = EngineChoice::Lignin(Options {
        sync: super::SyncPolicy::EveryCommit,
        vfs: Arc::new(sim.clone()),
        ..Options::default()
    });
    let engine = EitherEngine::open_with(&synced, &every_commit).expect("a database");
    let db = GroveDb::from_engine(engine);
    for n in 0..20u8 {
        let before = sim.stats().syncs;
        (db.insert(ROOT, &[n], item(n), None, None, v).unwrap()).expect("an item");
        assert!(sim.stats().syncs > before, "write {n} asked for no sync");
    }
}

#[test]
fn an_open_that_names_no_engine_goes_by_what_the_directory_holds() {
    let v = GroveVersion::latest();
    // RocksDB makes the database where there is none, as for a `GroveDb`
    // that names no engine. The run of the suite that is registered as
    // this type holding LigninDB makes LigninDB's there.
    let expected = match cfg!(grovedb_test_engine = "either-lignin") {
        true => EngineKind::Lignin,
        false => EngineKind::RocksDb,
    };
    assert_eq!(WHERE_NONE_IS_SAID, expected);
    let dir = tempfile::tempdir().expect("a directory");
    let db = Either::open_with_engine(dir.path()).expect("a database");
    assert_eq!(db.engine().kind(), expected);
    assert_eq!(
        EitherEngine::found_at(dir.path()).expect("a look"),
        Some(expected)
    );
    drop(db);

    for kind in KINDS {
        let dir = tempfile::tempdir().expect("a directory");
        filled(dir.path(), kind, v);
        let db = Either::open_with_engine(dir.path()).expect("the database it holds");
        assert_eq!(db.engine().kind(), kind);
        assert_eq!(db.engine().rocksdb().is_some(), kind == EngineKind::RocksDb);
        assert_eq!(db.engine().lignin().is_some(), kind == EngineKind::Lignin);
        assert!(holds_the_item(&db, v));
    }
}

#[test]
fn a_checkpoint_is_made_opened_and_removed_by_the_engine_that_is_held() {
    let v = GroveVersion::latest();
    for kind in KINDS {
        let dir = tempfile::tempdir().expect("a directory");
        let live = dir.path().join("live");
        let checkpoints = [dir.path().join("first"), dir.path().join("second")];
        filled(&live, kind, v);
        let db = open(&live, kind);
        for checkpoint in &checkpoints {
            db.create_checkpoint(checkpoint).expect("a checkpoint");
            assert_eq!(
                EitherEngine::found_at(checkpoint).expect("a look"),
                Some(kind)
            );
            db.create_checkpoint(checkpoint)
                .expect_err("a target that is there");
        }
        // Written after the checkpoints: they do not hold it.
        (db.insert(TREE, b"later", item(9), None, None, v).unwrap()).expect("an item");

        let opened = Either::open_checkpoint_with_engine(&checkpoints[0]).expect("a checkpoint");
        assert_eq!(opened.engine().kind(), kind);
        assert!(holds_the_item(&opened, v));
        assert!(opened.get(TREE, b"later", None, v).unwrap().is_err());
        drop(opened);
        let by_choice = EitherEngine::open_checkpoint_with(&checkpoints[1], &choice(kind));
        let opened = GroveDb::from_engine(by_choice.expect("a checkpoint"));
        assert!(holds_the_item(&opened, v));
        drop(opened);

        Either::delete_checkpoint_with_engine(&checkpoints[0]).expect("a removal");
        EitherEngine::delete_checkpoint_of(kind, &checkpoints[1]).expect("a removal");
        for checkpoint in &checkpoints {
            assert!(!checkpoint.exists());
        }
        assert!(holds_the_item(&db, v));
    }
}

/// The words with which the type refuses a directory that holds no
/// database as a checkpoint.
fn no_checkpoint<T>(refused: Result<T, impl Into<Error>>) {
    match refused.map(drop).map_err(Into::into) {
        Err(Error::StorageError(StorageError::StorageError(words))) => {
            assert!(
                words.contains("has no entry of a database of either engine"),
                "{words}"
            )
        }
        other => panic!("no refusal of the type: {other:?}"),
    }
}

#[test]
fn a_directory_that_holds_no_database_is_no_checkpoint_and_stays_as_it_is() {
    let dir = tempfile::tempdir().expect("a directory");
    let (none, marked) = (dir.path().join("none"), dir.path().join("marked"));
    fs::create_dir(&marked).expect("a directory");
    fs::write(marked.join("a marker"), b"of the caller").expect("a file");
    fs::write(marked.join("LOCK"), b"").expect("a file");
    let before = files(dir.path());
    for path in [&none, &marked] {
        // With no engine named, and with RocksDB named, whose own open of
        // a checkpoint would make a directory, a lock file and a log.
        no_checkpoint(EitherEngine::open_checkpoint(path));
        no_checkpoint(EitherEngine::delete_checkpoint(path));
        no_checkpoint(Either::open_checkpoint_with_engine(path));
        no_checkpoint(Either::delete_checkpoint_with_engine(path));
        no_checkpoint(EitherEngine::open_checkpoint_with(
            path,
            &EngineChoice::RocksDb,
        ));
        no_checkpoint(EitherEngine::delete_checkpoint_of(
            EngineKind::RocksDb,
            path,
        ));
        // With LigninDB named, the refusal is LigninDB's.
        let by_lignin = EitherEngine::open_checkpoint_with(path, &choice(EngineKind::Lignin));
        let words = by_lignin.expect_err("no database").to_string();
        assert!(words.contains("lignindb"), "{words}");
        assert_eq!(files(dir.path()), before);
    }
    assert!(EitherEngine::delete_checkpoint_of(EngineKind::Lignin, &marked).is_err());
    assert_eq!(files(dir.path()), before);

    // What a removal of LigninDB leaves when it is cut short after the
    // last name of the database is gone, an empty directory, is no
    // database of either engine. The engine is named to have it removed.
    let left = dir.path().join("left");
    fs::create_dir(&left).expect("a directory");
    no_checkpoint(EitherEngine::delete_checkpoint(&left));
    no_checkpoint(EitherEngine::delete_checkpoint_of(
        EngineKind::RocksDb,
        &left,
    ));
    assert!(left.exists());
    EitherEngine::delete_checkpoint_of(EngineKind::Lignin, &left).expect("what was left goes");
    assert!(!left.exists());
    assert_eq!(files(dir.path()), before);
}

#[test]
fn rocksdbs_write_scope_is_the_one_answer_the_type_changes() {
    let dir = tempfile::tempdir().expect("a directory");

    // RocksDB itself grants one scope at a time, and rolls none back.
    let alone = RocksDbEngine::open(dir.path().join("alone")).expect("a database");
    let tx = alone.start_transaction();
    assert!(!alone.rolls_back_write_scopes());
    let scope = alone.open_write_scope(&tx).expect("the scope of RocksDB");
    alone.merge_write_scope(scope).expect("a merge");

    // Holding RocksDB the type opens none, and says so in words.
    let engine = EitherEngine::open_with(dir.path().join("held"), &EngineChoice::RocksDb);
    let engine = engine.expect("a database");
    assert!(!engine.rolls_back_write_scopes());
    let tx = engine.start_transaction();
    let words = refusal(engine.open_write_scope(&tx));
    assert!(words.contains("a write scope of RocksDB"), "{words}");
    // The refusal took no scope of the engine inside, which grants its
    // one to a caller that asks it by name.
    let (inside, its_own) = (
        engine.rocksdb().expect("held"),
        tx.rocksdb().expect("its own"),
    );
    let held = inside
        .open_write_scope(its_own)
        .expect("the scope of RocksDB");
    let words = refusal(engine.open_write_scope(&tx));
    assert!(words.contains("a write scope of RocksDB"), "{words}");
    drop(held);
    // A snapshot read transaction is refused as RocksDB refuses it.
    let snapshot = engine.start_snapshot_read_transaction();
    assert!(snapshot.is_snapshot_read() && !tx.is_snapshot_read());
    let of_snapshot = snapshot.rocksdb().expect("its own");
    let by_rocksdb = inside.open_write_scope(of_snapshot).map(drop);
    let by_the_type = engine.open_write_scope(&snapshot).map(drop);
    assert!(matches!(
        by_rocksdb,
        Err(StorageError::SnapshotReadOnlyTransaction("write scope"))
    ));
    assert_eq!(format!("{by_rocksdb:?}"), format!("{by_the_type:?}"));
}

#[test]
fn a_write_scope_of_lignindb_is_lignindbs() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    filled(dir.path(), EngineKind::Lignin, v);
    let db = open(dir.path(), EngineKind::Lignin);
    let engine = db.engine();
    assert!(engine.rolls_back_write_scopes());
    let tx = db.start_transaction();
    let reads = |key: &[u8]| db.get(TREE, key, Some(&tx), v).unwrap().is_ok();
    let put_in = |key: &[u8]| {
        let scope = engine.open_write_scope(&tx).expect("a scope");
        let view = EitherEngine::scope_transaction(&scope);
        assert_eq!(view.kind(), EngineKind::Lignin);
        let put = db.insert(TREE, key, item(1), None, Some(view), v);
        put.unwrap().expect("an item through the scope");
        assert!(db.get(TREE, key, Some(view), v).unwrap().is_ok());
        scope
    };

    engine
        .merge_write_scope(put_in(b"merged"))
        .expect("a merge");
    assert!(reads(b"merged"));
    engine
        .rollback_write_scope(put_in(b"rolled back"))
        .expect("a rollback");
    assert!(!reads(b"rolled back"));
    drop(put_in(b"dropped"));
    assert!(!reads(b"dropped"));

    // While a scope is open the transaction sets no savepoint.
    let scope = engine.open_write_scope(&tx).expect("a scope");
    assert!(format!("{scope:?}").contains("EitherWriteScope"));
    assert!(matches!(
        engine.set_savepoint(&tx),
        Err(StorageError::WriteScope(WriteScopeError::ScopesOpen))
    ));
    engine.merge_write_scope(scope).expect("a merge");
    db.commit_transaction(tx).unwrap().expect("a commit");
    assert!(db.get(TREE, b"merged", None, v).unwrap().is_ok());
    assert!(db.get(TREE, b"dropped", None, v).unwrap().is_err());
}

/// The words of a panic of `call`.
fn panics(call: impl FnOnce()) -> String {
    let panic = catch_unwind(AssertUnwindSafe(call)).expect_err("a panic");
    let words = panic.downcast_ref::<String>().cloned();
    words.unwrap_or_else(|| panic.downcast_ref::<&str>().expect("words").to_string())
}

#[test]
fn what_is_of_the_other_engine_is_refused_in_words() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    for kind in KINDS {
        let (held, other) = (dir.path().join(format!("{kind}")), kind.other());
        filled(&held, kind, v);
        filled(&dir.path().join(format!("beside {other}")), other, v);
        let db = open(&held, kind);
        let beside = open(&dir.path().join(format!("beside {other}")), other);
        let (engine, of_other) = (db.engine(), beside.engine());
        let said = |words: String| {
            let expected = format!("of {other} was handed to an engine that is one of two");
            assert!(words.contains(&expected), "{words}");
            assert!(words.ends_with(&format!("holds {kind}")), "{words}");
            words
        };

        // A transaction of the other engine, where the call can fail. It
        // holds a write, which it still holds after every refusal.
        let tx = of_other.start_transaction();
        let put = beside.insert(TREE, b"theirs", item(3), None, Some(&tx), v);
        put.unwrap().expect("a write through their transaction");
        let refusals = [
            refusal(engine.rollback_transaction(&tx)),
            refusal(engine.set_savepoint(&tx)),
            refusal(engine.rollback_to_savepoint(&tx)),
            refusal(engine.open_write_scope(&tx)),
            refusal(engine.adopt_restored_subtrees(TREE.into(), &tx)),
            refusal(uncharged(engine.apply_batch(EitherBatch::default(), &tx))),
            refusal(uncharged(
                engine.apply_write_group(EitherWriteGroup::default(), &tx),
            )),
            // A commit ends the transaction it is given, also where it is
            // refused.
            refusal(uncharged(
                engine.commit_transaction(of_other.start_transaction()),
            )),
        ];
        for words in refusals {
            assert!(said(words).contains("a transaction"));
        }

        // Where the call opens a handle, which returns no `Result`.
        let batch = EitherBatch::default();
        let prefix = [7; 32];
        let panicked = [
            panics(|| drop(engine.subtree(TREE.into(), None, &tx))),
            panics(|| drop(engine.subtree_by_prefix(prefix, None, &tx))),
            panics(|| drop(engine.subtree_beside(TREE.into(), prefix, Some(&batch), &tx))),
            panics(|| drop(engine.immediate_subtree(TREE.into(), &tx))),
            panics(|| drop(engine.immediate_subtree_by_prefix(prefix, &tx))),
            panics(|| drop(engine.move_subtree(prefix, [8; 32], &batch, &tx))),
        ];
        for words in panicked {
            assert!(said(words).contains("a transaction"));
        }
        // The transaction that was refused everywhere holds its write,
        // and commits it.
        let in_it = beside.get(TREE, b"theirs", Some(&tx), v).unwrap().ok();
        let committed = || beside.get(TREE, b"theirs", None, v).unwrap().ok();
        assert_eq!((in_it, committed()), (Some(item(3)), None));
        of_other.commit_transaction(tx).unwrap().expect("a commit");
        assert_eq!(committed(), Some(item(3)));

        // A write group that the other engine settled a batch into.
        let own = engine.start_transaction();
        let mut group = EitherWriteGroup::default();
        assert_eq!(group.pending_cost(), &OperationCost::default());
        let settled = of_other.settle_batch(&mut group, EitherBatch::default());
        settled.unwrap().expect("a batch is settled");
        assert_eq!(group.pending_cost(), &OperationCost::default());
        let settled = engine.settle_batch(&mut group, EitherBatch::default());
        assert!(said(refusal(uncharged(settled))).contains("a write group"));
        let applied = engine.apply_write_group(group, &own);
        assert!(said(refusal(uncharged(applied))).contains("a write group"));

        // A batch of the other engine: one that a handle of that engine
        // was opened with. This engine settles and applies none, opens no
        // handle and starts no move with it, and the batch that is taken
        // out of it is of the same engine.
        let theirs = of_other.start_transaction();
        let staged = || {
            let batch = EitherBatch::default();
            let handle = of_other
                .subtree(TREE.into(), Some(&batch), &theirs)
                .unwrap();
            handle
                .aux()
                .put(b"staged", b"1", None)
                .unwrap()
                .expect("a put");
            drop(handle);
            batch
        };
        let applied = engine.apply_batch(staged(), &own);
        assert!(said(refusal(uncharged(applied))).contains("a batch"));
        let mut group = EitherWriteGroup::default();
        let settled = engine.settle_batch(&mut group, staged());
        assert!(said(refusal(uncharged(settled))).contains("a batch"));
        let applied = engine.apply_batch(staged().take_pending(), &own);
        assert!(said(refusal(uncharged(applied))).contains("a batch"));
        let of_theirs = staged();
        let panicked = [
            panics(|| drop(engine.subtree(TREE.into(), Some(&of_theirs), &own))),
            panics(|| drop(engine.subtree_by_prefix(prefix, Some(&of_theirs), &own))),
            panics(|| drop(engine.subtree_beside(TREE.into(), prefix, Some(&of_theirs), &own))),
            panics(|| drop(engine.move_subtree(prefix, [8; 32], &of_theirs, &own))),
        ];
        for words in panicked {
            assert!(said(words).contains("a batch"));
        }
        // It is merged into no batch of this engine, and a batch of
        // neither that it is merged into is of its engine afterwards.
        let of_ours = EitherBatch::default();
        drop(engine.subtree(TREE.into(), Some(&of_ours), &own).unwrap());
        let words = panics(|| of_ours.merge_overwriting(staged()));
        let expected = format!("a batch of {other} was merged into a batch of {kind}");
        assert!(words.contains(&expected), "{words}");
        let of_neither = EitherBatch::default();
        of_neither.merge_overwriting(staged());
        let applied = engine.apply_batch(of_neither, &own);
        assert!(said(refusal(uncharged(applied))).contains("a batch"));
        // The engine whose handle staged it applies it, and the write is
        // made.
        let read = || {
            let handle = of_other.subtree(TREE.into(), None, &theirs).unwrap();
            let read = handle.aux().get(b"staged").unwrap().expect("a read");
            drop(handle);
            read
        };
        assert_eq!(read(), None);
        of_other
            .apply_batch(staged(), &theirs)
            .unwrap()
            .expect("its own batch");
        assert_eq!(read(), Some(b"1".to_vec()));
        drop(theirs);

        // A write scope of the other engine, which LigninDB alone opens.
        if other == EngineKind::Lignin {
            let tx = of_other.start_transaction();
            let scope = of_other.open_write_scope(&tx).expect("a scope");
            assert!(said(refusal(engine.merge_write_scope(scope))).contains("a write scope"));
            let scope = of_other.open_write_scope(&tx).expect("a scope");
            assert!(said(refusal(engine.rollback_write_scope(scope))).contains("a write scope"));
        }

        // A change set and a prepared node of a handle of the other.
        let theirs = of_other.start_transaction();
        let (handle, of_theirs) = (
            engine.subtree(TREE.into(), None, &own).unwrap(),
            of_other.subtree(TREE.into(), None, &theirs).unwrap(),
        );
        let node = handle.get_node(b"k", NO_COST, v).unwrap();
        let node = node.expect("a read").expect("the item's node");
        let mut changes = of_theirs.change_set();
        let prepared = changes.prepare_node(&node, NodeChange::ValueWritten);
        let committed = handle.commit(changes);
        assert_eq!(committed.cost, OperationCost::default());
        let words = committed
            .unwrap()
            .expect_err("a change set of the other")
            .to_string();
        assert!(
            words.contains("a change set of the other engine"),
            "{words}"
        );
        let put = handle.change_set().put_node(b"k", prepared, None);
        let words = put.expect_err("a prepared node of the other").to_string();
        assert!(
            words.contains("a prepared node of the other engine"),
            "{words}"
        );
    }
}

/// What a write group is charged and answers on engine `E`: applied
/// with nothing settled into it, to a write transaction and to a
/// snapshot, and with two batches settled into it, with the cost of each
/// settle, the pending cost after it, and the application.
fn group_answers<E: Engine>(engine: &E) -> Vec<(String, OperationCost)> {
    let mut seen = Vec::new();
    let tx = engine.start_transaction();
    let snapshot = engine.start_snapshot_read_transaction();
    for (name, through) in [("a write transaction", &tx), ("a snapshot", &snapshot)] {
        let applied = engine.apply_write_group(E::WriteGroup::default(), through);
        let cost = applied.cost.clone();
        let answer = applied.unwrap().map(drop);
        seen.push((
            format!("nothing settled, applied to {name}: {answer:?}"),
            cost,
        ));
    }
    let mut group = E::WriteGroup::default();
    seen.push(("pending before".to_owned(), group.pending_cost().clone()));
    for n in 0..2u8 {
        let batch = E::Batch::default();
        let handle = engine.subtree(TREE.into(), Some(&batch), &tx).unwrap();
        let staged = handle.aux().put([b'g', n], &[n; 30], None);
        staged.unwrap().expect("a put is staged");
        drop(handle);
        let settled = engine.settle_batch(&mut group, batch);
        let cost = settled.cost.clone();
        seen.push((format!("settle {n}: {:?}", settled.unwrap()), cost));
        seen.push((format!("pending after {n}"), group.pending_cost().clone()));
    }
    let applied = engine.apply_write_group(group, &tx);
    let cost = applied.cost.clone();
    seen.push((format!("applied: {:?}", applied.unwrap().map(drop)), cost));
    // What the two batches staged is in the transaction.
    let handle = engine.subtree(TREE.into(), None, &tx).unwrap();
    for n in 0..2u8 {
        let read = handle.aux().get([b'g', n]);
        let cost = read.cost.clone();
        seen.push((format!("record {n}: {:?}", read.unwrap()), cost));
    }
    drop(handle);
    engine.rollback_transaction(&tx).expect("a rollback");
    seen
}

#[test]
fn a_write_group_is_settled_and_applied_as_by_the_engine_that_is_held() {
    let v = GroveVersion::latest();
    for kind in KINDS {
        let dir = tempfile::tempdir().expect("a directory");
        filled(dir.path(), kind, v);
        let db = open(dir.path(), kind);
        // The same database, through the type and by the engine inside.
        let through = group_answers(db.engine());
        let alone = match kind {
            EngineKind::RocksDb => group_answers(db.engine().rocksdb().expect("held")),
            EngineKind::Lignin => group_answers(db.engine().lignin().expect("held")),
        };
        assert_eq!(through, alone, "holding {kind}");
        // A settle or the application has a cost.
        let none = OperationCost::default();
        let charged = |(call, cost): &(String, OperationCost)| {
            (call.starts_with("settle") || call.starts_with("applied")) && *cost != none
        };
        assert!(through.iter().any(charged), "{through:?}");
        assert_eq!(through[2], ("pending before".to_owned(), none));
        let answered = |call: &str| through.iter().any(|(answer, _)| answer == call);
        assert!(answered("applied: Ok(())"), "{through:?}");
        for n in 0..2u8 {
            let record = format!("record {n}: Ok(Some({:?}))", vec![n; 30]);
            assert!(answered(&record), "{through:?}");
        }
    }
}

/// The names under which LigninDB builds a checkpoint, as a caller can
/// spell them.
const RESERVED: [&str; 2] = ["checkpoint.creating", "checkpoint.CREATING"];

/// Asserts that a refusal is the type's of a name ending in `.creating`.
fn reserved<T>(refused: Result<T, impl Into<Error>>) {
    let words = refusal(refused);
    assert!(words.contains("ends in `.creating`"), "{words}");
}

/// A database that RocksDB's own engine makes at `dir`, with a tree and an
/// item in it, closed.
fn filled_by_rocksdb_itself(dir: &Path, v: &GroveVersion) {
    let db = GroveDb::from_engine(RocksDbEngine::open(dir).expect("a database of RocksDB"));
    (db.insert(ROOT, b"tree", Element::empty_tree(), None, None, v)
        .unwrap())
    .expect("a tree");
    (db.insert(TREE, b"k", item(7), None, None, v).unwrap()).expect("an item");
    db.flush().expect("a flush");
}

#[test]
fn rocksdb_opens_for_the_caller_and_checkpoint_creation_refuse_reserved_names() {
    let v = GroveVersion::latest();

    // No database of RocksDB is made at such a path, in any case of the
    // letters, by the choice or by an open that names no engine, where no
    // entry says an engine: where there is no directory, in one that has
    // no entry, as one that LigninDB has just made to build in, in one
    // that has a lock file, which either engine keeps, and in one that has
    // a log beside it, which is what RocksDB's open of a checkpoint leaves
    // where it finds no database.
    for name in RESERVED {
        // A directory for each spelling: a file system can take the two
        // for one name.
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join(name);
        for make in [
            |_: &Path| {},
            |path: &Path| fs::create_dir(path).expect("a directory"),
            |path: &Path| fs::write(path.join("LOCK"), b"").expect("a file"),
            |path: &Path| fs::write(path.join("LOG"), b"of RocksDB").expect("a file"),
        ] {
            make(&path);
            let before = path.exists().then(|| files(&path));
            reserved(EitherEngine::open_with(&path, &EngineChoice::RocksDb));
            reserved(EitherEngine::open_checkpoint_with(
                &path,
                &EngineChoice::RocksDb,
            ));
            let unnamed = EitherEngine::open(&path);
            match WHERE_NONE_IS_SAID {
                EngineKind::RocksDb => reserved(unnamed),
                // LigninDB refuses the name itself, and the error is its
                // own.
                EngineKind::Lignin => {
                    let by_the_type = unnamed.err().expect("a name of LigninDB's");
                    let by_lignin = LigninEngine::open(&path).err().expect("the same name");
                    assert!(matches!(by_the_type, StorageError::StorageError(_)));
                    assert_eq!(by_the_type.to_string(), by_lignin.to_string());
                }
            }
            no_checkpoint(EitherEngine::open_checkpoint(&path));
            assert_eq!(path.exists().then(|| files(&path)), before);
        }

        // Where the path cannot be listed, and where the listing has
        // entries of both engines, the error is the listing's, as under
        // any other name, and no engine is called.
        type Opened = Result<EitherEngine, StorageError>;
        let opens: [fn(&Path) -> Opened; 4] = [
            |path| EitherEngine::open_with(path, &EngineChoice::RocksDb),
            |path| EitherEngine::open_checkpoint_with(path, &EngineChoice::RocksDb),
            |path| EitherEngine::open(path),
            |path| EitherEngine::open_checkpoint(path),
        ];
        fs::remove_dir_all(&path).expect("a removal");
        fs::write(&path, b"no directory").expect("a file");
        for open in opens {
            match open(&path) {
                Err(StorageError::StorageError(words)) => {
                    assert!(words.contains("could not be listed"), "{words}")
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(fs::read(&path).expect("the file"), b"no directory");
        fs::remove_file(&path).expect("a removal");
        filled_by_rocksdb_itself(&path, v);
        fs::create_dir(path.join("wal")).expect("an entry of LigninDB's name");
        let before = files(&path);
        for open in opens {
            let words = refusal(open(&path));
            assert!(words.contains("entries of both engines"), "{words}");
        }
        assert_eq!(files(&path), before);
    }

    // Nor a checkpoint of a database on RocksDB, whatever is at the target:
    // nothing, a directory with no entry, or a database of RocksDB, which
    // is as it was.
    let dir = tempfile::tempdir().expect("a directory");
    let live = dir.path().join("live");
    filled(&live, EngineKind::RocksDb, v);
    let db = open(&live, EngineKind::RocksDb);
    let removals: [fn(&Path) -> Result<(), Error>; 2] = [
        |path| EitherEngine::delete_checkpoint(path),
        |path| EitherEngine::delete_checkpoint_of(EngineKind::RocksDb, path),
    ];
    for (name, remove) in RESERVED.into_iter().zip(removals) {
        let target = dir.path().join(name);
        reserved(db.create_checkpoint(&target));
        assert!(!target.exists());
        fs::create_dir(&target).expect("a directory");
        reserved(db.create_checkpoint(&target));
        assert!(files(&target).is_empty());
        filled_by_rocksdb_itself(&target, v);
        let before = files(&target);
        reserved(db.create_checkpoint(&target));
        assert_eq!(files(&target), before);

        // That database is RocksDB's by its entries, so the name keeps no
        // open of the type from it, and it is removed as a database of
        // RocksDB, by a removal that names no engine or by one that names
        // RocksDB.
        let opened = EitherEngine::open_with(&target, &EngineChoice::RocksDb);
        assert!(holds_the_item(
            &GroveDb::from_engine(opened.expect("a database of RocksDB")),
            v
        ));
        remove(&target).expect("a removal by RocksDB");
        assert!(!target.exists());
    }
}

/// The names of the entries of `dir` with each run of digits as one `#`:
/// the numbers RocksDB gives its files, and the time in the name of a log
/// it has put aside.
fn shapes(dir: &Path) -> Vec<String> {
    let shape = |name: String| {
        let mut shape = String::new();
        for letter in name.chars() {
            match letter.is_ascii_digit() {
                true if shape.ends_with('#') => {}
                true => shape.push('#'),
                false => shape.push(letter),
            }
        }
        shape
    };
    let mut shapes: Vec<_> = names(dir).into_iter().map(shape).collect();
    shapes.sort();
    shapes
}

type Opened = Result<EitherEngine, StorageError>;
type OpenedByRocksDb = Result<RocksDbEngine, StorageError>;

/// Each open of the type that can go to RocksDB, by its name, with
/// RocksDB's own, and whether that one makes a database where there is
/// none.
const OPENS: [(
    &str,
    fn(&Path) -> Opened,
    fn(&Path) -> OpenedByRocksDb,
    bool,
); 4] = [
    (
        "open_with",
        |path| EitherEngine::open_with(path, &EngineChoice::RocksDb),
        |path| RocksDbEngine::open(path),
        true,
    ),
    (
        "open",
        |path| EitherEngine::open(path),
        |path| RocksDbEngine::open(path),
        true,
    ),
    (
        "open_checkpoint",
        |path| EitherEngine::open_checkpoint(path),
        |path| RocksDbEngine::open_checkpoint(path),
        false,
    ),
    (
        "open_checkpoint_with",
        |path| EitherEngine::open_checkpoint_with(path, &EngineChoice::RocksDb),
        |path| RocksDbEngine::open_checkpoint(path),
        false,
    ),
];

#[test]
fn a_database_of_rocksdb_under_a_reserved_name_is_opened_as_under_any_other() {
    let v = GroveVersion::latest();
    for name in RESERVED {
        // The same database twice, made by RocksDB's own engine: under the
        // reserved name, and under a name that is not. Each open of the
        // type under the reserved name is held to RocksDB's own of the
        // other.
        let dir = tempfile::tempdir().expect("a directory");
        let (path, twin) = (dir.path().join(name), dir.path().join("checkpoint"));
        for made in [&path, &twin] {
            filled_by_rocksdb_itself(made, v);
        }
        assert_eq!(shapes(&path), shapes(&twin));

        for (n, (call, of_the_type, of_rocksdb, _)) in (1..).zip(OPENS) {
            let engine = of_the_type(&path).unwrap_or_else(|refused| panic!("{call}: {refused}"));
            assert_eq!(engine.kind(), EngineKind::RocksDb, "{call}");
            let db = GroveDb::from_engine(engine);
            let beside = GroveDb::from_engine(of_rocksdb(&twin).expect("RocksDB's own open"));
            // It reads what was written before it, by RocksDB's own engine
            // and through each earlier open, and is written through.
            assert!(holds_the_item(&db, v), "{call}");
            for earlier in 1..n {
                let read = db.get(TREE, &[earlier], None, v).unwrap();
                assert_eq!(read.ok(), Some(item(earlier)), "{call}");
            }
            (db.insert(TREE, &[n], item(n), None, None, v).unwrap()).expect("an item");
            (beside.insert(TREE, &[n], item(n), None, None, v).unwrap()).expect("an item");
            let root = db.root_hash(None, v).unwrap().expect("a root hash");
            let beside_root = beside.root_hash(None, v).unwrap().expect("a root hash");
            assert_eq!(root, beside_root, "{call}");
            drop((db, beside));

            // The directory holds what RocksDB's own open leaves.
            assert_eq!(shapes(&path), shapes(&twin), "{call}");
            let found = EitherEngine::found_at(&path).expect("a look");
            assert_eq!(found, Some(EngineKind::RocksDb), "{call}");
        }
    }
}

#[test]
fn an_entry_of_rocksdbs_name_alone_under_a_reserved_name_is_rocksdbs_to_answer() {
    // The listing goes by names: one entry of a name of RocksDB's, in a
    // directory that holds no database, says RocksDB. Each open is then
    // RocksDB's own of the same directory under another name: one of a
    // database makes the database, and one of a checkpoint fails, makes
    // none, and leaves what RocksDB's leaves. Where the entry is a
    // `CURRENT` that names no manifest, every open fails. An error is
    // RocksDB's own, word for word, and none of the type's.
    let entries = [
        ("OPTIONS-of a caller", "", true),
        ("single.sst", "", true),
        ("CURRENT", "no manifest", false),
    ];
    let cases = RESERVED
        .into_iter()
        .flat_map(|name| entries.map(|entry| (name, entry)));
    for (name, (entry, bytes, opens)) in cases {
        for (call, of_the_type, of_rocksdb, makes) in OPENS {
            let dir = tempfile::tempdir().expect("a directory");
            let (path, twin) = (dir.path().join(name), dir.path().join("checkpoint"));
            for made in [&path, &twin] {
                fs::create_dir(made).expect("a directory");
                fs::write(made.join(entry), bytes).expect("a file");
            }
            let found = EitherEngine::found_at(&path).expect("a look");
            assert_eq!(found, Some(EngineKind::RocksDb), "{call} {entry}");

            let opened = of_the_type(&path);
            let by_rocksdb = of_rocksdb(&twin);
            assert_eq!(by_rocksdb.is_ok(), opens && makes, "{call} {entry}");
            // The words of an error, but for the directory they name.
            let words = |error: &StorageError, dir: &Path| {
                assert!(matches!(error, StorageError::RocksDBError(_)), "{error:?}");
                error.to_string().replace(&dir.display().to_string(), "")
            };
            match (&opened, &by_rocksdb) {
                (Ok(engine), Ok(_)) => assert_eq!(engine.kind(), EngineKind::RocksDb, "{call}"),
                (Err(refused), Err(by_rocksdb)) => {
                    assert_eq!(words(refused, &path), words(by_rocksdb, &twin), "{call}")
                }
                _ => panic!("{call} {entry}: {opened:?}"),
            }
            drop((opened, by_rocksdb));
            if opens {
                assert_eq!(path.join("CURRENT").exists(), makes, "{call} {entry}");
            }
            assert_eq!(shapes(&path), shapes(&twin), "{call} {entry}");
        }
    }
}

/// Whether no open that can go to RocksDB opens `path`, the two that name
/// RocksDB refuse it in the type's words, for its name where no entry
/// says an engine and as LigninDB's where one says LigninDB, each of the
/// two that name no engine answers what it is to answer there, and every
/// entry below it is afterwards what it was.
///
/// An open that names no engine and is handed to LigninDB answers
/// LigninDB's own error, in variant and words: the one of a database where
/// an entry says LigninDB, or where none says an engine and such a path
/// goes to LigninDB, and the one of a checkpoint where an entry says
/// LigninDB. Elsewhere the answer is the type's: of the name for a
/// database, and that the directory is no checkpoint.
fn kept_from_rocksdb(path: &Path) -> bool {
    let before = files(path);
    let found = match EitherEngine::found_at(path) {
        Ok(found @ (None | Some(EngineKind::Lignin))) => found,
        _ => return false,
    };
    let why = match found {
        None => "ends in `.creating`",
        Some(_) => "a database of LigninDB",
    };
    let in_words = |opened: Opened| match opened {
        Err(StorageError::NotSupported(words)) => words.contains(why),
        _ => false,
    };
    let as_lignindb = |opened: Opened, by_lignindb: Result<LigninEngine, StorageError>| match (
        opened,
        by_lignindb,
    ) {
        (Err(of_the_type), Err(of_lignindb)) => {
            std::mem::discriminant(&of_the_type) == std::mem::discriminant(&of_lignindb)
                && of_the_type.to_string() == of_lignindb.to_string()
        }
        _ => false,
    };
    let unnamed = match found.unwrap_or(WHERE_NONE_IS_SAID) {
        EngineKind::Lignin => as_lignindb(EitherEngine::open(path), LigninEngine::open(path)),
        EngineKind::RocksDb => in_words(EitherEngine::open(path)),
    };
    let of_a_checkpoint = match found {
        Some(_) => as_lignindb(
            EitherEngine::open_checkpoint(path),
            LigninEngine::open_checkpoint(path),
        ),
        None => matches!(
            EitherEngine::open_checkpoint(path),
            Err(StorageError::StorageError(words))
                if words.contains("no entry of a database of either engine")
        ),
    };
    in_words(EitherEngine::open_with(path, &EngineChoice::RocksDb))
        && in_words(EitherEngine::open_checkpoint_with(
            path,
            &EngineChoice::RocksDb,
        ))
        && unnamed
        && of_a_checkpoint
        && files(path) == before
}

/// What was seen of the directory a checkpoint is built in, at one moment
/// of its creation: the engine its entries say, their names, and whether
/// it was kept from RocksDB.
type Seen = (Option<EngineKind>, Vec<String>, bool);

/// The file system of the process, on which a call that makes, opens,
/// links or locks an entry in one directory, gives an entry a name in it,
/// or removes a file in it, is followed by a look at that directory. What
/// is written through an open file is followed by none, and neither is
/// the removal of a directory or a rename to a name outside it.
struct Watched {
    dir: PathBuf,
    seen: Mutex<Vec<Seen>>,
}

impl Watched {
    fn look(&self) {
        let said = EitherEngine::found_at(&self.dir).expect("a look");
        let seen = (said, names(&self.dir), kept_from_rocksdb(&self.dir));
        self.seen.lock().expect("what was seen").push(seen);
    }

    /// Looks after a call that succeeded on a path in the directory.
    fn after<T>(&self, path: &Path, done: io::Result<T>) -> io::Result<T> {
        if done.is_ok() && path.starts_with(&self.dir) {
            self.look();
        }
        done
    }
}

impl Debug for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watched").finish_non_exhaustive()
    }
}

impl Vfs for Watched {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.after(path, StdVfs.open(path, mode))
    }

    /// Looks before the directory is given another name, too: it is then
    /// as whole as it gets under this one.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if from == self.dir {
            self.look();
        }
        self.after(to, StdVfs.rename(from, to))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.after(link, StdVfs.hard_link(original, link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.after(path, StdVfs.remove_file(path))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.after(path, StdVfs.create_dir(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.sync_dir(path)
    }

    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        StdVfs.sync_together(items)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.after(path, StdVfs.lock(path))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.after(path, StdVfs.lock_existing(path))
    }
}

#[test]
fn what_lignindb_builds_or_holds_under_a_reserved_name_is_kept_from_rocksdb() {
    let v = GroveVersion::latest();
    for name in RESERVED {
        // A database of LigninDB under such a name, which LigninDB makes
        // under no such name: one that was given it.
        let dir = tempfile::tempdir().expect("a directory");
        let (made, path) = (dir.path().join("made"), dir.path().join(name));
        filled(&made, EngineKind::Lignin, v);
        fs::rename(&made, &path).expect("a rename");
        let found = EitherEngine::found_at(&path).expect("a look");
        assert_eq!(found, Some(EngineKind::Lignin));
        assert!(kept_from_rocksdb(&path));
        // It is the database it was.
        fs::rename(&path, &made).expect("a rename");
        assert!(holds_the_item(&open(&made, EngineKind::Lignin), v));
    }

    // A directory in which LigninDB is building a checkpoint, looked at
    // after each call of the file layer that makes, opens, links or locks
    // an entry of it, gives an entry a name in it or removes a file in it,
    // and before it is given the name of its target.
    let dir = tempfile::tempdir().expect("a directory");
    let (live, target) = (dir.path().join("live"), dir.path().join("checkpoint"));
    let watched = Arc::new(Watched {
        dir: dir.path().join(RESERVED[0]),
        seen: Mutex::default(),
    });
    filled(&live, EngineKind::Lignin, v);
    let on_watched = EngineChoice::Lignin(Options {
        vfs: watched.clone(),
        ..Options::default()
    });
    let db = GroveDb::from_engine(EitherEngine::open_with(&live, &on_watched).expect("a database"));
    assert!(watched.seen.lock().expect("what was seen").is_empty());
    db.create_checkpoint(&target).expect("a checkpoint");

    let seen = std::mem::take(&mut *watched.seen.lock().expect("what was seen"));
    for (said, names, kept) in &seen {
        assert!(kept, "{said:?} {names:?}");
    }
    // It was seen with no entry, with its lock file alone, and with
    // entries that say LigninDB, its manifest among them at the end.
    let was = |said: Option<EngineKind>, held: &[&str]| {
        (seen.iter()).any(|(seen, names, _)| *seen == said && names.iter().eq(held))
    };
    assert!(was(None, &[]), "{seen:?}");
    assert!(was(None, &["LOCK"]), "{seen:?}");
    let (said, names, _) = seen.last().expect("a last look");
    assert_eq!(*said, Some(EngineKind::Lignin));
    assert!(names.iter().any(|name| name == "MANIFEST"), "{names:?}");

    // The creation was not disturbed: the checkpoint is whole.
    assert!(!watched.dir.exists());
    let opened = Either::open_checkpoint_with_engine(&target).expect("a checkpoint");
    assert_eq!(opened.engine().kind(), EngineKind::Lignin);
    assert!(holds_the_item(&opened, v));
    drop(opened);

    // The limit of the rule. A creation takes a directory that is there
    // under the name for what an earlier one left, and locks it before it
    // empties it. One that holds an entry of a name of RocksDB's still
    // says RocksDB then: the type would hand it to RocksDB, and it is not
    // opened here. Once the entry is gone, it is kept from RocksDB as
    // above.
    Either::delete_checkpoint_with_engine(&target).expect("a removal");
    fs::create_dir(&watched.dir).expect("a directory");
    fs::write(watched.dir.join("OPTIONS-of a caller"), b"").expect("a file");
    db.create_checkpoint(&target).expect("a checkpoint");
    let seen = std::mem::take(&mut *watched.seen.lock().expect("what was seen"));
    let emptied = (seen.iter())
        .position(|(said, ..)| *said != Some(EngineKind::RocksDb))
        .expect("a look after the entry went");
    assert!(emptied > 0, "{seen:?}");
    for (said, names, kept) in &seen[..emptied] {
        assert_eq!(names[..], ["LOCK", "OPTIONS-of a caller"], "{said:?}");
        assert!(!kept);
    }
    for (said, names, kept) in &seen[emptied..] {
        assert!(kept, "{said:?} {names:?}");
    }
    assert!(!watched.dir.exists());
    let opened = Either::open_checkpoint_with_engine(&target).expect("a checkpoint");
    assert!(holds_the_item(&opened, v));
}

/// The keys a cursor shows from its first record on.
fn walked(cursor: &mut impl RawIterator) -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        keys.push(cursor.key().unwrap().expect("a key").to_vec());
        assert!(cursor.value().unwrap().is_some());
        cursor.next().unwrap();
    }
    cursor.status().expect("a cursor at its end");
    keys
}

#[test]
fn a_handle_a_cursor_and_a_batch_are_those_of_the_engine_that_is_held() {
    let v = GroveVersion::latest();
    for kind in KINDS {
        let dir = tempfile::tempdir().expect("a directory");
        filled(dir.path(), kind, v);
        let db = open(dir.path(), kind);
        for key in [b"a", b"b", b"c"] {
            (db.insert(TREE, key, item(key[0]), None, None, v).unwrap()).expect("an item");
        }
        let engine = db.engine();
        let tx = engine.start_transaction();
        let keys: [&[u8]; 4] = [b"a", b"b", b"c", b"k"];

        // A deferred handle with no batch reads, as a node store.
        let handle = engine.subtree(TREE.into(), None, &tx).unwrap();
        assert_eq!(handle.kind(), kind);
        assert!(handle.has_node(b"b").unwrap().expect("a look"));
        assert!(!handle.has_node(b"z").unwrap().expect("a look"));
        assert!(handle
            .get_node_for_read(b"b")
            .unwrap()
            .expect("a read")
            .is_some());
        assert_eq!(
            handle.takes_links_to_absent_nodes(),
            kind == EngineKind::RocksDb
        );
        assert_eq!(
            handle.get_aux_record(b"none").unwrap().expect("a read"),
            None
        );
        assert_eq!(
            handle.get_meta_record(b"none").unwrap().expect("a read"),
            None
        );
        let mut cursor = NodeStore::cursor(&handle);
        assert_eq!(walked(&mut cursor), keys);
        cursor.seek_to_last().unwrap();
        assert_eq!(cursor.key().unwrap(), Some(keys[3]));
        cursor.prev().unwrap();
        assert_eq!(cursor.key().unwrap(), Some(keys[2]));
        cursor.seek(b"b").unwrap();
        assert_eq!(cursor.key().unwrap(), Some(keys[1]));
        cursor.seek_for_prev(b"bb").unwrap();
        assert_eq!(cursor.key().unwrap(), Some(keys[1]));

        // The cursor is pointed at another subtree and back.
        let prefix = |path: &[&[u8]]| grovedb_storage::prefix::build_prefix(path.into()).unwrap();
        let mut cursor = EitherEngine::retarget_cursor(cursor, prefix(ROOT));
        assert_eq!(walked(&mut cursor), [b"tree".to_vec()]);
        let mut cursor = EitherEngine::retarget_cursor(cursor, prefix(TREE));
        assert_eq!(walked(&mut cursor), keys);
        let by_prefix = engine.subtree_by_prefix(prefix(TREE), None, &tx).unwrap();
        assert_eq!(walked(&mut NodeStore::cursor(&by_prefix)), keys);
        drop((cursor, handle, by_prefix));

        // An immediate handle writes its aux and meta keyspaces at once,
        // and a flat cursor walks them.
        let immediate = engine.immediate_subtree(TREE.into(), &tx).unwrap();
        for (keyspace, name) in [(0, b"aux"), (1, b"met")] {
            let (aux, meta) = (immediate.aux(), immediate.meta());
            let records: &dyn Fn(&[u8]) -> Option<Vec<u8>> = match keyspace {
                0 => &|key| aux.get(key).unwrap().expect("a read"),
                _ => &|key| meta.get(key).unwrap().expect("a read"),
            };
            for n in 0..3u8 {
                let key = [name[0], n];
                match keyspace {
                    0 => aux.put(key, name, None).unwrap().expect("a put"),
                    _ => meta.put(key, name, None).unwrap().expect("a put"),
                }
                assert_eq!(records(&key), Some(name.to_vec()));
            }
            let walk = match keyspace {
                0 => walked(&mut aux.cursor()),
                _ => walked(&mut meta.cursor()),
            };
            assert_eq!(walk.len(), 3);
            match keyspace {
                0 => aux.delete([name[0], 0], None).unwrap().expect("a delete"),
                _ => meta.delete([name[0], 0], None).unwrap().expect("a delete"),
            }
            assert_eq!(records(&[name[0], 0]), None);
            match keyspace {
                0 => aux.clear().unwrap().expect("a clear"),
                _ => meta.clear().unwrap().expect("a clear"),
            }
            assert_eq!(records(&[name[0], 1]), None);
        }
        immediate
            .put_meta_record(b"m", b"1")
            .unwrap()
            .expect("a put");
        assert_eq!(
            immediate.get_meta_record(b"m").unwrap().expect("a read"),
            Some(b"1".to_vec())
        );
        immediate
            .delete_meta_record(b"m")
            .unwrap()
            .expect("a delete");
        assert_eq!(
            immediate.get_meta_record(b"m").unwrap().expect("a read"),
            None
        );
        let by_prefix = engine
            .immediate_subtree_by_prefix(prefix(TREE), &tx)
            .unwrap();
        assert_eq!(by_prefix.kind(), kind);
        drop((immediate, by_prefix));

        // A batch holds what a deferred handle staged until it is applied,
        // and gives it to another batch.
        let batch = EitherBatch::default();
        let deferred = engine.subtree(TREE.into(), Some(&batch), &tx).unwrap();
        deferred
            .aux()
            .put(b"staged", b"1", None)
            .unwrap()
            .expect("a put");
        assert_eq!(
            deferred.aux().get(b"staged").unwrap().expect("a read"),
            None
        );
        drop(deferred);
        batch.next_operation();
        let taken = batch.take_pending();
        let aux_of = |key: &[u8]| {
            let handle = engine.subtree(TREE.into(), None, &tx).unwrap();
            let read = handle.aux().get(key).unwrap().expect("a read");
            drop(handle);
            read
        };
        engine
            .apply_batch(batch, &tx)
            .unwrap()
            .expect("an empty batch");
        assert_eq!(aux_of(b"staged"), None);
        let into = EitherBatch::default();
        into.merge_overwriting(taken);
        let mut group = EitherWriteGroup::default();
        (engine.settle_batch(&mut group, into).unwrap()).expect("a batch is settled");
        let pending = group.pending_cost().clone();
        let applied = engine.apply_write_group(group, &tx);
        assert_eq!(applied.cost, pending);
        assert_eq!(applied.unwrap().expect("a group").kind(), kind);
        assert_eq!(aux_of(b"staged"), Some(b"1".to_vec()));
        engine.rollback_transaction(&tx).expect("a rollback");
        assert_eq!(aux_of(b"staged"), None);
        drop(tx);

        // Outside a transaction.
        assert_eq!(engine.pending_prefix_drop_records().expect("a listing"), []);
        engine.delete_prefix_ranges(&[]).expect("no range");
        (engine.remove_pending_prefix_drop_record(&[9; 32])).expect("no record to remove");
        engine.flush().expect("a flush");
        engine.wipe().expect("a wipe");
        assert!(!holds_the_item(&db, v));
    }
}

#[test]
fn the_engine_is_shared_between_threads_and_its_transaction_moves_to_another() {
    fn shared<T: Send + Sync + 'static>() {}
    shared::<EitherEngine>();
    shared::<Either>();
    // As a transaction of RocksDB and one of LigninDB do.
    fn sent<T: Send>() {}
    sent::<<RocksDbEngine as Engine>::Transaction<'static>>();
    sent::<<LigninEngine as Engine>::Transaction<'static>>();
    sent::<<EitherEngine as Engine>::Transaction<'static>>();
    sent::<<EitherEngine as Engine>::WriteScope<'static>>();
}

#[test]
fn the_names_of_the_two_engines() {
    let dir = tempfile::tempdir().expect("a directory");
    for kind in KINDS {
        assert_eq!(kind.other().other(), kind);
        assert_ne!(kind.other(), kind);
        assert_eq!(choice(kind).kind(), kind);
        let engine = EitherEngine::open_with(dir.path().join(kind.to_string()), &choice(kind));
        let engine = engine.expect("a database");
        let named = format!("{engine:?} by {:?}", choice(kind).clone());
        assert_eq!(
            named,
            format!("EitherEngine({kind}) by EngineChoice({kind})")
        );
        let (rocksdb, lignin): (OneOf<u8, u8>, OneOf<u8, u8>) =
            (OneOf::RocksDb(1), OneOf::Lignin(2));
        assert_eq!((rocksdb.rocksdb(), rocksdb.lignin()), (Some(&1), None));
        assert_eq!((lignin.rocksdb(), lignin.lignin()), (None, Some(&2)));
        // An engine that its caller opened is held as it is.
        drop(engine);
        let held = match kind {
            EngineKind::RocksDb => {
                let opened = RocksDbEngine::open(dir.path().join(kind.to_string()));
                EitherEngine::from(opened.expect("a database"))
            }
            EngineKind::Lignin => {
                let opened = LigninEngine::open(dir.path().join(kind.to_string()));
                EitherEngine::from(opened.expect("a database"))
            }
        };
        assert_eq!(held.kind(), kind);
    }
    assert_eq!(
        (
            EngineKind::RocksDb.to_string(),
            EngineKind::Lignin.to_string()
        ),
        ("RocksDB".to_owned(), "LigninDB".to_owned())
    );
}

/// What a caller holds across a call can be given a shorter lifetime of
/// its database, as the same thing of each engine can: a function that
/// takes a database and a transaction with lifetimes of their own compiles
/// on this engine as it does on either. These are tests of the compiler.
mod shortened {
    use crate::{engine::Engine, EitherEngine, LigninEngine, RocksDbEngine};

    macro_rules! shortens {
        ($name:ident, $engine:ty, $what:ident) => {
            #[allow(dead_code)]
            fn $name<'long: 'short, 'short>(
                held: &'short <$engine as Engine>::$what<'long>,
            ) -> &'short <$engine as Engine>::$what<'short> {
                held
            }
        };
    }

    shortens!(a_transaction_of_rocksdb, RocksDbEngine, Transaction);
    shortens!(a_transaction_of_lignindb, LigninEngine, Transaction);
    shortens!(a_transaction, EitherEngine, Transaction);
    shortens!(a_write_scope_of_lignindb, LigninEngine, WriteScope);
    shortens!(a_write_scope, EitherEngine, WriteScope);
    shortens!(a_handle_of_rocksdb, RocksDbEngine, Subtree);
    shortens!(a_handle_of_lignindb, LigninEngine, Subtree);
    shortens!(a_handle, EitherEngine, Subtree);
    shortens!(
        an_immediate_handle_of_rocksdb,
        RocksDbEngine,
        ImmediateSubtree
    );
    shortens!(
        an_immediate_handle_of_lignindb,
        LigninEngine,
        ImmediateSubtree
    );
    shortens!(an_immediate_handle, EitherEngine, ImmediateSubtree);
}
