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
use grovedb_version::version::GroveVersion;

pub(crate) use handling::*;

/// What a write or removal declares about the stored value it displaces.
///
/// GroveDB reads the displaced value anyway on `GROVE_V4`+ (the batch
/// old-value observer, the Merk retained for a live write), so for a keyed
/// operation the declaration decides only what happens when that value takes
/// part in backward references: `MayBeParticipant` maintains the references,
/// `NotParticipant` refuses the operation before anything commits. Where
/// nothing reads the contents — a flat drop, a raw `clear_subtree`, the
/// replacement of a populated subtree — `NotParticipant` is trusted and
/// leaves any participant's registrations stale, exactly like the storage it
/// strands. Recursive removals that already walk their contents check the
/// claim on the way at no extra cost.
///
/// The estimator cannot see stored state, so the same declaration decides
/// whether a plain write or delete is charged the displaced-participant
/// fan-out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DisplacedValue {
    /// The displaced value, or for a subtree removal its contents, may take
    /// part in backward references: register, propagate and cascade with
    /// each referrer's consent before committing. Partial batches cannot
    /// plan that maintenance and refuse participant mutations.
    #[default]
    MayBeParticipant,
    /// The caller knows the displaced value takes no part in backward
    /// references. Checked for free wherever the value is read; refused if
    /// the claim is false. Required for a flat drop.
    NotParticipant,
}

impl DisplacedValue {
    pub(crate) fn may_be_participant(self) -> bool {
        matches!(self, Self::MayBeParticipant)
    }
}

/// Whether batches maintain backward references on `grove_version`
/// (`apply_batch.backward_references_maintenance`, V4+). Released versions
/// never plan references and reject the family payloads outright.
pub(crate) fn batch_maintains_backward_references(grove_version: &GroveVersion) -> bool {
    grove_version
        .grovedb_versions
        .apply_batch
        .backward_references_maintenance
        >= 1
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
        grove_version: &GroveVersion,
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
