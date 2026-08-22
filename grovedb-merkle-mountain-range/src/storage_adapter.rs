//! Storage adapter bridging GroveDB's `StorageContext` to MMR traits.
//!
//! Provides `MmrStore`, which implements `MMRStoreReadOps` and
//! `MMRStoreWriteOps` backed by a GroveDB storage context.

use grovedb_costs::{
    storage_cost::{
        key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::NoStorageRemoval,
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
use grovedb_storage::StorageContext;
use integer_encoding::VarInt;

use crate::{
    helper::{mmr_node_key_sized, MmrKeySize},
    MMRStoreReadOps, MMRStoreWriteOps, MmrNode,
};

/// Storage adapter wrapping a GroveDB `StorageContext` for MMR operations.
///
/// Reads and writes MMR nodes to data storage keyed by position.
/// Costs from storage operations are returned directly via `CostResult`.
///
/// The `key_size` field controls the byte width of storage keys:
/// [`MmrKeySize::U64`] (default) uses 8-byte keys, [`MmrKeySize::U32`]
/// uses 4-byte keys for space savings when positions fit in a `u32`.
///
/// Callers should call `get_root()` **before** `commit()` so that
/// recently-pushed nodes are still available in the `MMRBatch` overlay.
/// This eliminates the need for a write-through cache.
pub struct MmrStore<'a, C> {
    ctx: &'a C,
    key_size: MmrKeySize,
    leaf_value_storage_cost: LeafValueStorageCost,
}

/// How the value carried by a leaf node is reported to the storage cost
/// layer when the node is written by [`MMRStoreWriteOps::append`].
///
/// Internal nodes (hash only) are always new storage. Leaf values usually are
/// too — an `MmrTree` append stores a fresh value — but an owner that has
/// already charged part of a leaf's bytes as added storage before the flush
/// can say so, and that part is then reported as replaced rather than added.
/// The bulk-append tree does exactly this: every entry's chunk-blob share is
/// charged at its own append, so the blob written at compaction replaces
/// bytes that were paid for, and only its framing is new.
#[derive(Clone, Copy)]
pub enum LeafValueStorageCost {
    /// Issue the put with no cost information: key and value are charged as
    /// new storage (what every shipped version reports).
    New,
    /// `prepaid(value)` bytes of the leaf value were already charged as added
    /// storage by the owner. The put reports them as `replaced_bytes` and the
    /// remainder of the paid value size (value length plus its length varint)
    /// as `added_bytes`; a `prepaid` figure above the paid size is clamped to
    /// it. The key is new and charged in full. The callback must be a pure
    /// function of the value bytes — it runs at flush time, which may be long
    /// after the leaf was pushed.
    PartlyPrepaid(fn(&[u8]) -> u32),
}

impl<'a, C> MmrStore<'a, C> {
    /// Create a new store backed by the given storage context.
    ///
    /// Uses [`MmrKeySize::U64`] (8-byte keys) by default and reports every
    /// written node as new storage.
    pub fn new(ctx: &'a C) -> Self {
        Self {
            ctx,
            key_size: MmrKeySize::U64,
            leaf_value_storage_cost: LeafValueStorageCost::New,
        }
    }

    /// Create a new store with a specific key size.
    ///
    /// Use [`MmrKeySize::U32`] for compact 4-byte keys when positions
    /// are guaranteed to fit in a `u32`.
    pub fn with_key_size(ctx: &'a C, key_size: MmrKeySize) -> Self {
        Self {
            ctx,
            key_size,
            leaf_value_storage_cost: LeafValueStorageCost::New,
        }
    }

    /// Select how leaf values are reported to the storage cost layer on
    /// write; see [`LeafValueStorageCost`].
    pub fn with_leaf_value_storage_cost(mut self, policy: LeafValueStorageCost) -> Self {
        self.leaf_value_storage_cost = policy;
        self
    }

    /// Cost information for writing `serialized` — a node whose value part is
    /// `value` (none for an internal node) — under this store's leaf policy.
    fn write_cost_info(
        &self,
        value: Option<&[u8]>,
        serialized_len: u32,
    ) -> Option<KeyValueStorageCost> {
        match (self.leaf_value_storage_cost, value) {
            (LeafValueStorageCost::New, _) | (_, None) => None,
            (LeafValueStorageCost::PartlyPrepaid(prepaid), Some(value)) => {
                // The paid size of a stored value is its length plus the
                // varint encoding that length — exactly what the commit path
                // verifies `added + replaced` against.
                let paid = serialized_len + serialized_len.required_space() as u32;
                let replaced = prepaid(value).min(paid);
                Some(KeyValueStorageCost {
                    // Supplied without the path prefix; the storage context
                    // completes it for a new node.
                    key_storage_cost: StorageCost::default(),
                    value_storage_cost: StorageCost {
                        added_bytes: paid - replaced,
                        replaced_bytes: replaced,
                        removed_bytes: NoStorageRemoval,
                    },
                    new_node: true,
                    needs_value_verification: true,
                })
            }
        }
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreReadOps for &MmrStore<'_, C> {
    fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, crate::Error> {
        let key = match mmr_node_key_sized(pos, self.key_size) {
            Ok(k) => k,
            Err(e) => return Err(e).wrap_with_cost(OperationCost::default()),
        };
        let result = self.ctx.get(key);
        let cost = result.cost;
        match result.value {
            Ok(Some(bytes)) => {
                let node = MmrNode::deserialize(&bytes).map_err(|e| {
                    crate::Error::StoreError(format!("deserialize node at pos {}: {}", pos, e))
                });
                match node {
                    Ok(n) => Ok(Some(n)).wrap_with_cost(cost),
                    Err(e) => Err(e).wrap_with_cost(cost),
                }
            }
            Ok(None) => Ok(None).wrap_with_cost(cost),
            Err(e) => Err(crate::Error::StoreError(format!(
                "get at pos {}: {}",
                pos, e
            )))
            .wrap_with_cost(cost),
        }
    }
}

impl<'db, C: StorageContext<'db>> MMRStoreWriteOps for &MmrStore<'_, C> {
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> CostResult<(), crate::Error> {
        let mut cost = OperationCost::default();
        for (i, elem) in elems.into_iter().enumerate() {
            let node_pos = pos + i as u64;
            let key = match mmr_node_key_sized(node_pos, self.key_size) {
                Ok(k) => k,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };
            let serialized = match elem.serialize() {
                Ok(s) => s,
                Err(e) => {
                    return Err(crate::Error::StoreError(format!(
                        "serialize at pos {}: {}",
                        node_pos, e
                    )))
                    .wrap_with_cost(cost);
                }
            };
            let cost_info = self.write_cost_info(elem.value(), serialized.len() as u32);
            let result = self.ctx.put(key, &serialized, None, cost_info);
            cost += result.cost;
            if let Err(e) = result.value {
                return Err(crate::Error::StoreError(format!(
                    "put at pos {}: {}",
                    node_pos, e
                )))
                .wrap_with_cost(cost);
            }
        }
        Ok(()).wrap_with_cost(cost)
    }
}
