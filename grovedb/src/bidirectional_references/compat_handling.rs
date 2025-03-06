//! Bidirectional references handling module dedicated to compatibility with batches code.
//!
//! `_ops` suffix means that instead of application to cached subtrees, Merk operations
//! will be returned

use std::{collections::VecDeque, io::Write};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, storage_cost::removal::StorageRemovedBytes,
    CostResult, CostsExt,
};
use grovedb_merk::{tree::MetaOp, CryptoHash, MerkBatch};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_version::version::GroveVersion;

use super::{
    handling_common::{self, BackwardReference},
    BidirectionalReference, SlotIdx,
};
use crate::{
    element::Delta,
    merk_cache::{MerkCache, MerkHandle},
    operations::insert::InsertOptions,
    reference_path::{follow_reference, follow_reference_once, ResolvedReference},
    Element, Error,
};

impl BidirectionalReference {
    /// Given current path removes backward reference from the merk and key
    /// where this bidirectional references points to
    fn remove_backward_reference_ops<'b, B: AsRef<[u8]>>(
        self,
        merk_cache: &MerkCache<'_, 'b, B>,
        current_path: SubtreePathBuilder<'b, B>,
        current_key: &[u8],
    ) -> CostResult<(), Error> {
        let mut cost = Default::default();

        match follow_reference_once(
            merk_cache,
            current_path,
            current_key,
            self.forward_reference_path,
        )
        .unwrap_add_cost(&mut cost)
        {
            Ok(ResolvedReference {
                mut target_merk,
                target_key,
                ..
            }) => {
                cost_return_on_error!(
                    &mut cost,
                    Self::remove_backward_reference_resolved_ops(
                        &mut target_merk,
                        &target_key,
                        self.backward_reference_slot,
                        merk_cache.version,
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

    /// "Resolved" means we know exactly where backward reference is located
    /// to remove it from there opposed to non-"resolved" version that starts with
    /// a forward reference resolution.
    fn remove_backward_reference_resolved_ops(
        target_merk: &mut MerkHandle<'_, '_>,
        target_key: &[u8],
        slot_idx: SlotIdx,
        version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = Default::default();

        let (prefix, mut bits) = cost_return_on_error!(
            &mut cost,
            handling_common::get_backward_references_bitvec(target_merk, target_key)
        );

        bits.set(slot_idx, false);

        let mut indexed_prefix = prefix.clone();
        write!(&mut indexed_prefix, "{}", slot_idx).expect("no io involved");

        cost_return_on_error!(
            &mut cost,
            target_merk.for_merk(|m| m
                .apply::<Vec<_>, Vec<_>, _>(
                    &MerkBatch {
                        batch_entries: Default::default(),
                        aux_batch_entries: Default::default(),
                        meta_batch_entries: &[
                            (
                                prefix.clone(),
                                MetaOp::PutMeta(bits.into_inner()[0].to_be_bytes().to_vec()),
                                None
                            ),
                            (indexed_prefix, MetaOp::DeleteMeta, None)
                        ]
                    },
                    None,
                    version
                )
                .map_err(Error::MerkError))
        );

        Ok(()).wrap_with_cost(cost)
    }
}

/// Insert bidirectional reference at specified location performing required
/// checks and updates
pub(crate) fn process_bidirectional_reference_insertion_ops<'b, B: AsRef<[u8]>>(
    merk_cache: &MerkCache<'_, 'b, B>,
    path: SubtreePath<'b, B>,
    key: &[u8],
    mut reference: BidirectionalReference,
    options: Option<InsertOptions>,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    // Since we limit what kind of elements a bidirectional reference can target, a
    // check goes first:
    let ResolvedReference {
        mut target_merk,
        target_key,
        target_element,
        target_node_value_hash,
        target_path,
    } = cost_return_on_error!(
        &mut cost,
        follow_reference_once(
            merk_cache,
            path.derive_owned(),
            key,
            reference.forward_reference_path.clone(),
        )
    );

    if !matches!(
        target_element,
        Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)
            | Element::BidirectionalReference(..)
    ) {
        return Err(Error::BidirectionalReferenceRule(
            "Bidirectional references can only point variants with backward references support"
                .to_owned(),
        ))
        .wrap_with_cost(cost);
    }

    // If the closest target item is a bidirectional reference itself, a few
    // additional steps are required:
    // 1. We limit the number of backward references it supports by 1.
    // 2. We ignore the value hash of the reference and continue following the
    //    chain.
    let target_value_hash = if let Element::BidirectionalReference(BidirectionalReference {
        forward_reference_path,
        ..
    }) = target_element
    {
        let (_, bitvec) = cost_return_on_error!(
            &mut cost,
            handling_common::get_backward_references_bitvec(&mut target_merk, &target_key)
        );

        if !bitvec.not_any() {
            return Err(Error::BidirectionalReferenceRule(
                "Number of backward references for a single bidirectional references is limited \
                 to 1"
                    .to_owned(),
            ))
            .wrap_with_cost(cost);
        }

        cost_return_on_error!(
            &mut cost,
            follow_reference(merk_cache, target_path, &target_key, forward_reference_path)
        )
        .target_node_value_hash
    } else {
        target_node_value_hash
    };

    // If the closest target element passes the first check, attempt to add backward
    // reference:
    let inverted_reference = cost_return_on_error_no_add!(
        cost,
        reference
            .forward_reference_path
            .invert(path.clone(), key)
            .ok_or_else(|| Error::BidirectionalReferenceRule(
                "unable to get an inverted reference".to_owned()
            ))
    );
    let slot = cost_return_on_error!(
        &mut cost,
        add_backward_reference_ops(
            &mut target_merk,
            &target_key,
            BackwardReference {
                inverted_reference,
                cascade_on_update: reference.cascade_on_update,
            },
            merk_cache.version
        )
    );

    // Update the reference we insert with a backward reference slot used
    reference.backward_reference_slot = slot;

    // Proceed with bidirectional reference insertion as regular reference
    let mut merk = cost_return_on_error!(&mut cost, merk_cache.get_merk(path.derive_owned()));

    let previous_value = cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| Element::get_optional(m, key, true, merk_cache.version))
    );

    if let Some(Element::BidirectionalReference(ref old_ref)) = previous_value {
        if old_ref == &reference {
            // Short-circuit if nothing was changed
            return Ok(()).wrap_with_cost(cost);
        }
    }

    cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| {
            Element::BidirectionalReference(reference).insert_reference(
                m,
                key,
                target_value_hash,
                options.map(|o| o.as_merk_options()),
                merk_cache.version,
            )
        })
    );

    match previous_value {
        // If previous value was another bidirectional reference, backward reference of
        // an older one shall be removed from target merks' meta
        Some(Element::BidirectionalReference(reference)) => {
            cost_return_on_error!(
                &mut cost,
                reference.remove_backward_reference_ops(merk_cache, path.derive_owned(), key)
            );

            // This also requires propagation, because new target means new value hash and
            // backward references' chain shall be notified:
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references_ops(
                    merk_cache,
                    merk,
                    path.derive_owned(),
                    key.to_vec(),
                    target_node_value_hash
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
        // Insertion into new place shall allocate empty bitvec of backward references
        None => {
            let prefix = handling_common::make_meta_prefix(key);
            cost_return_on_error!(
                &mut cost,
                merk.for_merk(|m| m
                    .apply::<Vec<_>, Vec<_>, _>(
                        &MerkBatch {
                            batch_entries: Default::default(),
                            aux_batch_entries: Default::default(),
                            meta_batch_entries: &[(
                                prefix.clone(),
                                MetaOp::PutMeta(0u32.to_be_bytes().to_vec()),
                                None
                            )]
                        },
                        None,
                        merk_cache.version
                    )
                    .map_err(Error::MerkError))
            );
        }
        // If regular item/sum item was overwritten then no actions needed
        _ => {}
    }

    Ok(()).wrap_with_cost(cost)
}

/// Post-processing of possible backward references relationships after
/// insertion of anything but bidirectional reference (because there is
/// [process_bidirectional_reference_insertion] for that).
pub(crate) fn process_update_element_with_backward_references_ops<'db, 'b, 'c, B: AsRef<[u8]>>(
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
            // Update with another backward references-compatible element variant, that
            // means value hash propagation across backward references' chains:
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references_ops(
                    merk_cache,
                    merk,
                    path,
                    key.to_vec(),
                    cost_return_on_error!(&mut cost, new.value_hash(merk_cache.version))
                )
            );
        }
        (
            Element::ItemWithBackwardsReferences(..) | Element::SumItemWithBackwardsReferences(..),
            _,
        ) => {
            // Update with non backward references-compatible element (or deletion), equals
            // to cascade deletion of references' chains:
            cost_return_on_error!(
                &mut cost,
                delete_backward_references_recursively_ops(merk_cache, merk, path, key.to_vec())
            );
        }

        (
            Element::BidirectionalReference(reference),
            Some(
                new @ (Element::ItemWithBackwardsReferences(..)
                | Element::SumItemWithBackwardsReferences(..)),
            ),
        ) => {
            // Overwrite of bidirectional reference with backward references-compatible
            // elements triggers propagation and removes one backward reference because of
            // removal of old bidi ref

            cost_return_on_error!(
                &mut cost,
                propagate_backward_references_ops(
                    merk_cache,
                    merk,
                    path.clone(),
                    key.to_vec(),
                    cost_return_on_error!(&mut cost, new.value_hash(merk_cache.version))
                )
            );

            cost_return_on_error!(
                &mut cost,
                reference.remove_backward_reference_ops(merk_cache, path, key)
            );
        }
        (Element::BidirectionalReference(reference), _) => {
            // Overwrite of bidirectional reference with non backward
            // references-compatible element (or with nothing aka deletion)
            // shall trigger recursive deletion and removal of backward refrence
            // from the element where the bidi ref in question used to point to

            // Since we're overwriting with backward references-incompatible element we
            // issue a recursive deletion of backward references chains:
            cost_return_on_error!(
                &mut cost,
                delete_backward_references_recursively_ops(
                    merk_cache,
                    merk,
                    path.clone(),
                    key.to_vec()
                )
            );

            cost_return_on_error!(
                &mut cost,
                reference.remove_backward_reference_ops(merk_cache, path, key)
            );
        }
        _ => {
            // All other overwrites don't require special attention
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Recursively deletes all backward references' chains of a key if all of them
/// allow cascade deletion.
fn delete_backward_references_recursively_ops<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    merk: MerkHandle<'db, 'c>,
    path: SubtreePathBuilder<'b, B>,
    key: Vec<u8>,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    let mut queue = VecDeque::new();

    queue.push_back((merk, path, key));
    let mut first = true;

    // Just like with propagation we follow all references chains...
    while let Some((mut current_merk, current_path, current_key)) = queue.pop_front() {
        let backward_references = cost_return_on_error!(
            &mut cost,
            handling_common::get_backward_references(&mut current_merk, &current_key)
        );
        for (idx, backward_ref) in backward_references.into_iter() {
            if !backward_ref.cascade_on_update {
                return Err(Error::BidirectionalReferenceRule(
                    "deletion of backward references through deletion of an element requires \
                     `cascade_on_update` setting"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }

            let ResolvedReference {
                target_merk: origin_bidi_merk,
                target_path: origin_bidi_path,
                target_key: origin_bidi_key,
                ..
            } = cost_return_on_error!(
                &mut cost,
                follow_reference_once(
                    merk_cache,
                    current_path.clone(),
                    &current_key,
                    backward_ref.inverted_reference
                )
            );

            // ... except removing backward references from meta...
            cost_return_on_error!(
                &mut cost,
                BidirectionalReference::remove_backward_reference_resolved_ops(
                    &mut current_merk,
                    &current_key,
                    idx,
                    merk_cache.version,
                )
            );

            queue.push_back((origin_bidi_merk, origin_bidi_path, origin_bidi_key));
        }

        // ... and the element altogether, if it is later down the cascade (the original
        // item was overwritten or deleted, no need to delete it here)
        if !first {
            cost_return_on_error!(
                &mut cost,
                current_merk.for_merk(|m| Element::delete_with_sectioned_removal_bytes(
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
                    merk_cache.version
                ))
            );
        } else {
            first = false;
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Recursively updates all backward references' chains of a key.
fn propagate_backward_references_ops<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    merk: MerkHandle<'db, 'c>,
    path: SubtreePathBuilder<'b, B>,
    key: Vec<u8>,
    referenced_element_value_hash: CryptoHash,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    let mut queue = VecDeque::new();

    queue.push_back((merk, path, key));

    while let Some((mut current_merk, current_path, current_key)) = queue.pop_front() {
        let backward_references = cost_return_on_error!(
            &mut cost,
            handling_common::get_backward_references(&mut current_merk, &current_key)
        );
        for (_, backward_ref) in backward_references.into_iter() {
            let ResolvedReference {
                target_merk: mut origin_bidi_merk,
                target_path: origin_bidi_path,
                target_key: origin_bidi_key,
                target_element: origin_bidi_ref,
                ..
            } = cost_return_on_error!(
                &mut cost,
                follow_reference_once(
                    merk_cache,
                    current_path.clone(),
                    &current_key,
                    backward_ref.inverted_reference
                )
            );

            cost_return_on_error!(
                &mut cost,
                origin_bidi_merk.for_merk(|m| {
                    origin_bidi_ref.insert_reference(
                        m,
                        &origin_bidi_key,
                        referenced_element_value_hash,
                        None,
                        merk_cache.version,
                    )
                })
            );

            queue.push_back((origin_bidi_merk, origin_bidi_path, origin_bidi_key));
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Adds backward reference to meta storage of a subtree.
///
/// Only up to 32 backward references are allowed, for that reason we use a
/// bitvec to locate them. That way the bits is stored under
/// [META_BACKWARD_REFERENCES_PREFIX] with key and references themselves are
/// located under this prefix with index (0-31) appended.
fn add_backward_reference_ops(
    target_merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
    backward_reference: BackwardReference,
    version: &GroveVersion,
) -> CostResult<SlotIdx, Error> {
    let mut cost = Default::default();

    let (prefix, mut bits) = cost_return_on_error!(
        &mut cost,
        handling_common::get_backward_references_bitvec(target_merk, key)
    );

    if let Some(free_index) = bits.first_zero() {
        let mut idx_prefix = prefix.clone();
        write!(&mut idx_prefix, "{free_index}").expect("no io involved");

        let serialized_ref = cost_return_on_error_no_add!(cost, backward_reference.serialize());

        bits.set(free_index, true);

        cost_return_on_error!(
            &mut cost,
            target_merk.for_merk(|m| m
                .apply::<Vec<_>, Vec<_>, _>(
                    &MerkBatch {
                        batch_entries: Default::default(),
                        aux_batch_entries: Default::default(),
                        meta_batch_entries: &[
                            (idx_prefix, MetaOp::PutMeta(serialized_ref), None),
                            (
                                prefix,
                                MetaOp::PutMeta(bits.into_inner()[0].to_be_bytes().to_vec()),
                                None
                            ),
                        ]
                    },
                    None,
                    version
                )
                .map_err(Error::MerkError))
        );

        Ok(free_index)
    } else {
        Err(Error::BidirectionalReferenceRule(
            "only up to 32 backward references for an item are supported".to_owned(),
        ))
    }
    .wrap_with_cost(cost)
}
