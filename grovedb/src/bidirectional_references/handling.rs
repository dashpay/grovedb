//! Bidirectional references handling module.
//!
//! Backward references live ON their target element and are covered by the
//! node hash through the two-layer scheme described in
//! `grovedb_element::bidirectional_reference`: forward references commit to
//! the target's INNER (stripped) hash, so registering or removing a
//! referrer rewrites only the target itself — never the hashes stored by
//! other referrers.

use std::collections::VecDeque;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, storage_cost::removal::StorageRemovedBytes,
    CostResult, CostsExt,
};
use grovedb_merk::{
    element::{
        delete::ElementDeleteFromStorageExtensions,
        get::ElementFetchFromStorageExtensions,
        insert::{Delta, ElementInsertToStorageExtensions},
        ElementExt,
    },
    CryptoHash,
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};

use super::{BackwardReference, BidirectionalReference};
use crate::{
    merk_cache::{MerkCache, MerkHandle},
    operations::insert::InsertOptions,
    reference_path::{
        follow_reference, follow_reference_once, ReferencePathType, ResolvedReference,
    },
    Element, Error,
};

/// Write back a backward-references-capable element whose referrer list was
/// just modified. Items carry their combined hash implicitly
/// (`Element::insert` supplies it); a bidirectional reference additionally
/// needs the resolved end-of-chain hash its node commits to.
fn write_updated_target(
    merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
    element: Element,
    end_hash_for_reference: Option<CryptoHash>,
    version: &grovedb_version::version::GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    match &element {
        Element::BidirectionalReference(..) => {
            let end_hash = cost_return_on_error_no_add!(
                cost,
                end_hash_for_reference.ok_or(Error::InternalError(
                    "rewriting a bidirectional reference requires its resolved end hash".to_owned(),
                ))
            );
            cost_return_on_error!(
                &mut cost,
                merk.for_merk(|m| {
                    element
                        .insert_reference(m, key, end_hash, None, version)
                        .map_err(Error::MerkError)
                })
            );
        }
        _ => {
            cost_return_on_error!(
                &mut cost,
                merk.for_merk(|m| {
                    element
                        .insert(m, key, None, version)
                        .map_err(Error::MerkError)
                })
            );
        }
    }
    Ok(()).wrap_with_cost(cost)
}

/// Resolve the end-of-chain hash a bidirectional reference element at the
/// given position commits to (needed to rewrite it when only its referrer
/// list changes).
fn resolve_end_hash_for_reference_at<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    path: SubtreePathBuilder<'b, B>,
    key: &[u8],
    element: &Element,
) -> CostResult<Option<CryptoHash>, Error> {
    let mut cost = Default::default();
    let Element::BidirectionalReference(reference) = element else {
        return Ok(None).wrap_with_cost(cost);
    };
    let resolved = cost_return_on_error!(
        &mut cost,
        follow_reference(
            merk_cache,
            path,
            key,
            reference.forward_reference_path.clone()
        )
    );
    Ok(Some(resolved.target_node_value_hash)).wrap_with_cost(cost)
}

/// Register `backward_reference` on the target element, enforcing the
/// referrer budget (32 for items, 1 for references). `end_hash_for_target`
/// must be provided when the target is itself a bidirectional reference.
fn register_backward_reference(
    target_merk: &mut MerkHandle<'_, '_>,
    target_key: &[u8],
    mut target_element: Element,
    backward_reference: BackwardReference,
    end_hash_for_target: Option<CryptoHash>,
    version: &grovedb_version::version::GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    {
        let refs = cost_return_on_error_no_add!(
            cost,
            target_element
                .backward_references_mut()
                .ok_or(Error::BidirectionalReferenceRule(
                    "target does not support backward references".to_owned()
                ))
        );
        // Upsert: a referrer is identified by its inverted path, so an
        // edge update that only changes an option (e.g. cascade_on_update)
        // replaces the existing entry instead of appending a duplicate —
        // a duplicate would both break the budget check and be dropped
        // wholesale by a later removal of the same inverted path.
        if let Some(existing) = refs
            .iter_mut()
            .find(|r| r.inverted_reference == backward_reference.inverted_reference)
        {
            *existing = backward_reference;
        } else {
            refs.push(backward_reference);
        }
    }
    cost_return_on_error_no_add!(
        cost,
        target_element
            .validate_backward_references_limits()
            .map_err(|_| {
                Error::BidirectionalReferenceRule(
                    "backward references budget exceeded (32 per item, 1 per bidirectional \
                     reference)"
                        .to_owned(),
                )
            })
    );

    cost_return_on_error!(
        &mut cost,
        write_updated_target(
            target_merk,
            target_key,
            target_element,
            end_hash_for_target,
            version
        )
    );
    Ok(()).wrap_with_cost(cost)
}

/// Remove the referrer entry matching the inversion of
/// `forward_reference_path` from the element it resolves to (as seen from
/// `current_path`/`current_key`). Missing targets and missing entries are
/// tolerated — consistency can legitimately be bypassed by unflagged
/// writes.
fn remove_backward_reference<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    current_path: SubtreePathBuilder<'b, B>,
    current_key: &[u8],
    forward_reference_path: ReferencePathType,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    let inverted_reference = cost_return_on_error_no_add!(
        cost,
        forward_reference_path
            .invert(SubtreePath::from(&current_path), current_key)
            .ok_or_else(|| Error::BidirectionalReferenceRule(
                "unable to get an inverted reference".to_owned()
            ))
    );

    match follow_reference_once(
        merk_cache,
        current_path,
        current_key,
        forward_reference_path,
    )
    .unwrap_add_cost(&mut cost)
    {
        Ok(ResolvedReference {
            mut target_merk,
            target_key,
            mut target_element,
            target_path,
            ..
        }) => {
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
            let end_hash = cost_return_on_error!(
                &mut cost,
                resolve_end_hash_for_reference_at(
                    merk_cache,
                    target_path,
                    &target_key,
                    &target_element
                )
            );
            cost_return_on_error!(
                &mut cost,
                write_updated_target(
                    &mut target_merk,
                    &target_key,
                    target_element,
                    end_hash,
                    merk_cache.version
                )
            );
        }
        // We tolerate missing references because consistency can be bypassed,
        // and out-of-sync situations might be common.
        Err(Error::CorruptedReferencePathKeyNotFound(_)) => {}
        Err(e) => return Err(e).wrap_with_cost(cost),
    }

    Ok(()).wrap_with_cost(cost)
}

/// Insert bidirectional reference at specified location performing required
/// checks and updates
pub(crate) fn process_bidirectional_reference_insertion<'b, B: AsRef<[u8]>>(
    merk_cache: &MerkCache<'_, 'b, B>,
    path: SubtreePath<'b, B>,
    key: &[u8],
    mut reference: BidirectionalReference,
    options: Option<InsertOptions>,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    // Read what the key currently holds first. The stored referrer list is
    // carried over onto the new element (registrations survive an edge
    // update), and re-inserting an identical edge must be a true no-op.
    let mut merk = cost_return_on_error!(&mut cost, merk_cache.get_merk(path.derive_owned()));
    let previous_value = cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| {
            Element::get_optional(m, key, true, merk_cache.version).map_err(Error::MerkError)
        })
    );
    if let Some(Element::BidirectionalReference(ref old_ref)) = previous_value {
        // Carry the existing referrer list over.
        reference.backward_references = old_ref.backward_references.clone();
        if old_ref.forward_reference_path == reference.forward_reference_path
            && old_ref.cascade_on_update == reference.cascade_on_update
            && old_ref.max_hop == reference.max_hop
            && old_ref.flags == reference.flags
        {
            // Identical logical edge: nothing changed.
            return Ok(()).wrap_with_cost(cost);
        }
    } else {
        // The referrer list is bookkeeping this module maintains; whatever
        // the caller supplied is not theirs to claim.
        reference.backward_references.clear();
    }

    // Since we limit what kind of elements a bidirectional reference can target, a
    // check goes first:
    let ResolvedReference {
        mut target_merk,
        target_key,
        target_element,
        target_node_value_hash,
        ..
    } = cost_return_on_error!(
        &mut cost,
        follow_reference_once(
            merk_cache,
            path.derive_owned(),
            key,
            reference.forward_reference_path.clone(),
        )
    );

    if !target_element.supports_backward_references() {
        return Err(Error::BidirectionalReferenceRule(
            "Bidirectional references can only point variants with backward references support"
                .to_owned(),
        ))
        .wrap_with_cost(cost);
    }

    // If the closest target is a bidirectional reference itself, follow the
    // FULL chain starting from the position being written: the resolved
    // end-of-chain hash is what every chain member stores, and
    // `follow_reference` seeds its visited set with the starting qualified
    // path — so a chain that loops back through this key (a cycle that
    // would only materialize AFTER the write) is rejected with
    // `CyclicReference` before any mutation.
    let target_value_hash = if let Element::BidirectionalReference(..) = target_element {
        cost_return_on_error!(
            &mut cost,
            follow_reference(
                merk_cache,
                path.derive_owned(),
                key,
                reference.forward_reference_path.clone()
            )
        )
        .target_node_value_hash
    } else {
        target_node_value_hash
    };

    // Register the backward edge on the target:
    let inverted_reference = cost_return_on_error_no_add!(
        cost,
        reference
            .forward_reference_path
            .invert(path.clone(), key)
            .ok_or_else(|| Error::BidirectionalReferenceRule(
                "unable to get an inverted reference".to_owned()
            ))
    );
    // Rewriting a bidirectional-reference target needs the end hash ITS
    // node commits to — the same end-of-chain hash just resolved.
    let end_hash_for_target = if matches!(target_element, Element::BidirectionalReference(..)) {
        Some(target_value_hash)
    } else {
        None
    };
    cost_return_on_error!(
        &mut cost,
        register_backward_reference(
            &mut target_merk,
            &target_key,
            target_element,
            BackwardReference {
                inverted_reference,
                cascade_on_update: reference.cascade_on_update,
            },
            end_hash_for_target,
            merk_cache.version,
        )
    );

    // Write the new reference (its node hash combines its stripped bytes,
    // the resolved end hash, and its carried referrer list):
    cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| {
            Element::BidirectionalReference(reference.clone())
                .insert_reference(
                    m,
                    key,
                    target_value_hash,
                    options.map(|o| o.as_merk_options()),
                    merk_cache.version,
                )
                .map_err(Error::MerkError)
        })
    );

    match previous_value {
        // If previous value was another bidirectional reference, its backward
        // registration on the OLD target must be removed
        Some(Element::BidirectionalReference(old_reference)) => {
            // Same forward path means same target and same inverted path:
            // the registration was just refreshed in place by the upsert
            // above, and removing it here would strip the edge entirely.
            if old_reference.forward_reference_path != reference.forward_reference_path {
                cost_return_on_error!(
                    &mut cost,
                    remove_backward_reference(
                        merk_cache,
                        path.derive_owned(),
                        key,
                        old_reference.forward_reference_path,
                    )
                );
            }

            // The chain now resolves to a new end hash; referrers of THIS
            // reference must be updated with it.
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references(
                    merk_cache,
                    merk,
                    path.derive_owned(),
                    key.to_vec(),
                    target_value_hash
                )
            );
        }
        // If overwriting items with backward references it is an error since they can have many
        // backward references when inserted bidirectional reference can have only one
        Some(
            Element::ItemWithBackwardsReferences(..) | Element::SumItemWithBackwardsReferences(..),
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

    Ok(()).wrap_with_cost(cost)
}

/// Post-processing of possible backward references relationships after
/// insertion of anything but bidirectional reference (because there is
/// [process_bidirectional_reference_insertion] for that).
pub(crate) fn process_update_element_with_backward_references<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    merk: MerkHandle<'db, 'c>,
    path: SubtreePathBuilder<'b, B>,
    key: &[u8],
    delta: Delta,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    // On no changes no propagations shall happen:
    if !delta.has_changed() {
        return Ok(()).wrap_with_cost(cost);
    }

    // If there was no overwrite we short-circuit as well:
    let Some(old) = delta.old else {
        return Ok(()).wrap_with_cost(cost);
    };

    match (old, delta.new) {
        (
            Element::ItemWithBackwardsReferences(..) | Element::SumItemWithBackwardsReferences(..),
            Some(
                new @ (Element::ItemWithBackwardsReferences(..)
                | Element::SumItemWithBackwardsReferences(..)),
            ),
        ) => {
            // Update with another backward references-compatible element:
            // referrers commit to the INNER hash, so propagate the new one
            // along every chain.
            let new_logical_hash = cost_return_on_error!(
                &mut cost,
                new.logical_value_hash(merk_cache.version)
                    .map_err(Error::from)
            );
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references(
                    merk_cache,
                    merk,
                    path,
                    key.to_vec(),
                    new_logical_hash
                )
            );
        }
        (
            old @ (Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)),
            _,
        ) => {
            // Update with non backward references-compatible element (or deletion), equals
            // to cascade deletion of references' chains:
            cost_return_on_error!(
                &mut cost,
                delete_backward_references_recursively(merk_cache, path, key.to_vec(), old)
            );
        }

        (
            Element::BidirectionalReference(old_reference),
            Some(
                new @ (Element::ItemWithBackwardsReferences(..)
                | Element::SumItemWithBackwardsReferences(..)),
            ),
        ) => {
            // Overwrite of bidirectional reference with backward references-compatible
            // elements triggers propagation and removes the old backward
            // registration on the old target
            let new_logical_hash = cost_return_on_error!(
                &mut cost,
                new.logical_value_hash(merk_cache.version)
                    .map_err(Error::from)
            );
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references(
                    merk_cache,
                    merk,
                    path.clone(),
                    key.to_vec(),
                    new_logical_hash
                )
            );

            cost_return_on_error!(
                &mut cost,
                remove_backward_reference(
                    merk_cache,
                    path,
                    key,
                    old_reference.forward_reference_path
                )
            );
        }
        (Element::BidirectionalReference(old_reference), _) => {
            // Overwrite of bidirectional reference with non backward
            // references-compatible element (or with nothing aka deletion)
            // shall trigger recursive deletion and removal of the backward
            // registration from where the bidi ref used to point to
            cost_return_on_error!(
                &mut cost,
                delete_backward_references_recursively(
                    merk_cache,
                    path.clone(),
                    key.to_vec(),
                    Element::BidirectionalReference(old_reference.clone()),
                )
            );

            cost_return_on_error!(
                &mut cost,
                remove_backward_reference(
                    merk_cache,
                    path,
                    key,
                    old_reference.forward_reference_path
                )
            );
        }
        _ => {
            // All other overwrites don't require special attention
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Recursively deletes all backward references' chains of a key if all of
/// them allow cascade deletion. `start_element` is the (already
/// overwritten/deleted) element whose referrer list seeds the cascade.
fn delete_backward_references_recursively<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    path: SubtreePathBuilder<'b, B>,
    key: Vec<u8>,
    start_element: Element,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    let mut queue = VecDeque::new();
    // Each node has exactly one forward edge, so reverse reachability from
    // one start forms a tree; a revisit means the on-disk graph encodes a
    // cycle, which insertion rejects — corrupted state, bail instead of
    // looping forever.
    let mut visited: std::collections::HashSet<(Vec<Vec<u8>>, Vec<u8>)> = Default::default();

    visited.insert((path.to_vec(), key.clone()));
    queue.push_back((path, key, start_element, true));

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

            let resolved = follow_reference_once(
                merk_cache,
                current_path.clone(),
                &current_key,
                backward_ref.inverted_reference,
            )
            .unwrap_add_cost(&mut cost);

            let ResolvedReference {
                target_path: origin_path,
                target_key: origin_key,
                target_element: origin_element,
                ..
            } = match resolved {
                Ok(resolved) => resolved,
                // Dangling referrer (removed by an unflagged write or a
                // batch): nothing left to cascade there.
                Err(Error::CorruptedReferencePathKeyNotFound(_)) => continue,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };

            if !visited.insert((origin_path.to_vec(), origin_key.clone())) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            queue.push_back((origin_path, origin_key, origin_element, false));
        }

        // Delete the element itself, unless it is the cascade's start (the
        // original was already overwritten or deleted by the caller).
        if !first {
            let mut origin_merk =
                cost_return_on_error!(&mut cost, merk_cache.get_merk(current_path.clone()));
            cost_return_on_error!(
                &mut cost,
                origin_merk.for_merk(|m| {
                    Element::delete_with_sectioned_removal_bytes(
                        m,
                        current_key,
                        None,
                        false,
                        m.tree_type,
                        &mut |_, removed_key_bytes, removed_value_bytes| {
                            Ok((
                                StorageRemovedBytes::BasicStorageRemoval(removed_key_bytes),
                                StorageRemovedBytes::BasicStorageRemoval(removed_value_bytes),
                            ))
                        },
                        merk_cache.version,
                    )
                    .map_err(Error::MerkError)
                })
            );
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Recursively updates all backward references' chains of a key with the
/// new end-of-chain value hash.
fn propagate_backward_references<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    mut merk: MerkHandle<'db, 'c>,
    path: SubtreePathBuilder<'b, B>,
    key: Vec<u8>,
    referenced_element_value_hash: CryptoHash,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    let mut queue = VecDeque::new();
    // See the identical bound in `delete_backward_references_recursively`.
    let mut visited: std::collections::HashSet<(Vec<Vec<u8>>, Vec<u8>)> = Default::default();

    // Seed with the updated element's current referrer list.
    let start_element = cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| {
            Element::get(m, &key, true, merk_cache.version).map_err(Error::MerkError)
        })
    );
    visited.insert((path.to_vec(), key.clone()));
    queue.push_back((path, key, start_element));

    while let Some((current_path, current_key, current_element)) = queue.pop_front() {
        let backward_references = current_element
            .backward_references()
            .map(|refs| refs.to_vec())
            .unwrap_or_default();
        let mut dangling: Vec<ReferencePathType> = Vec::new();

        for backward_ref in backward_references {
            let resolved = follow_reference_once(
                merk_cache,
                current_path.clone(),
                &current_key,
                backward_ref.inverted_reference.clone(),
            )
            .unwrap_add_cost(&mut cost);

            let ResolvedReference {
                target_merk: mut origin_merk,
                target_path: origin_path,
                target_key: origin_key,
                target_element: origin_element,
                ..
            } = match resolved {
                Ok(resolved) => resolved,
                // Dangling referrer (removed by an unflagged write or a
                // batch): clean the stale entry lazily and keep going.
                Err(Error::CorruptedReferencePathKeyNotFound(_)) => {
                    dangling.push(backward_ref.inverted_reference);
                    continue;
                }
                Err(e) => return Err(e).wrap_with_cost(cost),
            };

            // Rewrite the referrer with the new end hash (its own referrer
            // list rides along inside the element bytes).
            cost_return_on_error!(
                &mut cost,
                origin_merk.for_merk(|m| {
                    origin_element
                        .clone()
                        .insert_reference(
                            m,
                            &origin_key,
                            referenced_element_value_hash,
                            None,
                            merk_cache.version,
                        )
                        .map_err(Error::MerkError)
                })
            );

            if !visited.insert((origin_path.to_vec(), origin_key.clone())) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            queue.push_back((origin_path, origin_key, origin_element));
        }

        if !dangling.is_empty() {
            // Drop the dangling entries from the current element and write
            // it back (for a bidirectional reference the end hash it
            // commits to is exactly the one being propagated).
            let mut current_merk =
                cost_return_on_error!(&mut cost, merk_cache.get_merk(current_path.clone()));
            let mut updated = current_element;
            if let Some(refs) = updated.backward_references_mut() {
                refs.retain(|r| !dangling.contains(&r.inverted_reference));
            }
            let end_hash = if matches!(updated, Element::BidirectionalReference(..)) {
                Some(referenced_element_value_hash)
            } else {
                None
            };
            cost_return_on_error!(
                &mut cost,
                write_updated_target(
                    &mut current_merk,
                    &current_key,
                    updated,
                    end_hash,
                    merk_cache.version
                )
            );
        }
    }

    Ok(()).wrap_with_cost(cost)
}
