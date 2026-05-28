#[cfg(test)]
mod storage_tests {
    use std::{collections::BTreeMap, marker::PhantomData};

    use grovedb_bulk_append_tree::BulkAppendTree;
    use grovedb_costs::{
        storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
        CostResult, CostsExt, OperationCost,
    };
    use grovedb_storage::StorageContext;

    use crate::{
        commitment_tree::*, test_utils::test_leaf, CommitmentFrontier, DashMemo, NoteBytesData,
        TransmittedNoteCiphertext,
    };

    // ── Mock StorageContext with working data storage ─────────────────────

    /// In-memory key-value store implementing `StorageContext`.
    ///
    /// Only `get` and `put` are functional — the rest are stubs
    /// since `CommitmentTree` only uses data storage operations.
    struct MockDataStorageContext {
        data: std::cell::RefCell<BTreeMap<Vec<u8>, Vec<u8>>>,
        /// Toggle-able fault flags. Shared via `Rc` so a test can grab a handle
        /// before the context is moved into `CommitmentTree`, then flip the
        /// flag mid-test to exercise storage-error branches.
        fail_get: std::rc::Rc<std::cell::Cell<bool>>,
        fail_put: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl MockDataStorageContext {
        fn new() -> Self {
            Self {
                data: std::cell::RefCell::new(BTreeMap::new()),
                fail_get: Default::default(),
                fail_put: Default::default(),
            }
        }

        /// Create a context pre-seeded with raw bytes at the given key.
        fn with_raw_data(key: &[u8], value: Vec<u8>) -> Self {
            let mut data = BTreeMap::new();
            data.insert(key.to_vec(), value);
            Self {
                data: std::cell::RefCell::new(data),
                fail_get: Default::default(),
                fail_put: Default::default(),
            }
        }

        /// Clone the (fail_get, fail_put) toggles so a test can flip them
        /// after the context has been moved into a `CommitmentTree`.
        fn fault_handles(
            &self,
        ) -> (
            std::rc::Rc<std::cell::Cell<bool>>,
            std::rc::Rc<std::cell::Cell<bool>>,
        ) {
            (self.fail_get.clone(), self.fail_put.clone())
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
            if self.fail_put.get() {
                return Err(grovedb_storage::Error::StorageError(
                    "injected put failure".to_string(),
                ))
                .wrap_with_cost(OperationCost {
                    seek_count: 1,
                    ..Default::default()
                });
            }
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
            if self.fail_get.get() {
                return Err(grovedb_storage::Error::StorageError(
                    "injected get failure".to_string(),
                ))
                .wrap_with_cost(OperationCost {
                    seek_count: 1,
                    ..Default::default()
                });
            }
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

    /// Create a deterministic test ciphertext for DashMemo from an index.
    ///
    /// Layout: `epk_bytes (32) || enc_ciphertext (104) || out_ciphertext (80)`
    /// = 216 bytes.
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

    /// Create a deterministic 32-byte rho (nullifier) from an index.
    fn test_rho(index: u8) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = index;
        bytes[1] = 0xAA; // distinguishes rho from cmx/ciphertext
        bytes
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
            ct.append(test_leaf(i), test_rho(i as u8), &test_ciphertext(i as u8))
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
        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
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
    #[ignore] // ~60s: runs 500 Sinsemilla appends; use `cargo test -- --ignored`
    fn test_roundtrip_with_many_leaves() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        for i in 0..500u64 {
            ct.append(test_leaf(i), test_rho(i as u8), &test_ciphertext(i as u8))
                .value
                .expect("append should succeed");
        }

        let total_count = ct.total_count();
        ct.save().value.expect("save should succeed");

        let storage = ct.bulk_tree.dense_tree.storage;
        let loaded = CommitmentTree::<_, DashMemo>::open(total_count, TEST_CHUNK_POWER, storage)
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
            .append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("first append");
        assert_eq!(r0.global_position, 0, "first append should be position 0");
        assert_ne!(r0.sinsemilla_root, [0u8; 32], "root should be non-zero");
        assert_ne!(
            r0.bulk_state_root, [0u8; 32],
            "state root should be non-zero"
        );

        let r1 = ct
            .append(test_leaf(1), test_rho(1), &test_ciphertext(1))
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
        let ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, ctx).expect("new should succeed");

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
        let result = ct.append_raw(test_leaf(0), test_rho(0), &[0u8; 10]);
        let err = result.value.expect_err("should reject wrong size");
        let msg = format!("{}", err);
        assert!(
            msg.contains("invalid payload size"),
            "error message should mention payload size: {}",
            msg
        );

        // Too large
        let result = ct.append_raw(test_leaf(0), test_rho(0), &[0u8; 300]);
        assert!(
            result.value.is_err(),
            "should reject payload that is too large"
        );

        // Exact correct size should succeed
        let expected_size = ciphertext_payload_size::<DashMemo>();
        let result = ct.append_raw(test_leaf(0), test_rho(0), &vec![0u8; expected_size]);
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

    // ── Coverage gap tests ─────────────────────────────────────────────

    #[test]
    fn test_deserialize_ciphertext_too_short() {
        // Less than 32 + 80 = 112 bytes minimum
        let result: Option<TransmittedNoteCiphertext<DashMemo>> =
            deserialize_ciphertext(&[0u8; 50]);
        assert!(result.is_none(), "should return None for too-short data");
    }

    #[test]
    fn test_deserialize_ciphertext_empty() {
        let result: Option<TransmittedNoteCiphertext<DashMemo>> = deserialize_ciphertext(&[]);
        assert!(result.is_none(), "should return None for empty data");
    }

    #[test]
    fn test_deserialize_ciphertext_wrong_enc_size() {
        // 32 (epk) + wrong enc size + 80 (out) = 113 bytes total
        // enc_size = 113 - 32 - 80 = 1 byte, but DashMemo expects 104
        let result: Option<TransmittedNoteCiphertext<DashMemo>> =
            deserialize_ciphertext(&[0u8; 113]);
        assert!(
            result.is_none(),
            "should return None for wrong enc_ciphertext size"
        );
    }

    #[test]
    fn test_get_buffer_value_empty_tree() {
        let ctx = MockDataStorageContext::new();
        let ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        let result = ct
            .get_buffer_value(0)
            .expect("get_buffer_value should not error");
        assert!(result.is_none(), "empty tree should have no buffer values");
    }

    #[test]
    fn test_get_buffer_value_after_appends() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        // Append one item (goes into buffer since epoch_size = 2 for chunk_power=1)
        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append should succeed");

        let val = ct
            .get_buffer_value(0)
            .expect("get_buffer_value should not error");
        assert!(val.is_some(), "buffer should contain the first entry");

        // Position beyond buffer should be None
        let val = ct
            .get_buffer_value(100)
            .expect("get_buffer_value should not error");
        assert!(val.is_none(), "out-of-range position should return None");
    }

    #[test]
    fn test_get_chunk_value_empty_tree() {
        let ctx = MockDataStorageContext::new();
        let ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        let result = ct
            .get_chunk_value(0)
            .expect("get_chunk_value should not error");
        assert!(result.is_none(), "empty tree should have no chunks");
    }

    #[test]
    fn test_get_chunk_value_after_compaction() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        // chunk_power=1 → epoch_size=2. Append 2 items to trigger compaction.
        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append 0");
        let r = ct
            .append(test_leaf(1), test_rho(1), &test_ciphertext(1))
            .value
            .expect("append 1");
        assert!(r.compacted, "second append should trigger compaction");

        let chunk = ct
            .get_chunk_value(0)
            .expect("get_chunk_value should not error");
        assert!(chunk.is_some(), "chunk 0 should exist after compaction");

        let no_chunk = ct
            .get_chunk_value(99)
            .expect("get_chunk_value should not error");
        assert!(no_chunk.is_none(), "non-existent chunk should return None");
    }

    #[test]
    fn test_compute_current_state_root_empty() {
        let ctx = MockDataStorageContext::new();
        let ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        let root = ct
            .compute_current_state_root()
            .expect("state root should succeed");
        // Empty tree still has a deterministic root
        assert_ne!(root, [0u8; 32], "empty state root should be non-zero");
    }

    #[test]
    fn test_compute_current_state_root_matches_append_result() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        let r = ct
            .append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append 0");

        let computed = ct
            .compute_current_state_root()
            .expect("state root should succeed");
        let expected =
            crate::compute_commitment_tree_state_root(&r.sinsemilla_root, &r.bulk_state_root);
        assert_eq!(
            computed, expected,
            "computed state root should match combined sinsemilla + bulk root"
        );
    }

    #[test]
    fn test_epoch_size_and_chunk_count() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        assert_eq!(ct.epoch_size(), 2, "chunk_power=1 → epoch_size=2");
        assert_eq!(ct.chunk_count(), 0, "no chunks initially");

        // Fill one epoch
        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append 0");
        ct.append(test_leaf(1), test_rho(1), &test_ciphertext(1))
            .value
            .expect("append 1");

        assert_eq!(ct.chunk_count(), 1, "one chunk after filling one epoch");

        // Fill another epoch
        ct.append(test_leaf(2), test_rho(2), &test_ciphertext(2))
            .value
            .expect("append 2");
        ct.append(test_leaf(3), test_rho(3), &test_ciphertext(3))
            .value
            .expect("append 3");

        assert_eq!(ct.chunk_count(), 2, "two chunks after filling two epochs");
    }

    #[test]
    fn test_anchor_on_commitment_tree() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        let empty_anchor = ct.anchor();
        assert_eq!(
            empty_anchor,
            crate::Anchor::empty_tree(),
            "empty tree should have empty anchor"
        );

        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append 0");

        let anchor = ct.anchor();
        assert_ne!(
            anchor,
            crate::Anchor::empty_tree(),
            "non-empty tree should have non-empty anchor"
        );
    }

    #[test]
    fn test_debug_fmt() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        // Debug on empty tree
        let s = format!("{:?}", ct);
        assert!(s.contains("CommitmentTree"), "should contain struct name");
        assert!(
            s.contains("total_count"),
            "should contain total_count field"
        );
        assert!(s.contains("frontier"), "should contain frontier field");

        // Debug after appending
        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append should succeed");
        let s = format!("{:?}", ct);
        assert!(
            s.contains("CommitmentTree"),
            "should still contain struct name after append"
        );
    }

    #[test]
    fn test_open_from_state_error_invalid_chunk_power() {
        let ctx = MockDataStorageContext::new();
        // chunk_power=0 is invalid (must be 1..=16), causing from_state to fail
        let result = CommitmentTree::<_, DashMemo>::open(0, 0, ctx);
        let err = result
            .value
            .expect_err("should fail with invalid chunk_power");
        let msg = format!("{}", err);
        assert!(
            msg.contains("bulk tree from_state"),
            "error should mention from_state: {}",
            msg
        );
    }

    #[test]
    fn test_open_frontier_total_count_mismatch() {
        // 1. Create a tree, append 1 item, save
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");
        ct.append(test_leaf(0), test_rho(0), &test_ciphertext(0))
            .value
            .expect("append should succeed");
        ct.save().value.expect("save should succeed");

        // 2. Re-open with total_count=0 but the frontier has tree_size=1
        let storage = ct.bulk_tree.dense_tree.storage;
        let result = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, storage);
        let err = result
            .value
            .expect_err("should fail with frontier/total_count mismatch");
        let msg = format!("{}", err);
        assert!(
            msg.contains("frontier tree_size"),
            "error should mention frontier mismatch: {}",
            msg
        );
    }

    #[test]
    fn test_append_raw_rejects_invalid_cmx() {
        let ctx = MockDataStorageContext::new();
        let mut ct = CommitmentTree::<_, DashMemo>::open(0, TEST_CHUNK_POWER, ctx)
            .value
            .expect("open should succeed");

        // All 0xFF is not a valid Pallas field element
        let payload = vec![0u8; ciphertext_payload_size::<DashMemo>()];
        let result = ct.append_raw([0xFF; 32], test_rho(0), &payload);
        assert!(
            result.value.is_err(),
            "should reject invalid cmx field element"
        );
        let msg = format!("{}", result.value.expect_err("should be an error"));
        assert!(
            msg.contains("invalid Pallas field element"),
            "error should mention field element: {msg}"
        );

        // Verify tree was NOT mutated
        assert_eq!(
            ct.total_count(),
            0,
            "tree should not have been mutated by invalid cmx"
        );
    }

    // ── Batched append (append_many_raw) ────────────────────────────────

    /// A correctly-sized DashMemo payload filled with a deterministic pattern.
    fn seed_payload(index: u8) -> Vec<u8> {
        let mut p = vec![0u8; ciphertext_payload_size::<DashMemo>()];
        p[0] = index;
        p[1] = 0x5D;
        p
    }

    /// Build a fresh tree, append all `entries` one-by-one via [`append_raw`].
    fn build_via_per_leaf_append(
        entries: &[([u8; 32], [u8; 32], Vec<u8>)],
    ) -> CommitmentTree<MockDataStorageContext, DashMemo> {
        let mut ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, MockDataStorageContext::new())
                .expect("new should succeed");
        for (cmx, rho, payload) in entries {
            ct.append_raw(*cmx, *rho, payload)
                .value
                .expect("per-leaf append_raw should succeed");
        }
        ct
    }

    /// Build a fresh tree by passing all `entries` to [`append_many_raw`] in one call.
    fn build_via_append_many_raw(
        entries: Vec<([u8; 32], [u8; 32], Vec<u8>)>,
    ) -> CommitmentTree<MockDataStorageContext, DashMemo> {
        let mut ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, MockDataStorageContext::new())
                .expect("new should succeed");
        ct.append_many_raw(entries)
            .value
            .expect("append_many_raw should succeed");
        ct
    }

    /// Build a deterministic test sequence of `n` entries.
    fn make_entries(n: u64) -> Vec<([u8; 32], [u8; 32], Vec<u8>)> {
        (0..n)
            .map(|i| {
                (
                    test_leaf(i),
                    test_rho((i % 256) as u8),
                    seed_payload((i % 256) as u8),
                )
            })
            .collect()
    }

    /// Byte-for-byte equivalence: for the same input sequence, `append_many_raw`
    /// must produce the same `CommitmentFrontier` state and the same
    /// BulkAppendTree state root as N × `append_raw`. This is the core
    /// invariant — if it ever fails, batched callers can produce anchors that
    /// don't match what the per-leaf path would have produced.
    #[test]
    fn append_many_raw_byte_for_byte_matches_per_leaf() {
        // 0 / 1 / 2 / 3 cover edge cases around the very-empty and pre-compaction
        // shapes; 100 spans the buffer mid-range; 2048 fills exactly one epoch
        // (with TEST_CHUNK_POWER=1 we hit MANY compactions, exercising the cache
        // + MMR path); 10_000 spans several epochs at meaningful scale.
        for n in [0u64, 1, 2, 3, 100, 2048, 10_000] {
            let entries = make_entries(n);
            let a = build_via_per_leaf_append(&entries);
            let b = build_via_append_many_raw(entries);

            assert_eq!(
                a.root_hash(),
                b.root_hash(),
                "Sinsemilla root mismatch at N={}: per-leaf vs append_many_raw",
                n
            );
            assert_eq!(
                a.frontier.serialize(),
                b.frontier.serialize(),
                "frontier serialization mismatch at N={}: per-leaf vs append_many_raw",
                n
            );
            let a_state = a.compute_current_state_root().expect("state root A");
            let b_state = b.compute_current_state_root().expect("state root B");
            assert_eq!(
                a_state, b_state,
                "bulk tree state_root mismatch at N={}: per-leaf vs append_many_raw",
                n
            );
            assert_eq!(
                a.total_count(),
                b.total_count(),
                "total_count mismatch at N={}: per-leaf vs append_many_raw",
                n
            );
            assert_eq!(a.total_count(), n, "tree should hold exactly N={} items", n);
        }
    }

    /// `CommitmentFrontier::append_no_root` is just the carry-chain part of the
    /// upstream `Frontier::append`. Its [`OperationCost`] must therefore omit
    /// the depth-32 root walk that the eager [`append`] would have counted.
    ///
    /// Pairing N × `append_no_root` with a single `root_hash_with_cost` recovers
    /// the depth walk *once*, exactly as the batched API does in production —
    /// so this is also the structural check that justifies the speedup.
    #[test]
    fn append_no_root_cost_omits_per_leaf_depth_walk() {
        // We use a small N here to keep the test fast — the relationship is
        // independent of N.
        const N: u64 = 16;

        let mut frontier_eager = CommitmentFrontier::new();
        let mut frontier_lazy = CommitmentFrontier::new();
        let mut eager_cost = OperationCost::default();
        let mut lazy_cost = OperationCost::default();

        for i in 0..N {
            let cmx = test_leaf(i);
            frontier_eager
                .append(cmx)
                .unwrap_add_cost(&mut eager_cost)
                .expect("eager append");
            frontier_lazy
                .append_no_root(cmx)
                .unwrap_add_cost(&mut lazy_cost)
                .expect("lazy append");
        }

        // Both paths must produce identical frontier state.
        assert_eq!(frontier_eager.root_hash(), frontier_lazy.root_hash());
        assert_eq!(frontier_eager.serialize(), frontier_lazy.serialize());

        // The eager path counts 32 Sinsemilla hashes per call for the depth
        // walk; the lazy path counts only the carry chain. Difference is
        // exactly 32 × N.
        assert_eq!(
            eager_cost.sinsemilla_hash_calls - lazy_cost.sinsemilla_hash_calls,
            32 * N as u32,
            "eager append must include 32 depth-walk hashes per leaf; lazy must not"
        );

        // Recover the depth walk *once* via root_hash_with_cost. The lazy
        // path then matches the eager path minus the deferred walks
        // (32 × (N − 1)).
        let root_ctx = frontier_lazy.root_hash_with_cost();
        let lazy_total = lazy_cost.sinsemilla_hash_calls + root_ctx.cost.sinsemilla_hash_calls;
        assert_eq!(
            eager_cost.sinsemilla_hash_calls,
            lazy_total + 32 * (N as u32 - 1),
            "N × append == N × append_no_root + 1 × root_hash + 32 × (N − 1)"
        );

        // Sanity: lazy carry-chain cost is bounded by N (much less than 32 × N).
        assert!(
            lazy_cost.sinsemilla_hash_calls < N as u32,
            "carry-chain cost should be amortized ~O(1) per leaf, not 32"
        );
    }

    /// The Sinsemilla anchor produced by `append_many_raw` must be **spend-
    /// usable** — i.e. an Orchard Merkle auth path for a leaf at a known
    /// position must verify against the batched-built anchor, exactly as it
    /// would against an anchor built leaf-by-leaf.
    ///
    /// We compute the auth path independently (pairwise Sinsemilla combine to
    /// the root, padding upper levels with `empty_root`), then use the orchard
    /// `MerklePath` API to derive an anchor from `(position, path, cmx)` and
    /// require equality with `ct.anchor()`.
    #[test]
    fn append_many_raw_anchor_is_spend_usable() {
        use crate::{ExtractedNoteCommitment, MerkleHashOrchard, MerklePath};

        // Modest N keeps the manual auth-path recomputation cheap; the
        // verification property is N-agnostic, so a smaller N proves the
        // anchor shape.
        const N: u64 = 257;
        const TARGET_POSITION: usize = 113;

        let entries = make_entries(N);
        let owned_cmx_bytes = entries[TARGET_POSITION].0;
        let ct = build_via_append_many_raw(entries.clone());
        let anchor = ct.anchor();

        // Independent auth-path recomputation over the same leaf set.
        let leaves: Vec<MerkleHashOrchard> = entries
            .iter()
            .map(|(cmx, _, _)| {
                Option::from(MerkleHashOrchard::from_bytes(cmx))
                    .expect("test_leaf produces valid Pallas elements")
            })
            .collect();

        let auth_path = compute_orchard_auth_path(&leaves, TARGET_POSITION);
        let target_cmx = Option::<ExtractedNoteCommitment>::from(
            ExtractedNoteCommitment::from_bytes(&owned_cmx_bytes),
        )
        .expect("test_leaf produces valid Pallas elements");

        let merkle_path = MerklePath::from_parts(TARGET_POSITION as u32, auth_path);
        let computed = merkle_path.root(target_cmx);

        assert_eq!(
            computed, anchor,
            "auth path against batched anchor failed — anchor is not spend-usable"
        );
    }

    /// Build the Orchard Sinsemilla auth path for `position` over `leaves`,
    /// padding upper levels with `MerkleHashOrchard::empty_root(level)` once
    /// the tree thins beyond the leaf set.
    fn compute_orchard_auth_path(
        leaves: &[crate::MerkleHashOrchard],
        position: usize,
    ) -> [crate::MerkleHashOrchard; 32] {
        use crate::{Hashable, Level, MerkleHashOrchard};

        let mut current: Vec<MerkleHashOrchard> = leaves.to_vec();
        let mut path: Vec<MerkleHashOrchard> = Vec::with_capacity(32);
        let mut idx = position;

        for level in 0..32u8 {
            let l = Level::from(level);
            let empty = MerkleHashOrchard::empty_root(l);

            let sibling_idx = idx ^ 1;
            let sibling = current.get(sibling_idx).copied().unwrap_or(empty);
            path.push(sibling);

            // Combine pairs up to the next level. Pad with `empty` so the
            // upper-level positions align with `idx /= 2` regardless of how
            // many leaves there are.
            let next_len = current.len().div_ceil(2);
            let mut next: Vec<MerkleHashOrchard> = Vec::with_capacity(next_len);
            let mut i = 0;
            while i < current.len() {
                let left = current[i];
                let right = current.get(i + 1).copied().unwrap_or(empty);
                next.push(MerkleHashOrchard::combine(l, &left, &right));
                i += 2;
            }
            current = next;
            idx /= 2;
        }

        path.try_into()
            .expect("32 levels of auth path produced from the loop above")
    }

    // ── append_many_raw error-branch coverage ───────────────────────────

    /// Mid-batch invalid cmx must be rejected via the per-entry pre-validation.
    #[test]
    fn append_many_raw_rejects_invalid_cmx_mid_batch() {
        let ctx = MockDataStorageContext::new();
        let mut ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, ctx).expect("new should succeed");

        // First entry is valid; second has a non-Pallas cmx. The first must be
        // accepted, then the batch errors with `InvalidFieldElement` — matching
        // the per-leaf `append_raw` semantics.
        let entries = vec![
            (test_leaf(0), test_rho(0), seed_payload(0)),
            ([0xFFu8; 32], test_rho(1), seed_payload(1)),
        ];
        let err = ct
            .append_many_raw(entries)
            .value
            .expect_err("invalid cmx must propagate out of the batch");
        assert!(
            matches!(err, CommitmentTreeError::InvalidFieldElement),
            "expected InvalidFieldElement, got {err}"
        );
        // The first (valid) entry was already applied.
        assert_eq!(ct.total_count(), 1, "valid prefix entry was applied");
    }

    /// Mid-batch wrong-sized payload must be rejected via the per-entry
    /// pre-validation.
    #[test]
    fn append_many_raw_rejects_wrong_payload_size_mid_batch() {
        let ctx = MockDataStorageContext::new();
        let mut ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, ctx).expect("new should succeed");

        let entries = vec![
            (test_leaf(0), test_rho(0), seed_payload(0)),
            (test_leaf(1), test_rho(1), vec![0u8; 1]), // wrong size
        ];
        let err = ct
            .append_many_raw(entries)
            .value
            .expect_err("bad payload size must propagate out of the batch");
        assert!(
            matches!(err, CommitmentTreeError::InvalidPayloadSize { .. }),
            "expected InvalidPayloadSize, got {err}"
        );
    }

    /// A storage fault during the bulk-tree side of `append_many_raw` must
    /// surface as the wrapped `InvalidData("bulk append: …")` error.
    #[test]
    fn append_many_raw_surfaces_bulk_storage_error() {
        let ctx = MockDataStorageContext::new();
        let (fail_get, fail_put) = ctx.fault_handles();
        let mut ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, ctx).expect("new should succeed");

        // Make the underlying dense-tree storage fail on both reads and writes
        // so `BulkAppendTree::append_no_state_root` errors out.
        fail_get.set(true);
        fail_put.set(true);

        let entries = vec![(test_leaf(0), test_rho(0), seed_payload(0))];
        let err = ct
            .append_many_raw(entries)
            .value
            .expect_err("bulk storage failure should surface");
        assert!(
            format!("{err}").contains("bulk append"),
            "error should be wrapped as a bulk-append failure: {err}"
        );
    }

    /// `CommitmentFrontier::append_no_root` must reject a non-Pallas cmx with
    /// `InvalidFieldElement` (mirrors the eager `append` behavior).
    #[test]
    fn frontier_append_no_root_rejects_invalid_cmx() {
        let mut f = CommitmentFrontier::new();
        let err = f
            .append_no_root([0xFFu8; 32])
            .value
            .expect_err("a non-Pallas cmx must be rejected before any frontier mutation");
        assert!(
            matches!(err, CommitmentTreeError::InvalidFieldElement),
            "expected InvalidFieldElement, got {err}"
        );
        assert_eq!(
            f.tree_size(),
            0,
            "frontier must not have been mutated by the rejection"
        );
    }

    /// `CommitmentTree::commit_mmr` must run the underlying bulk-tree commit
    /// to completion (success path).
    #[test]
    fn commit_mmr_success() {
        let ctx = MockDataStorageContext::new();
        let mut ct =
            CommitmentTree::<_, DashMemo>::new(TEST_CHUNK_POWER, ctx).expect("new should succeed");

        // Append enough to trigger at least one compaction (TEST_CHUNK_POWER=1
        // → epoch_size=2), so the MMR overlay has staged nodes.
        let notes = (0..4u8).map(|i| (test_leaf(i as u64), test_rho(i), seed_payload(i)));
        ct.append_many_raw(notes).value.expect("warmup");

        ct.commit_mmr().expect("commit_mmr should flush cleanly");
    }
}
