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

use super::{is_provable_count_bearing, ProverCountOffsetResult};
use crate::{
    proofs::query::QueryItem,
    tree::{kv::ValueDefinedCostType, Fetch, Link, RefWalker, TreeNode},
    Error, TreeType,
};

/// Serves Merk node fetches from one pinned raw iterator. The iterator needs
/// `&mut` to seek while `Fetch` takes `&self`, and walkers clone their source,
/// so the clones share it through a `RefCell`; the walk is single threaded.
struct PinnedViewSource<I> {
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
    fn fetch_key(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        let mut cost = OperationCost::default();
        let mut view = self.view.borrow_mut();
        view.seek(key).unwrap_add_cost(&mut cost);
        // A failed seek reports no record, as a missing key does.
        cost_return_on_error_no_add!(cost, view.status().map_err(Error::StorageError));
        let found = view
            .key()
            .unwrap_add_cost(&mut cost)
            .is_some_and(|found| found == key);
        // A key this view's own parent or element names must be in this view,
        // so a miss is corruption rather than a race.
        if !found {
            return Err(Error::KeyNotFoundError("Key not found for fetch")).wrap_with_cost(cost);
        }
        let Some(bytes) = view.value().unwrap_add_cost(&mut cost) else {
            return Err(Error::KeyNotFoundError("Key not found for fetch")).wrap_with_cost(cost);
        };
        TreeNode::decode_raw(bytes, key.to_vec(), value_defined_cost_fn, grove_version)
            .wrap_with_cost(cost)
    }
}

impl<I: RawIterator> Fetch for PinnedViewSource<I> {
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        self.fetch_key(link.key(), value_defined_cost_fn, grove_version)
    }
}

/// Runs the local count-unit traversal of
/// [`Merk::prove_local_count_offset_on_range`](crate::Merk::prove_local_count_offset_on_range)
/// against `view`, a raw iterator scoped to the tree's prefix, so the page comes
/// from one stable state. `root_key` must have been read through the same view.
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
) -> CostResult<ProverCountOffsetResult, Error> {
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
        return Ok(ProverCountOffsetResult {
            ops: Default::default(),
            returned: 0,
            offset_remaining: offset,
            returned_rows: Vec::new(),
        })
        .wrap_with_cost(cost);
    };
    let source = PinnedViewSource {
        view: Rc::new(RefCell::new(view)),
    };
    let mut root = cost_return_on_error!(
        &mut cost,
        source.fetch_key(
            &root_key,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
    );
    RefWalker::new(&mut root, source)
        .create_local_count_offset_on_range_proof(
            inner_range,
            offset,
            limit,
            left_to_right,
            tree_type,
            grove_version,
        )
        .add_cost(cost)
}

#[cfg(test)]
mod tests {
    use grovedb_costs::CostContext;

    use super::*;

    /// Holds no record at any position, as an iterator past its records and one
    /// stopped on an error both report. Only `status` tells them apart.
    struct NoRecordIterator {
        failure: Option<&'static str>,
    }

    impl RawIterator for NoRecordIterator {
        fn seek_to_first(&mut self) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek_to_last(&mut self) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn next(&mut self) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn prev(&mut self) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn value(&self) -> CostContext<Option<&[u8]>> {
            None.wrap_with_cost(OperationCost::default())
        }

        fn key(&self) -> CostContext<Option<&[u8]>> {
            None.wrap_with_cost(OperationCost::default())
        }

        fn valid(&self) -> CostContext<bool> {
            false.wrap_with_cost(OperationCost::default())
        }

        fn status(&self) -> Result<(), grovedb_storage::Error> {
            match self.failure {
                Some(failure) => Err(grovedb_storage::Error::StorageError(failure.to_owned())),
                None => Ok(()),
            }
        }
    }

    fn page_from(view: NoRecordIterator) -> CostContext<Result<ProverCountOffsetResult, Error>> {
        local_count_offset_page_from_view(
            view,
            Some(b"root".to_vec()),
            TreeType::ProvableCountTree,
            &QueryItem::RangeFull(..),
            1,
            Some(1),
            true,
            GroveVersion::latest(),
        )
    }

    #[test]
    fn should_report_a_failed_seek_rather_than_a_missing_key() {
        let page = page_from(NoRecordIterator {
            failure: Some("injected read failure"),
        });
        assert_eq!(page.cost.seek_count, 1, "the seek's cost must be retained");
        assert!(matches!(
            page.value,
            Err(Error::StorageError(grovedb_storage::Error::StorageError(message)))
                if message == "injected read failure"
        ));
    }

    #[test]
    fn should_report_a_clean_miss_as_a_missing_key() {
        let page = page_from(NoRecordIterator { failure: None });
        assert_eq!(page.cost.seek_count, 1);
        assert!(matches!(page.value, Err(Error::KeyNotFoundError(_))));
    }
}
