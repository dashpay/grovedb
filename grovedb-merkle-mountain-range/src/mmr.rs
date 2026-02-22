//! Merkle Mountain Range (MMR) — an append-only authenticated data structure.

use std::{borrow::Cow, collections::VecDeque};

use grovedb_costs::{CostResult, CostsExt, OperationCost};

use crate::{
    Error, MmrNode, Result,
    helper::{get_peak_map, get_peaks, parent_offset, pos_height_in_tree, sibling_offset},
    mmr_store::{MMRBatch, MMRStoreReadOps, MMRStoreWriteOps},
    proof::{MerkleProof, take_while_vec},
};

/// A Merkle Mountain Range backed by a pluggable store.
///
/// `S` is the backing store (implements [`MMRStoreReadOps`] and/or
/// [`MMRStoreWriteOps`]). Elements are always [`MmrNode`] values merged
/// with Blake3 domain-separated hashing.
///
/// Mutations are buffered in an [`MMRBatch`]; call [`MMR::commit`] to flush
/// them to the store.
#[allow(clippy::upper_case_acronyms)]
pub struct MMR<S> {
    mmr_size: u64,
    batch: MMRBatch<S>,
}

impl<S> MMR<S> {
    /// Create a new MMR starting at the given size, backed by `store`.
    ///
    /// Use `mmr_size = 0` for a fresh, empty MMR. To resume an existing
    /// MMR, pass the size returned by [`MMR::mmr_size`] after the last
    /// committed operation.
    pub fn new(mmr_size: u64, store: S) -> Self {
        MMR {
            mmr_size,
            batch: MMRBatch::new(store),
        }
    }

    /// The current total number of nodes (leaves + internal) in the MMR.
    pub fn mmr_size(&self) -> u64 {
        self.mmr_size
    }

    /// Returns `true` if the MMR contains no elements.
    pub fn is_empty(&self) -> bool {
        self.mmr_size == 0
    }

    /// Return a reference to the internal [`MMRBatch`].
    pub fn batch(&self) -> &MMRBatch<S> {
        &self.batch
    }

    /// Return a reference to the underlying store.
    pub fn store(&self) -> &S {
        self.batch.store()
    }
}

impl<S: MMRStoreReadOps> MMR<S> {
    // Find an element by position, checking the in-flight batch first.
    fn find_element_at_position<'b>(
        &self,
        pos: u64,
        hashes: &'b [MmrNode],
    ) -> CostResult<Cow<'b, MmrNode>, Error> {
        let mut cost = OperationCost::default();
        let pos_offset = pos.checked_sub(self.mmr_size);
        if let Some(elem) = pos_offset.and_then(|i| hashes.get(i as usize)) {
            return Ok(Cow::Borrowed(elem)).wrap_with_cost(cost);
        }
        let elem = self
            .batch
            .element_at_position(pos)
            .unwrap_add_cost(&mut cost);
        match elem {
            Ok(Some(e)) => Ok(Cow::Owned(e)).wrap_with_cost(cost),
            Ok(None) => Err(Error::InconsistentStore).wrap_with_cost(cost),
            Err(e) => Err(e).wrap_with_cost(cost),
        }
    }

    /// Append a leaf element and return its position in the MMR.
    ///
    /// This may also create internal (merged) nodes. The new nodes are
    /// buffered until [`MMR::commit`] is called.
    pub fn push(&mut self, elem: MmrNode) -> CostResult<u64, Error> {
        let mut cost = OperationCost::default();
        let mut elems = vec![elem];
        let elem_pos = self.mmr_size;
        let peak_map = get_peak_map(self.mmr_size);
        let mut pos = self.mmr_size;
        let mut peak = 1;
        while (peak_map & peak) != 0 {
            peak <<= 1;
            pos += 1;
            let left_pos = pos - peak;
            let left_elem = self
                .find_element_at_position(left_pos, &elems)
                .unwrap_add_cost(&mut cost);
            let left_elem = match left_elem {
                Ok(e) => e,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };
            let right_elem = elems.last().expect("checked");
            let parent_elem = MmrNode::merge(&left_elem, right_elem);
            elems.push(parent_elem);
        }
        // store hashes
        self.batch.append(elem_pos, elems);
        // update mmr_size
        self.mmr_size = pos + 1;
        Ok(elem_pos).wrap_with_cost(cost)
    }

    /// Compute the root hash by bagging all peaks right-to-left.
    ///
    /// Returns [`Error::GetRootOnEmpty`] for an empty MMR.
    pub fn get_root(&self) -> CostResult<MmrNode, Error> {
        let mut cost = OperationCost::default();
        if self.mmr_size == 0 {
            return Err(Error::GetRootOnEmpty).wrap_with_cost(cost);
        } else if self.mmr_size == 1 {
            let elem = self.batch.element_at_position(0).unwrap_add_cost(&mut cost);
            return match elem {
                Ok(Some(e)) => Ok(e).wrap_with_cost(cost),
                Ok(None) => Err(Error::InconsistentStore).wrap_with_cost(cost),
                Err(e) => Err(e).wrap_with_cost(cost),
            };
        }
        let peaks_result: Result<Vec<MmrNode>> = get_peaks(self.mmr_size)
            .into_iter()
            .map(|peak_pos| {
                let elem = self
                    .batch
                    .element_at_position(peak_pos)
                    .unwrap_add_cost(&mut cost);
                elem.and_then(|e| e.ok_or(Error::InconsistentStore))
            })
            .collect();
        let peaks = match peaks_result {
            Ok(p) => p,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        match bag_peaks(peaks) {
            Ok(Some(root)) => Ok(root).wrap_with_cost(cost),
            Ok(None) => Err(Error::InconsistentStore).wrap_with_cost(cost),
            Err(e) => Err(e).wrap_with_cost(cost),
        }
    }

    // Generate the Merkle proof fragment for a single peak sub-tree.
    // `pos_list` must be sorted.
    fn gen_proof_for_peak(
        &self,
        proof: &mut Vec<MmrNode>,
        pos_list: Vec<u64>,
        peak_pos: u64,
        cost: &mut OperationCost,
    ) -> Result<()> {
        // do nothing if position itself is the peak
        if pos_list.len() == 1 && pos_list == [peak_pos] {
            return Ok(());
        }
        // take peak root from store if no positions need to be proof
        if pos_list.is_empty() {
            let elem = self
                .batch
                .element_at_position(peak_pos)
                .unwrap_add_cost(cost);
            proof.push(elem?.ok_or(Error::InconsistentStore)?);
            return Ok(());
        }

        let mut queue: VecDeque<_> = pos_list.into_iter().map(|pos| (pos, 0)).collect();

        // Generate sub-tree merkle proof for positions
        while let Some((pos, height)) = queue.pop_front() {
            debug_assert!(pos <= peak_pos);
            if pos == peak_pos {
                if queue.is_empty() {
                    break;
                } else {
                    return Err(Error::NodeProofsNotSupported);
                }
            }

            // calculate sibling
            let (sib_pos, parent_pos) = {
                let next_height = pos_height_in_tree(pos + 1);
                let sibling_offset = sibling_offset(height);
                if next_height > height {
                    // implies pos is right sibling
                    (pos - sibling_offset, pos + 1)
                } else {
                    // pos is left sibling
                    (pos + sibling_offset, pos + parent_offset(height))
                }
            };

            if Some(&sib_pos) == queue.front().map(|(pos, _)| pos) {
                // drop sibling
                queue.pop_front();
            } else {
                let elem = self
                    .batch
                    .element_at_position(sib_pos)
                    .unwrap_add_cost(cost);
                proof.push(elem?.ok_or(Error::InconsistentStore)?);
            }
            if parent_pos < peak_pos {
                // save pos to tree buf
                queue.push_back((parent_pos, height + 1));
            }
        }
        Ok(())
    }

    /// Generate a Merkle inclusion proof for the given leaf positions.
    ///
    /// Positions are sorted and deduplicated internally. Returns
    /// [`Error::GenProofForInvalidLeaves`] if any position is out of range
    /// or the list is empty.
    pub fn gen_proof(&self, mut pos_list: Vec<u64>) -> CostResult<MerkleProof, Error> {
        let mut cost = OperationCost::default();
        if pos_list.is_empty() {
            return Err(Error::GenProofForInvalidLeaves).wrap_with_cost(cost);
        }
        if self.mmr_size == 1 && pos_list == [0] {
            return Ok(MerkleProof::new(self.mmr_size, Vec::new())).wrap_with_cost(cost);
        }
        if pos_list.iter().any(|pos| pos_height_in_tree(*pos) > 0) {
            return Err(Error::NodeProofsNotSupported).wrap_with_cost(cost);
        }
        // ensure positions are sorted and unique
        pos_list.sort_unstable();
        pos_list.dedup();
        let peaks = get_peaks(self.mmr_size);
        let mut proof: Vec<MmrNode> = Vec::new();
        // generate merkle proof for each peaks
        let mut bagging_track = 0;
        for peak_pos in peaks {
            let pos_list: Vec<_> = take_while_vec(&mut pos_list, |&pos| pos <= peak_pos);
            if pos_list.is_empty() {
                bagging_track += 1;
            } else {
                bagging_track = 0;
            }
            match self.gen_proof_for_peak(&mut proof, pos_list, peak_pos, &mut cost) {
                Ok(()) => {}
                Err(e) => return Err(e).wrap_with_cost(cost),
            }
        }

        // ensure no remain positions
        if !pos_list.is_empty() {
            return Err(Error::GenProofForInvalidLeaves).wrap_with_cost(cost);
        }

        if bagging_track > 1 {
            let rhs_peaks = proof.split_off(proof.len() - bagging_track);
            match bag_peaks(rhs_peaks) {
                Ok(Some(bagged)) => proof.push(bagged),
                Ok(None) => {
                    return Err(Error::InconsistentStore).wrap_with_cost(cost);
                }
                Err(e) => return Err(e).wrap_with_cost(cost),
            }
        }

        Ok(MerkleProof::new(self.mmr_size, proof)).wrap_with_cost(cost)
    }
}

impl<S: MMRStoreWriteOps> MMR<S> {
    /// Flush all buffered mutations to the underlying store.
    pub fn commit(&mut self) -> CostResult<(), Error> {
        self.batch.commit()
    }
}

/// Bag peaks right-to-left: hash(right, left) repeatedly until one remains.
pub(crate) fn bag_peaks(mut peaks: Vec<MmrNode>) -> Result<Option<MmrNode>> {
    while peaks.len() > 1 {
        let right_peak = peaks.pop().expect("pop");
        let left_peak = peaks.pop().expect("pop");
        peaks.push(MmrNode::merge(&right_peak, &left_peak));
    }
    Ok(peaks.pop())
}
