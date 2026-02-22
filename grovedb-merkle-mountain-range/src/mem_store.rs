use std::{cell::RefCell, collections::BTreeMap};

use grovedb_costs::{CostResult, CostsExt, OperationCost};

use crate::{MMRStoreReadOps, MMRStoreWriteOps, MmrNode};

/// In-memory MMR store backed by a `BTreeMap`.
///
/// Useful for tests and ephemeral computations. All operations are zero-cost
/// (no storage I/O is tracked).
#[derive(Clone)]
pub struct MemStore(RefCell<BTreeMap<u64, MmrNode>>);

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemStore {
    fn new() -> Self {
        MemStore(RefCell::new(Default::default()))
    }
}

impl MMRStoreReadOps for &MemStore {
    fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, crate::Error> {
        Ok(self.0.borrow().get(&pos).cloned()).wrap_with_cost(OperationCost::default())
    }
}

impl MMRStoreWriteOps for &MemStore {
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> CostResult<(), crate::Error> {
        let mut store = self.0.borrow_mut();
        for (i, elem) in elems.into_iter().enumerate() {
            store.insert(pos + i as u64, elem);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}
