//! A branch-local count page read through one pinned storage view.
//!
//! A RocksDB transaction iterator pins an implicit snapshot of the committed
//! state at its creation, plus the transaction's own uncommitted writes.
//! Independent point reads through a plain transaction do not: a commit landing
//! between reading a subtree's root key and loading that key can leave it naming
//! a demoted child, an internally consistent subtree every count check accepts,
//! which silently truncates the page. Serving the root and every traversal fetch
//! from the iterator that the caller read the root key through closes that gap.

use std::{cell::RefCell, rc::Rc};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_storage::RawIterator;
use grovedb_version::{check_merk_v0_with_cost, version::GroveVersion};

use super::is_provable_count_bearing;
use crate::{
    proofs::query::QueryItem,
    tree::{kv::ValueDefinedCostType, Fetch, Link, RefWalker, TreeNode},
    Error, TreeType,
};

/// Serves Merk node fetches from one pinned raw iterator. The iterator needs
/// `&mut` to seek while `Fetch` takes `&self`, and walkers clone their source,
/// so the clones share it through a `RefCell`; the walk is single threaded.
/// Only read-only page walks fetch through it, so its nodes skip the copy of the
/// value a node keeps as its mutation baseline, and none leaves this module.
pub(super) struct PinnedViewSource<I> {
    view: Rc<RefCell<I>>,
}

impl<I> Clone for PinnedViewSource<I> {
    fn clone(&self) -> Self {
        PinnedViewSource {
            view: self.view.clone(),
        }
    }
}

impl<I: RawIterator> PinnedViewSource<I> {
    pub(super) fn new(view: I) -> Self {
        PinnedViewSource {
            view: Rc::new(RefCell::new(view)),
        }
    }

    /// The node is decoded without the copy of its value a commit needs, so it
    /// must never reach one. It stays inside this read-only walk.
    pub(super) fn fetch_key(&self, key: &[u8]) -> CostResult<TreeNode, Error> {
        let mut cost = OperationCost::default();
        let mut view = self.view.borrow_mut();
        let stored = cost_return_on_error!(&mut cost, stored_node_bytes(&mut *view, key));
        // A key this view's own parent or element names must be in this view,
        // so a miss is corruption rather than a race.
        let Some(bytes) = stored else {
            return Err(Error::KeyNotFoundError("Key not found for fetch")).wrap_with_cost(cost);
        };
        TreeNode::decode_raw_for_read_only_walk(key.to_vec(), bytes).wrap_with_cost(cost)
    }
}

/// Reads the value of the element stored at `key` through `view`, or `None`
/// when the view holds no such key. It is charged as an ordinary node fetch
/// through a storage `get` is, with one seek and the stored bytes, so the same
/// nodes cost the same however they are read. Only the value leaves: the node
/// holding it is decoded without a mutation baseline.
pub fn read_value_from_view<I: RawIterator>(
    view: &mut I,
    key: &[u8],
) -> CostResult<Option<Vec<u8>>, Error> {
    let mut cost = OperationCost::default();
    let stored = cost_return_on_error!(&mut cost, stored_node_bytes(view, key));
    let Some(bytes) = stored else {
        return Ok(None).wrap_with_cost(cost);
    };
    TreeNode::decode_raw_for_read(bytes)
        .map(|node| Some(node.value_as_owned()))
        .wrap_with_cost(cost)
}

/// Seeks `key` in `view` and returns the node bytes stored there, or `None`
/// when the view holds no such key, charging one seek and the bytes loaded. The
/// iterator's own charges for reading the key, once to compare it and again
/// inside its value read, are not billed.
fn stored_node_bytes<'v, I: RawIterator>(
    view: &'v mut I,
    key: &[u8],
) -> CostResult<Option<&'v [u8]>, Error> {
    let mut cost = OperationCost::default();
    view.seek(key).unwrap_add_cost(&mut cost);
    // A failed seek reports no record, as a missing key does.
    cost_return_on_error_no_add!(cost, view.status().map_err(Error::StorageError));
    if view.key().unwrap() != Some(key) {
        return Ok(None).wrap_with_cost(cost);
    }
    let bytes = view.value().unwrap();
    if let Some(bytes) = bytes {
        cost.storage_loaded_bytes += bytes.len() as u64;
    }
    Ok(bytes).wrap_with_cost(cost)
}

impl<I: RawIterator> Fetch for PinnedViewSource<I> {
    fn fetch(
        &self,
        link: &Link,
        _value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        _grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        self.fetch_key(link.key())
    }
}

/// Runs the local count-unit traversal of
/// [`Merk::prove_local_count_offset_on_range`](crate::Merk::prove_local_count_offset_on_range)
/// against `view`, a raw iterator scoped to the tree's prefix, so the page comes
/// from one stable state, and returns its `(key, value)` rows in walk order.
/// `root_key` must have been read through the same view.
#[allow(clippy::too_many_arguments)]
pub fn local_count_offset_page_from_view<I: RawIterator>(
    view: I,
    root_key: Option<Vec<u8>>,
    tree_type: TreeType,
    inner_range: &QueryItem,
    offset: u64,
    limit: Option<u64>,
    left_to_right: bool,
    grove_version: &GroveVersion,
) -> CostResult<Vec<(Vec<u8>, Vec<u8>)>, Error> {
    check_merk_v0_with_cost!(
        "local_count_offset_page_from_view",
        grove_version
            .merk_versions
            .proof
            .prove_count_offset_on_range
    );
    let mut cost = OperationCost::default();
    if !is_provable_count_bearing(tree_type) {
        return Err(Error::InvalidProofError(format!(
            "count-offset paginated proof is only valid against ProvableCountTree, \
             ProvableCountSumTree, or ProvableCountProvableSumTree, got {:?}",
            tree_type
        )))
        .wrap_with_cost(cost);
    }
    let Some(root_key) = root_key else {
        return Ok(Vec::new()).wrap_with_cost(cost);
    };
    let source = PinnedViewSource::new(view);
    let mut root = cost_return_on_error!(&mut cost, source.fetch_key(&root_key));
    RefWalker::new(&mut root, source)
        .read_local_count_offset_rows(
            inner_range,
            offset,
            limit,
            left_to_right,
            tree_type,
            grove_version,
        )
        .add_cost(cost)
}
