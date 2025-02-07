//! Utilities to traverse GroveDB with custom logic.

use std::{collections::VecDeque, marker::PhantomData};

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use crate::{Element, Error, Transaction};

/// Structure for traversing a GroveDb in a breadth-first manner.
///
/// This implementation employs raw iterators directly on storage for
/// performance reasons. It bypasses Merk, skipping any caching features as
/// well. Originally designed for tree deletions, caution is advised when
/// involving other cached operations in related processes.
pub(crate) struct GroveVisitor<'db, 'b, B, V: Visit<'b, B>> {
    storage: &'db RocksDbStorage,
    transaction: &'db Transaction<'db>,
    visitor: V,
    grove_version: &'db GroveVersion,
    batch: StorageBatch,
    _base: PhantomData<&'b B>,
}

impl<'db, 'b, B, V> GroveVisitor<'db, 'b, B, V>
where
    V: Visit<'b, B>,
{
    pub(crate) fn new(
        storage: &'db RocksDbStorage,
        transaction: &'db Transaction<'db>,
        visitor: V,
        grove_version: &'db GroveVersion,
    ) -> Self {
        Self {
            storage,
            transaction,
            visitor,
            grove_version,
            batch: Default::default(),
            _base: PhantomData,
        }
    }

    pub(crate) fn walk_from(
        mut self,
        from: SubtreePathBuilder<'b, B>,
    ) -> CostResult<(StorageBatch, V), Error>
    where
        B: AsRef<[u8]>,
    {
        let mut cost = OperationCost::default();

        let mut queue = VecDeque::new();
        queue.push_back(from);

        while let Some(subtree_path) = queue.pop_front() {
            let storage = self
                .storage
                .get_transactional_storage_context(
                    SubtreePath::from(&subtree_path),
                    Some(&self.batch),
                    self.transaction,
                )
                .unwrap_add_cost(&mut cost);

            cost_return_on_error!(&mut cost, self.visitor.visit_merk(subtree_path.clone()));

            let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);

            while let Some((key, value)) =
                cost_return_on_error!(&mut cost, raw_iter.next_element(self.grove_version))
            {
                if value.is_any_tree() {
                    let mut path = subtree_path.clone();
                    path.push_segment(&key);
                    queue.push_back(path);
                }

                cost_return_on_error!(
                    &mut cost,
                    self.visitor
                        .visit_element(subtree_path.clone(), &key, &storage, value)
                );
            }
        }

        Ok((self.batch, self.visitor)).wrap_with_cost(cost)
    }
}

/// Configurable logic to execute during a traversal process.
pub(crate) trait Visit<'b, B> {
    fn visit_merk(&mut self, path: SubtreePathBuilder<'b, B>) -> CostResult<(), Error>;

    fn visit_element(
        &mut self,
        path: SubtreePathBuilder<'b, B>,
        key: &[u8],
        storage: &PrefixedRocksDbTransactionContext,
        element: Element,
    ) -> CostResult<(), Error>;
}
