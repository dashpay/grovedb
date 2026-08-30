//! The pure semantic core of backward-references bookkeeping.
//!
//! Every rule about WHAT a write to the backward-references family implies —
//! which target gains a registration, which referrers get rewritten with a
//! new end hash, which chains cascade away, which budgets bound the result —
//! lives here as planning functions over an abstract read-only view of the
//! grove ([`ChainStore`]). Planners never mutate anything: they return a
//! [`Plan`], an ordered list of [`DerivedMutation`]s for the driver to
//! apply.
//!
//! Two drivers share this core so their semantics cannot drift:
//! - the live `MerkCache` flow in [`super::handling`], which applies the
//!   plan inside the current transaction, and
//! - the (future) `apply_batch` preprocessor, which turns the plan into
//!   ordinary batch ops.
//!
//! Planners must never depend on their own writes: wherever the old
//! interleaved code re-read a value it had just written, the planner now
//! threads the known new value explicitly. Reads through [`ChainStore`]
//! observe the PRE-plan state plus whatever the driver already committed.

use std::collections::VecDeque;

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::{element::ElementExt, CryptoHash};

use super::{BackwardReference, BidirectionalReference};
use crate::{
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{path_from_reference_path_type, ReferencePathType},
    Element, Error,
};

/// A fully-qualified element position: subtree path segments plus the key.
pub(crate) type Position = (Vec<Vec<u8>>, Vec<u8>);

/// The outcome of resolving a reference (one hop or a full chain) through a
/// [`ChainStore`].
pub(crate) struct ResolvedPosition {
    pub path: Vec<Vec<u8>>,
    pub key: Vec<u8>,
    pub element: Element,
    /// The node value hash of the resolved element (for a full-chain
    /// resolution: the hash every chain member commits to).
    pub node_value_hash: CryptoHash,
    /// Reference edges traversed (1 for a direct target).
    pub hops: usize,
}

impl ResolvedPosition {
    pub(crate) fn position(&self) -> Position {
        (self.path.clone(), self.key.clone())
    }
}

/// Read-only view of the grove the planners run against. Implementations
/// must surface the same error vocabulary the live flows use — notably
/// `Error::CorruptedReferencePathKeyNotFound` for dangling resolutions,
/// which several rules deliberately tolerate.
pub(crate) trait ChainStore {
    /// The element at the position, or `None` when the key is absent.
    fn element_at(&self, path: &[Vec<u8>], key: &[u8]) -> CostResult<Option<Element>, Error>;

    /// Resolve exactly one reference hop from `(path, key)` along
    /// `reference_path`.
    fn resolve_once(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
    ) -> CostResult<ResolvedPosition, Error>;

    /// Resolve the full chain from `(path, key)` along `reference_path`,
    /// applying the global hop budget and cycle detection seeded with the
    /// starting position (so a chain looping back through the start is
    /// reported as `Error::CyclicReference`).
    fn resolve_chain(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
    ) -> CostResult<ResolvedPosition, Error>;

    /// The grove version planning runs under.
    fn version(&self) -> &grovedb_version::version::GroveVersion;
}

/// One mutation a plan requires. Mutations are ordered; drivers apply them
/// in sequence.
#[derive(Debug)]
pub(crate) enum DerivedMutation {
    /// Write `element` at the position. For a bidirectional reference,
    /// `end_hash` carries the resolved end-of-chain hash its node commits
    /// to; for the other family members it is `None` (their combined hash
    /// derives from the element bytes alone).
    Write {
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
        end_hash: Option<CryptoHash>,
        /// Set only for the user-visible write a plan was derived FROM
        /// (the inserted reference itself), so the driver can apply the
        /// caller's merk options to exactly that write.
        is_primary: bool,
    },
    /// Delete the element at the position (a cascade member).
    Delete { path: Vec<Vec<u8>>, key: Vec<u8> },
}

/// An ordered list of derived mutations.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub mutations: Vec<DerivedMutation>,
}

impl Plan {
    fn write(
        &mut self,
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
        end_hash: Option<CryptoHash>,
    ) {
        self.mutations.push(DerivedMutation::Write {
            path,
            key,
            element,
            end_hash,
            is_primary: false,
        });
    }

    fn write_primary(
        &mut self,
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
        end_hash: Option<CryptoHash>,
    ) {
        self.mutations.push(DerivedMutation::Write {
            path,
            key,
            element,
            end_hash,
            is_primary: true,
        });
    }

    fn delete(&mut self, path: Vec<Vec<u8>>, key: Vec<u8>) {
        self.mutations.push(DerivedMutation::Delete { path, key });
    }
}

/// A backward entry only identifies its referrer by position. Before acting
/// on the occupant of that position, confirm it really is the registered
/// referrer: a `BidirectionalReference` whose forward path resolves back to
/// the element carrying the entry.
pub(crate) fn referrer_points_back(
    origin_element: &Element,
    origin_path: &[Vec<u8>],
    origin_key: &[u8],
    expected_path: &[Vec<u8>],
    expected_key: &[u8],
) -> bool {
    let Element::BidirectionalReference(reference) = origin_element else {
        return false;
    };
    let Ok(forward_qualified) = path_from_reference_path_type(
        reference.forward_reference_path.clone(),
        origin_path,
        Some(origin_key),
    ) else {
        return false;
    };
    let mut expected_qualified = expected_path.to_vec();
    expected_qualified.push(expected_key.to_vec());
    forward_qualified == expected_qualified
}

/// Upsert `entry` into `target`'s referrer list (a referrer is identified
/// by its inverted path) and enforce the budgets. Returns the updated
/// element.
fn with_registration(target: Element, entry: BackwardReference) -> Result<Element, Error> {
    let mut target = target;
    {
        let refs = target
            .backward_references_mut()
            .ok_or(Error::BidirectionalReferenceRule(
                "target does not support backward references".to_owned(),
            ))?;
        if let Some(existing) = refs
            .iter_mut()
            .find(|r| r.inverted_reference == entry.inverted_reference)
        {
            *existing = entry;
        } else {
            refs.push(entry);
        }
    }
    target.validate_backward_references_limits().map_err(|_| {
        Error::BidirectionalReferenceRule(
            "backward references budget exceeded (32 per item, 1 per bidirectional reference)"
                .to_owned(),
        )
    })?;
    Ok(target)
}

/// Plan the removal of the referrer entry matching the inversion of
/// `forward_reference_path` from whatever it resolves to (as seen from
/// `(current_path, current_key)`). Missing targets and missing entries are
/// tolerated — consistency can legitimately be bypassed by unflagged
/// writes.
fn plan_remove_registration(
    store: &impl ChainStore,
    plan: &mut Plan,
    current_path: &[Vec<u8>],
    current_key: &[u8],
    forward_reference_path: ReferencePathType,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    let inverted_reference = cost_return_on_error_no_add!(
        cost,
        forward_reference_path
            .invert(grovedb_path::SubtreePath::from(current_path), current_key)
            .ok_or_else(|| Error::BidirectionalReferenceRule(
                "unable to get an inverted reference".to_owned()
            ))
    );

    match store
        .resolve_once(current_path, current_key, forward_reference_path)
        .unwrap_add_cost(&mut cost)
    {
        Ok(resolved) => {
            let mut target_element = resolved.element;
            let Some(refs) = target_element.backward_references_mut() else {
                // The target was overwritten by something without backward
                // references support through a path that skipped
                // bookkeeping; nothing to clean.
                return Ok(()).wrap_with_cost(cost);
            };
            let before = refs.len();
            refs.retain(|r| r.inverted_reference != inverted_reference);
            if refs.len() == before {
                // Entry already gone — tolerated.
                return Ok(()).wrap_with_cost(cost);
            }
            // Rewriting a bidirectional-reference target needs the end hash
            // its node commits to.
            let end_hash = if let Element::BidirectionalReference(ref reference) = target_element {
                Some(
                    cost_return_on_error!(
                        &mut cost,
                        store.resolve_chain(
                            &resolved.path,
                            &resolved.key,
                            reference.forward_reference_path.clone(),
                        )
                    )
                    .node_value_hash,
                )
            } else {
                None
            };
            plan.write(resolved.path, resolved.key, target_element, end_hash);
        }
        // We tolerate missing references because consistency can be
        // bypassed, and out-of-sync situations might be common.
        Err(Error::CorruptedReferencePathKeyNotFound(_)) => {}
        Err(e) => return Err(e).wrap_with_cost(cost),
    }

    Ok(()).wrap_with_cost(cost)
}

/// Plan the insertion of a bidirectional reference at `(path, key)`:
/// target-eligibility and prospective-component checks, registration on the
/// target, the write of the reference itself, removal of a superseded
/// registration, and propagation to the reference's own referrers.
///
/// Returns `None` when the insertion is an identical-edge no-op.
pub(crate) fn plan_reference_insertion(
    store: &impl ChainStore,
    path: &[Vec<u8>],
    key: &[u8],
    mut reference: BidirectionalReference,
) -> CostResult<Option<Plan>, Error> {
    let mut cost = Default::default();
    let mut plan = Plan::default();

    // Read what the key currently holds first. The stored referrer list is
    // carried over onto the new element (registrations survive an edge
    // update), and re-inserting an identical edge must be a true no-op.
    let previous_value = cost_return_on_error!(&mut cost, store.element_at(path, key));
    if let Some(Element::BidirectionalReference(ref old_ref)) = previous_value {
        // Carry the existing referrer list over.
        reference.backward_references = old_ref.backward_references.clone();
        if old_ref.forward_reference_path == reference.forward_reference_path
            && old_ref.cascade_on_update == reference.cascade_on_update
            && old_ref.max_hop == reference.max_hop
            && old_ref.flags == reference.flags
        {
            // Identical logical edge: nothing changed.
            return Ok(None).wrap_with_cost(cost);
        }
    } else {
        // The referrer list is bookkeeping this module maintains; whatever
        // the caller supplied is not theirs to claim.
        reference.backward_references.clear();
    }

    // Since we limit what kind of elements a bidirectional reference can
    // target, a check goes first:
    let target = cost_return_on_error!(
        &mut cost,
        store.resolve_once(path, key, reference.forward_reference_path.clone())
    );

    if !target.element.supports_backward_references() {
        return Err(Error::BidirectionalReferenceRule(
            "Bidirectional references can only point variants with backward references support"
                .to_owned(),
        ))
        .wrap_with_cost(cost);
    }

    // Both ends of the edge must sit at a bounded Grove depth: every later
    // derived write (propagation, cascade, cleanup) lands at one of these
    // positions, and cost estimation charges ancestor propagation up to
    // exactly this bound — an unboundedly deep referrer would make its
    // propagation cost exceed any fixed estimate.
    if path.len() > super::MAX_BACKWARD_REFERENCES_GROVE_DEPTH
        || target.path.len() > super::MAX_BACKWARD_REFERENCES_GROVE_DEPTH
    {
        return Err(Error::BidirectionalReferenceRule(format!(
            "bidirectional-reference positions may sit at most {} subtree levels deep",
            super::MAX_BACKWARD_REFERENCES_GROVE_DEPTH
        )))
        .wrap_with_cost(cost);
    }

    // If the closest target is a bidirectional reference itself, follow the
    // FULL chain starting from the position being written: the resolved
    // end-of-chain hash is what every chain member stores, and the chain
    // resolution seeds its visited set with the starting qualified path —
    // so a cycle that would only materialize AFTER the write is rejected
    // before any mutation.
    let (target_value_hash, downstream_hops) =
        if let Element::BidirectionalReference(..) = target.element {
            let resolved = cost_return_on_error!(
                &mut cost,
                store.resolve_chain(path, key, reference.forward_reference_path.clone())
            );
            (resolved.node_value_hash, resolved.hops)
        } else {
            (target.node_value_hash, 1)
        };

    // The edge's own declared budget must admit its downstream chain:
    // public reads enforce `max_hop` deterministically, so an edge whose
    // chain is already longer than its declaration would never resolve —
    // reject it at insertion instead of persisting a dead edge.
    if let Some(declared) = reference.max_hop
        && downstream_hops > declared as usize
    {
        return Err(Error::BidirectionalReferenceRule(format!(
            "the reference's chain needs {downstream_hops} hops but its max_hop declares \
             {declared}"
        )))
        .wrap_with_cost(cost);
    }

    // The whole PROSPECTIVE component must fit the global hop budget:
    // downstream was just measured; upstream is this position's referrer
    // chain (each bidirectional reference holds at most one referrer, so it
    // is a single path). Without this, repeated retargets could splice
    // independently valid segments into chains longer than any reader will
    // follow.
    let mut upstream_hops: usize = 0;
    {
        let mut current_refs = reference.backward_references.clone();
        let mut current_path = path.to_vec();
        let mut current_key = key.to_vec();
        while let Some(entry) = current_refs.first().cloned() {
            upstream_hops += 1;
            if upstream_hops + downstream_hops > MAX_REFERENCE_HOPS {
                break;
            }
            match store
                .resolve_once(&current_path, &current_key, entry.inverted_reference)
                .unwrap_add_cost(&mut cost)
            {
                Ok(resolved)
                    if referrer_points_back(
                        &resolved.element,
                        &resolved.path,
                        &resolved.key,
                        &current_path,
                        &current_key,
                    ) =>
                {
                    // Each upstream ancestor's OWN declared budget must
                    // still admit its chain through the retargeted edge:
                    // from this ancestor the chain runs `upstream_hops`
                    // hops down to the position being written, then the
                    // new `downstream_hops` beyond it. Without this, a
                    // valid `A(max_hop=2) -> B -> C` breaks silently when
                    // B is retargeted onto a two-hop chain — reads
                    // through A would deterministically hit
                    // `ReferenceLimit`.
                    if let Element::BidirectionalReference(ref ancestor) = resolved.element
                        && let Some(declared) = ancestor.max_hop
                        && (declared as usize) < upstream_hops + downstream_hops
                    {
                        return Err(Error::BidirectionalReferenceRule(format!(
                            "an upstream referrer's chain would need {} hops but its max_hop \
                             declares {declared}",
                            upstream_hops + downstream_hops
                        )))
                        .wrap_with_cost(cost);
                    }
                    current_refs = resolved
                        .element
                        .backward_references()
                        .map(|refs| refs.to_vec())
                        .unwrap_or_default();
                    current_path = resolved.path;
                    current_key = resolved.key;
                }
                // Dangling or stale entries end the live upstream chain.
                _ => break,
            }
        }
    }
    if upstream_hops + downstream_hops > MAX_REFERENCE_HOPS {
        return Err(Error::BidirectionalReferenceRule(format!(
            "the resulting reference component would exceed the global budget of {} hops",
            MAX_REFERENCE_HOPS
        )))
        .wrap_with_cost(cost);
    }

    // Different `ReferencePathType` encodings can resolve to the same
    // position, so "did the target change?" must compare RESOLVED
    // positions, never encodings. When a retarget stays on the same
    // target, its old entry is replaced on the element being registered
    // (planners never read their own writes — a separate removal write
    // computed from the pre-plan target would win over the registration
    // and strip the live edge entirely).
    let old_edge_same_target = match previous_value {
        Some(Element::BidirectionalReference(ref old_ref)) => {
            let old_qualified = path_from_reference_path_type(
                old_ref.forward_reference_path.clone(),
                path,
                Some(key),
            )
            .ok();
            let mut new_qualified = target.path.clone();
            new_qualified.push(target.key.clone());
            old_qualified == Some(new_qualified)
        }
        _ => false,
    };

    // Register the backward edge on the target:
    let inverted_reference = cost_return_on_error_no_add!(
        cost,
        reference
            .forward_reference_path
            .invert(grovedb_path::SubtreePath::from(path), key)
            .ok_or_else(|| Error::BidirectionalReferenceRule(
                "unable to get an inverted reference".to_owned()
            ))
    );
    // Rewriting a bidirectional-reference target needs the end hash ITS
    // node commits to — the same end-of-chain hash just resolved.
    let end_hash_for_target = if matches!(target.element, Element::BidirectionalReference(..)) {
        Some(target_value_hash)
    } else {
        None
    };
    let mut target_element = target.element;
    if old_edge_same_target
        && let Some(Element::BidirectionalReference(ref old_ref)) = previous_value
        && let Some(old_inverted) = old_ref
            .forward_reference_path
            .clone()
            .invert(grovedb_path::SubtreePath::from(path), key)
        && let Some(refs) = target_element.backward_references_mut()
    {
        // Same target under a different encoding: drop the old entry so
        // the upsert below replaces it instead of accumulating a
        // duplicate referrer.
        refs.retain(|r| r.inverted_reference != old_inverted);
    }
    let registered_target = cost_return_on_error_no_add!(
        cost,
        with_registration(
            target_element,
            BackwardReference {
                inverted_reference,
                cascade_on_update: reference.cascade_on_update,
            },
        )
    );
    plan.write(
        target.path,
        target.key,
        registered_target,
        end_hash_for_target,
    );

    // Write the new reference itself (its node hash combines its stripped
    // bytes, the resolved end hash, and its carried referrer list).
    plan.write_primary(
        path.to_vec(),
        key.to_vec(),
        Element::BidirectionalReference(reference.clone()),
        Some(target_value_hash),
    );

    match previous_value {
        // If previous value was another bidirectional reference, its
        // backward registration on the OLD target must be removed.
        Some(Element::BidirectionalReference(old_reference)) => {
            // Same RESOLVED target (whatever the encoding): the old entry
            // was already replaced in place on the registration write
            // above, and a separate removal — planned from the pre-plan
            // target — would win over that write and strip the edge
            // entirely.
            if !old_edge_same_target {
                cost_return_on_error!(
                    &mut cost,
                    plan_remove_registration(
                        store,
                        &mut plan,
                        path,
                        key,
                        old_reference.forward_reference_path,
                    )
                );
            }

            // The chain now resolves to a new end hash; referrers of THIS
            // reference must be updated with it. The planner threads the
            // NEW element explicitly (the interleaved flow re-read its own
            // write here).
            cost_return_on_error!(
                &mut cost,
                plan_propagation(
                    store,
                    &mut plan,
                    path,
                    key,
                    Element::BidirectionalReference(reference),
                    target_value_hash,
                )
            );
        }
        // Overwriting an item with backward references is an error: those
        // may carry up to 32 registrations while a bidirectional reference
        // supports only one.
        Some(
            Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)
            | Element::ItemWithSumItemWithBackwardsReferences(..),
        ) => {
            return Err(Error::BidirectionalReferenceRule(
                "insertion of bidirectional reference cannot override elements with backward \
                 references (item/sum item) since only one backward reference is supported for \
                 bidirectional reference and those may have up to 32"
                    .to_owned(),
            ))
            .wrap_with_cost(cost)
        }
        // Fresh insertion or overwrite of a plain element: nothing extra.
        _ => {}
    }

    Ok(Some(plan)).wrap_with_cost(cost)
}

/// Plan the follow-up for an already-performed update of the element at
/// `(path, key)`: `old` is what the position held, `new` what it holds now
/// (`None` for deletion). Handles propagation, cascade, and registration
/// removal per the family rules. Returns an empty plan when nothing is
/// required.
pub(crate) fn plan_element_update(
    store: &impl ChainStore,
    path: &[Vec<u8>],
    key: &[u8],
    old: Element,
    new: Option<Element>,
) -> CostResult<Plan, Error> {
    let mut cost = Default::default();
    let mut plan = Plan::default();

    match (old, new) {
        (
            Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)
            | Element::ItemWithSumItemWithBackwardsReferences(..),
            Some(
                new @ (Element::ItemWithBackwardsReferences(..)
                | Element::SumItemWithBackwardsReferences(..)
                | Element::ItemWithSumItemWithBackwardsReferences(..)),
            ),
        ) => {
            // Update with another backward references-compatible element:
            // referrers commit to the INNER hash, so propagate the new one
            // along every chain.
            let new_logical_hash = cost_return_on_error!(
                &mut cost,
                new.logical_value_hash(store.version()).map_err(Error::from)
            );
            cost_return_on_error!(
                &mut cost,
                plan_propagation(store, &mut plan, path, key, new, new_logical_hash)
            );
        }
        (
            old @ (Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)
            | Element::ItemWithSumItemWithBackwardsReferences(..)),
            _,
        ) => {
            // Update with a non-compatible element (or deletion) equals
            // cascade deletion of the referrer chains.
            cost_return_on_error!(&mut cost, plan_cascade(store, &mut plan, path, key, old));
        }
        (
            Element::BidirectionalReference(old_reference),
            Some(
                new @ (Element::ItemWithBackwardsReferences(..)
                | Element::SumItemWithBackwardsReferences(..)
                | Element::ItemWithSumItemWithBackwardsReferences(..)),
            ),
        ) => {
            // Overwrite of a bidirectional reference with a compatible item
            // triggers propagation and removes the old backward
            // registration on the old target.
            let new_logical_hash = cost_return_on_error!(
                &mut cost,
                new.logical_value_hash(store.version()).map_err(Error::from)
            );
            cost_return_on_error!(
                &mut cost,
                plan_propagation(store, &mut plan, path, key, new, new_logical_hash)
            );
            cost_return_on_error!(
                &mut cost,
                plan_remove_registration(
                    store,
                    &mut plan,
                    path,
                    key,
                    old_reference.forward_reference_path,
                )
            );
        }
        (Element::BidirectionalReference(old_reference), _) => {
            // Overwrite with a non-compatible element (or deletion):
            // cascade the referrer chains away and remove the backward
            // registration from where the reference used to point.
            cost_return_on_error!(
                &mut cost,
                plan_cascade(
                    store,
                    &mut plan,
                    path,
                    key,
                    Element::BidirectionalReference(old_reference.clone()),
                )
            );
            cost_return_on_error!(
                &mut cost,
                plan_remove_registration(
                    store,
                    &mut plan,
                    path,
                    key,
                    old_reference.forward_reference_path,
                )
            );
        }
        _ => {
            // All other overwrites don't require special attention.
        }
    }

    Ok(plan).wrap_with_cost(cost)
}

/// Plan the recursive cascade deletion of every referrer chain of
/// `(path, key)`. `start_element` is the (already overwritten/deleted)
/// element whose referrer list seeds the cascade; every affected referrer
/// must have opted in via `cascade_on_update`.
pub(crate) fn plan_cascade(
    store: &impl ChainStore,
    plan: &mut Plan,
    path: &[Vec<u8>],
    key: &[u8],
    start_element: Element,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    let mut queue = VecDeque::new();
    // Each node has exactly one forward edge, so reverse reachability from
    // one start forms a tree; a revisit means the on-disk graph encodes a
    // cycle, which insertion rejects — corrupted state, bail instead of
    // looping forever.
    let mut visited: std::collections::HashSet<Position> = Default::default();

    visited.insert((path.to_vec(), key.to_vec()));
    queue.push_back((path.to_vec(), key.to_vec(), start_element, true));

    while let Some((current_path, current_key, current_element, first)) = queue.pop_front() {
        let backward_references = current_element
            .backward_references()
            .map(|refs| refs.to_vec())
            .unwrap_or_default();

        for backward_ref in backward_references {
            if !backward_ref.cascade_on_update {
                return Err(Error::BidirectionalReferenceRule(
                    "deletion of backward references through deletion of an element requires \
                     `cascade_on_update` setting"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }

            let resolved = store
                .resolve_once(&current_path, &current_key, backward_ref.inverted_reference)
                .unwrap_add_cost(&mut cost);

            let origin = match resolved {
                Ok(resolved) => resolved,
                // Dangling referrer (removed by an unflagged write or a
                // batch): nothing left to cascade there.
                Err(Error::CorruptedReferencePathKeyNotFound(_)) => continue,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };

            // A reused position holding something other than the registered
            // referrer is stale bookkeeping, not a cascade member; the
            // entry disappears with the element being deleted.
            if !referrer_points_back(
                &origin.element,
                &origin.path,
                &origin.key,
                &current_path,
                &current_key,
            ) {
                continue;
            }

            if !visited.insert(origin.position()) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            queue.push_back((origin.path, origin.key, origin.element, false));
        }

        // Delete the element itself, unless it is the cascade's start (the
        // original was already overwritten or deleted by the caller).
        if !first {
            plan.delete(current_path, current_key);
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Plan the propagation of a new end-of-chain value hash to every referrer
/// chain of `(path, key)`. `current` is the element now stored at the
/// position (threaded explicitly — the plan may include its own write, and
/// planners never read their own writes). Dangling and stale referrer
/// entries are lazily cleaned.
pub(crate) fn plan_propagation(
    store: &impl ChainStore,
    plan: &mut Plan,
    path: &[Vec<u8>],
    key: &[u8],
    current: Element,
    referenced_element_value_hash: CryptoHash,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    let mut queue = VecDeque::new();
    // See the identical bound in `plan_cascade`.
    let mut visited: std::collections::HashSet<Position> = Default::default();

    visited.insert((path.to_vec(), key.to_vec()));
    queue.push_back((path.to_vec(), key.to_vec(), current));

    while let Some((current_path, current_key, current_element)) = queue.pop_front() {
        let backward_references = current_element
            .backward_references()
            .map(|refs| refs.to_vec())
            .unwrap_or_default();
        let mut dangling: Vec<ReferencePathType> = Vec::new();

        for backward_ref in backward_references {
            let resolved = store
                .resolve_once(
                    &current_path,
                    &current_key,
                    backward_ref.inverted_reference.clone(),
                )
                .unwrap_add_cost(&mut cost);

            let origin = match resolved {
                Ok(resolved) => resolved,
                // Dangling referrer (removed by an unflagged write or a
                // batch): clean the stale entry lazily and keep going.
                Err(Error::CorruptedReferencePathKeyNotFound(_)) => {
                    dangling.push(backward_ref.inverted_reference);
                    continue;
                }
                Err(e) => return Err(e).wrap_with_cost(cost),
            };

            // A reused position holding something other than the registered
            // referrer must not be rewritten — treat the entry as stale and
            // clean it lazily like a dangling one.
            if !referrer_points_back(
                &origin.element,
                &origin.path,
                &origin.key,
                &current_path,
                &current_key,
            ) {
                dangling.push(backward_ref.inverted_reference);
                continue;
            }

            // Rewrite the referrer with the new end hash (its own referrer
            // list rides along inside the element bytes).
            plan.write(
                origin.path.clone(),
                origin.key.clone(),
                origin.element.clone(),
                Some(referenced_element_value_hash),
            );

            if !visited.insert(origin.position()) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            queue.push_back((origin.path, origin.key, origin.element));
        }

        if !dangling.is_empty() {
            // Drop the dangling entries from the current element and write
            // it back (for a bidirectional reference the end hash it
            // commits to is exactly the one being propagated).
            let mut updated = current_element;
            if let Some(refs) = updated.backward_references_mut() {
                refs.retain(|r| !dangling.contains(&r.inverted_reference));
            }
            let end_hash = if matches!(updated, Element::BidirectionalReference(..)) {
                Some(referenced_element_value_hash)
            } else {
                None
            };
            plan.write(current_path, current_key, updated, end_hash);
        }
    }

    Ok(()).wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use grovedb_version::version::GroveVersion;

    use super::*;

    /// A minimal in-memory [`ChainStore`]: proves the planners run against
    /// any driver, not just the `MerkCache`.
    struct MapStore {
        elements: RefCell<HashMap<Position, Element>>,
        version: &'static GroveVersion,
    }

    impl MapStore {
        fn new() -> Self {
            Self {
                elements: RefCell::new(HashMap::new()),
                version: GroveVersion::latest(),
            }
        }

        fn put(&self, path: &[&[u8]], key: &[u8], element: Element) {
            self.elements.borrow_mut().insert(
                (path.iter().map(|p| p.to_vec()).collect(), key.to_vec()),
                element,
            );
        }

        fn resolve_position(
            &self,
            path: &[Vec<u8>],
            key: &[u8],
            reference_path: ReferencePathType,
        ) -> Result<(Position, Element), Error> {
            let qualified = path_from_reference_path_type(reference_path, path, Some(key))?;
            let (target_key, target_path) = qualified
                .split_last()
                .ok_or(Error::CorruptedPath("empty reference".to_string()))?;
            let position = (target_path.to_vec(), target_key.clone());
            let element = self
                .elements
                .borrow()
                .get(&position)
                .cloned()
                .ok_or_else(|| {
                    Error::CorruptedReferencePathKeyNotFound("missing in mock store".to_string())
                })?;
            Ok((position, element))
        }
    }

    impl ChainStore for MapStore {
        fn element_at(&self, path: &[Vec<u8>], key: &[u8]) -> CostResult<Option<Element>, Error> {
            Ok(self
                .elements
                .borrow()
                .get(&(path.to_vec(), key.to_vec()))
                .cloned())
            .wrap_with_cost(Default::default())
        }

        fn resolve_once(
            &self,
            path: &[Vec<u8>],
            key: &[u8],
            reference_path: ReferencePathType,
        ) -> CostResult<ResolvedPosition, Error> {
            self.resolve_position(path, key, reference_path)
                .map(|((path, key), element)| {
                    let node_value_hash = element
                        .logical_value_hash(self.version)
                        .unwrap()
                        .expect("mock elements hash");
                    ResolvedPosition {
                        path,
                        key,
                        element,
                        node_value_hash,
                        hops: 1,
                    }
                })
                .wrap_with_cost(Default::default())
        }

        fn resolve_chain(
            &self,
            path: &[Vec<u8>],
            key: &[u8],
            reference_path: ReferencePathType,
        ) -> CostResult<ResolvedPosition, Error> {
            let mut visited: std::collections::HashSet<Position> = Default::default();
            visited.insert((path.to_vec(), key.to_vec()));
            let mut hops = 0usize;
            let mut current = (path.to_vec(), key.to_vec(), reference_path);
            loop {
                hops += 1;
                if hops > MAX_REFERENCE_HOPS {
                    return Err(Error::ReferenceLimit).wrap_with_cost(Default::default());
                }
                let (position, element) =
                    match self.resolve_position(&current.0, &current.1, current.2.clone()) {
                        Ok(resolved) => resolved,
                        Err(e) => return Err(e).wrap_with_cost(Default::default()),
                    };
                if !visited.insert(position.clone()) {
                    return Err(Error::CyclicReference).wrap_with_cost(Default::default());
                }
                match element {
                    Element::BidirectionalReference(reference) => {
                        current = (position.0, position.1, reference.forward_reference_path);
                    }
                    element => {
                        let node_value_hash = element
                            .logical_value_hash(self.version)
                            .unwrap()
                            .expect("mock elements hash");
                        return Ok(ResolvedPosition {
                            path: position.0,
                            key: position.1,
                            element,
                            node_value_hash,
                            hops,
                        })
                        .wrap_with_cost(Default::default());
                    }
                }
            }
        }

        fn version(&self) -> &grovedb_version::version::GroveVersion {
            self.version
        }
    }

    fn sibling_bidi(key: &[u8], cascade: bool) -> BidirectionalReference {
        BidirectionalReference {
            forward_reference_path: ReferencePathType::SiblingReference(key.to_vec()),
            backward_references: Vec::new(),
            cascade_on_update: cascade,
            max_hop: None,
            flags: None,
        }
    }

    const LEAF: &[u8] = b"leaf";

    fn leaf() -> Vec<Vec<u8>> {
        vec![LEAF.to_vec()]
    }

    #[test]
    fn insertion_plan_registers_then_writes_primary() {
        let store = MapStore::new();
        store.put(
            &[LEAF],
            b"target",
            Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
        );

        let plan = plan_reference_insertion(&store, &leaf(), b"ref", sibling_bidi(b"target", true))
            .unwrap()
            .unwrap()
            .expect("not an identical edge");

        assert_eq!(plan.mutations.len(), 2);
        let DerivedMutation::Write {
            key,
            element,
            end_hash,
            is_primary,
            ..
        } = &plan.mutations[0]
        else {
            panic!("expected the target registration write");
        };
        assert_eq!(key, b"target");
        assert!(!is_primary);
        assert!(end_hash.is_none(), "item targets carry no end hash");
        assert_eq!(
            element.backward_references().unwrap().len(),
            1,
            "the registration is on the element"
        );
        let DerivedMutation::Write {
            key,
            end_hash,
            is_primary,
            ..
        } = &plan.mutations[1]
        else {
            panic!("expected the primary reference write");
        };
        assert_eq!(key, b"ref");
        assert!(is_primary);
        assert!(end_hash.is_some(), "the reference commits to the end hash");
    }

    #[test]
    fn identical_edge_plans_nothing() {
        let store = MapStore::new();
        store.put(
            &[LEAF],
            b"target",
            Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
        );
        store.put(
            &[LEAF],
            b"ref",
            Element::BidirectionalReference(sibling_bidi(b"target", true)),
        );

        assert!(
            plan_reference_insertion(&store, &leaf(), b"ref", sibling_bidi(b"target", true))
                .unwrap()
                .unwrap()
                .is_none(),
            "re-inserting an identical edge is a no-op"
        );
    }

    #[test]
    fn same_target_retarget_under_a_different_encoding_replaces_the_entry() {
        let store = MapStore::new();
        // The target carries the sibling-encoded registration of `ref`.
        let mut target = Element::new_item_allowing_bidirectional_references(b"v".to_vec());
        target
            .backward_references_mut()
            .unwrap()
            .push(BackwardReference {
                inverted_reference: ReferencePathType::SiblingReference(b"ref".to_vec()),
                cascade_on_update: true,
            });
        store.put(&[LEAF], b"target", target);
        store.put(
            &[LEAF],
            b"ref",
            Element::BidirectionalReference(sibling_bidi(b"target", true)),
        );

        // Re-point the SAME target through an absolute-path encoding.
        let retarget = BidirectionalReference {
            forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                LEAF.to_vec(),
                b"target".to_vec(),
            ]),
            backward_references: Vec::new(),
            cascade_on_update: true,
            max_hop: None,
            flags: None,
        };
        let plan = plan_reference_insertion(&store, &leaf(), b"ref", retarget)
            .unwrap()
            .unwrap()
            .expect("a changed encoding is a real edge update");

        // Registration write + primary write, and nothing else: no
        // separate removal write may race the registration.
        assert_eq!(plan.mutations.len(), 2);
        let DerivedMutation::Write { key, element, .. } = &plan.mutations[0] else {
            panic!("expected the target registration write");
        };
        assert_eq!(key, b"target");
        let refs = element.backward_references().unwrap();
        assert_eq!(
            refs.len(),
            1,
            "the old entry must be REPLACED, not duplicated and not stripped"
        );
        assert!(
            matches!(
                refs[0].inverted_reference,
                ReferencePathType::AbsolutePathReference(..)
            ),
            "the surviving entry is the new encoding's inversion"
        );
    }

    #[test]
    fn cascade_plan_deletes_the_whole_chain_in_order() {
        let store = MapStore::new();
        // item <- r1 <- r2 (registrations carried on the elements).
        let mut item = Element::new_item_allowing_bidirectional_references(b"v".to_vec());
        item.backward_references_mut()
            .unwrap()
            .push(BackwardReference {
                inverted_reference: ReferencePathType::SiblingReference(b"r1".to_vec()),
                cascade_on_update: true,
            });
        let mut r1 = sibling_bidi(b"item", true);
        r1.backward_references.push(BackwardReference {
            inverted_reference: ReferencePathType::SiblingReference(b"r2".to_vec()),
            cascade_on_update: true,
        });
        store.put(&[LEAF], b"item", item.clone());
        store.put(&[LEAF], b"r1", Element::BidirectionalReference(r1));
        store.put(
            &[LEAF],
            b"r2",
            Element::BidirectionalReference(sibling_bidi(b"r1", true)),
        );

        let mut plan = Plan::default();
        plan_cascade(&store, &mut plan, &leaf(), b"item", item)
            .unwrap()
            .unwrap();

        let deleted: Vec<&[u8]> = plan
            .mutations
            .iter()
            .map(|m| match m {
                DerivedMutation::Delete { key, .. } => key.as_slice(),
                other => panic!("cascade plans only deletes, got {other:?}"),
            })
            .collect();
        assert_eq!(deleted, vec![b"r1".as_slice(), b"r2".as_slice()]);
    }

    #[test]
    fn cascade_plan_requires_consent() {
        let store = MapStore::new();
        let mut item = Element::new_item_allowing_bidirectional_references(b"v".to_vec());
        item.backward_references_mut()
            .unwrap()
            .push(BackwardReference {
                inverted_reference: ReferencePathType::SiblingReference(b"r1".to_vec()),
                cascade_on_update: false,
            });
        store.put(
            &[LEAF],
            b"r1",
            Element::BidirectionalReference(sibling_bidi(b"item", false)),
        );

        let mut plan = Plan::default();
        assert!(matches!(
            plan_cascade(&store, &mut plan, &leaf(), b"item", item).unwrap(),
            Err(Error::BidirectionalReferenceRule(_))
        ));
    }

    #[test]
    fn propagation_plan_rewrites_live_referrers_and_cleans_stale_ones() {
        let store = MapStore::new();
        let mut item = Element::new_item_allowing_bidirectional_references(b"v2".to_vec());
        for referrer in [b"live".as_slice(), b"gone"] {
            item.backward_references_mut()
                .unwrap()
                .push(BackwardReference {
                    inverted_reference: ReferencePathType::SiblingReference(referrer.to_vec()),
                    cascade_on_update: true,
                });
        }
        store.put(&[LEAF], b"item", item.clone());
        store.put(
            &[LEAF],
            b"live",
            Element::BidirectionalReference(sibling_bidi(b"item", true)),
        );
        // `gone` is absent: a dangling entry to be lazily cleaned.

        let new_end_hash = [42u8; 32];
        let mut plan = Plan::default();
        plan_propagation(&store, &mut plan, &leaf(), b"item", item, new_end_hash)
            .unwrap()
            .unwrap();

        assert_eq!(plan.mutations.len(), 2);
        let DerivedMutation::Write { key, end_hash, .. } = &plan.mutations[0] else {
            panic!("expected the live referrer rewrite");
        };
        assert_eq!(key, b"live");
        assert_eq!(*end_hash, Some(new_end_hash));
        let DerivedMutation::Write { key, element, .. } = &plan.mutations[1] else {
            panic!("expected the lazy cleanup write");
        };
        assert_eq!(key, b"item");
        assert_eq!(
            element.backward_references().unwrap().len(),
            1,
            "the dangling entry is dropped, the live one kept"
        );
    }

    #[test]
    fn component_budget_bounds_the_prospective_chain() {
        let store = MapStore::new();
        // Downstream chain of MAX_REFERENCE_HOPS - 1 references ending on an
        // item, plus one upstream referrer on the edge being written: the
        // total exceeds the budget.
        store.put(
            &[LEAF],
            b"t0",
            Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
        );
        for i in 1..MAX_REFERENCE_HOPS {
            let mut reference = sibling_bidi(format!("t{}", i - 1).as_bytes(), true);
            if i == MAX_REFERENCE_HOPS - 1 {
                // The insertion carries over this referrer list.
                reference.backward_references.push(BackwardReference {
                    inverted_reference: ReferencePathType::SiblingReference(b"up".to_vec()),
                    cascade_on_update: true,
                });
            }
            store.put(
                &[LEAF],
                format!("t{i}").as_bytes(),
                Element::BidirectionalReference(reference),
            );
        }
        let top = format!("t{}", MAX_REFERENCE_HOPS - 1);
        let mut up = sibling_bidi(top.as_bytes(), true);
        up.backward_references.push(BackwardReference {
            inverted_reference: ReferencePathType::SiblingReference(b"up2".to_vec()),
            cascade_on_update: true,
        });
        store.put(&[LEAF], b"up", Element::BidirectionalReference(up));
        store.put(
            &[LEAF],
            b"up2",
            Element::BidirectionalReference(sibling_bidi(b"up", true)),
        );

        // Retarget the top of the chain (which carries the `up` referrer,
        // itself referred to by `up2`): upstream 2 + downstream 9 = 11
        // exceeds the 10-hop budget. Rebuilding the same forward edge with
        // a changed option forces a real plan.
        let mut retarget = sibling_bidi(format!("t{}", MAX_REFERENCE_HOPS - 2).as_bytes(), true);
        retarget.cascade_on_update = false;
        let result = plan_reference_insertion(&store, &leaf(), top.as_bytes(), retarget).unwrap();
        assert!(
            matches!(result, Err(Error::BidirectionalReferenceRule(ref m)) if m.contains("budget")),
            "got: {result:?}"
        );
    }
}
