//! Bidirectional references handling module.
//! Definitions are stored at parent for proper feature gating.

use std::{collections::VecDeque, io::Write};

use bincode::{config, Decode, Encode};
use bitvec::{array::BitArray, order::Lsb0};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, storage_cost::removal::StorageRemovedBytes,
    CostResult, CostsExt,
};
use grovedb_merk::CryptoHash;
use grovedb_path::{SubtreePath, SubtreePathBuilder};

use super::{BidirectionalReference, SlotIdx, META_BACKWARD_REFERENCES_PREFIX};
use crate::{
    element::Delta,
    merk_cache::{MerkCache, MerkHandle},
    operations::insert::InsertOptions,
    reference_path::{
        follow_reference, follow_reference_once, ReferencePathType, ResolvedReference,
    },
    Element, Error,
};

impl BidirectionalReference {
    /// Given current path removes backward reference from the merk and key
    /// where this bidirectional references points to
    fn remove_backward_reference<'b, B: AsRef<[u8]>>(
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
                    Self::remove_backward_reference_resolved(
                        &mut target_merk,
                        &target_key,
                        self.backward_reference_slot
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

    fn remove_backward_reference_resolved(
        target_merk: &mut MerkHandle<'_, '_>,
        target_key: &[u8],
        slot_idx: SlotIdx,
    ) -> CostResult<(), Error> {
        let mut cost = Default::default();

        let (prefix, mut bits) = cost_return_on_error!(
            &mut cost,
            get_backward_references_bitvec(target_merk, target_key)
        );

        bits.set(slot_idx, false);

        cost_return_on_error!(
            &mut cost,
            target_merk.for_merk(|m| m
                .put_meta(prefix.clone(), bits.into_inner()[0].to_be_bytes().to_vec())
                .map_err(Error::MerkError))
        );

        let mut indexed_prefix = prefix;
        write!(&mut indexed_prefix, "{}", slot_idx).expect("no io involved");

        cost_return_on_error!(
            &mut cost,
            target_merk.for_merk(|m| m.delete_meta(&indexed_prefix).map_err(Error::MerkError))
        );

        Ok(()).wrap_with_cost(cost)
    }
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
            get_backward_references_bitvec(&mut target_merk, &target_key)
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
        add_backward_reference(
            &mut target_merk,
            &target_key,
            BackwardReference {
                inverted_reference,
                cascade_on_update: reference.cascade_on_update,
            },
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
                reference.remove_backward_reference(merk_cache, path.derive_owned(), key)
            );

            // This also requires propagation, because new target means new value hash and
            // backward references' chain shall be notified:
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references(
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
            let prefix = make_meta_prefix(key);
            cost_return_on_error!(
                &mut cost,
                merk.for_merk(|m| m
                    .put_meta(prefix, 0u32.to_be_bytes().to_vec())
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
            // Update with another backward references-compatible element variant, that
            // means value hash propagation across backward references' chains:
            cost_return_on_error!(
                &mut cost,
                propagate_backward_references(
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
                delete_backward_references_recursively(merk_cache, merk, path, key.to_vec())
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
                propagate_backward_references(
                    merk_cache,
                    merk,
                    path.clone(),
                    key.to_vec(),
                    cost_return_on_error!(&mut cost, new.value_hash(merk_cache.version))
                )
            );

            cost_return_on_error!(
                &mut cost,
                reference.remove_backward_reference(merk_cache, path, key)
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
                delete_backward_references_recursively(
                    merk_cache,
                    merk,
                    path.clone(),
                    key.to_vec()
                )
            );

            cost_return_on_error!(
                &mut cost,
                reference.remove_backward_reference(merk_cache, path, key)
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
fn delete_backward_references_recursively<'db, 'b, 'c, B: AsRef<[u8]>>(
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
            get_backward_references(&mut current_merk, &current_key)
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
                BidirectionalReference::remove_backward_reference_resolved(
                    &mut current_merk,
                    &current_key,
                    idx
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
fn propagate_backward_references<'db, 'b, 'c, B: AsRef<[u8]>>(
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
            get_backward_references(&mut current_merk, &current_key)
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

#[derive(Debug, Encode, Decode, PartialEq)]
pub(crate) struct BackwardReference {
    pub(crate) inverted_reference: ReferencePathType,
    pub(crate) cascade_on_update: bool,
}

impl BackwardReference {
    fn serialize(&self) -> Result<Vec<u8>, Error> {
        let config = config::standard().with_big_endian().with_no_limit();
        bincode::encode_to_vec(self, config).map_err(|e| {
            Error::CorruptedData(format!("unable to serialize backward reference {}", e))
        })
    }

    fn deserialize(bytes: &[u8]) -> Result<BackwardReference, Error> {
        let config = config::standard().with_big_endian().with_no_limit();
        Ok(bincode::decode_from_slice(bytes, config)
            .map_err(|e| Error::CorruptedData(format!("unable to deserialize element {}", e)))?
            .0)
    }
}

type Prefix = Vec<u8>;

fn make_meta_prefix(key: &[u8]) -> Vec<u8> {
    let mut backrefs_for_key = META_BACKWARD_REFERENCES_PREFIX.to_vec();
    backrefs_for_key.extend_from_slice(&key.len().to_be_bytes());
    backrefs_for_key.extend_from_slice(key);

    backrefs_for_key
}

/// Get bitvec of backward references' slots for a key of a subtree.
/// Prefix for a Merk's meta storage is made of constant keyword, lenght of the
/// key and the key itself. Under the prefix GroveDB stores bitvec, and slots
/// for backward references are integers appended to the prefix.
fn get_backward_references_bitvec(
    merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
) -> CostResult<(Prefix, BitArray<[u32; 1], Lsb0>), Error> {
    let mut cost = Default::default();

    let backrefs_for_key = make_meta_prefix(key);

    let stored_bytes = cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| m
            .get_meta(backrefs_for_key.clone())
            .map_ok(|opt_v| opt_v.map(|v| v.to_vec()))
            .map_err(Error::MerkError))
    );

    let bits: BitArray<[u32; 1], Lsb0> = if let Some(bytes) = stored_bytes {
        cost_return_on_error_no_add!(
            cost,
            bytes
                .try_into()
                .map(|b| BitArray::new([u32::from_be_bytes(b)]))
                .map_err(|_| Error::InternalError(
                    "backward references' bitvec is expected to be 4 bytes".to_owned()
                ))
        )
    } else {
        Default::default()
    };

    Ok((backrefs_for_key, bits)).wrap_with_cost(cost)
}

/// Adds backward reference to meta storage of a subtree.
///
/// Only up to 32 backward references are allowed, for that reason we use a
/// bitvec to locate them. That way the bits is stored under
/// [META_BACKWARD_REFERENCES_PREFIX] with key and references themselves are
/// located under this prefix with index (0-31) appended.
fn add_backward_reference(
    target_merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
    backward_reference: BackwardReference,
) -> CostResult<SlotIdx, Error> {
    let mut cost = Default::default();

    let (prefix, mut bits) =
        cost_return_on_error!(&mut cost, get_backward_references_bitvec(target_merk, key));

    if let Some(free_index) = bits.first_zero() {
        let mut idx_prefix = prefix.clone();
        write!(&mut idx_prefix, "{free_index}").expect("no io involved");

        let serialized_ref = cost_return_on_error_no_add!(cost, backward_reference.serialize());
        cost_return_on_error!(
            &mut cost,
            target_merk.for_merk(|m| m
                .put_meta(idx_prefix, serialized_ref)
                .map_err(Error::MerkError))
        );

        bits.set(free_index, true);

        cost_return_on_error!(
            &mut cost,
            target_merk.for_merk(|m| m
                .put_meta(prefix, bits.into_inner()[0].to_be_bytes().to_vec())
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

/// Return a vector of backward references to the item
fn get_backward_references(
    merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
) -> CostResult<Vec<(SlotIdx, BackwardReference)>, Error> {
    let mut cost = Default::default();

    let (prefix, bits) =
        cost_return_on_error!(&mut cost, get_backward_references_bitvec(merk, key));

    let mut backward_references = Vec::new();

    for idx in bits.iter_ones() {
        let mut indexed_prefix = prefix.clone();
        write!(&mut indexed_prefix, "{idx}").expect("no io involved");

        let bytes_opt = cost_return_on_error!(
            &mut cost,
            merk.for_merk(|m| m
                .get_meta(indexed_prefix)
                .map_err(Error::MerkError)
                .map_ok(|opt_v| opt_v.map(|v| v.to_vec())))
        );

        let bytes = cost_return_on_error_no_add!(
            cost,
            bytes_opt.ok_or_else(|| {
                Error::InternalError(
                    "backward references bitvec and slot are out of sync".to_owned(),
                )
            })
        );

        backward_references.push((
            idx,
            cost_return_on_error_no_add!(cost, BackwardReference::deserialize(&bytes)),
        ));
    }

    Ok(backward_references).wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use grovedb_path::{SubtreePath, SubtreePathBuilder};
    use grovedb_version::version::GroveVersion;
    use pretty_assertions::{assert_eq, assert_ne};

    use super::*;
    use crate::{
        merk_cache::MerkCache,
        tests::{make_deep_tree, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
    };

    #[test]
    fn add_multiple_backward_references() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(&version);
        let tx = db.start_transaction();
        let cache = MerkCache::new(&db, &tx, &version);

        let mut merk = cache.get_merk(SubtreePathBuilder::new()).unwrap().unwrap();

        let slot0 = add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy1".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .unwrap();

        let slot1 = add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy2".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .unwrap();

        let slot2 = add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy3".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(slot0, 0);
        assert_eq!(slot1, 1);
        assert_eq!(slot2, 2);

        assert_eq!(
            get_backward_references(&mut merk, TEST_LEAF)
                .unwrap()
                .unwrap()
                .into_iter()
                .map(
                    |(
                        _,
                        BackwardReference {
                            inverted_reference, ..
                        },
                    )| inverted_reference
                )
                .collect::<Vec<_>>(),
            vec![
                ReferencePathType::AbsolutePathReference(vec![b"dummy1".to_vec()]),
                ReferencePathType::AbsolutePathReference(vec![b"dummy2".to_vec()]),
                ReferencePathType::AbsolutePathReference(vec![b"dummy3".to_vec()]),
            ]
        );
    }

    #[test]
    fn using_free_slots() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(&version);
        let tx = db.start_transaction();
        let cache = MerkCache::new(&db, &tx, &version);

        let mut merk = cache.get_merk(SubtreePathBuilder::new()).unwrap().unwrap();

        let slot0 = add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy1".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .unwrap();

        let slot1 = add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy2".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .unwrap();

        let slot2 = add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy3".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(slot0, 0);
        assert_eq!(slot1, 1);
        assert_eq!(slot2, 2);

        BidirectionalReference::remove_backward_reference_resolved(&mut merk, TEST_LEAF, 1)
            .unwrap()
            .unwrap();

        assert_eq!(
            add_backward_reference(
                &mut merk,
                TEST_LEAF,
                BackwardReference {
                    inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                        b"dummy4".to_vec()
                    ]),
                    cascade_on_update: false,
                },
            )
            .unwrap()
            .unwrap(),
            1
        );
    }

    #[test]
    fn overflow() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(&version);
        let tx = db.start_transaction();
        let cache = MerkCache::new(&db, &tx, &version);

        let mut merk = cache.get_merk(SubtreePathBuilder::new()).unwrap().unwrap();

        (0..32).for_each(|_| {
            add_backward_reference(
                &mut merk,
                TEST_LEAF,
                BackwardReference {
                    inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                        b"dummy1".to_vec()
                    ]),
                    cascade_on_update: false,
                },
            )
            .unwrap()
            .unwrap();
        });

        assert!(add_backward_reference(
            &mut merk,
            TEST_LEAF,
            BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![
                    b"dummy1".to_vec()
                ]),
                cascade_on_update: false,
            },
        )
        .unwrap()
        .is_err());
    }

    #[test]
    fn overflow_for_bidi_reference() {
        let version = GroveVersion::latest();
        let db = make_test_grovedb(&version);

        // Create an item that support backward references
        db.insert(
            SubtreePath::from(&[TEST_LEAF]),
            b"key1",
            Element::new_item_allowing_bidirectional_references(b"value".to_vec()),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();

        let tx = db.start_transaction();

        let mut cache = MerkCache::new(&db, &tx, &version);

        // Add a bidirectional reference to the item
        process_bidirectional_reference_insertion(
            &mut cache,
            SubtreePath::from(&[ANOTHER_TEST_LEAF]),
            b"key2",
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"key1".to_vec(),
                ]),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        // Add a bidirectional reference that points to the bidirectional reference
        // above
        assert!(matches!(
            process_bidirectional_reference_insertion(
                &mut cache,
                SubtreePath::from(&[ANOTHER_TEST_LEAF]),
                b"key3",
                BidirectionalReference {
                    forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                        ANOTHER_TEST_LEAF.to_vec(),
                        b"key2".to_vec(),
                    ]),
                    backward_reference_slot: 0,
                    cascade_on_update: false,
                    max_hop: None,
                    flags: None,
                },
                None,
            )
            .unwrap(),
            Ok(())
        ));

        // Try to add another one, this should fail as only one bidirectional reference
        // can point to single bidirectional reference
        assert!(matches!(
            process_bidirectional_reference_insertion(
                &mut cache,
                SubtreePath::from(&[ANOTHER_TEST_LEAF]),
                b"key4",
                BidirectionalReference {
                    forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                        ANOTHER_TEST_LEAF.to_vec(),
                        b"key2".to_vec(),
                    ]),
                    backward_reference_slot: 0,
                    cascade_on_update: false,
                    max_hop: None,
                    flags: None,
                },
                None,
            )
            .unwrap(),
            Err(Error::BidirectionalReferenceRule(..))
        ));
    }

    // Following tests will cover all insertion scenarios.
    // Roughly speaking there are three types of elements we care about:
    // 1. Bidirectional reference
    // 2. Items that support backward references
    // 3. The rest

    #[test]
    fn bidi_reference_insertion_no_overwrite() {
        // Expecting to add backward reference to the pointed to key

        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        let ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let ref_key = b"ref_key";

        let ref_path2 = SubtreePath::from(&[TEST_LEAF, b"innertree4"]);
        let ref_key2 = b"ref_key2";

        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path2.clone(),
            ref_key2,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let backward_reference1 = &get_backward_references(
            &mut merk_cache
                .get_merk(target_path.derive_owned())
                .unwrap()
                .unwrap(),
            target_key,
        )
        .unwrap()
        .unwrap()[0]
            .1;

        let backward_reference2 = &get_backward_references(
            &mut merk_cache
                .get_merk(target_path.derive_owned())
                .unwrap()
                .unwrap(),
            target_key,
        )
        .unwrap()
        .unwrap()[1]
            .1;

        // Backward references on target item points to bidirectional references that
        // were inserted
        assert_eq!(
            backward_reference1
                .inverted_reference
                .clone()
                .absolute_qualified_path(target_path.derive_owned(), target_key)
                .unwrap(),
            ref_path.derive_owned_with_child(ref_key)
        );

        assert_eq!(
            backward_reference2
                .inverted_reference
                .clone()
                .absolute_qualified_path(target_path.derive_owned(), target_key)
                .unwrap(),
            ref_path2.derive_owned_with_child(ref_key2)
        );
    }

    #[test]
    fn overwriting_bidi_reference_with_another_bidi_reference() {
        // Expecting to add new backward reference, remove the old one and value hash
        // propagation as well since it will target different item with different hash

        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        let target_path2 = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree3"]);
        let mut target_merk2 = merk_cache
            .get_merk(target_path2.derive_owned())
            .unwrap()
            .unwrap();
        let target_key2 = b"item_key2";

        let inserted_item2 =
            Element::new_item_allowing_bidirectional_references(b"item_value2".to_vec());

        target_merk2
            .for_merk(|m| inserted_item2.insert(m, target_key2, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk2,
            target_path2.derive_owned(),
            target_key2,
            Delta {
                new: Some(&inserted_item2),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        let ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let ref_key = b"ref_key";

        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let backward_reference = &get_backward_references(
            &mut merk_cache
                .get_merk(target_path.derive_owned())
                .unwrap()
                .unwrap(),
            target_key,
        )
        .unwrap()
        .unwrap()[0]
            .1;

        assert_eq!(
            backward_reference
                .inverted_reference
                .clone()
                .absolute_qualified_path(target_path.derive_owned(), target_key)
                .unwrap(),
            ref_path.derive_owned_with_child(ref_key)
        );

        let last_ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree4"]);
        let last_ref_key = b"last_ref_key";

        // Adding yet another bidirectional reference that points to the first one.
        // Keeping its value hash we can check if a change of the upstream reference
        // will affect the value hash of one in question
        process_bidirectional_reference_insertion(
            &merk_cache,
            last_ref_path.clone(),
            last_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    ref_path.derive_owned_with_child(ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let mut last_ref_merk = merk_cache
            .get_merk(last_ref_path.derive_owned())
            .unwrap()
            .unwrap();

        let prev_value_hash = last_ref_merk
            .for_merk(|m| Element::get_value_hash(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        // Overwrite bidirectional reference with another bidirectional reference:
        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path2.derive_owned_with_child(target_key2).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let post_value_hash = last_ref_merk
            .for_merk(|m| Element::get_value_hash(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        assert!(&get_backward_references(
            &mut merk_cache
                .get_merk(target_path.derive_owned())
                .unwrap()
                .unwrap(),
            target_key,
        )
        .unwrap()
        .unwrap()
        .is_empty());

        let backward_reference2 = &get_backward_references(
            &mut merk_cache
                .get_merk(target_path2.derive_owned())
                .unwrap()
                .unwrap(),
            target_key2,
        )
        .unwrap()
        .unwrap()[0]
            .1;

        assert_eq!(
            backward_reference2
                .inverted_reference
                .clone()
                .absolute_qualified_path(target_path2.derive_owned(), target_key2)
                .unwrap(),
            ref_path.derive_owned_with_child(ref_key)
        );

        assert_ne!(prev_value_hash, post_value_hash);
    }

    #[test]
    fn overwriting_bidi_reference_with_backward_reference_compatible_item() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        let ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let ref_key = b"ref_key";

        let last_ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree4"]);
        let last_ref_key = b"last_ref_key";

        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        // Insert yet another bidirectional reference that points to the previous one
        // and forms that way a chain of references
        process_bidirectional_reference_insertion(
            &merk_cache,
            last_ref_path.clone(),
            last_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    ref_path.derive_owned_with_child(ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let mut last_ref_merk = merk_cache
            .get_merk(last_ref_path.derive_owned())
            .unwrap()
            .unwrap();

        let prev_value_hash = last_ref_merk
            .for_merk(|m| Element::get_value_hash(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        // Overwrite first reference with item that supports backward references
        let mut ref_merk = merk_cache
            .get_merk(ref_path.derive_owned())
            .unwrap()
            .unwrap();

        let ref_overwrite_item =
            Element::new_item_allowing_bidirectional_references(b"newvalue".to_vec());
        let delta = ref_merk
            .for_merk(|m| ref_overwrite_item.insert_if_changed_value(m, ref_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            ref_merk,
            ref_path.derive_owned(),
            ref_key,
            delta,
        )
        .unwrap()
        .unwrap();

        let post_value_hash = last_ref_merk
            .for_merk(|m| Element::get_value_hash(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        // Latest reference in chain has its value hash updated
        assert_ne!(prev_value_hash, post_value_hash);

        // Old target has no backward references
        assert!(get_backward_references(
            &mut merk_cache
                .get_merk(target_path.derive_owned())
                .unwrap()
                .unwrap(),
            target_key,
        )
        .unwrap()
        .unwrap()
        .is_empty());
    }

    #[test]
    fn overwriting_bidi_reference_with_backward_reference_incompatible_item() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        let ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let ref_key = b"ref_key";

        let last_ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree4"]);
        let last_ref_key = b"last_ref_key";

        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        // Insert yet another bidirectional reference that points to the previous one
        // and forms that way a chain of references
        process_bidirectional_reference_insertion(
            &merk_cache,
            last_ref_path.clone(),
            last_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    ref_path.derive_owned_with_child(ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                // Note that it is set to true
                cascade_on_update: true,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let mut last_ref_merk = merk_cache
            .get_merk(last_ref_path.derive_owned())
            .unwrap()
            .unwrap();

        // Overwrite first reference with item that doesn't support backward references
        let mut ref_merk = merk_cache
            .get_merk(ref_path.derive_owned())
            .unwrap()
            .unwrap();

        let ref_overwrite_item = Element::new_item(b"newvalue".to_vec());
        let delta = ref_merk
            .for_merk(|m| ref_overwrite_item.insert_if_changed_value(m, ref_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            ref_merk,
            ref_path.derive_owned(),
            ref_key,
            delta,
        )
        .unwrap()
        .unwrap();

        let latest_ref_element = last_ref_merk
            .for_merk(|m| Element::get_optional(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        // Latest reference in chain has gone
        assert!(latest_ref_element.is_none());

        // Old target has no backward references
        assert!(get_backward_references(
            &mut merk_cache
                .get_merk(target_path.derive_owned())
                .unwrap()
                .unwrap(),
            target_key,
        )
        .unwrap()
        .unwrap()
        .is_empty());
    }

    #[test]
    fn overwriting_backward_reference_compatible_item_with_bidi_reference() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        // Attempt to overwrite item with bidi ref shall fail because it can't hold all
        // possible backward references
        assert!(process_bidirectional_reference_insertion(
            &merk_cache,
            target_path.clone(),
            target_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    b"literally".to_vec(),
                    b"anything".to_vec()
                ]),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .is_err());
    }

    #[test]
    fn overwriting_backward_reference_compatible_item_with_backward_reference_compatible_item() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        let ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let ref_key = b"ref_key";

        let next_ref_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree3"]);
        let next_ref_key = b"next_ref_key";

        let last_ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree4"]);
        let last_ref_key = b"last_ref_key";

        // Create bidi ref chain to the item to check propagation
        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        process_bidirectional_reference_insertion(
            &merk_cache,
            next_ref_path.clone(),
            next_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    ref_path.derive_owned_with_child(ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        process_bidirectional_reference_insertion(
            &merk_cache,
            last_ref_path.clone(),
            last_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    next_ref_path.derive_owned_with_child(next_ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: false,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let mut last_ref_merk = merk_cache
            .get_merk(last_ref_path.derive_owned())
            .unwrap()
            .unwrap();

        let prev_value_hash = last_ref_merk
            .for_merk(|m| Element::get_value_hash(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        // Update target item

        let inserted_item_new =
            Element::new_item_allowing_bidirectional_references(b"item_value2".to_vec());

        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let delta = target_merk
            .for_merk(|m| inserted_item_new.insert_if_changed_value(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            delta,
        )
        .unwrap()
        .unwrap();

        let post_value_hash = last_ref_merk
            .for_merk(|m| Element::get_value_hash(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap();

        assert_ne!(prev_value_hash, post_value_hash);
    }

    #[test]
    fn overwriting_backward_reference_compatible_item_with_backward_reference_incompatible_item() {
        let version = GroveVersion::latest();
        let db = make_deep_tree(&version);
        let tx = db.start_transaction();
        let merk_cache = MerkCache::new(&db, &tx, &version);

        let target_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree2"]);
        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let target_key = b"item_key";

        let inserted_item =
            Element::new_item_allowing_bidirectional_references(b"item_value".to_vec());

        target_merk
            .for_merk(|m| inserted_item.insert(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            Delta {
                new: Some(&inserted_item),
                old: None,
            },
        )
        .unwrap()
        .unwrap();

        let ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree"]);
        let ref_key = b"ref_key";

        let next_ref_path = SubtreePath::from(&[ANOTHER_TEST_LEAF, b"innertree3"]);
        let next_ref_key = b"next_ref_key";

        let last_ref_path = SubtreePath::from(&[TEST_LEAF, b"innertree4"]);
        let last_ref_key = b"last_ref_key";

        // Create bidi ref chain to the item to check propagation
        process_bidirectional_reference_insertion(
            &merk_cache,
            ref_path.clone(),
            ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    target_path.derive_owned_with_child(target_key).to_vec(),
                ),
                backward_reference_slot: 0,
                // Must be true to allow cascade deletion
                cascade_on_update: true,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        process_bidirectional_reference_insertion(
            &merk_cache,
            next_ref_path.clone(),
            next_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    ref_path.derive_owned_with_child(ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                // Must be true to allow cascade deletion
                cascade_on_update: true,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        process_bidirectional_reference_insertion(
            &merk_cache,
            last_ref_path.clone(),
            last_ref_key,
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(
                    next_ref_path.derive_owned_with_child(next_ref_key).to_vec(),
                ),
                backward_reference_slot: 0,
                cascade_on_update: true,
                max_hop: None,
                flags: None,
            },
            None,
        )
        .unwrap()
        .unwrap();

        let mut last_ref_merk = merk_cache
            .get_merk(last_ref_path.derive_owned())
            .unwrap()
            .unwrap();

        // Update target item

        let inserted_item_new = Element::new_item(b"item_value2".to_vec());

        let mut target_merk = merk_cache
            .get_merk(target_path.derive_owned())
            .unwrap()
            .unwrap();
        let delta = target_merk
            .for_merk(|m| inserted_item_new.insert_if_changed_value(m, target_key, None, &version))
            .unwrap()
            .unwrap();

        process_update_element_with_backward_references(
            &merk_cache,
            target_merk,
            target_path.derive_owned(),
            target_key,
            delta,
        )
        .unwrap()
        .unwrap();

        assert!(last_ref_merk
            .for_merk(|m| Element::get_optional(m, last_ref_key, true, &version))
            .unwrap()
            .unwrap()
            .is_none());

        assert!(merk_cache
            .get_merk(ref_path.derive_owned())
            .unwrap()
            .unwrap()
            .for_merk(|m| Element::get_optional(m, ref_key, true, &version))
            .unwrap()
            .unwrap()
            .is_none());
    }
}
