//! The cost rules of the storage layer, as functions of logical sizes.
//!
//! What it costs to open a subtree handle, to read, write or remove a
//! record, and to move a cursor is defined here and nowhere else. A storage
//! engine reports events and sizes: the length of a stored key, of a record,
//! of a path segment. It returns the `OperationCost` these functions give
//! and works out no charge of its own. Two engines that report the same
//! events therefore charge the same, whatever each of them does to serve
//! them.
//!
//! The rules are specified in `docs/lignindb/cost-model.md`, section 5, and
//! each function names its rule there (P1, R1, W2 and so on).
//!
//! # Versions
//!
//! [`StorageCostModel`] is the version of the rules. Every function is a
//! method of it, so a caller decides once which rules apply and a new
//! version cannot be forgotten at a call site: it is a new variant, and the
//! compiler names every rule that has to say what the variant charges.
//!
//! # Sizes
//!
//! A record is stored under a **stored key**: the 32-byte prefix of its
//! subtree followed by the caller's key
//! ([`StorageCostModel::stored_key_len`]). The charges are defined on that
//! length whatever an engine keeps on disk. A **record length** is the
//! number of bytes of the record: for a Merk node the length of its
//! encoding ([`StorageCostModel::node_record_len`]), for any other record
//! the length of the caller's value.

mod v0;

use crate::{
    error::Error, storage_cost::key_value_cost::KeyValueStorageCost, ChildKeyLength,
    ChildSumLength, ChildrenSizesWithIsSumTree, FeatureSumLength, OperationCost,
};

/// The length of a subtree prefix.
pub const PREFIX_LEN: usize = 32;

/// One of the four keyspaces of a subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyspace {
    /// Merk nodes, or the records of an append-only tree.
    Data,
    /// Auxiliary records.
    Aux,
    /// The root key of a Merk that is not under a parent element.
    Roots,
    /// Metadata.
    Meta,
}

/// A version of the storage layer's cost rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StorageCostModel {
    /// What grove versions 1 to 5 charge.
    V0,
}

impl StorageCostModel {
    /// The length of the key a record is stored under: the subtree prefix
    /// and the caller's key.
    pub fn stored_key_len(self, key_len: usize) -> usize {
        match self {
            Self::V0 => v0::stored_key_len(key_len),
        }
    }

    /// N1: the record length of a Merk node, from the sizes of its fields.
    ///
    /// `feature_len` is the length of the node's feature without its tag
    /// byte. `left` and `right` are the sizes of its links as Merk passes
    /// them with a node write: for each child, the length of its key plus
    /// 36, and the length of its aggregate without the tag byte.
    pub fn node_record_len(
        self,
        value_len: usize,
        feature_len: FeatureSumLength,
        left: Option<(ChildKeyLength, ChildSumLength)>,
        right: Option<(ChildKeyLength, ChildSumLength)>,
    ) -> usize {
        match self {
            Self::V0 => v0::node_record_len(value_len, feature_len, left, right),
        }
    }

    /// P1: a subtree handle opened by its path, deferred or immediate, and
    /// any other derivation of a prefix from a path. `segment_lens` are the
    /// lengths of the path's segments.
    pub fn open_by_path(self, segment_lens: impl IntoIterator<Item = usize>) -> OperationCost {
        match self {
            Self::V0 => v0::open_by_path(segment_lens),
        }
    }

    /// P2: a subtree handle opened by a prefix the caller holds.
    pub fn open_by_prefix(self) -> OperationCost {
        match self {
            Self::V0 => v0::open_by_prefix(),
        }
    }

    /// P3: the prefix of a secondary of an indexed tree, derived from its
    /// primary's.
    pub fn secondary_prefix(self) -> OperationCost {
        match self {
            Self::V0 => v0::secondary_prefix(),
        }
    }

    /// R1: a point read through a deferred handle, in any keyspace. `found`
    /// is the length of the record. A read that fails is charged as one
    /// that found nothing.
    pub fn read(self, found: Option<usize>) -> OperationCost {
        match self {
            Self::V0 => v0::read(found),
        }
    }

    /// R2: a point read through an immediate handle.
    pub fn read_immediate(self) -> OperationCost {
        match self {
            Self::V0 => v0::read_immediate(),
        }
    }

    /// W1: the stored key of a new data record. A writer that describes a
    /// new record leaves the key out of its cost, and the stored key is
    /// added when the put is staged. Applied to a put into the data
    /// keyspace only, and never to an update.
    pub fn complete_new_key_cost(
        self,
        cost_info: Option<KeyValueStorageCost>,
        stored_key_len: usize,
    ) -> Option<KeyValueStorageCost> {
        match self {
            Self::V0 => v0::complete_new_key_cost(cost_info, stored_key_len),
        }
    }

    /// A write staged through a deferred handle, and a change set merged
    /// into the batch of one. It is charged when the batch is applied
    /// ([`Self::batch`]).
    pub fn staged_write(self) -> OperationCost {
        match self {
            Self::V0 => v0::staged_write(),
        }
    }

    /// A write a handle refuses: a deferred handle that was opened with no
    /// batch.
    pub fn refused_write(self) -> OperationCost {
        match self {
            Self::V0 => v0::refused_write(),
        }
    }

    /// B1: the charge of a batch that is being applied, to which each of
    /// its operations is added in the order of [`Self::batch_order`].
    pub fn batch(self) -> BatchCharge {
        BatchCharge {
            model: self,
            at_once: OperationCost::default(),
            on_success: OperationCost::default(),
        }
    }

    /// B1: the order in which the keyspaces of a batch are charged. Within
    /// a keyspace the operations are charged by stored key.
    ///
    /// The order shows in the result. Storage costs are added to one
    /// running cost, and on grove versions 1 to 3 adding a plain removal
    /// and a sectioned one does not give the same in both orders. A put
    /// that is refused leaves charged what came before it.
    pub fn batch_order(self) -> [Keyspace; 4] {
        match self {
            Self::V0 => v0::BATCH_ORDER,
        }
    }

    /// B1: what the storage cost of a batch comes to once its writes were
    /// handed to the engine. `on_success` is owed only when the engine took
    /// them.
    pub fn batch_written(self, on_success: OperationCost, written: bool) -> OperationCost {
        match self {
            Self::V0 => v0::batch_written(on_success, written),
        }
    }

    /// M1: a put or a delete made directly through an immediate handle. The
    /// writer's cost is ignored.
    pub fn write_immediate(self) -> OperationCost {
        match self {
            Self::V0 => v0::write_immediate(),
        }
    }

    /// M2: the charge of the batch of an immediate handle, to which each
    /// operation is added as it joins the batch.
    pub fn immediate_batch(self) -> ImmediateBatchCharge {
        ImmediateBatchCharge {
            model: self,
            cost: OperationCost::default(),
        }
    }

    /// C1: any move of a cursor.
    pub fn cursor_move(self) -> OperationCost {
        match self {
            Self::V0 => v0::cursor_move(),
        }
    }

    /// C2: a cursor asked for its key, or whether it is on a record.
    /// `on_record` is the length of the stored key the cursor stands on,
    /// when that is a record of the cursor's subtree.
    pub fn cursor_key(self, on_record: Option<usize>) -> OperationCost {
        match self {
            Self::V0 => v0::cursor_key(on_record),
        }
    }

    /// C3: a cursor asked for its value. `on_record` is the length of the
    /// stored key and of the record the cursor stands on, when that is a
    /// record of the cursor's subtree.
    pub fn cursor_value(self, on_record: Option<(usize, usize)>) -> OperationCost {
        match self {
            Self::V0 => v0::cursor_value(on_record),
        }
    }

    /// C4: the walk of a keyspace that is cleared. `stored_key_lens` are
    /// the lengths of the stored keys of its records. Each record is also
    /// deleted with no writer's cost, which is charged apart: through
    /// [`BatchCharge::delete_uncosted`] when the batch of a deferred handle
    /// is applied, and by [`Self::write_immediate`] on an immediate handle.
    pub fn clear_walk(self, stored_key_lens: impl IntoIterator<Item = usize>) -> OperationCost {
        match self {
            Self::V0 => v0::clear_walk(stored_key_lens),
        }
    }

    /// Committing a transaction.
    pub fn transaction_commit(self) -> OperationCost {
        match self {
            Self::V0 => v0::transaction_commit(),
        }
    }
}

/// B1: the charge of a batch while it is applied to a transaction.
///
/// It has two parts. Seeks and loaded bytes are owed as each operation is
/// taken from the batch, whatever becomes of the batch. Storage costs are
/// held back and owed only when every operation was accepted and the engine
/// took the writes.
///
/// The operations are added in the order of
/// [`StorageCostModel::batch_order`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchCharge {
    model: StorageCostModel,
    at_once: OperationCost,
    on_success: OperationCost,
}

impl BatchCharge {
    /// W2 to W4: a put. `value_len` is the length of the record, and
    /// `children_sizes` what a Merk gave with a node. They are used in the
    /// data keyspace only.
    ///
    /// A cost the writer gave that does not match the record refuses the
    /// batch (K3). The seek of the refused put stays charged.
    pub fn put(
        &mut self,
        keyspace: Keyspace,
        stored_key_len: usize,
        value_len: usize,
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        match self.model {
            StorageCostModel::V0 => {
                self.at_once.seek_count += v0::put_seeks(cost_info.as_ref());
                v0::put_storage(
                    &mut self.on_success,
                    keyspace,
                    stored_key_len,
                    value_len,
                    children_sizes,
                    cost_info,
                )
            }
        }
    }

    /// D1: a delete with the writer's cost. Nothing is read.
    pub fn delete(&mut self, cost_info: KeyValueStorageCost) {
        match self.model {
            StorageCostModel::V0 => {
                self.at_once.seek_count += v0::DELETE_SEEKS;
                v0::delete_storage(&mut self.on_success, cost_info);
            }
        }
    }

    /// D2: a delete with no writer's cost, charged from the **committed**
    /// state. `committed_value_len` is the outcome of reading, outside the
    /// transaction, the length of the record the committed state holds
    /// under the key: `None` when it holds none, which counts as an empty
    /// record. What the transaction itself wrote under the key does not
    /// count.
    ///
    /// A read that failed fails the batch with its error. The seeks stay
    /// charged.
    pub fn delete_uncosted<E>(
        &mut self,
        stored_key_len: usize,
        committed_value_len: Result<Option<usize>, E>,
    ) -> Result<(), E> {
        match self.model {
            StorageCostModel::V0 => {
                self.at_once.seek_count += v0::UNCOSTED_DELETE_SEEKS;
                let value_len = v0::committed_value_len(committed_value_len?);
                self.at_once.storage_loaded_bytes += u64::from(value_len);
                v0::uncosted_delete_storage(&mut self.on_success, stored_key_len, value_len);
                Ok(())
            }
        }
    }

    /// The two parts of the charge: what is owed at once, and what
    /// [`StorageCostModel::batch_written`] turns into the storage cost of
    /// the batch.
    pub fn into_parts(self) -> (OperationCost, OperationCost) {
        (self.at_once, self.on_success)
    }

    /// The whole charge of the batch. `written` says whether every
    /// operation was accepted and the engine took the writes. The storage
    /// cost joins what was charged at once only then, as one sum.
    pub fn finish(self, written: bool) -> OperationCost {
        let mut cost = self.model.batch_written(self.on_success, written);
        cost += self.at_once;
        cost
    }
}

/// M2: the charge of the batch of an immediate handle. Each operation is
/// charged in full as it is added, and the sum is owed when the batch is
/// committed, whether or not the engine takes the writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImmediateBatchCharge {
    model: StorageCostModel,
    cost: OperationCost,
}

impl ImmediateBatchCharge {
    /// A put: [`BatchCharge::put`] with both parts charged at once. A put
    /// into the data keyspace has its cost completed first
    /// ([`StorageCostModel::complete_new_key_cost`]).
    pub fn put(
        &mut self,
        keyspace: Keyspace,
        stored_key_len: usize,
        value_len: usize,
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        match self.model {
            StorageCostModel::V0 => {
                self.cost.seek_count += v0::put_seeks(cost_info.as_ref());
                v0::put_storage(
                    &mut self.cost,
                    keyspace,
                    stored_key_len,
                    value_len,
                    children_sizes,
                    cost_info,
                )
            }
        }
    }

    /// A delete: one seek, and the writer's removed bytes when it gave a
    /// cost. With none nothing more is charged and nothing is read, unlike
    /// [`BatchCharge::delete_uncosted`].
    pub fn delete(&mut self, cost_info: Option<KeyValueStorageCost>) {
        match self.model {
            StorageCostModel::V0 => {
                self.cost.seek_count += v0::DELETE_SEEKS;
                if let Some(cost_info) = cost_info {
                    v0::delete_storage(&mut self.cost, cost_info);
                }
            }
        }
    }

    /// The charge of the batch.
    pub fn finish(self) -> OperationCost {
        self.cost
    }
}

#[cfg(test)]
mod tests;
