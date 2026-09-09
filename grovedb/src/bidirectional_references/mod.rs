//! Bidirectional references management module.
//!
//! The type definitions ([`BidirectionalReference`] and friends) live in the
//! `grovedb-element` crate because the `Element` enum embeds them; this
//! module hosts the propagation machinery: backward-reference meta storage
//! bookkeeping, hash propagation along reference chains, and cascade
//! deletion. See `adr/bidirectional_references.md`.

mod handling;
pub(crate) mod semantics;
pub(crate) use semantics::check_carried_referrers_fit;

pub use grovedb_element::{BackwardReference, BidirectionalReference};
pub(crate) use handling::*;

/// Whether a mutation maintains backward references on GROVE_V4 and later.
/// Ordinary operations maintain references automatically. `Skip` is an
/// explicit choice to permit dangling references and stale reference hashes;
/// it is not an assertion that the stored element has no references.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackwardReferencesPolicy {
    /// Register references, propagate changed values, and cascade deletions
    /// with each referrer's consent before committing the mutation.
    #[default]
    Maintain,
    /// Skip maintenance of displaced values. Live bidirectional-reference
    /// insertion still registers its edge; batches reject family payloads.
    Skip,
}

impl BackwardReferencesPolicy {
    pub(crate) fn maintains(self) -> bool {
        matches!(self, Self::Maintain)
    }
}

/// Maximum Grove path depth (number of subtree levels) of any position
/// participating in a bidirectional edge — the referrer's own position and
/// its resolved target. Enforced at registration time by the shared
/// semantic core, so every later derived write (propagation rewrite,
/// cascade deletion, registration cleanup) is guaranteed to land at a
/// bounded depth. Estimation relies on this: each derived foreign-subtree
/// propagation charges up to this many ancestor updates, which would be
/// unboundable otherwise (a referrer parked arbitrarily deep would make
/// its propagation cost exceed any fixed estimate).
pub const MAX_BACKWARD_REFERENCES_GROVE_DEPTH: usize = 32;

impl crate::GroveDb {
    /// Find participants before a recursive subtree removal. Raw non-Merk
    /// namespaces contain no Elements and must never be decoded as such.
    pub(crate) fn backward_reference_participants(
        &self,
        path: &[Vec<u8>],
        transaction: &crate::Transaction,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<Vec<(Vec<Vec<u8>>, Vec<u8>, crate::Element)>, crate::Error> {
        use crate::element::elements_iterator::ElementIteratorExtensions;
        use grovedb_costs::{cost_return_on_error, CostsExt};
        use grovedb_storage::{Storage, StorageContext};
        let mut cost = Default::default();
        let mut queue = vec![path.to_vec()];
        let mut participants = Vec::new();
        while let Some(path) = queue.pop() {
            let storage = self
                .db
                .get_transactional_storage_context(
                    grovedb_path::SubtreePath::from(path.as_slice()),
                    None,
                    transaction,
                )
                .unwrap_add_cost(&mut cost);
            let mut iter = crate::Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
            while let Some((key, element)) =
                cost_return_on_error!(&mut cost, iter.next_element(grove_version))
            {
                if element.is_any_tree() && !element.uses_non_merk_data_storage() {
                    let mut child = path.clone();
                    child.push(key.clone());
                    queue.push(child);
                }
                if element.supports_backward_references() {
                    participants.push((path.clone(), key, element));
                }
            }
        }
        Ok(participants).wrap_with_cost(cost)
    }
}
