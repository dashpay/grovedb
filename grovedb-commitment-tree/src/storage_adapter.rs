//! Storage adapter bridging GroveDB's `StorageContext` to commitment tree
//! frontier persistence.
//!
//! Provides [`CommitmentTree`], which owns both the in-memory
//! [`CommitmentFrontier`] and a `StorageContext`, combining state and storage
//! into a single struct. All operations return [`CostResult`] to propagate
//! storage costs.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{CommitmentFrontier, CommitmentTreeError};

/// Key used to store the serialized commitment frontier in data storage.
pub const COMMITMENT_TREE_DATA_KEY: &[u8] = b"__ct_data__";

/// Commitment tree combining in-memory frontier state with a storage context.
///
/// Owns both the [`CommitmentFrontier`] and the storage backend `S`. Follows
/// the same open→mutate→save pattern as `BulkAppendTree<S>` and `MMR`.
///
/// - [`open`](CommitmentTree::open) loads the frontier from storage (or starts
///   empty)
/// - [`append`](CommitmentTree::append) mutates the in-memory frontier only
/// - [`save`](CommitmentTree::save) persists the frontier back to storage
pub struct CommitmentTree<S> {
    frontier: CommitmentFrontier,
    storage: S,
}

impl<S> std::fmt::Debug for CommitmentTree<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitmentTree")
            .field("frontier", &self.frontier)
            .finish_non_exhaustive()
    }
}

impl<'db, S: StorageContext<'db>> CommitmentTree<S> {
    /// Load a commitment tree from storage, or start with an empty frontier if
    /// no data exists yet.
    ///
    /// Takes ownership of the storage context.
    pub fn open(storage: S) -> CostResult<Self, CommitmentTreeError> {
        let mut cost = OperationCost::default();
        let data = storage
            .get(COMMITMENT_TREE_DATA_KEY)
            .unwrap_add_cost(&mut cost);
        let frontier = match data {
            Ok(Some(bytes)) => match CommitmentFrontier::deserialize(&bytes) {
                Ok(f) => f,
                Err(e) => return Err(e).wrap_with_cost(cost),
            },
            Ok(None) => CommitmentFrontier::new(),
            Err(e) => {
                return Err(CommitmentTreeError::InvalidData(format!(
                    "storage error loading frontier: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        Ok(Self { frontier, storage }).wrap_with_cost(cost)
    }

    /// Append a commitment (cmx) to the in-memory frontier.
    ///
    /// Returns the new Sinsemilla root hash. Call [`save`](Self::save) to
    /// persist the updated state.
    pub fn append(&mut self, cmx: [u8; 32]) -> CostResult<[u8; 32], CommitmentTreeError> {
        self.frontier.append(cmx)
    }

    /// Persist the current frontier state to storage.
    pub fn save(&self) -> CostResult<(), CommitmentTreeError> {
        let mut cost = OperationCost::default();
        let serialized = self.frontier.serialize();
        let result = self
            .storage
            .put(COMMITMENT_TREE_DATA_KEY, &serialized, None, None)
            .unwrap_add_cost(&mut cost);
        match result {
            Ok(()) => Ok(()).wrap_with_cost(cost),
            Err(e) => Err(CommitmentTreeError::InvalidData(format!(
                "storage error saving frontier: {}",
                e
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Get the current Sinsemilla root hash as 32 bytes.
    pub fn root_hash(&self) -> [u8; 32] {
        self.frontier.root_hash()
    }

    /// Get the current root as an Orchard `Anchor`.
    pub fn anchor(&self) -> crate::Anchor {
        self.frontier.anchor()
    }

    /// Get the position of the most recently appended leaf, or `None` if empty.
    pub fn position(&self) -> Option<u64> {
        self.frontier.position()
    }

    /// Get the number of leaves that have been appended.
    pub fn tree_size(&self) -> u64 {
        self.frontier.tree_size()
    }

    /// Borrow the underlying storage context.
    pub fn storage(&self) -> &S {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use grovedb_costs::{
        storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
        CostResult, CostsExt, OperationCost,
    };
    use grovedb_storage::StorageContext;

    use super::*;
    use crate::CommitmentFrontier;

    // ── Mock StorageContext with working data storage ─────────────────────

    /// In-memory key-value store implementing `StorageContext`.
    ///
    /// Only `get` and `put` are functional — the rest are stubs
    /// since `CommitmentTree` only uses data storage operations.
    struct MockDataStorageContext {
        data: std::cell::RefCell<BTreeMap<Vec<u8>, Vec<u8>>>,
    }

    impl MockDataStorageContext {
        fn new() -> Self {
            Self {
                data: std::cell::RefCell::new(BTreeMap::new()),
            }
        }

        /// Create a context pre-seeded with raw bytes at the given key.
        fn with_raw_data(key: &[u8], value: Vec<u8>) -> Self {
            let mut data = BTreeMap::new();
            data.insert(key.to_vec(), value);
            Self {
                data: std::cell::RefCell::new(data),
            }
        }
    }

    struct StubBatch;

    impl grovedb_storage::Batch for StubBatch {
        fn put<K: AsRef<[u8]>>(
            &mut self,
            _key: K,
            _value: &[u8],
            _children_sizes: ChildrenSizesWithIsSumTree,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            Ok(())
        }

        fn put_aux<K: AsRef<[u8]>>(
            &mut self,
            _key: K,
            _value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            Ok(())
        }

        fn put_root<K: AsRef<[u8]>>(
            &mut self,
            _key: K,
            _value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            Ok(())
        }

        fn delete<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {}

        fn delete_aux<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {
        }

        fn delete_root<K: AsRef<[u8]>>(
            &mut self,
            _key: K,
            _cost_info: Option<KeyValueStorageCost>,
        ) {
        }
    }

    struct StubRawIterator;

    impl grovedb_storage::RawIterator for StubRawIterator {
        fn seek_to_first(&mut self) -> CostContext<()> {
            CostContext {
                value: (),
                cost: Default::default(),
            }
        }

        fn seek_to_last(&mut self) -> CostContext<()> {
            CostContext {
                value: (),
                cost: Default::default(),
            }
        }

        fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
            CostContext {
                value: (),
                cost: Default::default(),
            }
        }

        fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
            CostContext {
                value: (),
                cost: Default::default(),
            }
        }

        fn next(&mut self) -> CostContext<()> {
            CostContext {
                value: (),
                cost: Default::default(),
            }
        }

        fn prev(&mut self) -> CostContext<()> {
            CostContext {
                value: (),
                cost: Default::default(),
            }
        }

        fn value(&self) -> CostContext<Option<&[u8]>> {
            CostContext {
                value: None,
                cost: Default::default(),
            }
        }

        fn key(&self) -> CostContext<Option<&[u8]>> {
            CostContext {
                value: None,
                cost: Default::default(),
            }
        }

        fn valid(&self) -> CostContext<bool> {
            CostContext {
                value: false,
                cost: Default::default(),
            }
        }
    }

    impl<'db> StorageContext<'db> for MockDataStorageContext {
        type Batch = StubBatch;
        type RawIterator = StubRawIterator;

        fn put<K: AsRef<[u8]>>(
            &self,
            key: K,
            value: &[u8],
            _children_sizes: ChildrenSizesWithIsSumTree,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            self.data
                .borrow_mut()
                .insert(key.as_ref().to_vec(), value.to_vec());
            Ok(()).wrap_with_cost(OperationCost {
                seek_count: 1,
                ..Default::default()
            })
        }

        fn get<K: AsRef<[u8]>>(
            &self,
            key: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            let store = self.data.borrow();
            let val = store.get(key.as_ref()).cloned();
            let loaded = val.as_ref().map_or(0, |v| v.len() as u64);
            Ok(val).wrap_with_cost(OperationCost {
                seek_count: 1,
                storage_loaded_bytes: loaded,
                ..Default::default()
            })
        }

        fn put_aux<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn put_root<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn put_meta<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete_aux<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete_root<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete_meta<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn get_aux<K: AsRef<[u8]>>(
            &self,
            _key: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Ok(None).wrap_with_cost(Default::default())
        }

        fn get_root<K: AsRef<[u8]>>(
            &self,
            _key: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Ok(None).wrap_with_cost(Default::default())
        }

        fn get_meta<K: AsRef<[u8]>>(
            &self,
            _key: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Ok(None).wrap_with_cost(Default::default())
        }

        fn new_batch(&self) -> Self::Batch {
            StubBatch
        }

        fn commit_batch(&self, _batch: Self::Batch) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn raw_iter(&self) -> Self::RawIterator {
            StubRawIterator
        }
    }

    // ── Failing mock for error paths ────────────────────────────────────

    /// Mock StorageContext that returns errors for get and put.
    struct FailingDataStorageContext;

    impl<'db> StorageContext<'db> for FailingDataStorageContext {
        type Batch = StubBatch;
        type RawIterator = StubRawIterator;

        fn get<K: AsRef<[u8]>>(
            &self,
            _key: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Err(grovedb_storage::Error::StorageError("get failed".into()))
                .wrap_with_cost(Default::default())
        }

        fn put<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _value: &[u8],
            _c: ChildrenSizesWithIsSumTree,
            _i: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Err(grovedb_storage::Error::StorageError("put failed".into()))
                .wrap_with_cost(Default::default())
        }

        fn get_aux<K: AsRef<[u8]>>(
            &self,
            _key: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Ok(None).wrap_with_cost(Default::default())
        }

        fn put_aux<K: AsRef<[u8]>>(
            &self,
            _key: K,
            _value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn put_root<K: AsRef<[u8]>>(
            &self,
            _k: K,
            _v: &[u8],
            _c: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn put_meta<K: AsRef<[u8]>>(
            &self,
            _k: K,
            _v: &[u8],
            _c: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete<K: AsRef<[u8]>>(
            &self,
            _k: K,
            _c: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete_aux<K: AsRef<[u8]>>(
            &self,
            _k: K,
            _c: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete_root<K: AsRef<[u8]>>(
            &self,
            _k: K,
            _c: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn delete_meta<K: AsRef<[u8]>>(
            &self,
            _k: K,
            _c: Option<KeyValueStorageCost>,
        ) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn get_root<K: AsRef<[u8]>>(
            &self,
            _k: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Ok(None).wrap_with_cost(Default::default())
        }

        fn get_meta<K: AsRef<[u8]>>(
            &self,
            _k: K,
        ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
            Ok(None).wrap_with_cost(Default::default())
        }

        fn new_batch(&self) -> Self::Batch {
            StubBatch
        }

        fn commit_batch(&self, _batch: Self::Batch) -> CostResult<(), grovedb_storage::Error> {
            Ok(()).wrap_with_cost(Default::default())
        }

        fn raw_iter(&self) -> Self::RawIterator {
            StubRawIterator
        }
    }

    // ── Helper ──────────────────────────────────────────────────────────

    /// Create a deterministic test leaf from an index.
    fn test_leaf(index: u64) -> [u8; 32] {
        use incrementalmerkletree::{Hashable, Level};
        use orchard::tree::MerkleHashOrchard;

        let empty = MerkleHashOrchard::empty_leaf();
        let varied =
            MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty);
        MerkleHashOrchard::combine(Level::from(0), &empty, &varied).to_bytes()
    }

    // ── Tests ───────────────────────────────────────────────────────────

    #[test]
    fn test_open_empty_store() {
        let ctx = MockDataStorageContext::new();
        let result = CommitmentTree::open(ctx);
        let ct = result.value.expect("open should succeed on empty store");

        assert_eq!(
            ct.position(),
            None,
            "empty frontier should have no position"
        );
        assert_eq!(ct.tree_size(), 0, "empty frontier should have size 0");
        assert!(
            result.cost.seek_count > 0,
            "open should report non-zero seek_count"
        );
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let ctx = MockDataStorageContext::new();

        // Build a frontier with several leaves, save, then re-open
        let result = CommitmentTree::open(ctx);
        let mut ct = result.value.expect("open should succeed");
        for i in 0..20u64 {
            ct.append(test_leaf(i)).value.expect("append");
        }
        let expected_root = ct.root_hash();
        let expected_position = ct.position();
        let expected_size = ct.tree_size();

        // Save
        let save_result = ct.save();
        save_result.value.expect("save should succeed");
        assert!(
            save_result.cost.seek_count > 0,
            "save should report non-zero seek_count"
        );

        // Re-open from the same storage
        let storage = ct.storage;
        let load_result = CommitmentTree::open(storage);
        let loaded = load_result.value.expect("open should succeed");

        assert_eq!(loaded.root_hash(), expected_root, "root hash should match");
        assert_eq!(
            loaded.position(),
            expected_position,
            "position should match"
        );
        assert_eq!(loaded.tree_size(), expected_size, "tree size should match");
        assert!(
            load_result.cost.storage_loaded_bytes > 0,
            "open should report non-zero loaded bytes"
        );
    }

    #[test]
    fn test_save_overwrite_and_load() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::open(ctx)
            .value
            .expect("open should succeed");

        // Save empty
        ct.save().value.expect("save empty should succeed");

        // Append and save again (overwrites)
        ct.append(test_leaf(0)).value.expect("append");
        let expected_root = ct.root_hash();
        ct.save().value.expect("save non-empty should succeed");

        // Re-open should return the latest (non-empty) frontier
        let storage = ct.storage;
        let loaded = CommitmentTree::open(storage)
            .value
            .expect("open should succeed");
        assert_eq!(
            loaded.root_hash(),
            expected_root,
            "should load the overwritten frontier"
        );
    }

    #[test]
    fn test_open_corrupted_data_returns_error() {
        let ctx =
            MockDataStorageContext::with_raw_data(COMMITMENT_TREE_DATA_KEY, vec![0x01, 0x02, 0x03]);
        let result = CommitmentTree::open(ctx);
        assert!(
            result.value.is_err(),
            "should return error for corrupted data"
        );
    }

    #[test]
    fn test_open_storage_error_surfaces() {
        let ctx = FailingDataStorageContext;
        let result = CommitmentTree::open(ctx);
        assert!(result.value.is_err(), "should surface storage get error");
        let err_msg = format!("{}", result.value.expect_err("should be storage error"));
        assert!(
            err_msg.contains("storage error loading frontier"),
            "error should contain context: {}",
            err_msg
        );
    }

    #[test]
    fn test_save_storage_error_surfaces() {
        // FailingDataStorageContext.get fails, so open() would fail.
        // Construct directly to test save() error path.
        let ct = CommitmentTree {
            frontier: CommitmentFrontier::new(),
            storage: FailingDataStorageContext,
        };
        let result = ct.save();
        assert!(result.value.is_err(), "should surface storage put error");
        let err_msg = format!("{}", result.value.expect_err("should be storage error"));
        assert!(
            err_msg.contains("storage error saving frontier"),
            "error should contain context: {}",
            err_msg
        );
    }

    #[test]
    fn test_save_empty_and_reopen() {
        let ctx = MockDataStorageContext::new();
        let ct = CommitmentTree::open(ctx)
            .value
            .expect("open should succeed");

        ct.save().value.expect("save empty should succeed");

        let storage = ct.storage;
        let loaded = CommitmentTree::open(storage)
            .value
            .expect("open should succeed");
        assert_eq!(
            loaded.position(),
            None,
            "loaded empty should have no position"
        );
        assert_eq!(
            loaded.root_hash(),
            CommitmentFrontier::new().root_hash(),
            "root hash should match"
        );
    }

    #[test]
    fn test_roundtrip_with_many_leaves() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::open(ctx)
            .value
            .expect("open should succeed");

        for i in 0..500u64 {
            ct.append(test_leaf(i)).value.expect("append");
        }

        ct.save().value.expect("save should succeed");

        let storage = ct.storage;
        let loaded = CommitmentTree::open(storage)
            .value
            .expect("open should succeed");

        // Build an identical frontier to compare root hashes
        let mut expected = CommitmentFrontier::new();
        for i in 0..500u64 {
            expected.append(test_leaf(i)).value.expect("append");
        }
        assert_eq!(loaded.root_hash(), expected.root_hash());
        assert_eq!(loaded.tree_size(), 500);
        assert_eq!(loaded.position(), Some(499));
    }
}
