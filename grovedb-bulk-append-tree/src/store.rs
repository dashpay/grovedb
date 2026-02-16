//! Storage abstraction for the bulk append tree.

use std::{cell::RefCell, collections::HashMap};

/// Abstraction over key-value storage for the bulk append tree.
///
/// `put` and `delete` take `&self` (not `&mut self`) to match GroveDB's
/// `StorageContext` pattern where writes go through a batch with interior
/// mutability.
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}

/// Write-through caching wrapper around any `BulkStore`.
///
/// Caches all reads at the raw byte level. Writes go through to the inner
/// store immediately and update the cache, so subsequent reads see the latest
/// value even when the underlying store defers writes (e.g. batch-based
/// transactional storage).
///
/// Cache entries use `Option<Vec<u8>>`: `Some(bytes)` for a cached value,
/// `None` for a cached deletion / confirmed absence.
pub struct CachedBulkStore<S: BulkStore> {
    inner: S,
    cache: RefCell<HashMap<Vec<u8>, Option<Vec<u8>>>>,
}

impl<S: BulkStore> CachedBulkStore<S> {
    /// Create a new cached wrapper around the given store.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Unwrap, discarding the cache and returning the inner store.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: BulkStore> BulkStore for CachedBulkStore<S> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        // Check cache first
        if let Some(cached) = self.cache.borrow().get(key) {
            return Ok(cached.clone());
        }
        // Cache miss — read from inner store and cache the result
        let result = self.inner.get(key)?;
        self.cache.borrow_mut().insert(key.to_vec(), result.clone());
        Ok(result)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
        // Write through to inner store
        self.inner.put(key, value)?;
        // Update cache
        self.cache
            .borrow_mut()
            .insert(key.to_vec(), Some(value.to_vec()));
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), String> {
        // Delete from inner store
        self.inner.delete(key)?;
        // Cache the deletion as None
        self.cache.borrow_mut().insert(key.to_vec(), None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory store that tracks get call counts for verifying caching.
    struct CountingStore {
        data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
        get_count: RefCell<u64>,
    }

    impl CountingStore {
        fn new() -> Self {
            Self {
                data: RefCell::new(HashMap::new()),
                get_count: RefCell::new(0),
            }
        }

        fn get_count(&self) -> u64 {
            *self.get_count.borrow()
        }
    }

    impl BulkStore for CountingStore {
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
            *self.get_count.borrow_mut() += 1;
            Ok(self.data.borrow().get(key).cloned())
        }

        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
            self.data.borrow_mut().insert(key.to_vec(), value.to_vec());
            Ok(())
        }

        fn delete(&self, key: &[u8]) -> Result<(), String> {
            self.data.borrow_mut().remove(key);
            Ok(())
        }
    }

    #[test]
    fn test_cached_store_get_caches_result() {
        let inner = CountingStore::new();
        inner
            .put(b"key1", b"value1")
            .expect("seed inner store with key1");
        let cached = CachedBulkStore::new(inner);

        // First get — cache miss, hits inner store
        let v1 = cached.get(b"key1").expect("first get");
        assert_eq!(v1, Some(b"value1".to_vec()));
        assert_eq!(cached.into_inner().get_count(), 1);
    }

    #[test]
    fn test_cached_store_second_get_uses_cache() {
        let inner = CountingStore::new();
        inner
            .put(b"key1", b"value1")
            .expect("seed inner store with key1");
        let cached = CachedBulkStore::new(inner);

        // First get — cache miss
        let _ = cached.get(b"key1").expect("first get");
        // Second get — should use cache, not hit inner
        let v2 = cached.get(b"key1").expect("second get");
        assert_eq!(v2, Some(b"value1".to_vec()));
        assert_eq!(
            cached.into_inner().get_count(),
            1,
            "inner should only be hit once"
        );
    }

    #[test]
    fn test_cached_store_put_updates_cache() {
        let inner = CountingStore::new();
        let cached = CachedBulkStore::new(inner);

        cached.put(b"key1", b"value1").expect("put key1");

        // Subsequent get should return cached value without hitting inner
        let v = cached.get(b"key1").expect("get after put");
        assert_eq!(v, Some(b"value1".to_vec()));
        assert_eq!(
            cached.into_inner().get_count(),
            0,
            "inner get should not be called after put"
        );
    }

    #[test]
    fn test_cached_store_delete_caches_none() {
        let inner = CountingStore::new();
        inner
            .put(b"key1", b"value1")
            .expect("seed inner store with key1");
        let cached = CachedBulkStore::new(inner);

        // Delete the key
        cached.delete(b"key1").expect("delete key1");

        // Subsequent get should return None from cache
        let v = cached.get(b"key1").expect("get after delete");
        assert_eq!(v, None);
        assert_eq!(
            cached.into_inner().get_count(),
            0,
            "inner get should not be called for deleted key"
        );
    }

    #[test]
    fn test_cached_store_miss_falls_through() {
        let inner = CountingStore::new();
        let cached = CachedBulkStore::new(inner);

        // Key doesn't exist — falls through to inner, caches None
        let v = cached.get(b"missing").expect("get missing key");
        assert_eq!(v, None);
        assert_eq!(cached.into_inner().get_count(), 1);
    }

    #[test]
    fn test_cached_store_miss_caches_none() {
        let inner = CountingStore::new();
        let cached = CachedBulkStore::new(inner);

        // First get — miss, caches None
        let _ = cached.get(b"missing").expect("first get");
        // Second get — should use cached None
        let v = cached.get(b"missing").expect("second get");
        assert_eq!(v, None);
        assert_eq!(
            cached.into_inner().get_count(),
            1,
            "inner should only be hit once for missing key"
        );
    }

    #[test]
    fn test_cached_store_write_through() {
        let inner = CountingStore::new();
        let cached = CachedBulkStore::new(inner);

        cached.put(b"key1", b"value1").expect("put via cache");

        // Verify the inner store actually has the data
        let inner = cached.into_inner();
        let v = inner.get(b"key1").expect("get from inner directly");
        assert_eq!(v, Some(b"value1".to_vec()));
    }

    #[test]
    fn test_cached_store_into_inner() {
        let inner = CountingStore::new();
        inner
            .put(b"key1", b"value1")
            .expect("seed inner store with key1");
        let cached = CachedBulkStore::new(inner);

        // Do some operations through the cache
        cached.put(b"key2", b"value2").expect("put key2");

        // Unwrap and verify inner state
        let inner = cached.into_inner();
        assert_eq!(
            inner.get(b"key1").expect("get key1"),
            Some(b"value1".to_vec())
        );
        assert_eq!(
            inner.get(b"key2").expect("get key2"),
            Some(b"value2".to_vec())
        );
    }

    #[test]
    fn test_cached_store_with_bulk_append_tree() {
        use crate::BulkAppendTree;

        let inner = CountingStore::new();
        let cached = CachedBulkStore::new(inner);

        let mut tree = BulkAppendTree::new(4).expect("create tree");

        // Append values through cached store
        for i in 0..3u8 {
            tree.append(&cached, &[i]).expect("append value");
        }
        tree.save_meta(&cached).expect("save metadata");

        // Query values back — should hit cache, not inner store
        let initial_gets = cached.inner.get_count();
        let v0 = tree.get_value(&cached, 0).expect("get value 0");
        assert_eq!(v0, Some(vec![0u8]));
        // The get should have used the cache for buffer entries
        let gets_after = cached.inner.get_count();
        // Should have fewer inner gets than without cache
        assert!(
            gets_after - initial_gets <= 1,
            "expected at most 1 inner get for cached read, got {}",
            gets_after - initial_gets
        );
    }

    #[test]
    fn test_cached_store_compaction_cycle() {
        use crate::BulkAppendTree;

        let inner = CountingStore::new();
        let cached = CachedBulkStore::new(inner);

        let mut tree = BulkAppendTree::new(4).expect("create tree with epoch_size=4");

        // Append 8 values to trigger 2 compaction cycles
        for i in 0..8u8 {
            tree.append(&cached, &[i]).expect("append value");
        }
        tree.save_meta(&cached).expect("save metadata");

        assert_eq!(tree.epoch_count(), 2, "should have 2 completed epochs");
        assert_eq!(
            tree.buffer_count(),
            0,
            "buffer should be empty after full compaction"
        );

        // Read back all values — first reads may hit inner store (for epoch blobs),
        // but repeated reads should be cached
        for i in 0..8u8 {
            let v = tree
                .get_value(&cached, i as u64)
                .expect("get value after compaction");
            assert_eq!(v, Some(vec![i]));
        }

        // Read them again — all should come from cache now
        let gets_before = cached.inner.get_count();
        for i in 0..8u8 {
            let v = tree.get_value(&cached, i as u64).expect("re-read value");
            assert_eq!(v, Some(vec![i]));
        }
        let gets_after = cached.inner.get_count();
        assert_eq!(
            gets_after - gets_before,
            0,
            "repeated reads should all hit cache"
        );
    }
}
