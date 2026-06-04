//! `add_element_on_transaction` — **v0** (grovedb v4.1.0, `GROVE_V1` / `GROVE_V2`).
//!
//! CONSENSUS-CRITICAL. `CountSumTree` / `ProvableCountTree` /
//! `ProvableCountSumTree` are inserted via the **plain-value path** (`Op::Put`,
//! the `Item` arm below), NOT as layered subtrees. grovedb <= v4.1.0 routed
//! them through `_ => element.insert()` (`Op::Put`), and that is the behaviour
//! frozen into the live protocol-v11 activation chain — e.g. testnet block
//! 245,344's `transition_to_version_11`, which inserts an
//! `empty_provable_count_sum_tree` (CLEAR_ADDRESS_POOL) and an
//! `empty_count_sum_tree` (ADDRESS_BALANCES) through this non-batch path.
//!
//! Routing those three types through the layered-subtree arm (as [`super::v1`]
//! does for `GROVE_V3`+) changes the parent node's `value_hash` from
//! `value_hash(serialized)` to `combine_hash(value_hash(serialized), NULL_HASH)`,
//! which changes the grovedb root and breaks consensus when v11 is replayed.
//!
//! v0 therefore differs from [`super::v1`] ONLY in the placement of those three
//! match arms: here they join the `Op::Put` arm; there they join the layered
//! arm. Everything else is identical. (The v12-only `ProvableSumTree` /
//! `ProvableCountProvableSumTree` keep the layered behaviour in both — they were
//! never created via this path on the v11 chain.)

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_element::reference_path::path_from_reference_path_type;
use grovedb_merk::{
    element::{costs::ElementCostExtensions, insert::ElementInsertToStorageExtensions, ElementExt},
    tree::NULL_HASH,
    Merk,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch};
use grovedb_version::version::GroveVersion;

use super::super::InsertOptions;
use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// `add_element_on_transaction` v0 — see the module documentation.
    pub(crate) fn add_element_on_transaction_v0<'db, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        let mut cost = OperationCost::default();

        let mut subtree_to_insert_into = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        // if we don't allow a tree override then we should check

        if options.checks_for_override() {
            let maybe_element_bytes = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into
                    .get(
                        key,
                        true,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| Error::CorruptedData(e.to_string()))
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
                        Element::deserialize(element_bytes.as_slice(), grove_version).map_err(
                            |_| {
                                Error::CorruptedData(String::from("unable to deserialize element"))
                            }
                        )
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

        // Dispatch via the underlying element so a NonCounted wrapper takes
        // the same path as its inner element. The actual `element.insert*`
        // calls operate on the outer wrapper, which is what we want — the
        // serialized wrapper bytes go to storage.
        match element.underlying() {
            // `ReferenceWithSumItem` shares the reference resolution + proof
            // shape with `Reference`. The merk feature_type derived from the
            // element's `sum_value_or_default()` already routes the sum into
            // any sum-bearing parent; the call site is otherwise identical.
            Element::Reference(reference_path, ..)
            | Element::ReferenceWithSumItem(reference_path, ..) => {
                let path = path.to_vec(); // TODO: need for support for references in path library
                let reference_path = cost_return_on_error_into!(
                    &mut cost,
                    path_from_reference_path_type(reference_path.clone(), &path, Some(key))
                        .wrap_with_cost(OperationCost::default())
                );

                let referenced_item = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(
                        reference_path.as_slice().into(),
                        false,
                        Some(transaction),
                        grove_version
                    )
                );

                let referenced_element_value_hash = cost_return_on_error_into!(
                    &mut cost,
                    referenced_item.value_hash(grove_version)
                );

                cost_return_on_error_into!(
                    &mut cost,
                    element.insert_reference(
                        &mut subtree_to_insert_into,
                        key,
                        referenced_element_value_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    )
                );
            }
            // CONSENSUS-CRITICAL: `CountSumTree` / `ProvableCountTree` /
            // `ProvableCountSumTree` are NOT in this layered arm in v0 — they
            // take the `Op::Put` arm below to match grovedb v4.1.0 / the live
            // protocol-v11 root. See the module docs.
            Element::Tree(value, _)
            | Element::SumTree(value, ..)
            | Element::BigSumTree(value, ..)
            | Element::CountTree(value, ..)
            | Element::ProvableSumTree(value, ..)
            | Element::ProvableCountProvableSumTree(value, ..) => {
                if value.is_some() {
                    return Err(Error::InvalidCodeExecution(
                        "a tree should be empty at the moment of insertion when not using batches",
                    ))
                    .wrap_with_cost(cost);
                } else {
                    cost_return_on_error_into!(
                        &mut cost,
                        element.insert_subtree(
                            &mut subtree_to_insert_into,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version
                        )
                    );
                }
            }
            // CommitmentTree uses BulkAppendTree internally; the initial child
            // hash must include the empty sinsemilla root so V1 proof
            // verification works even before the first append.
            Element::CommitmentTree(..) => {
                cost_return_on_error_into!(
                    &mut cost,
                    element.insert_subtree(
                        &mut subtree_to_insert_into,
                        key,
                        grovedb_commitment_tree::EMPTY_COMMITMENT_TREE_STATE_ROOT,
                        Some(options.as_merk_options()),
                        grove_version
                    )
                );
            }
            // MmrTree, BulkAppendTree, DenseAppendOnlyFixedSizeTree: initial
            // insert uses NULL_HASH since these trees start empty.
            Element::MmrTree(..)
            | Element::BulkAppendTree(..)
            | Element::DenseAppendOnlyFixedSizeTree(..) => {
                cost_return_on_error_into!(
                    &mut cost,
                    element.insert_subtree(
                        &mut subtree_to_insert_into,
                        key,
                        NULL_HASH,
                        Some(options.as_merk_options()),
                        grove_version
                    )
                );
            }
            // `CountSumTree` / `ProvableCountTree` / `ProvableCountSumTree` are
            // inserted via `Op::Put` here (NOT the layered-subtree arm above) to
            // preserve the grovedb v4.1.0 / protocol-v11 consensus root — see the
            // module docs.
            Element::Item(..)
            | Element::SumItem(..)
            | Element::ItemWithSumItem(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountTree(..)
            | Element::ProvableCountSumTree(..) => {
                cost_return_on_error_into!(
                    &mut cost,
                    element.insert(
                        &mut subtree_to_insert_into,
                        key,
                        Some(options.as_merk_options()),
                        grove_version
                    )
                );
            }
            // `underlying()` only unwraps one level; nested wrappers are
            // forbidden by the constructor and (de)serializer, but the public
            // insert path can still receive a hand-built nested wrapper —
            // return a typed error rather than panic.
            Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_) => {
                return Err(Error::InvalidInput(
                    "nested element wrappers are not allowed",
                ))
                .wrap_with_cost(cost);
            }
        }

        Ok(subtree_to_insert_into).wrap_with_cost(cost)
    }
}
