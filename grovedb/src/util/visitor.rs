//! Utilities to traverse GroveDB with custom logic.

use std::collections::VecDeque;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::{rocksdb_storage::RocksDbStorage, Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    merk_cache::{MerkCache, MerkHandle},
    Element, Error, Transaction,
};

/// Structure for traversing a GroveDb in a breadth-first manner.
///
/// This implementation employs raw iterators directly on storage for
/// performance reasons. It bypasses Merk, skipping any caching features as
/// well. Originally designed for tree deletions, caution is advised when
/// involving other cached operations in related processes.
pub(crate) struct GroveVisitor<'db, V: Visit> {
    storage: &'db RocksDbStorage,
    transaction: &'db Transaction<'db>,
    visitor: V,
    grove_version: &'db GroveVersion,
}

impl<'db, V> GroveVisitor<'db, V>
where
    V: Visit,
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
        }
    }

    pub(crate) fn walk_from<'b, B: AsRef<[u8]>>(
        &mut self,
        from: SubtreePathBuilder<'b, B>,
    ) -> CostResult<V::Acc, Error> {
        let mut cost = OperationCost::default();

        let mut queue = VecDeque::new();
        queue.push_back(from);

        // TODO: use visitor
        while let Some(subtree_path) = queue.pop_front() {
            let storage = self
                .storage
                .get_transactional_storage_context(
                    SubtreePath::from(&subtree_path),
                    None,
                    self.transaction,
                )
                .unwrap_add_cost(&mut cost);
            let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);

            while let Some((key, value)) =
                cost_return_on_error!(&mut cost, raw_iter.next_element(self.grove_version))
            {
                let mut path = subtree_path.clone();
                path.push_segment(&key);
                if value.is_any_tree() {
                    queue.push_back(path);
                }
            }
        }

        todo!()
    }
}

/// Configurable logic to execute during a traversal process.
trait Visit {
    type Acc;

    fn visit_merk<B>(acc: &mut Self::Acc, path: SubtreePath<B>, m: MerkHandle);

    fn visit_element(acc: &mut Self::Acc, element: Element);
}
