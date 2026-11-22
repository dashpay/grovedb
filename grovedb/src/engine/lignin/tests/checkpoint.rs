//! Checkpoints of a grove (GROVE-08).
//!
//! The grove of the state-sync tests ([`Made`]) is made on an engine, a
//! checkpoint of it is made, opened, read and written, and its source is
//! written beside it. The histories are written for any engine and are
//! compared, call for call, between LigninDB and RocksDB where the RocksDB
//! engine is compiled in. The tests of what a creation or a deletion left
//! are of LigninDB alone: they are of names that only it keeps.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use grovedb_storage::{
    flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord, PendingPrefixDrops},
    Error as StorageError,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::Answer,
    sync::{assert_answered, assert_shape, open, reads, records, writes, Made, Side},
    version, Lignin, LigninEngine, ROOT,
};
use crate::{engine::Engine, Element, Error, GroveDb, PathBranchChunkQuery, PathTrunkChunkQuery};

/// The names a directory lists, in their order.
fn listed(dir: &Path) -> BTreeSet<String> {
    let entries = fs::read_dir(dir).expect("a directory that is listed");
    entries
        .map(|entry| entry.expect("an entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

fn root_of<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> [u8; 32] {
    db.root_hash(None, v).unwrap().expect("a root hash")
}

/// What a checkpoint of the grove answered, and its source beside it.
#[derive(Debug, PartialEq)]
struct Checkpointed {
    /// The stored records of the checkpoint when it was opened.
    records: Vec<String>,
    /// The reads of the checkpoint when it was opened.
    reads: Vec<Answer>,
    /// The writes into the checkpoint.
    writes: Vec<Answer>,
    /// The writes into the source, made after the checkpoint.
    source_writes: Vec<Answer>,
    /// The reads of the source after both were written.
    source_reads: Vec<Answer>,
    /// How many proofs the grove version does not make.
    unproven: usize,
}

/// A checkpoint of the grove on the engine `E`, made while a write
/// transaction of the source has written and not committed.
///
/// The checkpoint opens to the root hash, the stored records and the
/// answers, costs and proofs that the source had when it was made, with
/// nothing of the transaction. It takes writes that the source does not
/// show, and the source takes writes that the checkpoint does not show.
/// Both are closed and checked. The checkpoint opens again as any
/// database does, and is deleted, and the source is as it was.
fn checkpointed<E: Side>(v: &GroveVersion, number: usize) -> Checkpointed {
    let Made {
        dir,
        db,
        shape,
        root,
    } = Made::<E>::new(v);
    assert_shape(&shape, number);
    let held = records(&db, &shape);
    let answered = reads(&db, &shape, v);
    let unproven = assert_answered(&answered, &shape);

    let above = tempfile::tempdir().expect("a directory");
    let target = above.path().join("checkpoint");
    {
        let tx = db.start_transaction();
        let beside = Element::new_item(b"committed after the checkpoint".to_vec());
        (db.insert(ROOT, b"beside", beside, None, Some(&tx), v))
            .unwrap()
            .expect("an insert in a transaction");
        db.create_checkpoint(&target).expect("a checkpoint");
        db.commit_transaction(tx).unwrap().expect("a commit");
    }
    let source_writes = writes(&db, &shape, v);
    assert_eq!(assert_answered(&source_writes, &shape), 0);
    let source_reads = reads(&db, &shape, v);
    assert_ne!(root_of(&db, v), root, "the source has gone on");

    let checkpoint =
        GroveDb::<E>::open_checkpoint_with_engine(&target).expect("the checkpoint opens");
    assert_eq!(
        root_of(&checkpoint, v),
        root,
        "{} on version {number}",
        E::NAME
    );
    let records_then = records(&checkpoint, &shape);
    assert_eq!(records_then, held, "{} on version {number}", E::NAME);
    let reads_then = reads(&checkpoint, &shape, v);
    assert_eq!(reads_then, answered, "{} on version {number}", E::NAME);
    let has = checkpoint.has_raw(ROOT, b"beside", None, v);
    assert!(!has.unwrap().expect("a read"), "nothing of the transaction");

    let written = writes(&checkpoint, &shape, v);
    assert_eq!(assert_answered(&written, &shape), 0);
    let root_written = root_of(&checkpoint, v);
    assert_ne!(root_written, root);
    assert_eq!(reads(&db, &shape, v), source_reads, "the source beside it");

    checkpoint.flush().expect("a flush");
    drop(checkpoint);
    E::check(&target, v);
    let again = open::<E>(&target);
    assert_eq!(root_of(&again, v), root_written);
    drop(again);

    GroveDb::<E>::delete_checkpoint_with_engine(&target).expect("the checkpoint is deleted");
    assert!(!target.exists());
    assert_eq!(reads(&db, &shape, v), source_reads, "the source after it");
    db.flush().expect("a flush");
    drop(db);
    E::check(dir.path(), v);
    let source = open::<E>(dir.path());
    assert_eq!(reads(&source, &shape, v), source_reads);

    Checkpointed {
        records: records_then,
        reads: reads_then,
        writes: written,
        source_writes,
        source_reads,
        unproven,
    }
}

#[test]
fn a_checkpoint_on_lignindb_opens_to_the_grove_of_the_moment_it_was_made() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let made = checkpointed::<LigninEngine>(v, number + 1);
        assert_eq!(made.unproven > 0, number + 1 < 3);
    }
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_checkpoint_answers_on_lignindb_as_on_rocksdb() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let rocks = checkpointed::<crate::RocksDbEngine>(v, number);
        let lignin = checkpointed::<LigninEngine>(v, number);
        assert_eq!(rocks.records, lignin.records, "grove version {number}");
        assert_eq!(rocks.reads, lignin.reads, "grove version {number}");
        assert_eq!(rocks.writes, lignin.writes, "grove version {number}");
        assert_eq!(
            rocks.source_writes, lignin.source_writes,
            "grove version {number}"
        );
        assert_eq!(
            rocks.source_reads, lignin.source_reads,
            "grove version {number}"
        );
        assert_eq!(rocks.unproven > 0, number < 3, "grove version {number}");
        println!(
            "grove version {number}: {} records, {} reads of the checkpoint, {} writes into it, \
             {} writes into the source",
            rocks.records.len(),
            rocks.reads.len(),
            rocks.writes.len(),
            rocks.source_writes.len(),
        );
    }
}

fn item(n: u8) -> Element {
    Element::new_item(vec![n; 60])
}

fn put<E: Engine>(db: &GroveDb<E>, key: &[u8], element: Element, v: &GroveVersion) {
    (db.insert(ROOT, key, element, None, None, v))
        .unwrap()
        .expect("an insert");
}

/// Checkpoints kept as Platform keeps them
/// (`docs/lignindb/platform-requirements.md`, P-14 to P-18), on the engine
/// `E`: in a directory inside the directory of the live database, opened
/// by the open of any database with a file of Platform's beside the
/// engine's, and removed by the removal of the directory while a handle
/// has the checkpoint open. Returns what was answered, for another engine
/// to be held to.
fn kept_as_platform_keeps_them<E: Side>(v: &GroveVersion) -> Vec<String> {
    let mut told = Vec::new();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<E>(dir.path());
    put(&db, b"tree", Element::empty_tree(), v);
    put(&db, COUNTED, Element::empty_provable_count_tree(), v);
    for n in 0..70u8 {
        let at = if n < 20 { &b"tree"[..] } else { COUNTED };
        let inserted = db.insert([at].as_ref(), &[n], item(n), None, None, v);
        inserted.unwrap().expect("an item");
    }
    let kept = dir.path().join("checkpoints");
    fs::create_dir(&kept).expect("the directory of the checkpoints");

    let first = kept.join("7");
    let root_7 = root_of(&db, v);
    db.create_checkpoint(&first).expect("a checkpoint");
    fs::write(first.join("platform_state.bin"), b"the state").expect("a file beside");

    // A target that is there is refused and left as it is: a checkpoint,
    // a file, and an empty directory.
    let holds = listed(&first);
    assert!(db.create_checkpoint(&first).is_err());
    assert_eq!(listed(&first), holds);
    let file = kept.join("a file");
    fs::write(&file, b"kept").expect("a file");
    assert!(db.create_checkpoint(&file).is_err());
    assert_eq!(fs::read(&file).expect("the file"), b"kept");
    let empty = kept.join("empty");
    fs::create_dir(&empty).expect("a directory");
    assert!(db.create_checkpoint(&empty).is_err());
    assert!(listed(&empty).is_empty());
    // And so is a target with no directory above it.
    assert!(db.create_checkpoint(kept.join("no").join("8")).is_err());
    assert!(!kept.join("no").exists());

    put(&db, b"after 7", item(7), v);
    let checkpoint = open::<E>(&first);
    assert_eq!(root_of(&checkpoint, v), root_7);
    told.push(format!("7: {}", hex::encode(root_7)));

    // A second checkpoint beside the first, which is open, and a
    // checkpoint of the checkpoint.
    let second = kept.join("8");
    let root_8 = root_of(&db, v);
    assert_ne!(root_8, root_7);
    db.create_checkpoint(&second).expect("a second checkpoint");
    put(&checkpoint, b"into 7", item(70), v);
    let root_7_written = root_of(&checkpoint, v);
    let of_the_checkpoint = kept.join("of 7");
    (checkpoint.create_checkpoint(&of_the_checkpoint)).expect("a checkpoint of a checkpoint");
    {
        let second = open::<E>(&second);
        assert_eq!(root_of(&second, v), root_8);
        let third = GroveDb::<E>::open_checkpoint_with_engine(&of_the_checkpoint);
        let third = third.expect("the checkpoint of the checkpoint opens");
        assert_eq!(root_of(&third, v), root_7_written);
        let query = crate::PathQuery::new_unsized(vec![b"tree".to_vec()], {
            let mut query = grovedb_merk::proofs::Query::new();
            query.insert_all();
            query
        });
        let proven = second.prove_query(&query, None, v);
        let live = db.prove_query(&query, None, v);
        assert_eq!(proven.cost, live.cost);
        let cost = proven.cost.clone();
        let proof = proven.unwrap().expect("a proof");
        assert_eq!(proof, live.unwrap().expect("a proof"));
        told.push(format!("8: {} {cost:?}", hex::encode(&proof)));
        // The chunk proofs that Platform serves from a checkpoint (P-16):
        // the trunk of a tree that counts, and the branch under each key
        // the trunk ends at.
        let chunks = chunk_proofs(&second, v);
        assert_eq!(chunks, chunk_proofs(&db, v));
        told.extend(chunks);
    }
    told.push(format!("7 written: {}", hex::encode(root_7_written)));

    // The live database is closed and opened again with the checkpoints
    // in its directory, and the file beside the first is as it was.
    db.flush().expect("a flush");
    drop(db);
    E::check(dir.path(), v);
    let db = open::<E>(dir.path());
    assert_eq!(root_of(&db, v), root_8);
    let state = fs::read(first.join("platform_state.bin")).expect("the file");
    assert_eq!(state, b"the state");

    // The directory of the first is removed while its handle is open.
    // The handle reads on and is closed, nothing is made at the path
    // again, and the live database is as it was.
    fs::remove_dir_all(&first).expect("the directory is removed");
    assert_eq!(root_of(&checkpoint, v), root_7_written);
    let read = checkpoint.get([&b"tree"[..]].as_ref(), &[3], None, v);
    assert_eq!(read.unwrap().expect("an item"), item(3));
    drop(checkpoint);
    assert!(!first.exists());
    assert_eq!(root_of(&db, v), root_8);
    put(&db, b"after all", item(9), v);
    told.push(format!("live: {}", hex::encode(root_of(&db, v))));
    for left in [&second, &of_the_checkpoint] {
        GroveDb::<E>::delete_checkpoint_with_engine(left).expect("a checkpoint is deleted");
        assert!(!left.exists());
    }
    db.flush().expect("a flush");
    drop(db);
    E::check(dir.path(), v);
    told
}

/// The key of the tree that counts, which the chunk proofs are of.
const COUNTED: &[u8] = b"counted";

/// The proof of the trunk of the tree that counts and of the branch under
/// each key the trunk ends at, each verified, with its bytes and its cost.
fn chunk_proofs<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<String> {
    let at = vec![COUNTED.to_vec()];
    let trunk = PathTrunkChunkQuery {
        path: at.clone(),
        max_depth: 3,
        min_depth: None,
    };
    let proved = db.prove_trunk_chunk(&trunk, v);
    let proof = proved.value.as_ref().expect("the trunk is proven");
    let mut told = vec![format!("trunk: {} {:?}", hex::encode(proof), proved.cost)];
    let verified = GroveDb::verify_trunk_chunk_proof(proof, &trunk, v);
    let (root, trunk) = verified.expect("the trunk is verified");
    assert_eq!(root, root_of(db, v));
    assert!(
        !trunk.leaf_keys.is_empty(),
        "the trunk ends above its leaves"
    );
    for (key, leaf) in trunk.leaf_keys {
        let branch = PathBranchChunkQuery {
            path: at.clone(),
            key: key.clone(),
            depth: 3,
        };
        let proved = db.prove_branch_chunk(&branch, v);
        let proof = proved.value.as_ref().expect("a branch is proven");
        (GroveDb::verify_branch_chunk_proof(proof, &branch, leaf.hash, v))
            .expect("the branch is verified");
        told.push(format!(
            "branch {}: {} {:?}",
            hex::encode(&key),
            hex::encode(proof),
            proved.cost
        ));
    }
    told
}

#[test]
fn checkpoints_are_kept_on_lignindb_as_platform_keeps_them() {
    for v in GROVE_VERSIONS.iter() {
        let told = kept_as_platform_keeps_them::<LigninEngine>(v);
        // On every grove version the trunk is proven, with a branch
        // under each of its leaf keys.
        let branches = told.iter().filter(|told| told.starts_with("branch "));
        let branches = branches.count();
        assert!(branches > 0);
        assert_eq!(told.len(), 5 + branches);
        #[cfg(feature = "rocksdb_storage")]
        assert_eq!(told, kept_as_platform_keeps_them::<crate::RocksDbEngine>(v));
    }
}

/// A grove with a tree and an item, and a directory to make checkpoints
/// in.
fn small() -> (tempfile::TempDir, Lignin, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a directory");
    let db = Lignin::open_with_engine(dir.path()).expect("a database");
    put(&db, b"tree", Element::empty_tree(), version());
    put(&db, b"item", item(1), version());
    (dir, db, tempfile::tempdir().expect("a directory"))
}

/// The error of an open that must not have opened anything.
fn not_opened(opened: Result<Lignin, Error>) -> Error {
    match opened {
        Ok(_) => panic!("a database was opened"),
        Err(error) => error,
    }
}

fn words(error: &Error) -> String {
    assert!(
        matches!(error, Error::StorageError(StorageError::StorageError(_))),
        "{error:?}"
    );
    let words = error.to_string();
    assert!(words.contains("lignindb"), "{words}");
    words
}

/// What a creation that was cut leaves beside its target: a directory
/// under the target's name with `.creating` added, here with a part of a
/// file in it.
fn leave(left: &Path) {
    fs::create_dir(left).expect("a directory");
    fs::write(left.join("MANIFEST.tmp"), b"a part").expect("a file");
}

#[test]
fn what_a_creation_left_is_no_checkpoint_and_is_removed() {
    let v = version();
    let (_dir, db, above) = small();
    let root = root_of(&db, v);
    let target = above.path().join("9");
    let left = above.path().join("9.creating");
    let delete = |path: &Path| Lignin::delete_checkpoint_with_engine(path);

    // The engine keeps the name for itself: a target that has it is
    // refused as a limit of the engine, in any case of its letters.
    for name in ["9.creating", "9.CREATING", "9.Creating"] {
        let refused = db.create_checkpoint(above.path().join(name));
        let refused = refused.expect_err("a name of the engine's");
        assert!(matches!(refused, Error::NotSupported(_)), "{refused:?}");
        assert!(refused.to_string().contains("lignindb"), "{refused}");
    }
    assert!(listed(above.path()).is_empty());

    // A directory of that name is opened by neither open, and nothing is
    // made in it.
    leave(&left);
    let holds = listed(&left);
    words(&not_opened(Lignin::open_checkpoint_with_engine(&left)));
    words(&not_opened(Lignin::open_with_engine(&left)));
    assert_eq!(listed(&left), holds);

    // The next creation at the target takes it over.
    db.create_checkpoint(&target).expect("a checkpoint");
    assert_eq!(listed(above.path()), BTreeSet::from(["9".to_string()]));
    let checkpoint = Lignin::open_checkpoint_with_engine(&target).expect("it opens");
    assert_eq!(root_of(&checkpoint, v), root);
    drop(checkpoint);

    // The deletion of the target removes what is beside it under that
    // name, with the target and where nothing has the target's name.
    leave(&left);
    delete(&target).expect("the checkpoint and what was left beside it");
    assert!(listed(above.path()).is_empty());
    leave(&left);
    delete(&target).expect("what was left beside a target that is not there");
    assert!(listed(above.path()).is_empty());
    // And so does the deletion of that directory itself, also through an
    // engine that wraps this one.
    leave(&left);
    delete(&left).expect("what was left");
    assert!(listed(above.path()).is_empty());
    leave(&left);
    type Wrapped = crate::tests::faultable_reads::RefusedReads<LigninEngine>;
    GroveDb::<Wrapped>::delete_checkpoint_with_engine(&left).expect("what was left");
    assert!(listed(above.path()).is_empty());

    // Where nothing is there, nothing is deleted.
    words(&delete(&target).expect_err("nothing"));
    words(&delete(&left).expect_err("nothing"));
    assert_eq!(root_of(&db, v), root);
}

#[test]
fn a_deletion_that_was_cut_is_finished_and_what_it_left_is_not_opened() {
    let v = version();
    let (_dir, db, above) = small();
    let target = above.path().join("checkpoint");

    // A removal gives the manifest another name first. A directory that
    // holds that name is opened by neither open, and stays as it is.
    db.create_checkpoint(&target).expect("a checkpoint");
    fs::rename(target.join("MANIFEST"), target.join("MANIFEST.deleting")).expect("a rename");
    let holds = listed(&target);
    for opened in [
        Lignin::open_checkpoint_with_engine(&target),
        Lignin::open_with_engine(&target),
    ] {
        let refused = words(&not_opened(opened));
        assert!(refused.contains("MANIFEST.deleting"), "{refused}");
    }
    assert_eq!(listed(&target), holds);
    Lignin::delete_checkpoint_with_engine(&target).expect("the removal is finished");
    assert!(!target.exists());

    // A whole checkpoint beside a file of that name which something else
    // made is not opened either, and is removed whole.
    db.create_checkpoint(&target).expect("a checkpoint");
    fs::write(target.join("MANIFEST.deleting"), b"").expect("a file");
    words(&not_opened(Lignin::open_checkpoint_with_engine(&target)));
    Lignin::delete_checkpoint_with_engine(&target).expect("it is removed");
    assert!(!target.exists());

    // Without that file it opens.
    db.create_checkpoint(&target).expect("a checkpoint");
    fs::write(target.join("MANIFEST.deleting"), b"").expect("a file");
    fs::remove_file(target.join("MANIFEST.deleting")).expect("the file goes");
    let checkpoint = Lignin::open_checkpoint_with_engine(&target).expect("it opens");
    assert_eq!(root_of(&checkpoint, v), root_of(&db, v));
}

#[test]
fn only_a_closed_database_of_the_engine_is_opened_or_deleted_as_a_checkpoint() {
    let v = version();
    let (_dir, db, above) = small();
    let delete = |path: &Path| Lignin::delete_checkpoint_with_engine(path);
    let open = |path: &Path| Lignin::open_checkpoint_with_engine(path);

    // Nothing at the path: no checkpoint is opened and none is made.
    let nothing = above.path().join("nothing");
    words(&not_opened(open(&nothing)));
    assert!(!nothing.exists());
    // An empty directory: none is made in it, and a deletion removes it,
    // as nothing is lost with it.
    let empty = above.path().join("empty");
    fs::create_dir(&empty).expect("a directory");
    words(&not_opened(open(&empty)));
    assert!(listed(&empty).is_empty());
    delete(&empty).expect("an empty directory goes");
    assert!(!empty.exists());
    // And so does a directory that holds an empty lock file alone.
    fs::create_dir(&empty).expect("a directory");
    fs::write(empty.join("LOCK"), b"").expect("a lock file");
    words(&not_opened(open(&empty)));
    delete(&empty).expect("a directory with its lock file alone goes");
    assert!(!empty.exists());
    // A directory that holds something and no database stays as it is.
    let other = above.path().join("other");
    fs::create_dir(&other).expect("a directory");
    fs::write(other.join("notes"), b"kept").expect("a file");
    words(&not_opened(open(&other)));
    words(&delete(&other).expect_err("no database"));
    assert_eq!(listed(&other), BTreeSet::from(["notes".to_string()]));
    // And so do a file, and the directory of another engine.
    let file = above.path().join("file");
    fs::write(&file, b"kept").expect("a file");
    words(&delete(&file).expect_err("a file"));
    assert_eq!(fs::read(&file).expect("the file"), b"kept");
    let rocks = above.path().join("rocks");
    fs::create_dir(&rocks).expect("a directory");
    fs::write(rocks.join("CURRENT"), b"MANIFEST-000001\n").expect("a file");
    words(&not_opened(open(&rocks)));
    words(&delete(&rocks).expect_err("another engine's"));
    assert_eq!(listed(&rocks), BTreeSet::from(["CURRENT".to_string()]));

    // A checkpoint that a handle has open is not deleted, and not opened
    // a second time.
    let target = above.path().join("checkpoint");
    db.create_checkpoint(&target).expect("a checkpoint");
    let checkpoint = open(&target).expect("it opens");
    let mut holds = listed(&target);
    holds.retain(|name| !name.ends_with(".tmp"));
    words(&delete(&target).expect_err("an open checkpoint"));
    words(&not_opened(open(&target)));
    // The open checkpoint goes on writing into its directory: what the
    // directory held before, but for a file it was replacing, is there.
    assert!(listed(&target).is_superset(&holds));
    assert_eq!(root_of(&checkpoint, v), root_of(&db, v));
    drop(checkpoint);
    let again = open(&target).expect("it opens again");
    assert_eq!(root_of(&again, v), root_of(&db, v));
    drop(again);
    delete(&target).expect("a closed checkpoint");
    assert!(!target.exists());

    // A path of fewer than two components is refused before the engine
    // is asked.
    let short = delete(Path::new("/")).expect_err("the root");
    assert!(matches!(short, Error::CorruptedData(_)), "{short:?}");
    let short = delete(&PathBuf::from("checkpoint")).expect_err("one name");
    assert!(matches!(short, Error::CorruptedData(_)), "{short:?}");
}

#[test]
fn a_checkpoint_holds_the_records_that_no_tree_holds() {
    let v = version();
    let (_dir, db, above) = small();
    let engine = db.engine();
    (db.put_aux(b"aux", b"kept", None, None))
        .unwrap()
        .expect("an aux record");
    let record = PendingPrefixDropRecord {
        primary_prefix: [3; 32],
        doomed_prefixes: vec![[3; 32], [4; 32]],
        path: vec![b"tree".to_vec(), b"dropped".to_vec()],
    };
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let namespace = *pending_prefix_drops_namespace();
    let handle = engine
        .subtree_by_prefix(namespace, Some(&batch), &tx)
        .unwrap();
    grovedb_storage::flat::stage_pending_prefix_drop(&handle.meta(), &record)
        .unwrap()
        .expect("a staged record");
    drop(handle);
    engine.apply_batch(batch, &tx).unwrap().expect("a batch");
    engine.commit_transaction(tx).unwrap().expect("a commit");

    let target = above.path().join("checkpoint");
    db.create_checkpoint(&target).expect("a checkpoint");
    (engine.remove_pending_prefix_drop_record(&record.primary_prefix)).expect("a removal");
    (db.delete_aux(b"aux", None, None))
        .unwrap()
        .expect("the aux record goes");

    let checkpoint = Lignin::open_checkpoint_with_engine(&target).expect("it opens");
    let aux = checkpoint.get_aux(b"aux", None).unwrap().expect("a read");
    assert_eq!(aux, Some(b"kept".to_vec()));
    assert_eq!(
        (checkpoint.engine().pending_prefix_drop_records()).expect("the record"),
        std::slice::from_ref(&record)
    );
    assert_eq!(engine.pending_prefix_drop_records().expect("none"), []);
    assert_eq!(root_of(&checkpoint, v), root_of(&db, v));
}

#[test]
fn a_creation_that_ends_the_session_says_that_the_database_is_opened_again() {
    use std::sync::Arc;

    use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet, SimConfig, SimVfs};
    use lignindb::Options;

    let v = version();
    let fs = SimVfs::new(SimConfig::new(8));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..Options::default()
    };
    let engine = LigninEngine::open_with("/grove", &options).expect("an engine");
    let db = GroveDb::from_engine(engine);
    put(&db, b"item", item(1), v);
    let root = root_of(&db, v);

    // The log is not durable through the latest version, so the creation
    // asks for a sync of it, which fails.
    let syncs = OpSet::of(&[OpKind::SyncData, OpKind::SyncAll]);
    fs.add_rule(FaultRule::new(Fault::Eio).on(syncs).forever());
    let failed = db
        .create_checkpoint("/checkpoint")
        .expect_err("a sync that fails");
    fs.clear_rules();
    assert!(words(&failed).contains("opened again"), "{failed}");
    // The session has ended by it, and the next creation says so too.
    assert!(db.engine().database().reopen().is_some());
    let refused = db
        .create_checkpoint("/checkpoint")
        .expect_err("a session that ended");
    assert!(words(&refused).contains("opened again"), "{refused}");
    assert_eq!(root_of(&db, v), root);
    drop(db);

    // Opened again, the database makes its checkpoint.
    let engine = LigninEngine::open_with("/grove", &options).expect("it opens again");
    let db = GroveDb::from_engine(engine);
    assert_eq!(root_of(&db, v), root);
    db.create_checkpoint("/checkpoint").expect("a checkpoint");
    let checkpoint = Options {
        create_if_missing: false,
        ..options
    };
    let checkpoint = LigninEngine::open_with("/checkpoint", &checkpoint).expect("it opens");
    assert_eq!(root_of(&GroveDb::from_engine(checkpoint), v), root);
}

#[test]
fn the_errors_of_a_creation_are_named_by_what_the_seam_knows() {
    use lignindb::{checkpoint::CheckpointError, commit::Stopped};

    use super::super::checkpoint::creation_error;

    let target = PathBuf::from("/above/7.creating");
    let reserved = creation_error(CheckpointError::ReservedName {
        target: target.clone(),
    });
    assert!(matches!(reserved, StorageError::NotSupported(_)));
    assert!(reserved.to_string().contains("7.creating"));
    let exists = creation_error(CheckpointError::TargetExists { target });
    assert!(matches!(exists, StorageError::StorageError(_)));
    assert!(!exists.to_string().contains("opened again"));
    let (_dir, db, _above) = small();
    let stopped = creation_error(CheckpointError::Stopped(Stopped::Unsettled {
        version: db.engine().database().version(),
        reason: "a reason".to_string(),
    }));
    assert!(matches!(stopped, StorageError::StorageError(_)));
    let stopped = stopped.to_string();
    assert!(stopped.contains("opened again") && stopped.contains("a reason"));
}

/// A checkpoint of a database that holds megabytes of log and values that
/// are kept out of their pages: it opens to every value, and reads them
/// still once its source has written every one of them again and deleted
/// half, has been closed and is open again.
#[test]
fn a_checkpoint_keeps_its_values_while_its_source_writes_them_again() {
    let v = version();
    let (dir, db, above) = small();
    let value = |n: u8, round: u8| Element::new_item(vec![n ^ round; 60 * 1024]);
    let fill = |db: &Lignin, round: u8| {
        for n in 0..40u8 {
            let inserted = db.insert(
                [&b"tree"[..]].as_ref(),
                &[n],
                value(n, round),
                None,
                None,
                v,
            );
            inserted.unwrap().expect("a value");
        }
    };
    fill(&db, 0);
    let root = root_of(&db, v);
    let target = above.path().join("checkpoint");
    db.create_checkpoint(&target).expect("a checkpoint");

    fill(&db, 0xff);
    for n in (0..40u8).step_by(2) {
        let deleted = db.delete([&b"tree"[..]].as_ref(), &[n], None, None, v);
        deleted.unwrap().expect("a delete");
    }
    db.flush().expect("a flush");
    drop(db);
    let db = Lignin::open_with_engine(dir.path()).expect("the source opens again");
    assert_ne!(root_of(&db, v), root);

    let checkpoint = Lignin::open_checkpoint_with_engine(&target).expect("it opens");
    assert_eq!(root_of(&checkpoint, v), root);
    for n in 0..40u8 {
        let read = checkpoint.get([&b"tree"[..]].as_ref(), &[n], None, v);
        assert_eq!(read.unwrap().expect("a value"), value(n, 0));
    }
    let issues = checkpoint.verify_grovedb(None, true, false, v);
    assert!(issues.expect("a verification").is_empty());
    let left = db.get([&b"tree"[..]].as_ref(), &[1], None, v);
    assert_eq!(left.unwrap().expect("a value"), value(1, 0xff));
}
