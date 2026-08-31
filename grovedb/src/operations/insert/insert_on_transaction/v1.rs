//! `insert_on_transaction` — **v1** (`GROVE_V4`+).
//!
//! A behaviour-preserving router. A call that neither inserts a
//! [`Element::BidirectionalReference`] nor sets
//! [`InsertOptions::propagate_backward_references`] runs the exact shipped
//! flow ([`super::v0::insert_on_transaction_body`]) — identical root hashes
//! and costs. The rest run through the `MerkCache`-based flow below, which
//! keeps every touched subtree open in one cache so backward-reference
//! bookkeeping, chain propagation, and ordinary parent propagation all see
//! each other's uncommitted writes (see `adr/bidirectional_references.md`
//! and `adr/merk_cache.md`).
//!
//! ## Element support under the backward-references flow
//!
//! The `MerkCache` flow supports items, references (all three variants),
//! and empty plain-Merk trees. Specialized tree types (commitment / MMR /
//! bulk-append / dense / private document store / indexed trees) and the
//! aggregation wrappers are rejected when `propagate_backward_references`
//! is set — their child-hash conventions live in
//! `add_element_on_transaction` and have no backward-references semantics
//! yet. Insert them without the flag (they cannot be targeted by
//! bidirectional references anyway).

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions,
    },
    tree::NULL_HASH,
};
use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
use grovedb_version::version::GroveVersion;

use super::super::InsertOptions;
use crate::{
    bidirectional_references::{
        process_bidirectional_reference_insertion, process_update_element_with_backward_references,
    },
    merk_cache::MerkCache,
    reference_path::follow_reference,
    Element, Error, GroveDb, Transaction,
};

pub(super) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &'db Transaction,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    // A bidirectional reference must always register itself in its target's
    // meta storage, flag or no flag; everything else opts into the
    // backward-references flow via the flag.
    if matches!(element, Element::BidirectionalReference(..))
        || options.propagate_backward_references
    {
        insert_with_backward_references(
            db,
            path,
            key,
            element,
            options,
            transaction,
            batch,
            grove_version,
        )
    } else {
        super::v0::insert_on_transaction_body(
            db,
            path,
            key,
            element,
            options,
            transaction,
            batch,
            grove_version,
        )
    }
}

/// The `MerkCache`-based insert flow with backward-references bookkeeping.
fn insert_with_backward_references<'db, 'b, B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &'db Transaction,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    let cache = MerkCache::new(db, transaction, grove_version);

    let mut subtree_to_insert_into =
        cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned()));

    if options.checks_for_override() {
        let maybe_element_bytes = cost_return_on_error!(
            &mut cost,
            subtree_to_insert_into.for_merk(|m| m
                .get(
                    key,
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(e.to_string())))
        );
        if let Some(element_bytes) = maybe_element_bytes {
            if options.validate_insertion_does_not_override {
                return Err(Error::OverrideNotAllowed(
                    "insertion not allowed to override",
                ))
                .wrap_with_cost(cost);
            }
            if options.validate_insertion_does_not_override_tree {
                let element = cost_return_on_error_no_add!(
                    cost,
                    Element::deserialize(element_bytes.as_slice(), grove_version).map_err(|_| {
                        Error::CorruptedData(String::from("unable to deserialize element"))
                    })
                );
                if element.is_any_tree() {
                    return Err(Error::OverrideNotAllowed(
                        "insertion not allowed to override tree",
                    ))
                    .wrap_with_cost(cost);
                }
            }
        }
    }

    match element {
        Element::BidirectionalReference(reference, flags) => {
            cost_return_on_error!(
                &mut cost,
                process_bidirectional_reference_insertion(
                    &cache,
                    path,
                    key,
                    reference,
                    flags,
                    Some(options)
                )
            );
        }
        Element::Reference(ref reference_path, ..)
        | Element::ReferenceWithSumItem(ref reference_path, ..) => {
            let resolved_reference = cost_return_on_error!(
                &mut cost,
                follow_reference(&cache, path.derive_owned(), key, reference_path.clone())
            );
            let referenced_item: Element = resolved_reference.target_element;

            if referenced_item.is_any_tree() {
                return Err(Error::NotSupported(
                    "References cannot point to subtrees".to_owned(),
                ))
                .wrap_with_cost(cost);
            }

            let delta = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into.for_merk(|m| {
                    element
                        .insert_reference_if_changed_value(
                            m,
                            key,
                            resolved_reference.target_node_value_hash,
                            Some(options.as_merk_options()),
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                })
            );

            cost_return_on_error!(
                &mut cost,
                process_update_element_with_backward_references(
                    &cache,
                    subtree_to_insert_into.clone(),
                    path.derive_owned(),
                    key,
                    delta
                )
            );
        }
        Element::Tree(ref root_key, _)
        | Element::SumTree(ref root_key, ..)
        | Element::BigSumTree(ref root_key, ..)
        | Element::CountTree(ref root_key, ..)
        | Element::CountSumTree(ref root_key, ..)
        | Element::ProvableCountTree(ref root_key, ..)
        | Element::ProvableCountSumTree(ref root_key, ..)
        | Element::ProvableSumTree(ref root_key, ..)
        | Element::ProvableCountProvableSumTree(ref root_key, ..) => {
            if root_key.is_some() {
                return Err(Error::InvalidCodeExecution(
                    "a tree should be empty at the moment of insertion when not using batches",
                ))
                .wrap_with_cost(cost);
            }
            let delta = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into.for_merk(|m| {
                    element
                        .insert_subtree_if_changed(
                            m,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                })
            );

            cost_return_on_error!(
                &mut cost,
                process_update_element_with_backward_references(
                    &cache,
                    subtree_to_insert_into.clone(),
                    path.derive_owned(),
                    key,
                    delta
                )
            );
        }
        Element::CommitmentTree(..)
        | Element::MmrTree(..)
        | Element::BulkAppendTree(..)
        | Element::DenseAppendOnlyFixedSizeTree(..)
        | Element::PrivateDocumentStore(..)
        | Element::ProvableSumIndexedTree(..)
        | Element::ProvableCountIndexedTree(..)
        | Element::ProvableCountProvableSumIndexedTree(..)
        | Element::NonCounted(..)
        | Element::NotSummed(..)
        | Element::NotCountedOrSummed(..) => {
            // These carry child-hash conventions or wrapper semantics that
            // the MerkCache flow does not model; and none of them can be
            // targeted by bidirectional references. Insert them without the
            // flag.
            return Err(Error::NotSupported(
                "this element type cannot be inserted with propagate_backward_references set; \
                 insert it without the flag"
                    .to_owned(),
            ))
            .wrap_with_cost(cost);
        }
        Element::Item(..)
        | Element::SumItem(..)
        | Element::ItemWithSumItem(..)
        | Element::ItemWithBackwardsReferences(..)
        | Element::SumItemWithBackwardsReferences(..)
        | Element::ItemWithSumItemWithBackwardsReferences(..) => {
            // A backward-references element's referrer list is bookkeeping
            // this flow maintains: carry the stored list over onto the new
            // element so an update never silently drops registrations (and
            // so the changed/unchanged comparison reflects the LOGICAL
            // value, the referrer lists being equal on both sides).
            let mut element = element;
            if element.supports_backward_references() {
                let previous = cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| {
                        Element::get_optional(m, key, true, grove_version).map_err(Error::MerkError)
                    })
                );
                if let Some(refs) = element.backward_references_mut() {
                    // The stored list is authoritative; whatever the caller
                    // supplied is not theirs to claim — forged entries would
                    // later let cascades and propagations follow arbitrary
                    // inverted paths.
                    *refs = previous
                        .as_ref()
                        .and_then(|p| p.backward_references())
                        .map(|p| p.to_vec())
                        .unwrap_or_default();
                }
            }
            let delta = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into.for_merk(|m| {
                    element
                        .insert_if_changed_value(
                            m,
                            key,
                            Some(options.as_merk_options()),
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                })
            );
            cost_return_on_error!(
                &mut cost,
                process_update_element_with_backward_references(
                    &cache,
                    subtree_to_insert_into.clone(),
                    path.derive_owned(),
                    key,
                    delta
                )
            );
        }
    }

    let result_batch = cost_return_on_error!(&mut cost, cache.into_batch());

    batch.merge(*result_batch);

    Ok(()).wrap_with_cost(cost)
}
