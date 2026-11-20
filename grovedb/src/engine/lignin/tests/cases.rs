//! The cases that `docs/lignindb/transactions.md` records of the RocksDB
//! engine (sections 16.3 and 16.4, run on RocksDB 0.25.0 by the experiment
//! of TXN-01), as tests of any engine: each case is written once, for an
//! `Engine`, and gives on every engine the result the table has for it.
//!
//! The raw cases (`A`) go through the seam: a key is a flat record of the
//! root subtree's aux keyspace, written through an immediate handle, and a
//! write outside the transaction is a transaction of its own that is
//! committed at once. Seven cases of the table are not cases of the seam
//! and are not here: `A10` to `A10d` (a range delete outside a
//! transaction, which the seam has as `wipe` alone: case `B17`), `A11` and
//! `A11b` (RocksDB's memtables) and `A18` (the ingestion of a file).
//!
//! The cases of GroveDB (`B`) start from two empty trees `A` and `B` under
//! the root.

use grovedb_path::SubtreePath;
use grovedb_storage::{FlatStore, RawIterator};
use grovedb_version::version::GroveVersion;

use crate::{
    engine::{Engine, SubtreeHandle},
    Element, Error, GroveDb, TransactionArg,
};

fn version() -> &'static GroveVersion {
    GroveVersion::latest()
}

/// How a commit ended: `OK`, or `Conflict` by the engine-neutral class.
fn outcome(committed: Result<(), Error>) -> &'static str {
    match committed {
        Ok(()) => "OK",
        Err(error) if error.is_commit_conflict() => "Conflict",
        Err(error) => panic!("a commit failed with no conflict: {error}"),
    }
}

/// The keyspace a raw case writes a key into.
#[derive(Clone, Copy)]
enum Space {
    Aux,
    Meta,
}

/// An engine with nothing in it, for one raw case.
struct Raw<E: Engine> {
    _dir: tempfile::TempDir,
    engine: E,
}

impl<E: Engine> Raw<E> {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let engine = E::open(dir.path()).expect("an engine");
        Self { _dir: dir, engine }
    }

    fn with<T>(
        &self,
        transaction: &E::Transaction<'_>,
        space: Space,
        call: impl FnOnce(&dyn Fn(&[u8]) -> Option<Vec<u8>>, &dyn Fn(&[u8], Option<&[u8]>)) -> T,
    ) -> T {
        let root = self
            .engine
            .immediate_subtree(SubtreePath::<[u8; 0]>::empty(), transaction)
            .unwrap();
        fn on<S: FlatStore, T>(
            keyspace: &S,
            call: impl FnOnce(&dyn Fn(&[u8]) -> Option<Vec<u8>>, &dyn Fn(&[u8], Option<&[u8]>)) -> T,
        ) -> T {
            call(
                &|key| keyspace.get(key).unwrap().expect("a read"),
                &|key, value| match value {
                    Some(value) => keyspace.put(key, value, None).unwrap().expect("a put"),
                    None => keyspace.delete(key, None).unwrap().expect("a delete"),
                },
            )
        }
        match space {
            Space::Aux => on(&root.aux(), call),
            Space::Meta => on(&root.meta(), call),
        }
    }

    fn put(&self, transaction: &E::Transaction<'_>, key: &[u8], value: &[u8]) {
        self.with(transaction, Space::Aux, |_, write| write(key, Some(value)));
    }

    fn delete(&self, transaction: &E::Transaction<'_>, key: &[u8]) {
        self.with(transaction, Space::Aux, |_, write| write(key, None));
    }

    /// What `transaction` reads under `key`: the value, or `-`.
    fn read(&self, transaction: &E::Transaction<'_>, key: &[u8]) -> String {
        let value = self.with(transaction, Space::Aux, |read, _| read(key));
        value.map_or("-".to_owned(), |value| {
            String::from_utf8(value).expect("a value of the case")
        })
    }

    fn commit(&self, transaction: E::Transaction<'_>) -> &'static str {
        let committed = self.engine.commit_transaction(transaction).unwrap();
        outcome(committed.map_err(Error::from))
    }

    /// A write outside every open transaction: a transaction of its own,
    /// committed at once.
    fn outside(&self, write: impl FnOnce(&E::Transaction<'_>)) {
        let transaction = self.engine.start_transaction();
        write(&transaction);
        assert_eq!(self.commit(transaction), "OK");
    }

    fn put_outside(&self, key: &[u8], value: &[u8]) {
        self.outside(|transaction| self.put(transaction, key, value));
    }

    /// What the committed state holds under `key`.
    fn committed(&self, key: &[u8]) -> String {
        self.read(&self.engine.start_read_transaction(), key)
    }

    /// The keys a cursor of `transaction` that `opened` hands over shows,
    /// from the first to the last.
    fn scan(cursor: &mut impl RawIterator) -> String {
        let mut keys = Vec::new();
        cursor.seek_to_first().unwrap();
        while cursor.valid().unwrap() {
            let key = cursor.key().unwrap().expect("a key").to_vec();
            keys.push(String::from_utf8(key).expect("a key of the case"));
            cursor.next().unwrap();
        }
        cursor.status().expect("a cursor that walked");
        keys.join(",")
    }
}

/// The raw cases of section 16.3 that are cases of the seam, each with
/// what the engine `E` gave.
pub(super) fn raw_cases<E: Engine>() -> Vec<(&'static str, String)> {
    let mut seen = Vec::new();
    let mut case = |name: &'static str, run: &dyn Fn(&Raw<E>) -> String| {
        seen.push((name, run(&Raw::new())));
    };

    case("A1", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t");
        // The write outside is the batch of an operation, applied in a
        // transaction of its own.
        raw.outside(|w| {
            let batch = E::Batch::default();
            let root = raw
                .engine
                .subtree(SubtreePath::<[u8; 0]>::empty(), Some(&batch), w)
                .unwrap();
            root.aux().put(b"k", b"w", None).unwrap().expect("a put");
            drop(root);
            raw.engine.apply_batch(batch, w).unwrap().expect("a batch");
        });
        raw.commit(t).to_owned()
    });
    case("A1b", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t");
        raw.put_outside(b"k", b"w");
        raw.commit(t).to_owned()
    });
    case("A2", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put_outside(b"k", b"w");
        raw.put(&t, b"k", b"t");
        format!("{} final={}", raw.commit(t), raw.committed(b"k"))
    });
    case("A3", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k1", b"t");
        raw.put_outside(b"k2", b"w");
        let read = raw.read(&t, b"k2");
        format!("read={read} {}", raw.commit(t))
    });
    case("A3b", &|raw| {
        raw.put_outside(b"k", b"0");
        let t = raw.engine.start_transaction();
        assert_eq!(raw.read(&t, b"k"), "0");
        raw.put_outside(b"k", b"w");
        let read = raw.read(&t, b"k");
        raw.put(&t, b"k2", b"t");
        format!("read={read} {}", raw.commit(t))
    });
    case("A3c", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t");
        raw.put_outside(b"k", b"w");
        format!("read={}", raw.read(&t, b"k"))
    });
    case("A3d", &|raw| {
        raw.put_outside(b"k", b"0");
        let t = raw.engine.start_transaction();
        raw.delete(&t, b"k");
        raw.put_outside(b"k", b"w");
        format!("read={}", raw.read(&t, b"k"))
    });
    case("A4", &|raw| {
        let t = raw.engine.start_transaction();
        raw.engine.set_savepoint(&t).expect("a savepoint");
        raw.put(&t, b"k", b"t");
        raw.put_outside(b"k", b"w");
        raw.engine.rollback_to_savepoint(&t).expect("a rollback");
        raw.commit(t).to_owned()
    });
    case("A4b", &|raw| {
        let t = raw.engine.start_transaction();
        raw.engine.set_savepoint(&t).expect("a savepoint");
        raw.put(&t, b"k", b"t");
        raw.put_outside(b"k", b"w");
        raw.engine.rollback_to_savepoint(&t).expect("a rollback");
        format!("read={}", raw.read(&t, b"k"))
    });
    case("A5", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t1");
        raw.engine.set_savepoint(&t).expect("a savepoint");
        raw.put(&t, b"k", b"t2");
        raw.put_outside(b"k", b"w");
        raw.engine.rollback_to_savepoint(&t).expect("a rollback");
        raw.commit(t).to_owned()
    });
    case("A5b", &|raw| {
        let t = raw.engine.start_transaction();
        raw.engine.set_savepoint(&t).expect("a savepoint");
        raw.put(&t, b"k", b"t1");
        raw.put_outside(b"k", b"w");
        raw.engine.rollback_to_savepoint(&t).expect("a rollback");
        raw.put(&t, b"k", b"t2");
        format!("{} final={}", raw.commit(t), raw.committed(b"k"))
    });
    case("A6", &|raw| {
        // The same key in two keyspaces is two keys.
        let t = raw.engine.start_transaction();
        raw.with(&t, Space::Aux, |_, write| write(b"k", Some(b"t")));
        raw.outside(|w| raw.with(w, Space::Meta, |_, write| write(b"k", Some(b"w"))));
        raw.commit(t).to_owned()
    });
    case("A7", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t");
        raw.outside(|w| raw.delete(w, b"k"));
        raw.commit(t).to_owned()
    });
    case("A7b", &|raw| {
        raw.put_outside(b"k", b"same");
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t");
        raw.put_outside(b"k", b"same");
        raw.commit(t).to_owned()
    });
    case("A8", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k1", b"t");
        raw.put_outside(b"k2", b"w");
        raw.put(&t, b"k2", b"t");
        format!("{} final={}", raw.commit(t), raw.committed(b"k2"))
    });
    case("A9", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t1");
        raw.put_outside(b"k", b"w");
        raw.put(&t, b"k", b"t2");
        raw.commit(t).to_owned()
    });
    case("A12", &|raw| {
        raw.put_outside(b"k1", b"0");
        let t = raw.engine.start_transaction();
        let root = raw
            .engine
            .immediate_subtree(SubtreePath::<[u8; 0]>::empty(), &t)
            .unwrap();
        let aux = root.aux();
        let mut first = aux.cursor();
        raw.put_outside(b"k2", b"w");
        let old = Raw::<E>::scan(&mut first);
        let new = Raw::<E>::scan(&mut aux.cursor());
        format!("old=[{old}] new=[{new}]")
    });
    case("A12b", &|raw| {
        raw.put_outside(b"k1", b"0");
        let t = raw.engine.start_transaction();
        let root = raw
            .engine
            .immediate_subtree(SubtreePath::<[u8; 0]>::empty(), &t)
            .unwrap();
        let aux = root.aux();
        let mut cursor = aux.cursor();
        raw.put(&t, b"k2", b"t");
        format!("[{}]", Raw::<E>::scan(&mut cursor))
    });
    case("A13", &|raw| {
        let first = raw.engine.start_transaction();
        let second = raw.engine.start_transaction();
        raw.put(&first, b"k", b"1");
        raw.put(&second, b"k", b"2");
        assert_eq!(raw.commit(second), "OK");
        raw.commit(first).to_owned()
    });
    case("A13b", &|raw| {
        let first = raw.engine.start_transaction();
        let second = raw.engine.start_transaction();
        raw.put(&first, b"k1", b"1");
        raw.put(&second, b"k2", b"2");
        format!("{} {}", raw.commit(second), raw.commit(first))
    });
    case("A14", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put(&t, b"k", b"t1");
        raw.put_outside(b"k", b"w");
        raw.engine.rollback_transaction(&t).expect("a rollback");
        raw.put(&t, b"k", b"t2");
        format!("{} final={}", raw.commit(t), raw.committed(b"k"))
    });
    case("A15", &|raw| {
        let t = raw.engine.start_transaction();
        raw.put_outside(b"k", b"w");
        raw.commit(t).to_owned()
    });
    case("A16", &|raw| {
        let t = raw.engine.start_transaction();
        raw.engine.set_savepoint(&t).expect("a savepoint");
        raw.put(&t, b"k", b"t");
        raw.engine.set_savepoint(&t).expect("a savepoint");
        raw.put(&t, b"k2", b"t");
        raw.put_outside(b"k", b"w");
        raw.engine.rollback_to_savepoint(&t).expect("a rollback");
        raw.commit(t).to_owned()
    });
    case("A17", &|raw| {
        let t = raw.engine.start_transaction();
        match raw.engine.rollback_to_savepoint(&t) {
            Ok(()) => "OK".to_owned(),
            Err(_) => "error".to_owned(),
        }
    });
    seen
}

/// What every engine gives for the raw cases: the column "LigninDB" of
/// section 16.3, which is RocksDB's with `Conflict` for `Busy`.
pub(super) const RAW: &[(&str, &str)] = &[
    ("A1", "Conflict"),
    ("A1b", "Conflict"),
    ("A2", "OK final=t"),
    ("A3", "read=w OK"),
    ("A3b", "read=w OK"),
    ("A3c", "read=t"),
    ("A3d", "read=-"),
    ("A4", "OK"),
    ("A4b", "read=w"),
    ("A5", "Conflict"),
    ("A5b", "OK final=t2"),
    ("A6", "OK"),
    ("A7", "Conflict"),
    ("A7b", "Conflict"),
    ("A8", "OK final=t"),
    ("A9", "Conflict"),
    ("A12", "old=[k1] new=[k1,k2]"),
    ("A12b", "[k1,k2]"),
    ("A13", "Conflict"),
    ("A13b", "OK OK"),
    ("A14", "OK final=t2"),
    ("A15", "OK"),
    ("A16", "Conflict"),
    ("A17", "error"),
];

pub(super) const A: &[u8] = b"A";
pub(super) const B: &[u8] = b"B";

/// A GroveDB on the engine `E` with the two empty trees of section 16.4.
pub(super) struct Grove<E: Engine> {
    pub(super) dir: tempfile::TempDir,
    pub(super) db: GroveDb<E>,
}

impl<E: Engine> Grove<E> {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<E>::open_with_engine(dir.path()).expect("a database");
        for tree in [A, B] {
            db.insert(
                SubtreePath::<[u8; 0]>::empty(),
                tree,
                Element::empty_tree(),
                None,
                None,
                version(),
            )
            .unwrap()
            .expect("a tree under the root");
        }
        Self { dir, db }
    }

    /// Inserts the item `key` into the tree `tree`, with its key as its
    /// value.
    pub(super) fn try_insert<'db>(
        &'db self,
        tree: &[u8],
        key: &[u8],
        transaction: TransactionArg<'db, '_, E>,
    ) -> Result<(), Error> {
        let item = Element::new_item(key.to_vec());
        self.db
            .insert([tree].as_ref(), key, item, None, transaction, version())
            .unwrap()
    }

    pub(super) fn insert<'db>(
        &'db self,
        tree: &[u8],
        key: &[u8],
        transaction: TransactionArg<'db, '_, E>,
    ) {
        self.try_insert(tree, key, transaction).expect("an insert");
    }

    /// Whether `transaction` finds the item `key` of the tree `tree`.
    pub(super) fn finds<'db>(
        &'db self,
        tree: &[u8],
        key: &[u8],
        transaction: TransactionArg<'db, '_, E>,
    ) -> bool {
        match (self.db)
            .get([tree].as_ref(), key, transaction, version())
            .unwrap()
        {
            Ok(found) => {
                assert_eq!(found, Element::new_item(key.to_vec()));
                true
            }
            Err(Error::PathKeyNotFound(_)) => false,
            Err(error) => panic!("a read failed: {error}"),
        }
    }

    fn found(found: bool) -> &'static str {
        match found {
            true => "found",
            false => "not found",
        }
    }

    pub(super) fn commit<'db>(&'db self, transaction: E::Transaction<'db>) -> &'static str {
        outcome(self.db.commit_transaction(transaction).unwrap())
    }

    fn put_aux<'db>(&'db self, key: &[u8], transaction: TransactionArg<'db, '_, E>) {
        (self.db.put_aux(key, b"aux", None, transaction))
            .unwrap()
            .expect("an aux record");
    }
}

/// The cases of section 16.4 but for `B8`, the trace of section 14.3,
/// each with what GroveDB on the engine `E` gave.
pub(super) fn grove_cases<E: Engine>() -> Vec<(&'static str, String)> {
    let mut seen = Vec::new();
    let mut case = |name: &'static str, run: &dyn Fn(&Grove<E>) -> String| {
        seen.push((name, run(&Grove::new())));
    };

    case("B1", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&t));
        grove.insert(B, b"b1", None);
        grove.commit(t).to_owned()
    });
    case("B2", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(B, b"b1", None);
        grove.insert(A, b"a1", Some(&t));
        let committed = grove.commit(t);
        let both = grove.finds(A, b"a1", None) && grove.finds(B, b"b1", None);
        format!("{committed}, both present: {both}")
    });
    case("B3", &|grove| {
        let t = grove.db.start_transaction();
        grove.put_aux(b"x", Some(&t));
        grove.insert(B, b"b1", None);
        grove.commit(t).to_owned()
    });
    case("B4", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&t));
        grove.put_aux(b"x", None);
        grove.commit(t).to_owned()
    });
    case("B5", &|grove| {
        let t = grove.db.start_transaction();
        assert!(!grove.finds(B, b"b1", Some(&t)));
        grove.insert(B, b"b1", None);
        let found = Grove::<E>::found(grove.finds(B, b"b1", Some(&t)));
        format!("{found}, {}", grove.commit(t))
    });
    case("B6", &|grove| {
        let t = grove.db.start_transaction();
        grove.db.set_savepoint(&t).expect("a savepoint");
        grove.insert(A, b"a1", Some(&t));
        grove.insert(B, b"b1", None);
        grove.db.rollback_to_savepoint(&t).expect("a rollback");
        grove.insert(A, b"a2", Some(&t));
        grove.commit(t).to_owned()
    });
    case("B7", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(A, b"a0", Some(&t));
        grove.db.set_savepoint(&t).expect("a savepoint");
        grove.insert(A, b"a1", Some(&t));
        grove.insert(B, b"b1", None);
        grove.db.rollback_to_savepoint(&t).expect("a rollback");
        grove.commit(t).to_owned()
    });
    case("B9", &|grove| {
        let first = grove.db.start_transaction();
        let second = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&first));
        grove.insert(B, b"b1", Some(&second));
        format!("{}, {}", grove.commit(first), grove.commit(second))
    });
    case("B10", &|grove| {
        let first = grove.db.start_transaction();
        let second = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&first));
        let first = grove.commit(first);
        grove.insert(B, b"b1", Some(&second));
        format!("{first}, {}", grove.commit(second))
    });
    case("B11", &|grove| {
        let snapshot = grove.db.start_snapshot_read_transaction();
        grove.insert(B, b"b1", None);
        format!(
            "snapshot: {}, none: {}",
            Grove::<E>::found(grove.finds(B, b"b1", Some(&snapshot))),
            Grove::<E>::found(grove.finds(B, b"b1", None)),
        )
    });
    case("B12", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&t));
        format!(
            "none: {}, T: {}",
            Grove::<E>::found(grove.finds(A, b"a1", None)),
            Grove::<E>::found(grove.finds(A, b"a1", Some(&t))),
        )
    });
    case("B13", &|grove| {
        let snapshot = grove.db.start_snapshot_read_transaction();
        match grove.try_insert(A, b"a1", Some(&snapshot)) {
            Err(Error::StorageError(grovedb_storage::Error::SnapshotReadOnlyTransaction(_))) => {
                "refused".to_owned()
            }
            other => format!("{other:?}"),
        }
    });
    case("B14", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&t));
        grove.insert(A, b"a2", None);
        grove.commit(t).to_owned()
    });
    case("B15", &|grove| {
        let t = grove.db.start_transaction();
        grove.put_aux(b"x", Some(&t));
        grove.put_aux(b"x", None);
        grove.commit(t).to_owned()
    });
    case("B16", &|grove| {
        let t = grove.db.start_transaction();
        grove.put_aux(b"x", Some(&t));
        grove.put_aux(b"y", None);
        let read = grove.db.get_aux(b"y", Some(&t)).unwrap().expect("a read");
        let found = Grove::<E>::found(read.is_some());
        format!("{found}, {}", grove.commit(t))
    });
    case("B17", &|grove| {
        let t = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&t));
        grove.db.wipe().expect("a wipe");
        grove.commit(t).to_owned()
    });
    case("B18", &|grove| {
        grove.insert(A, b"a1", None);
        let t = grove.db.start_transaction();
        let item = Element::new_item(b"a1".to_vec());
        let inserted = (grove.db)
            .insert_if_not_exists([A].as_ref(), b"a1", item, Some(&t), version())
            .unwrap()
            .expect("a conditional insert");
        grove.insert(B, b"b1", None);
        format!("inserted: {inserted}, {}", grove.commit(t))
    });
    case("B19", &|grove| {
        grove.insert(A, b"a1", None);
        let t = grove.db.start_transaction();
        grove.insert(A, b"a1", Some(&t));
        grove.insert(B, b"b1", None);
        grove.commit(t).to_owned()
    });
    case("B20", &|grove| {
        grove.insert(A, b"a1", None);
        let t = grove.db.start_transaction();
        let item = Element::new_item(b"a1".to_vec());
        let (changed, _) = (grove.db)
            .insert_if_changed_value([A].as_ref(), b"a1", item, Some(&t), version())
            .unwrap()
            .expect("a conditional insert");
        grove.insert(B, b"b1", None);
        format!("changed: {changed}, {}", grove.commit(t))
    });
    case("B21", &|grove| {
        grove.insert(A, b"a1", None);
        let t = grove.db.start_transaction();
        (grove.db)
            .delete([A].as_ref(), b"a1", None, Some(&t), version())
            .unwrap()
            .expect("a delete");
        grove.insert(B, b"b1", None);
        grove.commit(t).to_owned()
    });
    case("B22", &|grove| {
        let t = grove.db.start_transaction();
        let deleted = (grove.db)
            .delete([A].as_ref(), b"nope", None, Some(&t), version())
            .unwrap();
        assert!(deleted.is_err(), "the delete of a key that is not there");
        grove.insert(B, b"b1", None);
        format!("delete failed, {}", grove.commit(t))
    });
    seen
}

/// What GroveDB gives on every engine for the cases of section 16.4.
pub(super) const GROVE: &[(&str, &str)] = &[
    ("B1", "Conflict"),
    ("B2", "OK, both present: true"),
    ("B3", "OK"),
    ("B4", "OK"),
    ("B5", "found, OK"),
    ("B6", "OK"),
    ("B7", "Conflict"),
    ("B9", "OK, Conflict"),
    ("B10", "OK, OK"),
    ("B11", "snapshot: not found, none: found"),
    ("B12", "none: not found, T: found"),
    ("B13", "refused"),
    ("B14", "Conflict"),
    ("B15", "Conflict"),
    ("B16", "found, OK"),
    ("B17", "Conflict"),
    ("B18", "inserted: false, OK"),
    ("B19", "Conflict"),
    ("B20", "changed: false, OK"),
    ("B21", "Conflict"),
    ("B22", "delete failed, OK"),
];
