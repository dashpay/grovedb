use std::collections::BTreeMap;

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
    CostResult, CostsExt, OperationCost,
};
use grovedb_storage::StorageContext;

use crate::{
    helper::{mmr_node_key_sized, MmrKeySize},
    storage_adapter::MmrStore,
    MMRStoreReadOps, MMRStoreWriteOps, MmrNode, MMR,
};

// ── Minimal mock StorageContext ──────────────────────────────────────

/// In-memory key→value store implementing `StorageContext`.
///
/// Only `get` and `put` are functional — the rest are stubs since
/// `MmrStore` never calls them.
struct MockStorageContext {
    data: std::cell::RefCell<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MockStorageContext {
    fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(BTreeMap::new()),
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

    fn delete_aux<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {}

    fn delete_root<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {}
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

impl<'db> StorageContext<'db> for MockStorageContext {
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

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        let data = self.data.borrow();
        let val = data.get(key.as_ref()).cloned();
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

/// Mock StorageContext that returns errors for get and put.
struct FailingStorageContext;

impl<'db> StorageContext<'db> for FailingStorageContext {
    type Batch = StubBatch;
    type RawIterator = StubRawIterator;

    fn get<K: AsRef<[u8]>>(&self, _key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        Err(grovedb_storage::Error::StorageError("get failed".into()))
            .wrap_with_cost(Default::default())
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        Err(grovedb_storage::Error::StorageError("put failed".into()))
            .wrap_with_cost(Default::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        _k: K,
        _v: &[u8],
        _c: Option<KeyValueStorageCost>,
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

    fn get_aux<K: AsRef<[u8]>>(
        &self,
        _k: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        Ok(None).wrap_with_cost(Default::default())
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

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn mmr_store_write_then_read() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::new(&ctx);

    let leaf = MmrNode::leaf(b"hello world".to_vec());
    let expected_hash = leaf.hash();

    // Write a single leaf at position 0
    let mut store_ref: &MmrStore<'_, _> = &store;
    let write_result = MMRStoreWriteOps::append(&mut store_ref, 0, vec![leaf]);
    assert!(write_result.value.is_ok(), "append should succeed");

    // Read it back
    let read_result = MMRStoreReadOps::element_at_position(&store_ref, 0);
    let node = read_result
        .value
        .expect("read should succeed")
        .expect("node should exist");
    assert_eq!(node.hash(), expected_hash, "read-back hash should match");
    assert_eq!(
        node.value().expect("leaf should have value"),
        b"hello world"
    );
}

#[test]
fn mmr_store_read_missing_returns_none() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::new(&ctx);
    let store_ref: &MmrStore<'_, _> = &store;

    let result = MMRStoreReadOps::element_at_position(&store_ref, 42);
    let node = result.value.expect("read should succeed");
    assert!(
        node.is_none(),
        "reading missing position should return None"
    );
}

#[test]
fn mmr_store_write_multiple_elements() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::new(&ctx);

    let leaves: Vec<MmrNode> = (0..5u32)
        .map(|i| MmrNode::leaf(i.to_le_bytes().to_vec()))
        .collect();
    let hashes: Vec<[u8; 32]> = leaves.iter().map(|l| l.hash()).collect();

    // Write 5 elements starting at position 3
    let mut store_ref: &MmrStore<'_, _> = &store;
    MMRStoreWriteOps::append(&mut store_ref, 3, leaves)
        .value
        .expect("append should succeed");

    // Verify each element
    for i in 0..5u64 {
        let node = MMRStoreReadOps::element_at_position(&store_ref, 3 + i)
            .value
            .expect("read should succeed")
            .expect("node should exist");
        assert_eq!(node.hash(), hashes[i as usize]);
    }

    // Positions before and after should be empty
    assert!(MMRStoreReadOps::element_at_position(&store_ref, 2)
        .value
        .expect("read should succeed")
        .is_none());
    assert!(MMRStoreReadOps::element_at_position(&store_ref, 8)
        .value
        .expect("read should succeed")
        .is_none());
}

#[test]
fn mmr_store_costs_are_nonzero() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::new(&ctx);

    let leaf = MmrNode::leaf(b"cost check".to_vec());
    let mut store_ref: &MmrStore<'_, _> = &store;

    // Write cost should include seeks
    let write_result = MMRStoreWriteOps::append(&mut store_ref, 0, vec![leaf]);
    assert!(
        write_result.cost.seek_count > 0,
        "write should report non-zero seek_count"
    );

    // Read cost should include seek + loaded bytes
    let read_result = MMRStoreReadOps::element_at_position(&store_ref, 0);
    assert_eq!(read_result.cost.seek_count, 1, "read should report 1 seek");
    assert!(
        read_result.cost.storage_loaded_bytes > 0,
        "read should report non-zero loaded bytes"
    );
}

#[test]
fn mmr_store_full_mmr_roundtrip() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::new(&ctx);

    // Use MmrStore as the backend for a full MMR
    let mut mmr = MMR::new(0, &store);
    for i in 0u32..7 {
        mmr.push(MmrNode::leaf(i.to_le_bytes().to_vec()))
            .unwrap()
            .expect("push should succeed");
    }
    let root_before_commit = mmr.get_root().unwrap().expect("get_root should succeed");

    // Commit to storage
    mmr.commit().unwrap().expect("commit should succeed");

    // Re-open MMR from the same store and verify root
    let mmr2 = MMR::new(mmr.mmr_size, &store);
    let root_after_reopen = mmr2.get_root().unwrap().expect("get_root should succeed");

    assert_eq!(
        root_before_commit.hash(),
        root_after_reopen.hash(),
        "root should survive commit + reopen"
    );
}

#[test]
fn mmr_store_internal_node_roundtrip() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::new(&ctx);

    // Internal nodes (hash-only) should also round-trip
    let internal = MmrNode::internal([0xABu8; 32]);
    let mut store_ref: &MmrStore<'_, _> = &store;
    MMRStoreWriteOps::append(&mut store_ref, 0, vec![internal.clone()])
        .value
        .expect("append internal should succeed");

    let read = MMRStoreReadOps::element_at_position(&store_ref, 0)
        .value
        .expect("read should succeed")
        .expect("node should exist");
    assert_eq!(read.hash(), internal.hash());
    assert!(read.value().is_none(), "internal node should have no value");
}

#[test]
fn mmr_store_read_corrupted_data() {
    let ctx = MockStorageContext::new();

    // Manually insert corrupted bytes at position 0
    let key = mmr_node_key_sized(0, MmrKeySize::U64).expect("key for position 0 should succeed");
    ctx.data
        .borrow_mut()
        .insert(key.as_ref().to_vec(), vec![0xFF, 0xFF, 0xFF]);

    let store = MmrStore::new(&ctx);
    let store_ref: &MmrStore<'_, _> = &store;

    let result = MMRStoreReadOps::element_at_position(&store_ref, 0);
    assert!(
        result.value.is_err(),
        "should return error for corrupted data"
    );
    let err_msg = format!("{}", result.value.expect_err("should be deserialize error"));
    assert!(
        err_msg.contains("deserialize"),
        "error should mention deserialization: {}",
        err_msg
    );
}

#[test]
fn mmr_store_read_surfaces_storage_error() {
    let ctx = FailingStorageContext;
    let store = MmrStore::new(&ctx);
    let store_ref: &MmrStore<'_, _> = &store;

    let result = MMRStoreReadOps::element_at_position(&store_ref, 0);
    assert!(result.value.is_err(), "should surface storage get error");
    let err_msg = format!("{}", result.value.expect_err("should be store error"));
    assert!(
        err_msg.contains("get failed"),
        "error should contain original message: {}",
        err_msg
    );
}

#[test]
fn mmr_store_write_surfaces_storage_error() {
    let ctx = FailingStorageContext;
    let store = MmrStore::new(&ctx);
    let mut store_ref: &MmrStore<'_, _> = &store;

    let leaf = MmrNode::leaf(b"data".to_vec());
    let result = MMRStoreWriteOps::append(&mut store_ref, 0, vec![leaf]);
    assert!(result.value.is_err(), "should surface storage put error");
    let err_msg = format!("{}", result.value.expect_err("should be store error"));
    assert!(
        err_msg.contains("put failed"),
        "error should contain original message: {}",
        err_msg
    );
}

// ── Coverage for remaining production error paths ───────────────────

#[test]
fn mmr_store_with_key_size_constructor() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::with_key_size(&ctx, MmrKeySize::U32);

    let leaf = MmrNode::leaf(b"compact".to_vec());
    let mut store_ref: &MmrStore<'_, _> = &store;
    MMRStoreWriteOps::append(&mut store_ref, 0, vec![leaf.clone()])
        .value
        .expect("append with U32 keys should succeed");

    let read = MMRStoreReadOps::element_at_position(&store_ref, 0)
        .value
        .expect("read should succeed")
        .expect("node should exist");
    assert_eq!(read.hash(), leaf.hash());
}

#[test]
fn mmr_store_read_key_overflow_with_u32() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::with_key_size(&ctx, MmrKeySize::U32);
    let store_ref: &MmrStore<'_, _> = &store;

    // Position exceeding u32::MAX should trigger key error
    let pos = crate::helper::MAX_U32_MMR_POSITION + 1;
    let result = MMRStoreReadOps::element_at_position(&store_ref, pos);
    assert!(result.value.is_err(), "should error on key overflow");
}

#[test]
fn mmr_store_write_key_overflow_with_u32() {
    let ctx = MockStorageContext::new();
    let store = MmrStore::with_key_size(&ctx, MmrKeySize::U32);
    let mut store_ref: &MmrStore<'_, _> = &store;

    let pos = crate::helper::MAX_U32_MMR_POSITION + 1;
    let leaf = MmrNode::leaf(b"overflow".to_vec());
    let result = MMRStoreWriteOps::append(&mut store_ref, pos, vec![leaf]);
    assert!(result.value.is_err(), "should error on key overflow");
}
