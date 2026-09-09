//! Automatic backward-reference maintenance on GROVE_V4.
//!
//! Prepare the mutated node through the old-value observer and retain its
//! traversal in the same Merk used by the write. Reference maintenance and
//! parent propagation share that cache until the atomic batch is complete.

use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
use std::collections::HashMap;

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::element::{costs::ElementCostExtensions, insert::Delta};
use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
use grovedb_version::version::GroveVersion;

use super::super::InsertOptions;
use crate::{
    bidirectional_references::{
        basic_sectioned_removal, check_carried_referrers_fit,
        process_bidirectional_reference_insertion, process_update_element_with_backward_references,
    },
    merk_cache::MerkCache,
    Element, Error, GroveDb, Transaction,
};

pub(super) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<'b, B>,
    key: &[u8],
    mut element: Element,
    options: InsertOptions,
    transaction: &'db Transaction,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    if element.is_wrapped() && element.underlying().supports_backward_references() {
        return Err(Error::InvalidInput(
            "backward-references elements cannot be wrapped",
        ))
        .wrap_with_cost(Default::default());
    }
    // A new bidirectional reference must register its target even under Skip.
    if !options.backward_references_policy.maintains()
        && !matches!(element, Element::BidirectionalReference(..))
    {
        return super::v0::insert_on_transaction_body(
            db,
            path,
            key,
            element,
            options,
            transaction,
            batch,
            grove_version,
        );
    }
    if grove_version
        .grovedb_versions
        .operations
        .insert
        .add_element_on_transaction
        != 2
    {
        return super::v0::insert_on_transaction_body(
            db,
            path,
            key,
            element,
            options,
            transaction,
            batch,
            grove_version,
        );
    }
    let mut cost = Default::default();
    let mut merk = cost_return_on_error!(
        &mut cost,
        db.open_transactional_merk_at_path(path.clone(), transaction, Some(batch), grove_version)
    );
    let previous = cost_return_on_error!(
        &mut cost,
        (|| {
            let mut cost = Default::default();
            cost_return_on_error_no_add!(
                cost,
                crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                    merk.tree_type,
                    "insert",
                )
            );
            let mut previous = None;
            cost_return_on_error!(
                &mut cost,
                merk.observe_old_value(
                    key,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    &mut |_, bytes| {
                        previous = Some(Element::deserialize(bytes, grove_version));
                    },
                    grove_version,
                )
                .map_err(Error::MerkError)
            );
            previous
                .transpose()
                .map_err(Error::from)
                .wrap_with_cost(cost)
        })()
    );
    if previous.is_some() && options.validate_insertion_does_not_override {
        return Err(Error::OverrideNotAllowed(
            "insertion not allowed to override",
        ))
        .wrap_with_cost(cost);
    }
    if previous.as_ref().is_some_and(Element::is_any_tree)
        && options.validate_insertion_does_not_override_tree
    {
        return Err(Error::OverrideNotAllowed(
            "insertion not allowed to override tree",
        ))
        .wrap_with_cost(cost);
    }
    if previous.as_ref().is_some_and(|old| {
        old.is_any_tree()
            && !old.uses_non_merk_data_storage()
            && old
                .root_key_and_tree_type()
                .is_some_and(|(root, _)| root.is_some())
    }) && !cost_return_on_error!(
        &mut cost,
        db.backward_reference_participants(
            &path.derive_owned_with_child(key).to_vec(),
            transaction,
            grove_version,
        )
    )
    .is_empty()
    {
        return Err(Error::NotSupported(
            "delete a subtree containing backward-reference participants before replacing it"
                .to_owned(),
        ))
        .wrap_with_cost(cost);
    }
    let needs_maintenance = element.supports_backward_references()
        || previous
            .as_ref()
            .is_some_and(Element::supports_backward_references);
    if !needs_maintenance {
        cost_return_on_error!(
            &mut cost,
            db.add_element_to_cached_merk_v2(
                &mut merk,
                path.clone(),
                key,
                element,
                options,
                transaction,
                batch,
                grove_version,
            )
        );
        let mut merks = HashMap::new();
        merks.insert(path.clone(), merk);
        cost_return_on_error!(
            &mut cost,
            db.propagate_changes_with_transaction(merks, path, transaction, batch, grove_version,)
        );
        return Ok(()).wrap_with_cost(cost);
    }
    let cache = MerkCache::with_prepared_merk(
        db,
        transaction,
        grove_version,
        path.derive_owned(),
        merk,
        batch,
    );
    let mut handle = cost_return_on_error!(&mut cost, cache.get_merk(path.derive_owned()));

    if let Element::BidirectionalReference(reference, flags) = element {
        cost_return_on_error!(
            &mut cost,
            process_bidirectional_reference_insertion(
                &cache,
                path,
                key,
                reference,
                flags,
                Some(options),
            )
        );
    } else {
        if let Some(refs) = element.backward_references_mut() {
            *refs = previous
                .as_ref()
                .and_then(Element::backward_references)
                .map(<[_]>::to_vec)
                .unwrap_or_default();
            cost_return_on_error_no_add!(cost, check_carried_referrers_fit(&element));
        }
        let unchanged_family =
            element.supports_backward_references() && previous.as_ref() == Some(&element);
        if !unchanged_family {
            cost_return_on_error!(
                &mut cost,
                handle.for_merk(|merk| db.add_element_to_cached_merk_v2(
                    merk,
                    path.clone(),
                    key,
                    element.clone(),
                    options,
                    transaction,
                    batch,
                    grove_version,
                ))
            );
            cost_return_on_error!(
                &mut cost,
                process_update_element_with_backward_references(
                    &cache,
                    handle,
                    path.derive_owned(),
                    key,
                    Delta {
                        new: Some(&element),
                        old: previous
                    },
                    &mut basic_sectioned_removal(),
                )
            );
        }
    }
    cost_return_on_error!(&mut cost, cache.finish_prepared());
    Ok(()).wrap_with_cost(cost)
}
