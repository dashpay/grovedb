use crate::{
    store::{NodeChangeSet, NodeCursor, NodeStore},
    Error, Merk,
};
use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

impl<'db, S> Merk<S>
where
    S: NodeStore<'db>,
{
    /// Deletes all tree data (nodes) from storage.
    ///
    /// Deletes are issued with `cost_info: None`, so freed-bytes cost is
    /// estimated from committed DB state. This is acceptable for a
    /// bulk-clear operation. See `RocksDbStorage::continue_write_batch`
    /// for the full rationale.
    pub fn clear(&mut self) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut iter = self.storage.cursor();
        iter.seek_to_first().unwrap_add_cost(&mut cost);

        let mut to_delete = self.storage.change_set();
        while iter.valid().unwrap_add_cost(&mut cost) {
            if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                to_delete.delete_node(key, None);
            }
            iter.next().unwrap_add_cost(&mut cost);
        }
        cost_return_on_error!(&mut cost, self.storage.commit(to_delete));
        self.tree.set(None);
        Ok(()).wrap_with_cost(cost)
    }
}
