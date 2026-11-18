//! The storage rules of `docs/lignindb/cost-model.md`, as pure functions of
//! logical sizes.
//!
//! Every function here takes lengths and counts and returns what today's
//! code charges for one storage event. None of them takes a record, a
//! key or an engine handle. Each rule counts how often it was applied, so a
//! run over the corpus can say which rules the corpus exercises
//! ([`hits`]).

use std::sync::atomic::{AtomicU64, Ordering};

use grovedb_costs::{
    error::Error as CostError,
    storage_cost::{
        key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes, StorageCost,
    },
    ChildrenSizesWithIsSumTree, OperationCost, TreeCostType,
};

/// The length of a subtree prefix. Every stored key is the prefix followed
/// by the key the caller gave.
pub const PREFIX_LEN: u32 = 32;

/// What a cursor charges for looking at a position that is not a record of
/// its subtree: the longest stored key, 32 bytes of prefix and 256 of key.
pub const CURSOR_BOUNDARY_BYTES: u64 = 288;

/// The block size of Blake3, which the prefix charge is counted in.
pub const BLAKE3_BLOCK_LEN: usize = 64;

macro_rules! rules {
    ($($variant:ident = $section:literal, $text:literal;)*) => {
        /// One rule of the storage section of the cost model, as counted by
        /// [`hits`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Rule {
            $(
                #[doc = $text]
                $variant,
            )*
        }

        impl Rule {
            /// Every rule, in the order of the document.
            pub const ALL: &'static [Rule] = &[$(Rule::$variant,)*];

            /// The rule of the document this is a case of.
            pub fn section(self) -> &'static str {
                match self {
                    $(Rule::$variant => $section,)*
                }
            }

            /// What the case is.
            pub fn text(self) -> &'static str {
                match self {
                    $(Rule::$variant => $text,)*
                }
            }
        }
    };
}

rules! {
    OpenRootByPath = "P1", "a subtree handle opened by the empty path";
    OpenByPath = "P1", "a subtree handle opened by a path of one segment or more";
    OpenByPrefix = "P2", "a subtree handle opened by a prefix";
    SecondaryPrefix = "P3", "the prefix of an indexed tree's secondary derived";
    ReadDataFound = "R1", "a data record read through a deferred handle";
    ReadDataAbsent = "R1", "a data key with no record read through a deferred handle";
    ReadAuxFound = "R1", "an aux record read through a deferred handle";
    ReadAuxAbsent = "R1", "an aux key with no record read through a deferred handle";
    ReadRootsFound = "R1", "a root key record read through a deferred handle";
    ReadRootsAbsent = "R1", "an absent root key record read through a deferred handle";
    ReadMetaFound = "R1", "a meta record read through a deferred handle";
    ReadMetaAbsent = "R1", "a meta key with no record read through a deferred handle";
    ReadImmediate = "R2", "a read through an immediate handle";
    NewKeyCompleted = "W1", "the stored key added to the key cost of a new data record";
    PutDataNode = "W2", "a data put with the sizes of a node's links and feature";
    PutDataFlat = "W2", "a data put with no link sizes";
    PutAux = "W2", "an aux put";
    PutMeta = "W2", "a meta put";
    PutRootsCharged = "W3", "a root key put with a cost";
    PutRootsFree = "W3", "a root key put with no cost";
    PutPrepaid = "W4", "a prepaid put";
    ValueCostFromRecord = "K2", "a value cost worked out from the record length";
    ValueCostAsGiven = "K2", "a value cost taken from the writer as given";
    ValueCostChecked = "K3", "a writer's value cost checked against the record length";
    KeyCostChecked = "K3", "a writer's key cost checked against the stored key";
    CostRefused = "K3", "a write refused because the writer's cost does not match";
    DeleteDataWithCost = "D1", "a data delete with the writer's cost";
    DeleteAuxWithCost = "D1", "an aux delete with the writer's cost";
    DeleteRootsWithCost = "D1", "a root key delete with the writer's cost";
    DeleteMetaWithCost = "D1", "a meta delete with the writer's cost";
    DeleteCommittedRecord = "D2", "a delete with no cost of a record in the committed state";
    DeleteUncommittedRecord = "D2", "a delete with no cost of a key the committed state lacks";
    ImmediateWrite = "M1", "a write through an immediate handle";
    ImmediateBatchPut = "M2", "a put in the batch of an immediate handle";
    ImmediateBatchDelete = "M2", "a delete in the batch of an immediate handle";
    CursorSeekToFirst = "C1", "a cursor moved to the first record";
    CursorSeekToLast = "C1", "a cursor moved to the last record";
    CursorSeek = "C1", "a cursor moved to the first key at or after a key";
    CursorSeekForPrev = "C1", "a cursor moved to the last key at or before a key";
    CursorNext = "C1", "a cursor stepped forward";
    CursorPrev = "C1", "a cursor stepped back";
    CursorKeyOnRecord = "C2", "a cursor asked for its key or validity on a record";
    CursorKeyOffRecord = "C2", "a cursor asked for its key or validity off its subtree";
    CursorValue = "C3", "a cursor asked for the value of a record";
    BatchApplied = "B1", "a batch applied to a transaction";
    BatchRefused = "B1", "a batch whose writes were refused";
    NodeKept = "N1", "a data record kept as the fields of a node";
    BytesKept = "N1", "a data record that is not a node, kept as bytes";
    LinkSizesAgree = "N2", "the link and feature sizes Merk gave with a node are the node's";
    LinkSizesDiffer = "N2", "the link and feature sizes Merk gave with a node are not the node's";
}

static HITS: [AtomicU64; Rule::ALL.len()] = [const { AtomicU64::new(0) }; Rule::ALL.len()];

pub(super) fn hit(rule: Rule) {
    HITS[rule as usize].fetch_add(1, Ordering::Relaxed);
}

/// How often each rule was applied in this process, in the order of
/// [`Rule::ALL`].
pub fn hits() -> Vec<(Rule, u64)> {
    Rule::ALL
        .iter()
        .map(|rule| (*rule, HITS[*rule as usize].load(Ordering::Relaxed)))
        .collect()
}

/// The counts of [`hits`] as text: one line per rule, `section count text`.
pub fn hits_report() -> String {
    hits()
        .into_iter()
        .map(|(rule, count)| format!("{}\t{count}\t{}\n", rule.section(), rule.text()))
        .collect()
}

/// One of the four keyspaces of a subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Keyspace {
    /// Merk nodes, or the records of an append-only tree.
    Data,
    /// Auxiliary records.
    Aux,
    /// The root key of a Merk.
    Roots,
    /// Metadata.
    Meta,
}

impl Keyspace {
    /// The four keyspaces.
    pub const ALL: [Keyspace; 4] = [
        Keyspace::Data,
        Keyspace::Aux,
        Keyspace::Roots,
        Keyspace::Meta,
    ];

    /// The position of the keyspace in [`Keyspace::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }
}

/// The length of `value` as an unsigned LEB128 varint.
pub fn varint_len(value: u64) -> u32 {
    let bits = 64 - value.max(1).leading_zeros();
    bits.div_ceil(7)
}

/// The unsigned number a signed varint encodes `value` as.
pub fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

/// The length of `value` as a signed varint.
pub fn signed_varint_len(value: i64) -> u32 {
    varint_len(zigzag(value))
}

/// The number of Blake3 blocks a message of `len` bytes is hashed in.
pub fn blake3_blocks(len: usize) -> u32 {
    if len == 0 {
        1
    } else {
        (1 + (len - 1) / BLAKE3_BLOCK_LEN) as u32
    }
}

/// The length of the message a path is hashed from to give its prefix: the
/// segments, a machine word holding their number, and one length byte for
/// each.
pub fn prefix_body_len(segment_lens: impl IntoIterator<Item = usize>) -> (usize, usize) {
    let (bytes, segments) = segment_lens
        .into_iter()
        .fold((0, 0), |(bytes, segments), len| (bytes + len, segments + 1));
    (bytes + size_of::<usize>() + segments, segments)
}

/// P1: opening a subtree handle by its path, deferred or immediate, and any
/// other derivation of a prefix from a path.
pub fn open_by_path(segment_lens: impl IntoIterator<Item = usize>) -> OperationCost {
    let (body_len, segments) = prefix_body_len(segment_lens);
    if segments == 0 {
        hit(Rule::OpenRootByPath);
        OperationCost::default()
    } else {
        hit(Rule::OpenByPath);
        OperationCost::with_hash_node_calls(blake3_blocks(body_len))
    }
}

/// P2: opening a subtree handle by a prefix the caller holds.
pub fn open_by_prefix() -> OperationCost {
    hit(Rule::OpenByPrefix);
    OperationCost::default()
}

/// P3: deriving the prefix of a secondary of an indexed tree.
pub fn secondary_prefix() -> OperationCost {
    hit(Rule::SecondaryPrefix);
    OperationCost::with_hash_node_calls(1)
}

/// R1: a point read through a deferred handle. `found` is the length of
/// the record, for a Merk node the length its record encodes to.
pub fn read(keyspace: Keyspace, found: Option<u64>) -> OperationCost {
    hit(match (keyspace, found.is_some()) {
        (Keyspace::Data, true) => Rule::ReadDataFound,
        (Keyspace::Data, false) => Rule::ReadDataAbsent,
        (Keyspace::Aux, true) => Rule::ReadAuxFound,
        (Keyspace::Aux, false) => Rule::ReadAuxAbsent,
        (Keyspace::Roots, true) => Rule::ReadRootsFound,
        (Keyspace::Roots, false) => Rule::ReadRootsAbsent,
        (Keyspace::Meta, true) => Rule::ReadMetaFound,
        (Keyspace::Meta, false) => Rule::ReadMetaAbsent,
    });
    OperationCost {
        seek_count: 1,
        storage_loaded_bytes: found.unwrap_or(0),
        ..Default::default()
    }
}

/// R2: a point read through an immediate handle.
pub fn read_immediate() -> OperationCost {
    hit(Rule::ReadImmediate);
    OperationCost::default()
}

/// The length of the key a record is stored under: the prefix and the key.
pub fn stored_key_len(key_len: usize) -> u32 {
    PREFIX_LEN + key_len as u32
}

/// W1: a writer that describes a new data record gives the cost of the key
/// without the prefix, and the stored key with its length varint is added.
/// Applied when a data put is staged, to no other keyspace and to no
/// update.
pub fn complete_new_key_cost(
    cost_info: Option<KeyValueStorageCost>,
    key_len: usize,
) -> Option<KeyValueStorageCost> {
    cost_info.map(|mut info| {
        if info.new_node {
            hit(Rule::NewKeyCompleted);
            let stored = stored_key_len(key_len);
            info.key_storage_cost.added_bytes += stored + varint_len(u64::from(stored));
        }
        info
    })
}

/// The fixed number of bytes the feature of a node is charged as, whatever
/// its varints take.
pub fn feature_cost_size(cost_type: &TreeCostType) -> u32 {
    match cost_type {
        TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes => 8,
        TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes
        | TreeCostType::TreeFeatureUses16Bytes => 16,
        TreeCostType::TreeFeatureUsesVarIntCostAs8BytesAnd16Bytes => 24,
    }
}

/// K2: the bytes a value is charged as when the charge is worked out from
/// the record.
///
/// With no link sizes that is the record and its length varint. With them
/// (a Merk node) the two link option bytes, both links and the feature's
/// own bytes are taken off, the feature's fixed size is put on, the length
/// varint of the result is added, and then what the parent's link to this
/// node takes: the stored key, four bytes, and the feature's fixed size
/// again.
pub fn paid_value_len(
    stored_key_len: u32,
    value_len: u32,
    children_sizes: &ChildrenSizesWithIsSumTree,
) -> u32 {
    match children_sizes {
        None => value_len + varint_len(u64::from(value_len)),
        Some((feature, left, right)) => {
            let mut paid = value_len.saturating_sub(2);
            for (key_len, aggregate_len) in [left, right].into_iter().flatten() {
                paid = paid.saturating_sub(*key_len).saturating_sub(*aggregate_len);
            }
            let feature_size = match feature {
                Some((cost_type, feature_len)) => {
                    let size = feature_cost_size(cost_type);
                    paid = paid.saturating_sub(*feature_len) + size;
                    size
                }
                None => 0,
            };
            paid += varint_len(u64::from(paid));
            paid + stored_key_len + 4 + feature_size
        }
    }
}

/// K1 to K3: the storage cost of a record put under a stored key, as its
/// key part and its value part. The caller adds them in that order.
///
/// With no `cost_info` both parts are added bytes: the stored key with its
/// length varint, and [`paid_value_len`]. With one, the parts are the
/// writer's, after two checks. A new record's key cost must add up to the
/// stored key with its varint. The value cost must add up to
/// [`paid_value_len`], unless the writer asked for no check, in which case
/// it is taken as given.
pub fn key_value_storage(
    stored_key_len: u32,
    value_len: u32,
    children_sizes: &ChildrenSizesWithIsSumTree,
    cost_info: Option<&KeyValueStorageCost>,
) -> Result<(StorageCost, StorageCost), CostError> {
    let paid_key = stored_key_len + varint_len(u64::from(stored_key_len));
    let given_value = cost_info
        .filter(|info| !info.needs_value_verification)
        .map(|info| info.value_storage_cost.added_bytes + info.value_storage_cost.replaced_bytes);
    let paid_value = match given_value {
        Some(given) => {
            hit(Rule::ValueCostAsGiven);
            given
        }
        None => {
            hit(Rule::ValueCostFromRecord);
            paid_value_len(stored_key_len, value_len, children_sizes)
        }
    };
    let added = |added_bytes| StorageCost {
        added_bytes,
        ..Default::default()
    };
    let Some(info) = cost_info else {
        return Ok((added(paid_key), added(paid_value)));
    };
    if info.new_node {
        hit(Rule::KeyCostChecked);
        check(&info.key_storage_cost, paid_key)?;
    }
    if info.needs_value_verification {
        hit(Rule::ValueCostChecked);
    }
    check(&info.value_storage_cost, paid_value)?;
    Ok((
        info.key_storage_cost.clone(),
        info.value_storage_cost.clone(),
    ))
}

fn check(cost: &StorageCost, len: u32) -> Result<(), CostError> {
    if cost.added_bytes + cost.replaced_bytes == len {
        Ok(())
    } else {
        hit(Rule::CostRefused);
        Err(CostError::StorageCostMismatch {
            expected: cost.clone(),
            actual_total_bytes: len,
        })
    }
}

/// What one write of a batch is charged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteCharge {
    /// Charged as soon as the write is taken from the batch: seeks and
    /// loaded bytes. It stays charged when a later write of the batch is
    /// refused.
    pub at_once: OperationCost,
    /// Charged only when the whole batch has been written: the storage
    /// cost.
    pub on_success: OperationCost,
}

fn storage_charge(seeks: u32, key: StorageCost, value: StorageCost) -> WriteCharge {
    let mut on_success = OperationCost::default();
    on_success.storage_cost += key;
    on_success.storage_cost += value;
    WriteCharge {
        at_once: OperationCost::with_seek_count(seeks),
        on_success,
    }
}

/// W2 to W4: a put, charged when its batch is applied. `key_len` is the
/// caller's key without the prefix, `value_len` the length of the record,
/// and `children_sizes` what a Merk gave with a node (never given outside
/// the data keyspace).
///
/// On a refused cost the seek is still owed, so it comes back with the
/// error.
pub fn put(
    keyspace: Keyspace,
    key_len: usize,
    value_len: u64,
    children_sizes: &ChildrenSizesWithIsSumTree,
    cost_info: Option<&KeyValueStorageCost>,
) -> Result<WriteCharge, (CostError, OperationCost)> {
    let seeks = if cost_info.is_some_and(KeyValueStorageCost::is_prepaid) {
        hit(Rule::PutPrepaid);
        0
    } else {
        1
    };
    hit(
        match (keyspace, children_sizes.is_some(), cost_info.is_some()) {
            (Keyspace::Data, true, _) => Rule::PutDataNode,
            (Keyspace::Data, false, _) => Rule::PutDataFlat,
            (Keyspace::Aux, ..) => Rule::PutAux,
            (Keyspace::Meta, ..) => Rule::PutMeta,
            (Keyspace::Roots, _, true) => Rule::PutRootsCharged,
            (Keyspace::Roots, _, false) => Rule::PutRootsFree,
        },
    );
    if keyspace == Keyspace::Roots && cost_info.is_none() {
        return Ok(WriteCharge {
            at_once: OperationCost::with_seek_count(seeks),
            on_success: OperationCost::default(),
        });
    }
    let no_children = None;
    let children_sizes = if keyspace == Keyspace::Data {
        children_sizes
    } else {
        &no_children
    };
    match key_value_storage(
        stored_key_len(key_len),
        value_len as u32,
        children_sizes,
        cost_info,
    ) {
        Ok((key, value)) => Ok(storage_charge(seeks, key, value)),
        Err(error) => Err((error, OperationCost::with_seek_count(seeks))),
    }
}

/// D1 and D2: a delete, charged when its batch is applied.
///
/// With the writer's cost it is one seek and the removed bytes the writer
/// gave. With none it is charged from the committed state, whatever the
/// transaction holds: two seeks, the committed record's length as loaded
/// bytes, and as removed bytes the stored key and that record, each with
/// its length varint. `committed_value_len` is `None` for a key the
/// committed state has no record of, which counts as an empty record.
pub fn delete(
    keyspace: Keyspace,
    key_len: usize,
    cost_info: Option<KeyValueStorageCost>,
    committed_value_len: Option<u64>,
) -> WriteCharge {
    let mut on_success = OperationCost::default();
    match cost_info {
        Some(info) => {
            hit(match keyspace {
                Keyspace::Data => Rule::DeleteDataWithCost,
                Keyspace::Aux => Rule::DeleteAuxWithCost,
                Keyspace::Roots => Rule::DeleteRootsWithCost,
                Keyspace::Meta => Rule::DeleteMetaWithCost,
            });
            on_success.storage_cost.removed_bytes += info.combined_removed_bytes();
            WriteCharge {
                at_once: OperationCost::with_seek_count(1),
                on_success,
            }
        }
        None => {
            hit(if committed_value_len.is_some() {
                Rule::DeleteCommittedRecord
            } else {
                Rule::DeleteUncommittedRecord
            });
            let value_len = committed_value_len.unwrap_or(0) as u32;
            let stored = stored_key_len(key_len);
            on_success.storage_cost.removed_bytes += StorageRemovedBytes::BasicStorageRemoval(
                stored
                    + value_len
                    + varint_len(u64::from(stored))
                    + varint_len(u64::from(value_len)),
            );
            WriteCharge {
                at_once: OperationCost {
                    seek_count: 2,
                    storage_loaded_bytes: u64::from(value_len),
                    ..Default::default()
                },
                on_success,
            }
        }
    }
}

/// M1: a put or a delete made directly through an immediate handle.
pub fn write_immediate() -> OperationCost {
    hit(Rule::ImmediateWrite);
    OperationCost::default()
}

/// M2: a put in the batch of an immediate handle, charged as it is added.
/// It is [`put`] with both parts charged at once.
pub fn immediate_batch_put(
    keyspace: Keyspace,
    key_len: usize,
    value_len: u64,
    children_sizes: &ChildrenSizesWithIsSumTree,
    cost_info: Option<&KeyValueStorageCost>,
) -> Result<OperationCost, (CostError, OperationCost)> {
    hit(Rule::ImmediateBatchPut);
    put(keyspace, key_len, value_len, children_sizes, cost_info)
        .map(|charge| charge.at_once + charge.on_success)
}

/// M2: a delete in the batch of an immediate handle, charged as it is
/// added: one seek, and the writer's removed bytes when it gave a cost.
/// Nothing is read for a delete with no cost.
pub fn immediate_batch_delete(cost_info: Option<KeyValueStorageCost>) -> OperationCost {
    hit(Rule::ImmediateBatchDelete);
    let mut cost = OperationCost::with_seek_count(1);
    if let Some(info) = cost_info {
        cost.storage_cost.removed_bytes += info.combined_removed_bytes();
    }
    cost
}

/// A move of a cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMove {
    /// To the first record of the subtree.
    SeekToFirst,
    /// To the last record of the subtree.
    SeekToLast,
    /// To the first key at or after a key.
    Seek,
    /// To the last key at or before a key.
    SeekForPrev,
    /// One record forward.
    Next,
    /// One record back.
    Prev,
}

/// C1: any move of a cursor.
pub fn cursor_move(movement: CursorMove) -> OperationCost {
    hit(match movement {
        CursorMove::SeekToFirst => Rule::CursorSeekToFirst,
        CursorMove::SeekToLast => Rule::CursorSeekToLast,
        CursorMove::Seek => Rule::CursorSeek,
        CursorMove::SeekForPrev => Rule::CursorSeekForPrev,
        CursorMove::Next => Rule::CursorNext,
        CursorMove::Prev => Rule::CursorPrev,
    });
    OperationCost::with_seek_count(1)
}

/// C2: asking a cursor for its key, or whether it is on a record.
/// `on_record` is the length of the key the cursor stands on, when that is
/// a record of the cursor's subtree.
pub fn cursor_key(on_record: Option<usize>) -> OperationCost {
    OperationCost::with_storage_loaded_bytes(match on_record {
        Some(key_len) => {
            hit(Rule::CursorKeyOnRecord);
            u64::from(stored_key_len(key_len))
        }
        None => {
            hit(Rule::CursorKeyOffRecord);
            CURSOR_BOUNDARY_BYTES
        }
    })
}

/// C3: asking a cursor for its value: [`cursor_key`], and the length of the
/// record when the cursor is on one.
pub fn cursor_value(on_record: Option<(usize, u64)>) -> OperationCost {
    let mut cost = cursor_key(on_record.map(|(key_len, _)| key_len));
    if let Some((_, value_len)) = on_record {
        hit(Rule::CursorValue);
        cost.storage_loaded_bytes += value_len;
    }
    cost
}

/// B1: the writes of a batch are handed to the engine. What its writes
/// charged at once is owed whatever happens. The storage cost,
/// `on_success`, is owed only when the engine took the writes.
pub fn batch_written(on_success: OperationCost, written: bool) -> OperationCost {
    if written {
        hit(Rule::BatchApplied);
        on_success
    } else {
        hit(Rule::BatchRefused);
        OperationCost::default()
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::storage_cost::removal::StorageRemovedBytes::{
        BasicStorageRemoval, NoStorageRemoval,
    };

    use super::*;

    fn count(rule: Rule) -> u64 {
        HITS[rule as usize].load(Ordering::Relaxed)
    }

    fn storage(added_bytes: u32, replaced_bytes: u32, removed: u32) -> StorageCost {
        StorageCost {
            added_bytes,
            replaced_bytes,
            removed_bytes: if removed == 0 {
                NoStorageRemoval
            } else {
                BasicStorageRemoval(removed)
            },
        }
    }

    fn info(
        key: StorageCost,
        value: StorageCost,
        new_node: bool,
        verify: bool,
    ) -> KeyValueStorageCost {
        KeyValueStorageCost {
            key_storage_cost: key,
            value_storage_cost: value,
            new_node,
            needs_value_verification: verify,
            prepaid: false,
        }
    }

    #[test]
    fn varint_lengths_follow_seven_bits_a_byte() {
        for (value, len) in [
            (0, 1),
            (1, 1),
            (127, 1),
            (128, 2),
            (16_383, 2),
            (16_384, 3),
            (u64::from(u32::MAX), 5),
            (u64::MAX, 10),
        ] {
            assert_eq!(varint_len(value), len, "{value}");
        }
        for (value, len) in [(0, 1), (-1, 1), (63, 1), (-64, 1), (64, 2), (-65, 2)] {
            assert_eq!(signed_varint_len(value), len, "{value}");
        }
        assert_eq!(signed_varint_len(i64::MIN), 10);
        assert_eq!(signed_varint_len(i64::MAX), 10);
    }

    #[test]
    fn a_prefix_is_charged_by_the_blocks_of_its_message() {
        assert_eq!(blake3_blocks(0), 1);
        assert_eq!(blake3_blocks(64), 1);
        assert_eq!(blake3_blocks(65), 2);
        assert_eq!(prefix_body_len([3, 4]), (3 + 4 + 8 + 2, 2));
        assert_eq!(open_by_path([]), OperationCost::default());
        assert_eq!(open_by_path([1]), OperationCost::with_hash_node_calls(1));
        assert_eq!(open_by_path([55]), OperationCost::with_hash_node_calls(1));
        assert_eq!(open_by_path([56]), OperationCost::with_hash_node_calls(2));
        assert_eq!(
            open_by_path([255, 255]),
            OperationCost::with_hash_node_calls(9)
        );
        assert_eq!(open_by_prefix(), OperationCost::default());
        assert_eq!(secondary_prefix(), OperationCost::with_hash_node_calls(1));
    }

    #[test]
    fn a_read_is_a_seek_and_the_record() {
        for keyspace in Keyspace::ALL {
            assert_eq!(
                read(keyspace, Some(70)),
                OperationCost {
                    seek_count: 1,
                    storage_loaded_bytes: 70,
                    ..Default::default()
                }
            );
            assert_eq!(read(keyspace, None), OperationCost::with_seek_count(1));
        }
        assert_eq!(read_immediate(), OperationCost::default());
        assert_eq!(write_immediate(), OperationCost::default());
    }

    #[test]
    fn a_new_data_record_gets_its_stored_key_added() {
        let update = info(storage(0, 0, 0), storage(0, 5, 0), false, false);
        assert_eq!(complete_new_key_cost(Some(update.clone()), 4), Some(update));
        let new = info(storage(0, 0, 0), storage(5, 0, 0), true, false);
        let completed = complete_new_key_cost(Some(new), 4).expect("a cost");
        assert_eq!(completed.key_storage_cost, storage(37, 0, 0));
        let long = info(storage(0, 0, 0), storage(5, 0, 0), true, false);
        let completed = complete_new_key_cost(Some(long), 200).expect("a cost");
        assert_eq!(completed.key_storage_cost, storage(232 + 2, 0, 0));
        assert_eq!(complete_new_key_cost(None, 4), None);
    }

    #[test]
    fn a_record_with_no_link_sizes_pays_its_length() {
        assert_eq!(paid_value_len(36, 0, &None), 1);
        assert_eq!(paid_value_len(36, 127, &None), 128);
        assert_eq!(paid_value_len(36, 128, &None), 130);
    }

    #[test]
    fn a_node_pays_its_value_and_the_link_to_it() {
        let leaf = Some((None, None, None));
        assert_eq!(paid_value_len(33, 68, &leaf), 66 + 1 + 33 + 4);
        let with_children = Some((None, Some((1 + 36, 0)), Some((2 + 36, 3))));
        assert_eq!(
            paid_value_len(33, 68 + 37 + 41, &with_children),
            66 + 1 + 33 + 4
        );
        let summed = Some((
            Some((TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes, 2)),
            None,
            None,
        ));
        assert_eq!(paid_value_len(33, 68 + 2, &summed), 66 + 8 + 1 + 33 + 4 + 8);
        let wide = Some((
            Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8BytesAnd16Bytes,
                17,
            )),
            None,
            None,
        ));
        assert_eq!(
            paid_value_len(33, 68 + 17, &wide),
            66 + 24 + 1 + 33 + 4 + 24
        );
        for (cost_type, size) in [
            (TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes, 16),
            (TreeCostType::TreeFeatureUses16Bytes, 16),
        ] {
            assert_eq!(feature_cost_size(&cost_type), size);
        }
        assert_eq!(paid_value_len(33, 1, &leaf), 1 + 33 + 4);
    }

    #[test]
    fn a_put_with_no_cost_adds_its_key_and_value() {
        let charge = put(Keyspace::Aux, 3, 10, &None, None).expect("charged");
        assert_eq!(charge.at_once, OperationCost::with_seek_count(1));
        assert_eq!(
            charge.on_success.storage_cost,
            storage(35 + 1 + 10 + 1, 0, 0)
        );
        let ignored = Some((None, Some((40, 0)), None));
        assert_eq!(
            put(Keyspace::Meta, 3, 10, &ignored, None).expect("charged"),
            charge
        );
    }

    #[test]
    fn a_put_takes_the_writers_cost_after_checking_it() {
        let key = storage(36, 0, 0);
        let given = info(key.clone(), storage(4, 7, 2), true, false);
        let charge = put(Keyspace::Data, 3, 500, &None, Some(&given)).expect("charged");
        assert_eq!(charge.on_success.storage_cost, storage(40, 7, 2));

        let verified = info(key.clone(), storage(4, 7, 0), true, true);
        let charge = put(Keyspace::Data, 3, 10, &None, Some(&verified)).expect("charged");
        assert_eq!(charge.on_success.storage_cost, storage(40, 7, 0));

        let wrong_value = info(key, storage(4, 8, 0), true, true);
        let (error, owed) =
            put(Keyspace::Data, 3, 10, &None, Some(&wrong_value)).expect_err("refused");
        assert!(matches!(
            error,
            CostError::StorageCostMismatch {
                actual_total_bytes: 11,
                ..
            }
        ));
        assert_eq!(owed, OperationCost::with_seek_count(1));

        let wrong_key = info(storage(35, 0, 0), storage(4, 7, 0), true, true);
        let (error, _) = put(Keyspace::Data, 3, 10, &None, Some(&wrong_key)).expect_err("refused");
        assert!(matches!(
            error,
            CostError::StorageCostMismatch {
                actual_total_bytes: 36,
                ..
            }
        ));

        let update = info(storage(0, 0, 0), storage(4, 7, 0), false, true);
        let charge = put(Keyspace::Data, 3, 10, &None, Some(&update)).expect("charged");
        assert_eq!(charge.on_success.storage_cost, storage(4, 7, 0));
    }

    #[test]
    fn a_root_key_put_is_charged_only_with_a_cost() {
        let free = put(Keyspace::Roots, 1, 5, &None, None).expect("charged");
        assert_eq!(free.at_once, OperationCost::with_seek_count(1));
        assert_eq!(free.on_success, OperationCost::default());
        let given = KeyValueStorageCost::for_updated_root_cost(None, 5);
        let charged = put(Keyspace::Roots, 1, 5, &None, Some(&given)).expect("charged");
        assert_eq!(charged.on_success.storage_cost, storage(34 + 6, 0, 0));
    }

    #[test]
    fn a_prepaid_put_is_free() {
        let prepaid = KeyValueStorageCost::prepaid();
        for keyspace in Keyspace::ALL {
            let charge = put(keyspace, 3, 10, &None, Some(&prepaid)).expect("charged");
            assert_eq!(charge, WriteCharge::default());
        }
    }

    #[test]
    fn a_delete_is_charged_from_its_cost_or_the_committed_record() {
        let given = info(storage(0, 0, 37), storage(0, 0, 80), false, false);
        for keyspace in Keyspace::ALL {
            let charge = delete(keyspace, 4, Some(given.clone()), Some(999));
            assert_eq!(charge.at_once, OperationCost::with_seek_count(1));
            assert_eq!(charge.on_success.storage_cost, storage(0, 0, 117));
        }
        let committed = delete(Keyspace::Data, 4, None, Some(200));
        assert_eq!(
            committed.at_once,
            OperationCost {
                seek_count: 2,
                storage_loaded_bytes: 200,
                ..Default::default()
            }
        );
        assert_eq!(
            committed.on_success.storage_cost,
            storage(0, 0, 36 + 200 + 1 + 2)
        );
        let uncommitted = delete(Keyspace::Data, 4, None, None);
        assert_eq!(uncommitted.at_once, OperationCost::with_seek_count(2));
        assert_eq!(
            uncommitted.on_success.storage_cost,
            storage(0, 0, 36 + 1 + 1)
        );
    }

    #[test]
    fn the_batch_of_an_immediate_handle_is_charged_as_it_is_filled() {
        let cost = immediate_batch_put(Keyspace::Aux, 3, 10, &None, None).expect("charged");
        assert_eq!(cost.seek_count, 1);
        assert_eq!(cost.storage_cost, storage(47, 0, 0));
        let wrong = info(storage(0, 0, 0), storage(1, 0, 0), false, true);
        assert!(immediate_batch_put(Keyspace::Aux, 3, 10, &None, Some(&wrong)).is_err());

        assert_eq!(
            immediate_batch_delete(None),
            OperationCost::with_seek_count(1)
        );
        let given = info(storage(0, 0, 3), storage(0, 0, 4), false, false);
        let cost = immediate_batch_delete(Some(given));
        assert_eq!(cost.seek_count, 1);
        assert_eq!(cost.storage_cost, storage(0, 0, 7));
    }

    #[test]
    fn a_cursor_charges_moves_keys_and_values() {
        for movement in [
            CursorMove::SeekToFirst,
            CursorMove::SeekToLast,
            CursorMove::Seek,
            CursorMove::SeekForPrev,
            CursorMove::Next,
            CursorMove::Prev,
        ] {
            assert_eq!(cursor_move(movement), OperationCost::with_seek_count(1));
        }
        assert_eq!(
            cursor_key(Some(5)),
            OperationCost::with_storage_loaded_bytes(37)
        );
        assert_eq!(
            cursor_key(None),
            OperationCost::with_storage_loaded_bytes(288)
        );
        assert_eq!(
            cursor_value(Some((5, 100))),
            OperationCost::with_storage_loaded_bytes(137)
        );
        assert_eq!(
            cursor_value(None),
            OperationCost::with_storage_loaded_bytes(288)
        );
    }

    #[test]
    fn a_refused_batch_owes_no_storage_cost() {
        let on_success = OperationCost::with_storage_written_bytes(9);
        assert_eq!(
            batch_written(on_success.clone(), false),
            OperationCost::default()
        );
        assert_eq!(batch_written(on_success.clone(), true), on_success);
    }

    #[test]
    fn every_rule_is_counted_and_named() {
        let before = count(Rule::SecondaryPrefix);
        secondary_prefix();
        assert!(count(Rule::SecondaryPrefix) > before);
        let report = hits_report();
        assert_eq!(report.lines().count(), Rule::ALL.len());
        for (rule, _) in hits() {
            assert!(!rule.text().is_empty());
            assert!(report.contains(rule.text()));
            assert!(report.contains(rule.section()));
        }
        for keyspace in Keyspace::ALL {
            assert_eq!(Keyspace::ALL[keyspace.index()], keyspace);
        }
    }
}
