//! Recursive discovery for Grove V4+.
//!
//! Non-Merk descendants remain in the cleanup result but are never traversed:
//! their data records are not Merk nodes and cannot contain child subtrees.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use std::collections::HashSet;

use crate::{
    element::elements_iterator::ElementIteratorExtensions, util::TxRef, Element, Error, GroveDb,
    TransactionArg,
};

impl GroveDb {
    pub(super) fn find_subtrees_v1<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Vec<Vec<u8>>>, Error> {
        self.walk_subtrees_v1(path, transaction, grove_version, |_, _, _| Ok(()))
    }

    /// `find_subtrees_v1` that also checks the claim a recursive removal
    /// makes about its contents on the elements the walk decodes anyway:
    /// a backward-reference participant not in `accounted_deletes` (the
    /// positions the batch deletes explicitly) refuses the removal.
    pub(crate) fn find_subtrees_refusing_participants_v1<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
        accounted_deletes: &HashSet<Vec<Vec<u8>>>,
    ) -> CostResult<Vec<Vec<Vec<u8>>>, Error> {
        self.walk_subtrees_v1(
            path,
            transaction,
            grove_version,
            |subtree_path, key, element| {
                if !element.supports_backward_references() {
                    return Ok(());
                }
                let mut qualified = subtree_path.to_vec();
                qualified.push(key.to_vec());
                if accounted_deletes.contains(&qualified) {
                    return Ok(());
                }
                Err(Error::NotSupported(
                    "a recursive subtree removal reached a backward-reference participant it does \
                 not explicitly delete; delete the participant first, or use live delete with \
                 DisplacedValue::MayBeParticipant for recursive maintenance"
                        .to_owned(),
                ))
            },
        )
    }

    fn walk_subtrees_v1<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
        mut visit: impl FnMut(&[Vec<u8>], &[u8], &Element) -> Result<(), Error>,
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
                cost_return_on_error_no_add!(cost, visit(&q, &key, &value));
                if value.is_any_tree() {
                    let mut sub_path = q.clone();
                    sub_path.push(key.to_vec());
                    // Only Merk namespaces can contain more subtrees. Use
                    // the element already decoded from the parent to classify
                    // storage, including through element wrappers, without
                    // adding a read or dropping the namespace from cleanup.
                    if !value.uses_non_merk_data_storage() {
                        queue.push(sub_path.clone());
                    }
                    result.push(sub_path);
                }
            }
        }
        Ok(result).wrap_with_cost(cost)
    }
}
