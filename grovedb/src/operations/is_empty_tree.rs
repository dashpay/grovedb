//! Check if empty tree operations

#[cfg(feature = "full")]
use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
#[cfg(feature = "full")]
use grovedb_version::error::GroveVersionError;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

#[cfg(feature = "full")]
use crate::{util::merk_optional_tx, Element, Error, GroveDb, TransactionArg};

#[cfg(feature = "full")]
impl GroveDb {
    /// Check if it's an empty tree
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

        cost_return_on_error!(
            &mut cost,
            self.check_subtree_exists_path_not_found(path.clone(), transaction, grove_version)
        );
        merk_optional_tx!(
            &mut cost,
            self.db,
            path,
            None,
            transaction,
            subtree,
            grove_version,
            { Ok(subtree.is_empty_tree().unwrap_add_cost(&mut cost)).wrap_with_cost(cost) }
        )
    }
}
