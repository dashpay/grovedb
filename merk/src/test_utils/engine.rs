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

#[cfg(feature = "rocksdb_storage")]
mod rocksdb;
#[cfg(merk_test_engine)]
mod suite;

use std::path::Path;

use grovedb_costs::{CostContext, CostResult};
use grovedb_path::SubtreePath;
use grovedb_storage::Error;
use grovedb_version::version::GroveVersion;

#[cfg(feature = "rocksdb_storage")]
pub use self::rocksdb::{NodeStoreOnlyEngine, RocksDbTestEngine};
#[cfg(merk_test_engine)]
pub use self::suite::{
    SuiteBatch, SuiteEngine, SuiteImmediateSubtree, SuiteSubtree, SuiteTransaction, TempEngine,
};
use crate::{
    store::{NodeChange, NodeStore},
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Link,
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
        a_batch_is_staged_then_applied_then_committed::<RocksDbTestEngine>();
        a_batch_is_staged_then_applied_then_committed::<NodeStoreOnlyEngine>();
    }

    #[test]
    fn each_engine_writes_at_once_through_an_immediate_handle() {
        an_immediate_handle_writes_at_once::<RocksDbTestEngine>();
        an_immediate_handle_writes_at_once::<NodeStoreOnlyEngine>();
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
