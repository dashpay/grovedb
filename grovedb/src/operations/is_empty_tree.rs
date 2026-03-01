//! Check if empty tree operations

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{
    util::{compat, TxRef},
    Error, GroveDb, TransactionArg,
};

impl GroveDb {
    /// Check if it's an empty tree.
    ///
    /// For non-Merk data trees (CommitmentTree, MmrTree, BulkAppendTree,
    /// DenseAppendOnlyFixedSizeTree), this checks the element's entry count
    /// from the parent rather than iterating the Merk (which is always
    /// empty for these types).
    pub fn is_empty_tree<'b, B, P>(
        &self,
        path: P,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "is_empty_tree",
            grove_version.grovedb_versions.operations.get.is_empty_tree
        );
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();

        let tx = TxRef::new(&self.db, transaction);

        cost_return_on_error!(
            &mut cost,
            self.check_subtree_exists_path_not_found(path.clone(), tx.as_ref(), grove_version)
        );

        // For non-root paths, check if this is a non-Merk data tree by
        // reading the element from the parent.  Non-Merk trees have an
        // always-empty Merk, so we must check the element's entry count.
        if let Some((parent_path, key)) = path.derive_parent() {
            let element = cost_return_on_error!(
                &mut cost,
                self.get_raw(parent_path, key, Some(tx.as_ref()), grove_version)
            );
            if let Some(count) = element.non_merk_entry_count() {
                return Ok(count == 0).wrap_with_cost(cost);
            }
        }

        let subtree = cost_return_on_error!(
            &mut cost,
            compat::merk_optional_tx(&self.db, path, tx.as_ref(), None, grove_version)
        );

        subtree.is_empty_tree().add_cost(cost).map(Ok)
    }
}
