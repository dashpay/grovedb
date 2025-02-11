use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::tree::NULL_HASH;
use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
use grovedb_version::{dispatch_version, version::GroveVersion};

use super::InsertOptions;
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
    dispatch_version!(
        "insert_on_transaction",
        grove_version
            .grovedb_versions
            .operations
            .insert
            .insert_on_transaction,
        1 => {}
    );

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
        Element::Reference(ref reference_path, ..) => {
            let resolved_reference = cost_return_on_error!(
                &mut cost,
                follow_reference(&cache, path.derive_owned(), key, reference_path.clone())
            );
            let referenced_item: Element = resolved_reference.target_element;

            if matches!(
                referenced_item,
                Element::Tree(_, _) | Element::SumTree(_, _, _)
            ) {
                return Err(Error::NotSupported(
                    "References cannot point to subtrees".to_owned(),
                ))
                .wrap_with_cost(cost);
            }

            if options.propagate_backward_references {
                let delta = cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert_reference_if_changed_value(
                        m,
                        key,
                        resolved_reference.target_node_value_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    ))
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
            } else {
                cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert_reference(
                        m,
                        key,
                        resolved_reference.target_node_value_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    ))
                );
            }
        }
        Element::Tree(ref value, _)
        | Element::SumTree(ref value, ..)
        | Element::BigSumTree(ref value, ..)
        | Element::CountTree(ref value, ..) => {
            if value.is_some() {
                return Err(Error::InvalidCodeExecution(
                    "a tree should be empty at the moment of insertion when not using batches",
                ))
                .wrap_with_cost(cost);
            } else {
                if options.propagate_backward_references {
                    let delta = cost_return_on_error!(
                        &mut cost,
                        subtree_to_insert_into.for_merk(|m| element.insert_subtree_if_changed(
                            m,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version
                        ))
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
                } else {
                    cost_return_on_error!(
                        &mut cost,
                        subtree_to_insert_into.for_merk(|m| element.insert_subtree(
                            m,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version
                        ))
                    );
                }
            }
        }

        Element::BidirectionalReference(reference) => {
            cost_return_on_error!(
                &mut cost,
                process_bidirectional_reference_insertion(
                    &cache,
                    path,
                    key,
                    reference,
                    Some(options)
                )
            );
        }
        _ => {
            if options.propagate_backward_references {
                let delta = cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert_if_changed_value(
                        m,
                        key,
                        Some(options.as_merk_options()),
                        grove_version
                    ))
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
            } else {
                cost_return_on_error!(
                    &mut cost,
                    subtree_to_insert_into.for_merk(|m| element.insert(
                        m,
                        key,
                        Some(options.as_merk_options()),
                        grove_version
                    ))
                );
            }
        }
    }

    let result_batch = cost_return_on_error!(&mut cost, cache.into_batch());

    batch.merge(*result_batch);

    Ok(()).wrap_with_cost(cost)
}
