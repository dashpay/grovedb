use grovedb_costs::{CostResult, CostsExt, OperationCost};

use crate::{Error, MmrNode};

/// Write-ahead batch buffer for MMR mutations.
///
/// Appended elements are held in memory and served back on reads (overlay
/// semantics). [`MMRBatch::commit`] flushes the buffer to the underlying
/// store.
#[derive(Default)]
pub struct MMRBatch<Store> {
    memory_batch: Vec<(u64, Vec<MmrNode>)>,
    store: Store,
}

impl<Store> MMRBatch<Store> {
    /// Create a new batch wrapping the given store.
    pub fn new(store: Store) -> Self {
        MMRBatch {
            memory_batch: Vec::new(),
            store,
        }
    }

    /// Buffer a contiguous run of elements starting at `pos`.
    pub fn append(&mut self, pos: u64, elems: Vec<MmrNode>) {
        self.memory_batch.push((pos, elems));
    }

    /// Return a reference to the underlying store.
    pub fn store(&self) -> &Store {
        &self.store
    }
}

impl<Store: MMRStoreReadOps> MMRBatch<Store> {
    /// Look up an element by position, checking the in-memory batch first.
    ///
    /// Cache hits return the same cost as a store read (seek + loaded bytes)
    /// for deterministic fee estimation.
    pub fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, Error> {
        for (start_pos, elems) in self.memory_batch.iter().rev() {
            if pos < *start_pos {
                continue;
            } else if pos < start_pos + elems.len() as u64 {
                let elem = elems.get((pos - start_pos) as usize).cloned();
                let loaded_bytes = elem.as_ref().map_or(0, |e| e.serialized_size());
                return Ok(elem).wrap_with_cost(OperationCost {
                    seek_count: 1,
                    storage_loaded_bytes: loaded_bytes,
                    ..Default::default()
                });
            } else {
                break;
            }
        }
        self.store.element_at_position(pos)
    }
}

impl<Store: MMRStoreWriteOps> MMRBatch<Store> {
    /// Flush all buffered elements to the underlying store.
    pub fn commit(&mut self) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        for (pos, elems) in self.memory_batch.drain(..) {
            let result = self.store.append(pos, elems).unwrap_add_cost(&mut cost);
            if let Err(e) = result {
                return Err(e).wrap_with_cost(cost);
            }
        }
        Ok(()).wrap_with_cost(cost)
    }
}

impl<Store> IntoIterator for MMRBatch<Store> {
    type IntoIter = std::vec::IntoIter<Self::Item>;
    type Item = (u64, Vec<MmrNode>);

    fn into_iter(self) -> Self::IntoIter {
        self.memory_batch.into_iter()
    }
}

/// Read access to the MMR backing store.
///
/// Implementations return the element at a given MMR position, or `None` if
/// the position has not been written yet.
pub trait MMRStoreReadOps {
    /// Retrieve the element stored at `pos`, if any.
    fn element_at_position(&self, pos: u64) -> CostResult<Option<MmrNode>, Error>;
}

/// Write access to the MMR backing store.
///
/// Implementations persist a contiguous run of elements starting at `pos`.
pub trait MMRStoreWriteOps {
    /// Persist `elems` starting at position `pos`.
    fn append(&mut self, pos: u64, elems: Vec<MmrNode>) -> CostResult<(), Error>;
}
