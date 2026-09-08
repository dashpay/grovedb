//! Legacy recursive discovery for Grove V1..V3.
//!
//! Every descendant is traversed as Merk storage. Preserve the historical
//! traversal, costs, and non-Merk decoding errors for replay compatibility.

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    element::elements_iterator::ElementIteratorExtensions, util::TxRef, Element, Error, GroveDb,
    TransactionArg,
};

impl GroveDb {
    pub(super) fn find_subtrees_v0<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Vec<Vec<u8>>>, Error> {
        let mut cost = OperationCost::default();

        // TODO: remove conversion to vec;
        // However, it's not easy for a reason:
        // new keys to enqueue are taken from raw iterator which returns Vec<u8>;
        // changing that to slice is hard as cursor should be moved for next iteration
        // which requires exclusive (&mut) reference, also there is no guarantee that
        // slice which points into storage internals will remain valid if raw
        // iterator got altered so why that reference should be exclusive;
        //
        // Update: there are pinned views into RocksDB to return slices of data, perhaps
        // there is something for iterators

        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.to_vec()];
        let mut result: Vec<Vec<Vec<u8>>> = queue.clone();

        let tx = TxRef::new(&self.db, transaction);

        while let Some(q) = queue.pop() {
            let subtree_path: SubtreePath<Vec<u8>> = q.as_slice().into();
            // Get the correct subtree with q_ref as path
            let storage = self
                .db
                .get_transactional_storage_context(subtree_path, None, tx.as_ref())
                .unwrap_add_cost(&mut cost);
            let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
            while let Some((key, value)) =
                cost_return_on_error!(&mut cost, raw_iter.next_element(grove_version))
            {
                if value.is_any_tree() {
                    let mut sub_path = q.clone();
                    sub_path.push(key.to_vec());
                    queue.push(sub_path.clone());
                    result.push(sub_path);
                }
            }
        }
        Ok(result).wrap_with_cost(cost)
    }
}
