//! Where a Merk on LigninDB answers otherwise than on RocksDB, known and
//! not removed by MERK-08. Each test makes the same calls on both engines
//! and holds both answers, so a change of either shows here.
//! `docs/lignindb/test-engines.md` has the list, with the two differences
//! that tests of other modules hold.

use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    FlatStore, StorageContext,
};

use super::*;

/// A subtree that the database of a test has entered, with no node.
const PLACE: Prefix = [77; 32];

/// The transactions and the batches of one step on both engines.
struct Step<'a> {
    rocks: &'a TempStorage,
    rocks_transaction: <RocksDbStorage as Storage<'a>>::Transaction,
    storage_batch: StorageBatch,
    lignin: &'a Lignin,
    transaction: WriteTxn<'a>,
    staged: LigninBatch,
}

impl<'a> Step<'a> {
    fn new(rocks: &'a TempStorage, lignin: &'a Lignin) -> Self {
        Self {
            rocks,
            rocks_transaction: rocks.start_transaction(),
            storage_batch: StorageBatch::new(),
            lignin,
            transaction: lignin.path().begin(),
            staged: LigninBatch::new(),
        }
    }

    /// A deferred context of [`PLACE`] on RocksDB, which stages in the
    /// step's batch.
    fn on_rocks(&self) -> PrefixedRocksDbTransactionContext<'_> {
        self.rocks
            .get_transactional_storage_context_by_subtree_prefix(
                PLACE,
                Some(&self.storage_batch),
                &self.rocks_transaction,
            )
            .unwrap()
    }

    /// A deferred handle of [`PLACE`] on LigninDB, which stages in the
    /// step's batch.
    fn on_lignin(&self) -> LigninNodeStore<'_> {
        LigninNodeStore::deferred(
            &self.transaction,
            LigninSubtree::alone(PLACE),
            Some(&self.staged),
        )
    }

    /// Applies both batches and commits both transactions, and returns
    /// what the application of each batch was charged: RocksDB's first.
    fn commit(self) -> (OperationCost, OperationCost) {
        let on_rocks = self
            .rocks
            .commit_multi_context_batch(self.storage_batch, Some(&self.rocks_transaction));
        let on_lignin = self.staged.apply(&self.transaction);
        (on_rocks.value).expect("the batch is applied on RocksDB");
        (on_lignin.value).expect("the batch is applied on LigninDB");
        self.rocks
            .commit_transaction(self.rocks_transaction)
            .unwrap()
            .expect("the transaction commits on RocksDB");
        let committed = self.lignin.commit(&self.transaction);
        assert!(matches!(committed, Outcome::Committed(_)), "{committed:?}");
        (on_rocks.cost, on_lignin.cost)
    }
}

/// A node with no child.
fn lone(key: &[u8], value: &[u8]) -> TreeNode {
    TreeNode::new(
        key.to_vec(),
        value.to_vec(),
        None,
        TreeFeatureType::BasicMerkNode,
    )
    .unwrap()
}

/// Commits a change set that puts `node` and states no root. Where the
/// Merk has no root, no node links it: it is a record under its key.
fn put_unlinked<'db, S: NodeStore<'db>>(store: &S, node: &TreeNode) {
    let mut change_set = store.change_set();
    let prepared = change_set.prepare_node(node, NodeChange::ValueWritten);
    change_set.put_node(node.key(), prepared, None).unwrap();
    store.commit(change_set).unwrap().expect("a commit");
}

/// A Merk with no root that holds a record no node links is cleared as
/// the flat records of its keyspace (`FlatStore::clear`). RocksDB walks
/// the record and removes it at the length of Merk's record of the node.
/// LigninDB keeps such a record as a flat record in a form of its own, and
/// a Merk with no root is cleared by the walk of its flat records. The
/// walk reads keys and is charged the same on both. The removal, which
/// comes with no cost, is charged from the record as each engine keeps
/// it: on LigninDB at the length the record is stored in. Both leave no
/// record. (`GROVE-04-F1` changes it: a Merk with no root that still
/// holds such records is cleared by its node cursor.)
#[test]
fn a_flat_clear_of_a_merk_with_no_root_charges_an_unlinked_record_at_its_stored_length() {
    let rocks = TempStorage::new();
    let lignin = Lignin::with_stated_value_hashes();
    let node = lone(b"a record no node links", &[0xa5; 90]);
    let record_len = node.encode().len() as u64;

    let step = Step::new(&rocks, &lignin);
    put_unlinked(&step.on_rocks(), &node);
    put_unlinked(&step.on_lignin(), &node);
    let (on_rocks, on_lignin) = step.commit();
    assert_eq!(on_lignin, on_rocks, "the put of the record");

    let step = Step::new(&rocks, &lignin);
    // The Merk has no root, and both engines read the record by its key
    // at the length of Merk's record.
    let entry = step.on_lignin().entry().unwrap();
    assert_eq!(entry.expect("the subtree is entered").root_key, None);
    let read = |cost: OperationCost| (cost.seek_count, cost.storage_loaded_bytes);
    assert_eq!(
        read(step.on_rocks().has_node(node.key()).cost),
        (1, record_len)
    );
    assert_eq!(
        read(step.on_lignin().has_node(node.key()).cost),
        (1, record_len)
    );
    // The length the record is stored in on LigninDB, which for this
    // node, with no child and no aggregate, is one byte more.
    let stored = FlatStore::get(&step.on_lignin(), node.key()).unwrap();
    let stored_len = stored.expect("a read").expect("the record").len() as u64;
    assert_eq!(stored_len, record_len + 1);

    let walk_on_rocks = FlatStore::clear(&step.on_rocks());
    let walk_on_lignin = FlatStore::clear(&step.on_lignin());
    (walk_on_rocks.value).expect("a clear on RocksDB");
    (walk_on_lignin.value).expect("a clear on LigninDB");
    // The walk reads keys and no record, and is charged the same on both.
    assert_eq!(walk_on_lignin.cost, walk_on_rocks.cost, "the walk");

    // The removal: charged from the committed record, at each engine's
    // length of it.
    let (on_rocks, on_lignin) = step.commit();
    let removed = |cost: &OperationCost| {
        (
            cost.seek_count,
            cost.storage_loaded_bytes,
            cost.storage_cost.removed_bytes.total_removed_bytes() as u64,
        )
    };
    let (seeks, loaded, freed) = removed(&on_rocks);
    assert_eq!(loaded, record_len);
    assert_eq!(
        removed(&on_lignin),
        (
            seeks,
            stored_len,
            freed - record_len - required_space(record_len)
                + stored_len
                + required_space(stored_len)
        ),
        "{on_lignin:?} on LigninDB, {on_rocks:?} on RocksDB"
    );

    // Neither holds the record afterwards.
    let step = Step::new(&rocks, &lignin);
    assert!(!step
        .on_rocks()
        .has_node(node.key())
        .unwrap()
        .expect("a read"));
    assert!(!step
        .on_lignin()
        .has_node(node.key())
        .unwrap()
        .expect("a read"));
}

/// The bytes that the length of a record of `len` bytes takes in what a
/// removal frees.
fn required_space(len: u64) -> u64 {
    use integer_encoding::VarInt;

    len.required_space() as u64
}

/// The node cursor of a subtree that keeps flat records and no Merk, as
/// an append-only tree does. RocksDB keeps both kinds of record under one
/// prefix, and its cursor shows the flat record with its bytes. LigninDB's
/// node cursor shows the key of every flat record of the data column and
/// the value only of one that is in the form of an unlinked record: of
/// another, as the record of this test, it gives no value, and says why
/// in its status. (`MERK-05-F1` part 2 changes it, D-197.)
#[test]
fn a_node_cursor_over_flat_records_shows_the_key_and_refuses_the_value() {
    let rocks = TempStorage::new();
    let lignin = Lignin::with_stated_value_hashes();
    let (key, value) = (b"a flat record".to_vec(), vec![0x5a; 40]);

    let step = Step::new(&rocks, &lignin);
    StorageContext::put(&step.on_rocks(), &key, &value, None, None)
        .unwrap()
        .expect("a put on RocksDB");
    FlatStore::put(&step.on_lignin(), &key, &value, None)
        .unwrap()
        .expect("a put on LigninDB");
    step.commit();

    let step = Step::new(&rocks, &lignin);
    let mut on_rocks = NodeStore::cursor(&step.on_rocks());
    let mut on_lignin = NodeStore::cursor(&step.on_lignin());
    on_rocks.seek_to_first().unwrap();
    on_lignin.seek_to_first().unwrap();
    // Both stand on the record's key.
    assert!(on_rocks.valid().unwrap() && on_lignin.valid().unwrap());
    assert_eq!(on_rocks.key().unwrap(), Some(key.as_slice()));
    assert_eq!(on_lignin.key().unwrap(), Some(key.as_slice()));
    assert!(on_rocks.status().is_ok() && on_lignin.status().is_ok());
    // RocksDB shows its bytes, and LigninDB none, with the reason.
    assert_eq!(on_rocks.value().unwrap(), Some(value.as_slice()));
    assert!(on_rocks.status().is_ok());
    assert_eq!(on_lignin.value().unwrap(), None);
    let refused = on_lignin.status().expect_err("a record that is no node");
    assert!(
        matches!(&refused, grovedb_storage::Error::StorageError(_)),
        "{refused:?}"
    );
    // The flat side of the handle reads the record on both.
    for read in [
        FlatStore::get(&step.on_rocks(), &key),
        FlatStore::get(&step.on_lignin(), &key),
    ] {
        assert_eq!(read.unwrap().expect("a read"), Some(value.clone()));
    }
    // A step from the key comes to the end of the records on both.
    on_rocks.next().unwrap();
    on_lignin.next().unwrap();
    assert!(!on_rocks.valid().unwrap() && !on_lignin.valid().unwrap());
}
