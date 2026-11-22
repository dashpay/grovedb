//! The engines the Merk test suite runs on.
//!
//! A test of Merk asks one thing of what is underneath: handles of subtrees
//! that are a [`NodeStore`], inside a transaction, with writes that are staged
//! in a batch or made at once. [`TestEngine`] is that, and no more. A test
//! names [`TempEngine`], [`SuiteEngine`] and the aliases below, never a
//! storage type, so it runs on every engine of `merk/test-engines.txt`.
//!
//! `cargo test -p grovedb-merk` runs the suite on the first engine of that
//! file, and `MERK_TEST_ENGINE=<name> cargo test -p grovedb-merk` on another.

//!
//! The suite has an engine where the Cargo feature of the selected engine is
//! on, and then the cfg `merk_test_engine` is set. Without it there is only
//! the trait, and the tests that open a store are left out of the build.

#[cfg(feature = "lignin_storage")]
mod lignin;
#[cfg(feature = "rocksdb_storage")]
mod rocksdb;
#[cfg(merk_test_engine)]
mod suite;

use std::path::Path;

use grovedb_costs::{CostContext, CostResult};
use grovedb_path::SubtreePath;
use grovedb_storage::Error;
use grovedb_version::version::GroveVersion;

#[cfg(feature = "lignin_storage")]
pub use self::lignin::{LigninTestEngine, LigninTestTransaction};
#[cfg(feature = "rocksdb_storage")]
pub use self::rocksdb::{NodeStoreOnlyEngine, RocksDbTestEngine};
#[cfg(merk_test_engine)]
pub use self::suite::{
    SuiteBatch, SuiteEngine, SuiteImmediateSubtree, SuiteSubtree, SuiteTransaction, TempEngine,
};
use crate::{
    store::{NodeChange, NodeStore},
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Link, TreeType,
};

/// What the Merk suite asks of the engine it runs on.
pub trait TestEngine: Sized {
    /// A transaction. Every handle is opened in one.
    type Transaction<'db>
    where
        Self: 'db;

    /// The writes of deferred handles, staged until [`Self::apply_batch`].
    type Batch: Default;

    /// A deferred handle of one subtree: its writes go to the batch it was
    /// opened with, and with no batch it is read-only.
    type Subtree<'a>: NodeStore<'a>
    where
        Self: 'a;

    /// An immediate handle of one subtree: it writes to the transaction at
    /// once and reads its own writes.
    type ImmediateSubtree<'a>: NodeStore<'a>
    where
        Self: 'a;

    /// Opens the database in the directory `path`, creating it if there is
    /// none.
    fn open(path: &Path) -> Self;

    /// Starts a write transaction.
    fn start_transaction(&self) -> Self::Transaction<'_>;

    /// Commits and ends a transaction.
    fn commit_transaction(&self, transaction: Self::Transaction<'_>) -> CostResult<(), Error>;

    /// A deferred handle of the subtree at `path`.
    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b;

    /// An immediate handle of the subtree at `path`.
    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b;

    /// Applies the writes staged in `batch` to the transaction.
    fn apply_batch(
        &self,
        batch: Self::Batch,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<(), Error>;
}

/// A handle that passes every call of [`NodeStore`] on to the handle it
/// wraps, and offers nothing else of it.
pub struct NodeStoreOnly<S> {
    pub(super) store: S,
}

impl<'db, S: NodeStore<'db>> NodeStore<'db> for NodeStoreOnly<S> {
    type ChangeSet = S::ChangeSet;
    type Cursor = S::Cursor;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, crate::Error> {
        self.store
            .get_node(key, value_defined_cost_fn, grove_version)
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, crate::Error> {
        self.store.get_node_for_read(key)
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, crate::Error> {
        self.store.has_node(key)
    }

    fn fetch_node(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, crate::Error> {
        self.store
            .fetch_node(link, value_defined_cost_fn, grove_version)
    }

    fn fetch_node_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, crate::Error> {
        self.store.fetch_node_without_value(link, grove_version)
    }

    fn load_node_value(&self, node: &mut TreeNode) -> CostResult<(), crate::Error> {
        self.store.load_node_value(node)
    }

    fn takes_links_to_absent_nodes(&self) -> bool {
        self.store.takes_links_to_absent_nodes()
    }

    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), crate::Error> {
        self.store.write_node(node, change)
    }

    fn state_tree_type(&self, tree_type: TreeType) -> Result<(), crate::Error> {
        self.store.state_tree_type(tree_type)
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, crate::Error> {
        self.store.root_key()
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), crate::Error> {
        self.store.set_root_key(root_key)
    }

    fn cursor(&self) -> Self::Cursor {
        self.store.cursor()
    }

    fn change_set(&self) -> Self::ChangeSet {
        self.store.change_set()
    }

    fn commit(&self, change_set: Self::ChangeSet) -> CostResult<(), crate::Error> {
        self.store.commit(change_set)
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, crate::Error> {
        self.store.get_aux_record(key)
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, crate::Error> {
        self.store.get_meta_record(key)
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), crate::Error> {
        self.store.put_meta_record(key, value)
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), crate::Error> {
        self.store.delete_meta_record(key)
    }
}

#[cfg(all(test, merk_test_engine))]
mod tests {
    use std::any::type_name;

    #[cfg(all(feature = "rocksdb_storage", feature = "lignin_storage"))]
    use grovedb_costs::OperationCost;
    use grovedb_path::SubtreePath;
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        test_utils::{make_batch_seq, root_merk, TempMerk},
        tree::kv::ValueDefinedCostType,
        Merk, TreeType,
    };

    const REGISTRY: &str = include_str!("../../test-engines.txt");

    fn registered() -> Vec<&'static str> {
        REGISTRY
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_whitespace().next())
            .collect()
    }

    /// The engine type of a registered name. An engine that is registered
    /// and has no line here fails the first test below.
    fn engine_type(name: &str) -> Option<&'static str> {
        match name {
            "rocksdb" => Some("RocksDbTestEngine"),
            "node-store-only" => Some("NodeStoreOnlyEngine"),
            "lignin" => Some("LigninTestEngine"),
            _ => None,
        }
    }

    #[test]
    fn every_registered_engine_has_a_type() {
        let registered = registered();
        assert!(!registered.is_empty());
        for name in &registered {
            assert!(engine_type(name).is_some(), "{name} has no engine type");
        }
        let mut unique = registered.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), registered.len(), "a name is registered twice");
    }

    #[test]
    fn the_suite_is_built_for_the_selected_engine() {
        let name = env!("MERK_SUITE_ENGINE");
        assert!(registered().contains(&name), "{name} is not registered");
        let named = std::env::var("MERK_TEST_ENGINE").unwrap_or_default();
        if named.is_empty() {
            assert_eq!(name, registered()[0], "the default is the first engine");
        } else {
            assert_eq!(name, named, "the suite was built for another engine");
        }
        let expected = engine_type(name).expect("a registered engine");
        let engine = type_name::<SuiteEngine>();
        assert!(
            engine.ends_with(expected),
            "{name} is {expected}, and the suite runs on {engine}"
        );
        let _: &Merk<SuiteSubtree<'static>> = &TempMerk::new(GroveVersion::latest());
        println!("merk suite engine: {name} ({engine})");
    }

    fn open<'db, S: NodeStore<'db>>(store: S) -> Merk<S> {
        Merk::open_base(
            store,
            TreeType::NormalTree,
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            GroveVersion::latest(),
        )
        .unwrap()
        .expect("the merk opens")
    }

    fn holds<'db, S: NodeStore<'db>>(merk: &Merk<S>, key: u64) -> bool {
        merk.get(
            &key.to_be_bytes(),
            true,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            GroveVersion::latest(),
        )
        .unwrap()
        .expect("a read")
        .is_some()
    }

    /// A deferred handle stages its writes in its batch: no handle reads
    /// them before the batch is applied, and the transaction's handles read
    /// them after. They are in the database once the transaction commits.
    fn a_batch_is_staged_then_applied_then_committed<E: TestEngine>() {
        let grove_version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = E::open(directory.path());
        let transaction = engine.start_transaction();
        let batch = E::Batch::default();

        let mut merk = root_merk(&engine, &transaction, Some(&batch), grove_version);
        merk.apply::<_, Vec<_>>(&make_batch_seq(0..10), &[], None, grove_version)
            .unwrap()
            .expect("the batch applies");
        drop(merk);
        assert!(!holds(
            &root_merk(&engine, &transaction, None, grove_version),
            3
        ));

        engine
            .apply_batch(batch, &transaction)
            .unwrap()
            .expect("the batch is applied");
        assert!(holds(
            &root_merk(&engine, &transaction, None, grove_version),
            3
        ));
        let other = engine.start_transaction();
        assert!(!holds(&root_merk(&engine, &other, None, grove_version), 3));
        drop(other);

        engine
            .commit_transaction(transaction)
            .unwrap()
            .expect("the transaction commits");
        drop(engine);
        let reopened = E::open(directory.path());
        let transaction = reopened.start_transaction();
        assert!(holds(
            &root_merk(&reopened, &transaction, None, grove_version),
            3
        ));
    }

    /// An immediate handle writes to its transaction at once, and a subtree
    /// at another path does not hold what it wrote.
    fn an_immediate_handle_writes_at_once<E: TestEngine>() {
        let grove_version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = E::open(directory.path());
        let transaction = engine.start_transaction();

        let mut merk = open(
            engine
                .immediate_subtree(SubtreePath::empty(), &transaction)
                .unwrap(),
        );
        merk.apply::<_, Vec<_>>(&make_batch_seq(0..10), &[], None, grove_version)
            .unwrap()
            .expect("the batch applies");
        drop(merk);

        assert!(holds(
            &root_merk(&engine, &transaction, None, grove_version),
            3
        ));
        let elsewhere: &[&[u8]] = &[b"elsewhere"];
        assert!(!holds(
            &open(
                engine
                    .subtree(elsewhere.into(), None, &transaction)
                    .unwrap()
            ),
            3
        ));
    }

    #[test]
    fn each_engine_stages_applies_and_commits_a_batch() {
        #[cfg(feature = "rocksdb_storage")]
        a_batch_is_staged_then_applied_then_committed::<RocksDbTestEngine>();
        #[cfg(feature = "rocksdb_storage")]
        a_batch_is_staged_then_applied_then_committed::<NodeStoreOnlyEngine>();
        #[cfg(feature = "lignin_storage")]
        a_batch_is_staged_then_applied_then_committed::<LigninTestEngine>();
    }

    #[test]
    fn each_engine_writes_at_once_through_an_immediate_handle() {
        #[cfg(feature = "rocksdb_storage")]
        an_immediate_handle_writes_at_once::<RocksDbTestEngine>();
        #[cfg(feature = "rocksdb_storage")]
        an_immediate_handle_writes_at_once::<NodeStoreOnlyEngine>();
        #[cfg(feature = "lignin_storage")]
        an_immediate_handle_writes_at_once::<LigninTestEngine>();
    }

    /// Two transactions that are the first to write into one subtree,
    /// under keys of their own, both commit. Two that are the first to
    /// write into another subtree, under one key, do not: the commit of
    /// the second is refused, and the key holds what the first wrote.
    fn first_writers_of_one_subtree_commit_unless_their_keys_meet<E: TestEngine>() {
        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = E::open(directory.path());
        let (apart, met): (&[&[u8]], &[&[u8]]) = (&[b"keys apart"], &[b"keys that meet"]);
        let write = |transaction: &E::Transaction<'_>, path: &[&[u8]], key: &[u8], value: &[u8]| {
            let handle = engine.immediate_subtree(path.into(), transaction).unwrap();
            let written = handle.put_meta_record(key, value);
            written.unwrap().expect("a write");
        };

        let (first, second) = (engine.start_transaction(), engine.start_transaction());
        write(&first, apart, b"of the first", b"1");
        write(&second, apart, b"of the second", b"2");
        let committed = engine.commit_transaction(first).unwrap();
        committed.expect("the first commits");
        let committed = engine.commit_transaction(second).unwrap();
        committed.expect("the second commits");

        let (first, second) = (engine.start_transaction(), engine.start_transaction());
        write(&first, met, b"of both", b"1");
        write(&second, met, b"of both", b"2");
        let committed = engine.commit_transaction(first).unwrap();
        committed.expect("the first commits");
        let refused = engine.commit_transaction(second).unwrap();
        assert!(refused.is_err(), "a commit over a commit of its key");

        let transaction = engine.start_transaction();
        let read = |path: &[&[u8]], key: &[u8]| {
            let reader = engine.subtree(path.into(), None, &transaction).unwrap();
            reader.get_meta_record(key).unwrap().expect("a read")
        };
        assert_eq!(read(apart, b"of the first"), Some(b"1".to_vec()));
        assert_eq!(read(apart, b"of the second"), Some(b"2".to_vec()));
        assert_eq!(read(met, b"of both"), Some(b"1".to_vec()));
    }

    #[test]
    fn first_writers_of_one_subtree_commit_on_each_engine_unless_their_keys_meet() {
        #[cfg(feature = "rocksdb_storage")]
        first_writers_of_one_subtree_commit_unless_their_keys_meet::<RocksDbTestEngine>();
        #[cfg(feature = "rocksdb_storage")]
        first_writers_of_one_subtree_commit_unless_their_keys_meet::<NodeStoreOnlyEngine>();
        #[cfg(feature = "lignin_storage")]
        first_writers_of_one_subtree_commit_unless_their_keys_meet::<LigninTestEngine>();
    }

    /// What the calls of a test cost on an engine, and the root hashes
    /// they leave: the open of a handle of each kind, the open of a Merk
    /// on it, writes through a batch and at once, the application of a
    /// batch and the commit of a transaction. The last batch clears a Merk
    /// of which the transaction rewrote nodes at once, with values of
    /// another length: a removal that comes with no cost is charged from
    /// the committed record, whatever the transaction holds.
    #[cfg(all(feature = "rocksdb_storage", feature = "lignin_storage"))]
    fn costs_and_roots<E: TestEngine>() -> (Vec<OperationCost>, Vec<crate::CryptoHash>) {
        use crate::{Op, TreeFeatureType::BasicMerkNode};

        /// The answer of a call, with what it cost kept.
        fn take<T>(answer: CostContext<T>, costs: &mut Vec<OperationCost>) -> T {
            let CostContext { value, cost } = answer;
            costs.push(cost);
            value
        }
        fn open<'db, S: NodeStore<'db>>(store: S, costs: &mut Vec<OperationCost>) -> Merk<S> {
            let opened = Merk::open_base(
                store,
                TreeType::NormalTree,
                None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                GroveVersion::latest(),
            );
            take(opened, costs).expect("the merk opens")
        }

        let grove_version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = E::open(directory.path());
        let (mut costs, mut roots) = (Vec::new(), Vec::new());
        let path: &[&[u8]] = &[b"a tree", b"below it"];

        // A batch through a deferred handle, applied and committed.
        let transaction = engine.start_transaction();
        let batch = E::Batch::default();
        let handle = engine.subtree(path.into(), Some(&batch), &transaction);
        let mut merk = open(take(handle, &mut costs), &mut costs);
        let applied = merk.apply::<_, Vec<_>>(&make_batch_seq(0..40), &[], None, grove_version);
        take(applied, &mut costs).expect("the batch applies");
        roots.push(merk.root_hash().unwrap());
        drop(merk);
        let applied = engine.apply_batch(batch, &transaction);
        take(applied, &mut costs).expect("the batch is applied");
        let committed = engine.commit_transaction(transaction);
        take(committed, &mut costs).expect("the transaction commits");

        // Writes at once through an immediate handle: some nodes get a
        // value of another length, and some are removed.
        let transaction = engine.start_transaction();
        let handle = engine.immediate_subtree(path.into(), &transaction);
        let mut merk = open(take(handle, &mut costs), &mut costs);
        let mut rewritten: Vec<_> = (10..20u64)
            .map(|n| {
                (
                    n.to_be_bytes().to_vec(),
                    Op::Put(vec![7; 200], BasicMerkNode),
                )
            })
            .collect();
        rewritten.extend((30..35u64).map(|n| (n.to_be_bytes().to_vec(), Op::Delete)));
        let applied = merk.apply::<_, Vec<_>>(&rewritten, &[], None, grove_version);
        take(applied, &mut costs).expect("the batch applies");
        roots.push(merk.root_hash().unwrap());
        drop(merk);

        // A clear through a deferred handle of the same transaction.
        let batch = E::Batch::default();
        let handle = engine.subtree(path.into(), Some(&batch), &transaction);
        let mut merk = open(take(handle, &mut costs), &mut costs);
        take(merk.clear(), &mut costs).expect("the merk is cleared");
        roots.push(merk.root_hash().unwrap());
        drop(merk);
        let applied = engine.apply_batch(batch, &transaction);
        take(applied, &mut costs).expect("the batch is applied");
        let committed = engine.commit_transaction(transaction);
        take(committed, &mut costs).expect("the transaction commits");

        // What the next transaction reads.
        let transaction = engine.start_transaction();
        let handle = engine.subtree(path.into(), None, &transaction);
        let merk = open(take(handle, &mut costs), &mut costs);
        roots.push(merk.root_hash().unwrap());
        (costs, roots)
    }

    /// The LigninDB engine charges what the RocksDB engine charges, call
    /// for call, and leaves the same root hashes.
    #[cfg(all(feature = "rocksdb_storage", feature = "lignin_storage"))]
    #[test]
    fn the_lignin_engine_costs_what_the_rocksdb_engine_costs() {
        let (on_rocksdb, on_lignin) = (
            costs_and_roots::<RocksDbTestEngine>(),
            costs_and_roots::<LigninTestEngine>(),
        );
        for (call, (found, expected)) in on_lignin.0.iter().zip(&on_rocksdb.0).enumerate() {
            assert_eq!(found, expected, "the cost of call {call}");
        }
        assert_eq!(on_lignin, on_rocksdb);
        // The clear was charged for what it removed.
        let cleared = &on_lignin.0[11];
        assert!(cleared.storage_cost.removed_bytes.total_removed_bytes() > 0);
    }

    /// The LigninDB engine enters a subtree, as a Merk that stands alone,
    /// when a handle of it that writes is opened, and not for a handle
    /// that only reads. The enter is a write of the handle's transaction.
    #[cfg(feature = "lignin_storage")]
    #[test]
    fn the_lignin_engine_enters_the_subtree_of_a_handle_that_writes() {
        use crate::store::lignin::LigninEntry;

        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = LigninTestEngine::open(directory.path());
        let entry = |transaction: &LigninTestTransaction<'_>, path: &[&[u8]]| {
            let reader = engine.subtree(path.into(), None, transaction).unwrap();
            reader.entry().expect("the directory answers")
        };
        let entered = Some(LigninEntry {
            root_key: None,
            tree_type: LigninTestEngine::TREE_TYPE.to_vec(),
            host: None,
        });
        let (staged, at_once): (&[&[u8]], &[&[u8]]) = (&[b"staged"], &[b"at once"]);

        // The bytes a subtree is entered with are those of no tree type.
        assert!(entered
            .as_ref()
            .is_some_and(|entry| entry.tree_type().is_err()));

        let transaction = engine.start_transaction();
        assert_eq!(entry(&transaction, staged), None);
        let batch = <LigninTestEngine as TestEngine>::Batch::default();
        let handle = engine.subtree(staged.into(), Some(&batch), &transaction);
        drop(handle);
        assert_eq!(entry(&transaction, staged), entered);
        assert!(batch.is_empty(), "the enter is no write of the batch");
        assert_eq!(entry(&transaction, at_once), None);
        drop(engine.immediate_subtree(at_once.into(), &transaction));
        assert_eq!(entry(&transaction, at_once), entered);

        // Another transaction reads neither until the commit, and both
        // after it. Until then it refuses a batch that a handle of the
        // first staged, and the refusal is the answer of the application.
        let other = engine.start_transaction();
        assert_eq!(entry(&other, staged), None);
        let handle = engine.subtree(staged.into(), Some(&batch), &transaction);
        let written = handle.unwrap().put_meta_record(b"a key", b"a value");
        written.unwrap().expect("the write is staged");
        let refused = engine.apply_batch(batch, &other).unwrap();
        let refused = refused.expect_err("a batch of another transaction's handle");
        assert!(refused.to_string().contains("is not entered"), "{refused}");
        let committed = engine.commit_transaction(transaction).unwrap();
        committed.expect("the transaction commits");
        assert_eq!(entry(&other, staged), entered);
        assert_eq!(entry(&other, at_once), entered);
        // A handle of an entered subtree enters nothing: a transaction
        // that only opens one has nothing to commit.
        let version = engine.db().version();
        drop(engine.immediate_subtree(staged.into(), &other));
        let committed = engine.commit_transaction(other).unwrap();
        committed.expect("the transaction commits");
        assert_eq!(engine.db().version(), version);
    }

    /// A handle that passes the seam on passes on what a conversion
    /// states: the directory of LigninDB names the wide tree type behind
    /// a [`NodeStoreOnly`] as it does behind the handle itself.
    #[cfg(feature = "lignin_storage")]
    #[test]
    fn a_handle_that_passes_the_seam_on_states_a_conversion() {
        use crate::{tree::kv::KV, TreeFeatureType::SummedMerkNode};

        let grove_version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = LigninTestEngine::open(directory.path());
        let transaction = engine.start_transaction();
        let store = engine.immediate_subtree(SubtreePath::empty(), &transaction);
        let no_cost = None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
        let mut merk = Merk::open_base(
            NodeStoreOnly {
                store: store.unwrap(),
            },
            TreeType::SumTree,
            no_cost,
            grove_version,
        )
        .unwrap()
        .expect("the merk opens");
        let batch: Vec<_> = (0..9u8)
            .map(|n| {
                (
                    vec![n],
                    crate::Op::Put(vec![n; 9], SummedMerkNode(n.into())),
                )
            })
            .collect();
        merk.apply::<_, Vec<_>>(&batch, &[], None, grove_version)
            .unwrap()
            .expect("the batch applies");
        let node_type = TreeType::SumTree.inner_node_type();
        let converted = merk.convert_to_big_sum_tree(
            &|_key, _value, _widened| Ok(()),
            &|key, value| {
                let (key_len, value_len) = (key.len() as u32, value.len() as u32);
                Ok(KV::node_value_byte_cost_size(key_len, value_len, node_type))
            },
            no_cost.as_ref(),
            grove_version,
        );
        let converted = converted.unwrap().expect("the merk is converted");
        let entry = converted
            .storage
            .store
            .entry()
            .expect("the directory answers");
        let entry = entry.expect("the subtree is entered");
        assert_eq!(
            entry.tree_type().expect("a tree type"),
            TreeType::BigSumTree
        );
    }

    #[test]
    fn a_temp_engine_is_a_new_database_of_the_suite_engine() {
        let grove_version = GroveVersion::latest();
        let engine = TempEngine::default();
        let _: &SuiteEngine = &engine;
        let transaction: SuiteTransaction = engine.start_transaction();
        let batch = SuiteBatch::default();
        let merk: Merk<SuiteSubtree> =
            root_merk(&*engine, &transaction, Some(&batch), grove_version);
        assert!(merk.is_empty_tree().unwrap());
        let immediate: SuiteImmediateSubtree = engine
            .immediate_subtree(SubtreePath::empty(), &transaction)
            .unwrap();
        assert!(open(immediate).is_empty_tree().unwrap());
    }

    #[cfg(feature = "rocksdb_storage")]
    #[test]
    fn the_storage_of_the_rocksdb_engine_is_the_one_its_handles_write() {
        use grovedb_storage::{Storage, StorageContext};

        let grove_version = GroveVersion::latest();
        let directory = tempfile::TempDir::new().expect("a temporary directory");
        let engine = RocksDbTestEngine::open(directory.path());
        let transaction = engine.start_transaction();
        let mut merk = open(
            engine
                .immediate_subtree(SubtreePath::empty(), &transaction)
                .unwrap(),
        );
        merk.apply::<_, Vec<_>>(&make_batch_seq(0..3), &[], None, grove_version)
            .unwrap()
            .expect("the batch applies");
        drop(merk);

        let context = engine
            .storage()
            .get_immediate_storage_context(SubtreePath::empty(), &transaction)
            .unwrap();
        assert!(context
            .get(1_u64.to_be_bytes())
            .unwrap()
            .expect("a read")
            .is_some());
    }
}
