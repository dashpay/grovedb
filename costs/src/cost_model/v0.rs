//! Version 0 of the storage layer's cost rules: what grove versions 1 to 5
//! charge. Each function is one rule of `docs/lignindb/cost-model.md`,
//! section 5, and is documented on the method of
//! [`StorageCostModel`](super::StorageCostModel) that calls it.

use integer_encoding::VarInt;

use super::{Keyspace, PREFIX_LEN};
use crate::{
    error::Error,
    storage_cost::{
        key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::BasicStorageRemoval,
    },
    ChildKeyLength, ChildSumLength, ChildrenSizesWithIsSumTree, FeatureSumLength, OperationCost,
};

const BLAKE3_BLOCK_LEN: usize = 64;

/// The longest stored key: 32 bytes of prefix and 256 of key. A cursor that
/// is not on a record of its subtree is charged this, so that the charge
/// does not depend on which subtrees are neighbours.
const CURSOR_BOUNDARY_BYTES: u64 = 256 + 32;

/// The key-value hash and the value hash of a node.
const NODE_HASHES_LEN: usize = 64;

pub(super) const BATCH_ORDER: [Keyspace; 4] = [
    Keyspace::Meta,
    Keyspace::Aux,
    Keyspace::Roots,
    Keyspace::Data,
];

pub(super) const DELETE_SEEKS: u32 = 1;

pub(super) const UNCOSTED_DELETE_SEEKS: u32 = 2;

fn blake3_block_count(len: usize) -> usize {
    if len == 0 {
        1
    } else {
        1 + (len - 1) / BLAKE3_BLOCK_LEN
    }
}

pub(super) fn stored_key_len(key_len: usize) -> usize {
    PREFIX_LEN + key_len
}

pub(super) fn node_record_len(
    value_len: usize,
    feature_len: FeatureSumLength,
    left: Option<(ChildKeyLength, ChildSumLength)>,
    right: Option<(ChildKeyLength, ChildSumLength)>,
) -> usize {
    let side = |link: Option<(ChildKeyLength, ChildSumLength)>| {
        1 + link.map_or(0, |(key_and_fixed_len, aggregate_len)| {
            key_and_fixed_len as usize + aggregate_len as usize
        })
    };
    side(left) + side(right) + 1 + feature_len as usize + NODE_HASHES_LEN + value_len
}

pub(super) fn open_by_path(segment_lens: impl IntoIterator<Item = usize>) -> OperationCost {
    let (bytes, segments) = segment_lens
        .into_iter()
        .fold((0, 0), |(bytes, segments), len| (bytes + len, segments + 1));
    if segments == 0 {
        OperationCost::default()
    } else {
        let message_len = bytes + size_of::<usize>() + segments;
        OperationCost::with_hash_node_calls(blake3_block_count(message_len) as u32)
    }
}

pub(super) fn open_by_prefix() -> OperationCost {
    OperationCost::default()
}

pub(super) fn secondary_prefix() -> OperationCost {
    OperationCost::with_hash_node_calls(1)
}

pub(super) fn read(found: Option<usize>) -> OperationCost {
    OperationCost {
        seek_count: 1,
        storage_loaded_bytes: found.map_or(0, |len| len as u64),
        ..Default::default()
    }
}

pub(super) fn read_immediate() -> OperationCost {
    OperationCost::default()
}

pub(super) fn complete_new_key_cost(
    cost_info: Option<KeyValueStorageCost>,
    stored_key_len: usize,
) -> Option<KeyValueStorageCost> {
    cost_info.map(|mut cost_info| {
        if cost_info.new_node {
            cost_info.key_storage_cost.added_bytes +=
                (stored_key_len + stored_key_len.required_space()) as u32;
        }
        cost_info
    })
}

pub(super) fn staged_write() -> OperationCost {
    OperationCost::default()
}

pub(super) fn refused_write() -> OperationCost {
    OperationCost::default()
}

pub(super) fn put_seeks(cost_info: Option<&KeyValueStorageCost>) -> u32 {
    if cost_info.is_some_and(KeyValueStorageCost::is_prepaid) {
        0
    } else {
        1
    }
}

pub(super) fn put_storage(
    running: &mut OperationCost,
    keyspace: Keyspace,
    stored_key_len: usize,
    value_len: usize,
    children_sizes: ChildrenSizesWithIsSumTree,
    cost_info: Option<KeyValueStorageCost>,
) -> Result<(), Error> {
    if keyspace == Keyspace::Roots && cost_info.is_none() {
        return Ok(());
    }
    let children_sizes = if keyspace == Keyspace::Data {
        children_sizes
    } else {
        None
    };
    running.add_key_value_storage_costs(
        stored_key_len as u32,
        value_len as u32,
        children_sizes,
        cost_info,
    )
}

pub(super) fn delete_storage(running: &mut OperationCost, cost_info: KeyValueStorageCost) {
    running.storage_cost.removed_bytes += cost_info.combined_removed_bytes();
}

pub(super) fn committed_value_len(committed: Option<usize>) -> u32 {
    committed.map_or(0, |len| len as u32)
}

pub(super) fn uncosted_delete_storage(
    running: &mut OperationCost,
    stored_key_len: usize,
    value_len: u32,
) {
    let key_len = stored_key_len as u32;
    running.storage_cost.removed_bytes += BasicStorageRemoval(
        key_len + value_len + key_len.required_space() as u32 + value_len.required_space() as u32,
    );
}

pub(super) fn batch_written(on_success: OperationCost, written: bool) -> OperationCost {
    if written {
        on_success
    } else {
        OperationCost::default()
    }
}

pub(super) fn write_immediate() -> OperationCost {
    OperationCost::default()
}

pub(super) fn cursor_move() -> OperationCost {
    OperationCost::with_seek_count(1)
}

pub(super) fn cursor_key(on_record: Option<usize>) -> OperationCost {
    OperationCost::with_storage_loaded_bytes(
        on_record.map_or(CURSOR_BOUNDARY_BYTES, |stored_key_len| {
            stored_key_len as u64
        }),
    )
}

pub(super) fn cursor_value(on_record: Option<(usize, usize)>) -> OperationCost {
    let mut cost = cursor_key(on_record.map(|(stored_key_len, _)| stored_key_len));
    if let Some((_, value_len)) = on_record {
        cost.storage_loaded_bytes += value_len as u64;
    }
    cost
}

pub(super) fn clear_walk(stored_key_lens: impl IntoIterator<Item = usize>) -> OperationCost {
    let mut cost = cursor_move();
    for stored_key_len in stored_key_lens {
        cost += cursor_key(Some(stored_key_len));
        cost += cursor_key(Some(stored_key_len));
        cost += cursor_move();
    }
    cost += cursor_key(None);
    cost
}

pub(super) fn transaction_commit() -> OperationCost {
    OperationCost::default()
}
