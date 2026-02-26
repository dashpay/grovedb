#[cfg(test)]
mod storage_tests {
    use std::collections::BTreeMap;
    use std::marker::PhantomData;

    use grovedb_bulk_append_tree::BulkAppendTree;
    use grovedb_costs::{
        storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
        CostResult, CostsExt, OperationCost,
    };
    use grovedb_storage::StorageContext;

    use crate::commitment_tree::*;
    use crate::{CommitmentFrontier, DashMemo, MemoSize, NoteBytesData, TransmittedNoteCiphertext};

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

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Create a deterministic test leaf from an index.
    fn test_leaf(index: u64) -> [u8; 32] {
        use incrementalmerkletree::{Hashable, Level};
        use orchard::tree::MerkleHashOrchard;

        let empty = MerkleHashOrchard::empty_leaf();
        let varied =
            MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty);
        MerkleHashOrchard::combine(Level::from(0), &empty, &varied).to_bytes()
    }

    /// Create a deterministic test ciphertext for DashMemo from an index.
    ///
    /// Layout: `epk_bytes (32) || enc_ciphertext (104) || out_ciphertext (80)` = 216 bytes.
    fn test_ciphertext(index: u8) -> TransmittedNoteCiphertext<DashMemo> {
        let mut epk_bytes = [0u8; 32];
        epk_bytes[0] = index;
        epk_bytes[31] = 0xEE;
        epk_bytes[1] = index.wrapping_add(1);

        let mut enc_data = [0u8; 104];
        enc_data[0] = index;
        enc_data[1] = 0xEC;
        let enc_ciphertext = NoteBytesData(enc_data);

        let mut out_ciphertext = [0u8; 80];
        out_ciphertext[0] = index;
        out_ciphertext[1] = 0x0C;

        TransmittedNoteCiphertext::from_parts(epk_bytes, enc_ciphertext, out_ciphertext)
    }

    /// Default chunk_power for tests (height=1 → capacity=1, epoch_size=2).
    const TEST_CHUNK_POWER: u8 = 1;

    // ── Tests ───────────────────────────────────────────────────────────

    #[test]
    fn test_open_empty_store() {
        let ctx = MockDataStorageContext::new();
        let result = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx);
        let ct = result.value.expect("open should succeed on empty store");

        assert_eq!(
            ct.position(),
            None,
            "empty frontier should have no position"
        );
        assert_eq!(ct.tree_size(), 0, "empty frontier should have size 0");
        assert_eq!(ct.total_count(), 0, "total_count should be 0");
        assert!(
            result.cost.seek_count > 0,
            "open should report non-zero seek_count"
        );
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let ctx = MockDataStorageContext::new();

        // Build a frontier with several leaves, save, then re-open
        let result = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx);
        let mut ct = result.value.expect("open should succeed");
        for i in 0..20u64 {
            ct.append(test_leaf(i), &test_ciphertext(i as u8))
                .value
                .expect("append should succeed");
        }
        let expected_root = ct.root_hash();
        let expected_position = ct.position();
        let expected_size = ct.tree_size();
        let expected_total_count = ct.total_count();

        // Save
        let save_result = ct.save();
        save_result.value.expect("save should succeed");
        assert!(
            save_result.cost.seek_count > 0,
            "save should report non-zero seek_count"
        );

        // Re-open from the same storage (extract from bulk tree)
        let storage = ct.bulk_tree.dense_tree.storage;
        let load_result =
            CommitmentTree::<_, DashMemo>::open(expected_total_count, TEST_CHUNK_POWER, storage);
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
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        // Save empty
        ct.save().value.expect("save empty should succeed");

        // Append and save again (overwrites)
        ct.append(test_leaf(0), &test_ciphertext(0))
            .value
            .expect("append should succeed");
        let expected_root = ct.root_hash();
        let total_count = ct.total_count();
        ct.save().value.expect("save non-empty should succeed");

        // Re-open should return the latest (non-empty) frontier
        let storage = ct.bulk_tree.dense_tree.storage;
        let loaded = CommitmentTree::<_, DashMemo>::open(total_count, TEST_CHUNK_POWER, storage)
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
        let result = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx);
        assert!(
            result.value.is_err(),
            "should return error for corrupted data"
        );
    }

    #[test]
    fn test_open_storage_error_surfaces() {
        let ctx = FailingDataStorageContext;
        let result = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx);
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
        let bulk_tree = BulkAppendTree::new(TEST_CHUNK_POWER, FailingDataStorageContext)
            .expect("bulk tree new should succeed");
        let ct: CommitmentTree<_, DashMemo> = CommitmentTree {
            frontier: CommitmentFrontier::new(),
            bulk_tree,
            _memo: PhantomData,
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
        let ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        ct.save().value.expect("save empty should succeed");

        let storage = ct.bulk_tree.dense_tree.storage;
        let loaded = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, storage)
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
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        for i in 0..500u64 {
            ct.append(test_leaf(i), &test_ciphertext(i as u8))
                .value
                .expect("append should succeed");
        }

        let total_count = ct.total_count();
        ct.save().value.expect("save should succeed");

        let storage = ct.bulk_tree.dense_tree.storage;
        let loaded =
            CommitmentTree::<_, DashMemo>::open(total_count, TEST_CHUNK_POWER, storage)
                .value
                .expect("open should succeed");

        // Build an identical frontier to compare root hashes
        let mut expected = CommitmentFrontier::new();
        for i in 0..500u64 {
            expected
                .append(test_leaf(i))
                .value
                .expect("append should succeed");
        }
        assert_eq!(loaded.root_hash(), expected.root_hash());
        assert_eq!(loaded.tree_size(), 500);
        assert_eq!(loaded.position(), Some(499));
    }

    #[test]
    fn test_append_returns_result_with_position() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        let r0 = ct
            .append(test_leaf(0), &test_ciphertext(0))
            .value
            .expect("first append");
        assert_eq!(r0.global_position, 0, "first append should be position 0");
        assert_ne!(r0.sinsemilla_root, [0u8; 32], "root should be non-zero");
        assert_ne!(
            r0.bulk_state_root,
            [0u8; 32],
            "state root should be non-zero"
        );

        let r1 = ct
            .append(test_leaf(1), &test_ciphertext(1))
            .value
            .expect("second append");
        assert_eq!(r1.global_position, 1, "second append should be position 1");
        assert_ne!(
            r1.sinsemilla_root, r0.sinsemilla_root,
            "roots should differ"
        );
    }

    #[test]
    fn test_new_creates_empty_tree() {
        let ctx = MockDataStorageContext::new();
        let ct = CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, ctx)
            .expect("new should succeed");

        assert_eq!(ct.position(), None);
        assert_eq!(ct.tree_size(), 0);
        assert_eq!(ct.total_count(), 0);
    }

    #[test]
    fn test_append_raw_rejects_wrong_payload_size() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        // Too small
        let result = ct.append_raw(test_leaf(0), &[0u8; 10]);
        let err = result.value.expect_err("should reject wrong size");
        let msg = format!("{}", err);
        assert!(
            msg.contains("invalid payload size"),
            "error message should mention payload size: {}",
            msg
        );

        // Too large
        let result = ct.append_raw(test_leaf(0), &[0u8; 300]);
        assert!(
            result.value.is_err(),
            "should reject payload that is too large"
        );

        // Exact correct size should succeed
        let expected_size = ciphertext_payload_size::<DashMemo>();
        let result = ct.append_raw(test_leaf(0), &vec![0u8; expected_size]);
        assert!(result.value.is_ok(), "correct size should succeed");
    }

    #[test]
    fn test_serialize_deserialize_ciphertext_roundtrip() {
        let ct = test_ciphertext(42);
        let bytes = serialize_ciphertext(&ct);
        assert_eq!(
            bytes.len(),
            ciphertext_payload_size::<DashMemo>(),
            "serialized size should match expected"
        );

        let deserialized: TransmittedNoteCiphertext<DashMemo> =
            deserialize_ciphertext(&bytes).expect("deserialization should succeed");
        assert_eq!(deserialized.epk_bytes, ct.epk_bytes);
        assert_eq!(
            deserialized.enc_ciphertext.as_ref(),
            ct.enc_ciphertext.as_ref()
        );
        assert_eq!(deserialized.out_ciphertext, ct.out_ciphertext);
    }
}
