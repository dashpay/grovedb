use costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

use crate::{util::merk_optional_tx, Element, Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn is_empty_tree<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator + ExactSizeIterator,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();
        cost_return_on_error!(
            &mut cost,
            self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)
        );
        merk_optional_tx!(&mut cost, self.db, path_iter, transaction, subtree, {
            Ok(subtree.is_empty_tree().unwrap_add_cost(&mut cost)).wrap_with_cost(cost)
        })
    }
}
