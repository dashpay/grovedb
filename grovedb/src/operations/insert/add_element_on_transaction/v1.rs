//! `add_element_on_transaction` — **v1** (current behaviour, `GROVE_V3`+).
//!
//! `CountSumTree` / `ProvableCountTree` / `ProvableCountSumTree` are inserted as
//! **layered subtrees** (`Op::PutLayeredReference`), the same way the batch
//! insert path writes them. This makes the parent node's `value_hash` =
//! `combine_hash(value_hash(serialized), child_root_hash)` and the storage cost
//! the fixed layered tree cost — i.e. the non-batch path agrees with the batch
//! path on both the root hash and the fee.
//!
//! This differs from [`super::v0`] (grovedb v4.1.0 / `GROVE_V1` / `GROVE_V2`)
//! ONLY in the match arm for those three element types, where v0 uses the
//! plain-value `Op::Put` path instead. See the module docs in
//! [`super`][`mod@super`] for the consensus rationale.

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
    /// `add_element_on_transaction` v1 — see the module documentation.
    pub(crate) fn add_element_on_transaction_v1<'db, B: AsRef<[u8]>>(
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
            // v1: all tree types — including `CountSumTree` / `ProvableCountTree`
            // / `ProvableCountSumTree` — are written as layered subtrees. This is
            // the behaviour selected by `GROVE_V3`+; for `GROVE_V1` / `GROVE_V2`
            // those three types take the plain-value `Op::Put` arm in
            // [`super::v0`] instead, to preserve the protocol-v11 consensus root.
            Element::Tree(value, _)
            | Element::SumTree(value, ..)
            | Element::BigSumTree(value, ..)
            | Element::CountTree(value, ..)
            | Element::CountSumTree(value, ..)
            | Element::ProvableCountTree(value, ..)
            | Element::ProvableCountSumTree(value, ..)
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
            Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
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
            Element::ProvableCountIndexedTree(primary, secondary, count_value, _) => {
                let (primary_root_hash, secondary_root_hash) =
                    if primary.is_none() && secondary.is_none() && *count_value == 0 {
                        // Empty cidx: both root keys absent AND count
                        // is zero. NULL_HASH for both Merks.
                        (NULL_HASH, NULL_HASH)
                    } else {
                        // Non-empty cidx: REQUIRE both root_keys to be
                        // Some(_) AND validate them against on-disk
                        // state. Reject partially-initialized claims
                        // explicitly:
                        //   - (None, None, count > 0): a cidx claiming
                        //     entries but with no roots — would persist
                        //     a count_value disconnected from any real
                        //     index content.
                        //   - (Some, None, _) / (None, Some, _): only
                        //     one of the two Merks claimed; would
                        //     persist asymmetric roots that fail H1-A
                        //     reconstruction.
                        if primary.is_none() || secondary.is_none() {
                            return Err(Error::InvalidInput(
                                "CountIndexedTree direct insertion: non-empty cidx must \
                                 have BOTH primary_root_key and secondary_root_key set \
                                 to Some(_); partial state (one None, one Some, or \
                                 count>0 with no roots) is not permitted",
                            ))
                            .wrap_with_cost(cost);
                        }
                        // Both roots are Some(_); open and verify they
                        // match the on-disk state. Mismatch ⇒ the
                        // element bytes would diverge from on-disk
                        // state; refuse rather than persist an
                        // inconsistent root_hash chain.
                        let child_path_owned = path.derive_owned_with_child(key.to_vec());
                        let child_path = SubtreePath::from(&child_path_owned);
                        let primary_merk = cost_return_on_error!(
                            &mut cost,
                            self.open_transactional_merk_at_path(
                                child_path.clone(),
                                transaction,
                                Some(batch),
                                grove_version,
                            )
                        );
                        let (p_hash, p_root_key, p_aggregate) = cost_return_on_error!(
                            &mut cost,
                            primary_merk
                                .root_hash_key_and_aggregate_data()
                                .map_err(Error::MerkError)
                        );
                        if &p_root_key != primary {
                            return Err(Error::InvalidInput(
                                "CountIndexedTree direct insertion: provided \
                                 primary_root_key does not match the existing \
                                 primary Merk's root key",
                            ))
                            .wrap_with_cost(cost);
                        }
                        // Also bind the claimed count_value to the primary's
                        // actual aggregate. Without this, a caller could
                        // supply correct root keys but a forged count that
                        // then gets hash-committed and propagated into
                        // ancestor aggregates.
                        if p_aggregate.as_count_u64() != *count_value {
                            return Err(Error::InvalidInput(
                                "CountIndexedTree direct insertion: provided \
                                 count_value does not match the existing \
                                 primary Merk's aggregate count",
                            ))
                            .wrap_with_cost(cost);
                        }
                        let secondary_merk = cost_return_on_error!(
                            &mut cost,
                            self.open_count_indexed_secondary_at_path(
                                child_path,
                                secondary.clone(),
                                transaction,
                                Some(batch),
                                grove_version,
                            )
                        );
                        let (s_hash, s_root_key, _) = cost_return_on_error!(
                            &mut cost,
                            secondary_merk
                                .root_hash_key_and_aggregate_data()
                                .map_err(Error::MerkError)
                        );
                        if &s_root_key != secondary {
                            return Err(Error::InvalidInput(
                                "CountIndexedTree direct insertion: provided \
                                 secondary_root_key does not match the existing \
                                 secondary Merk's root key",
                            ))
                            .wrap_with_cost(cost);
                        }
                        (p_hash, s_hash)
                    };
                cost_return_on_error_into!(
                    &mut cost,
                    element.insert_count_indexed_subtree(
                        &mut subtree_to_insert_into,
                        key,
                        primary_root_hash,
                        secondary_root_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    )
                );
            }
            Element::ProvableSumIndexedTree(primary, secondary, sum_value, _) => {
                let (primary_root_hash, secondary_root_hash) =
                    if primary.is_none() && secondary.is_none() && *sum_value == 0 {
                        (NULL_HASH, NULL_HASH)
                    } else {
                        if primary.is_none() || secondary.is_none() {
                            return Err(Error::InvalidInput(
                                "ProvableSumIndexedTree direct insertion: non-empty PSIT must \
                                 have BOTH primary_root_key and secondary_root_key set to \
                                 Some(_); partial state is not permitted",
                            ))
                            .wrap_with_cost(cost);
                        }
                        let child_path_owned = path.derive_owned_with_child(key.to_vec());
                        let child_path = SubtreePath::from(&child_path_owned);
                        let primary_merk = cost_return_on_error!(
                            &mut cost,
                            self.open_transactional_merk_at_path(
                                child_path.clone(),
                                transaction,
                                Some(batch),
                                grove_version,
                            )
                        );
                        let (p_hash, p_root_key, p_aggregate) = cost_return_on_error!(
                            &mut cost,
                            primary_merk
                                .root_hash_key_and_aggregate_data()
                                .map_err(Error::MerkError)
                        );
                        if &p_root_key != primary {
                            return Err(Error::InvalidInput(
                                "ProvableSumIndexedTree direct insertion: provided \
                                 primary_root_key does not match the existing primary Merk's \
                                 root key",
                            ))
                            .wrap_with_cost(cost);
                        }
                        // Bind the claimed sum_value to the primary's actual
                        // aggregate so a forged sum cannot be hash-committed
                        // and propagated into ancestor aggregates.
                        if p_aggregate.as_sum_i64() != *sum_value {
                            return Err(Error::InvalidInput(
                                "ProvableSumIndexedTree direct insertion: provided \
                                 sum_value does not match the existing primary Merk's \
                                 aggregate sum",
                            ))
                            .wrap_with_cost(cost);
                        }
                        let secondary_merk = cost_return_on_error!(
                            &mut cost,
                            self.open_indexed_secondary_at_path(
                                child_path,
                                grovedb_element::indexed::IndexAxis::Sum,
                                secondary.clone(),
                                transaction,
                                Some(batch),
                                grove_version,
                            )
                        );
                        let (s_hash, s_root_key, _) = cost_return_on_error!(
                            &mut cost,
                            secondary_merk
                                .root_hash_key_and_aggregate_data()
                                .map_err(Error::MerkError)
                        );
                        if &s_root_key != secondary {
                            return Err(Error::InvalidInput(
                                "ProvableSumIndexedTree direct insertion: provided \
                                 secondary_root_key does not match the existing secondary \
                                 Merk's root key",
                            ))
                            .wrap_with_cost(cost);
                        }
                        (p_hash, s_hash)
                    };
                cost_return_on_error_into!(
                    &mut cost,
                    element.insert_count_indexed_subtree(
                        &mut subtree_to_insert_into,
                        key,
                        primary_root_hash,
                        secondary_root_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    )
                );
            }
            Element::ProvableCountProvableSumIndexedTree(
                primary,
                count_value,
                sum_value,
                axes,
                _,
            ) => {
                // Validate the axes TLV is canonical (sorted by tag, no
                // duplicates, 1..=3 entries, known tags) for BOTH the
                // empty and non-empty cases. The `Element` enum is public
                // and `axes_digest` does not validate, so without this an
                // empty PCPSIT with invalid/duplicate/unsorted axes could
                // be persisted (its digest would be computed over the
                // malformed TLV). Reuses the same check the constructors
                // run.
                cost_return_on_error_no_add!(
                    cost,
                    Element::validate_pcpsit_axes(axes).map_err(|_| Error::InvalidInput(
                        "ProvableCountProvableSumIndexedTree direct insertion: axes must be \
                         canonical (1..=3 entries, sorted ascending by tag, no duplicates, \
                         tags in 0..=2)"
                    ))
                );
                let axes_all_empty = axes.iter().all(|(_, sk)| sk.is_none());
                let (primary_root_hash, second_hash) = if primary.is_none()
                    && axes_all_empty
                    && *count_value == 0
                    && *sum_value == 0
                {
                    // Every axis slot is NULL_HASH for the empty case.
                    let zero_axes: Vec<(u8, grovedb_merk::CryptoHash)> =
                        axes.iter().map(|(t, _)| (*t, NULL_HASH)).collect();
                    let digest =
                        grovedb_merk::tree::axes_digest(&zero_axes).unwrap_add_cost(&mut cost);
                    (NULL_HASH, digest)
                } else {
                    if primary.is_none() {
                        return Err(Error::InvalidInput(
                            "ProvableCountProvableSumIndexedTree direct insertion: non-empty \
                             PCPSIT must have primary_root_key = Some(_); partial state is not \
                             permitted",
                        ))
                        .wrap_with_cost(cost);
                    }
                    // (Axes canonical form already validated above.)
                    let child_path_owned = path.derive_owned_with_child(key.to_vec());
                    let child_path = SubtreePath::from(&child_path_owned);
                    let primary_merk = cost_return_on_error!(
                        &mut cost,
                        self.open_transactional_merk_at_path(
                            child_path.clone(),
                            transaction,
                            Some(batch),
                            grove_version,
                        )
                    );
                    let (p_hash, p_root_key, p_aggregate) = cost_return_on_error!(
                        &mut cost,
                        primary_merk
                            .root_hash_key_and_aggregate_data()
                            .map_err(Error::MerkError)
                    );
                    if &p_root_key != primary {
                        return Err(Error::InvalidInput(
                            "ProvableCountProvableSumIndexedTree direct insertion: provided \
                             primary_root_key does not match the existing primary Merk's root \
                             key",
                        ))
                        .wrap_with_cost(cost);
                    }
                    // Bind BOTH the claimed count_value and sum_value to the
                    // primary's actual aggregate so forged count/sum cannot be
                    // hash-committed and propagated into ancestor aggregates.
                    if p_aggregate.as_count_u64() != *count_value {
                        return Err(Error::InvalidInput(
                            "ProvableCountProvableSumIndexedTree direct insertion: provided \
                             count_value does not match the existing primary Merk's aggregate \
                             count",
                        ))
                        .wrap_with_cost(cost);
                    }
                    if p_aggregate.as_sum_i64() != *sum_value {
                        return Err(Error::InvalidInput(
                            "ProvableCountProvableSumIndexedTree direct insertion: provided \
                             sum_value does not match the existing primary Merk's aggregate \
                             sum",
                        ))
                        .wrap_with_cost(cost);
                    }
                    // For each axis, open + verify and collect its root hash.
                    let mut axis_hashes: Vec<(u8, grovedb_merk::CryptoHash)> =
                        Vec::with_capacity(axes.len());
                    for (tag, sec_root_key) in axes.iter() {
                        let axis = cost_return_on_error_no_add!(
                            cost,
                            grovedb_element::indexed::IndexAxis::try_from_tag(*tag).map_err(|e| {
                                Error::CorruptedData(format!("invalid axis tag in PCPSIT: {e}"))
                            })
                        );
                        let secondary_merk = cost_return_on_error!(
                            &mut cost,
                            self.open_indexed_secondary_at_path(
                                child_path.clone(),
                                axis,
                                sec_root_key.clone(),
                                transaction,
                                Some(batch),
                                grove_version,
                            )
                        );
                        let (s_hash, s_root_key, _) = cost_return_on_error!(
                            &mut cost,
                            secondary_merk
                                .root_hash_key_and_aggregate_data()
                                .map_err(Error::MerkError)
                        );
                        if &s_root_key != sec_root_key {
                            return Err(Error::InvalidInput(
                                "ProvableCountProvableSumIndexedTree direct insertion: provided \
                                 axis secondary_root_key does not match the existing secondary \
                                 Merk's root key",
                            ))
                            .wrap_with_cost(cost);
                        }
                        axis_hashes.push((*tag, s_hash));
                    }
                    let digest =
                        grovedb_merk::tree::axes_digest(&axis_hashes).unwrap_add_cost(&mut cost);
                    (p_hash, digest)
                };
                cost_return_on_error_into!(
                    &mut cost,
                    element.insert_count_indexed_subtree(
                        &mut subtree_to_insert_into,
                        key,
                        primary_root_hash,
                        second_hash,
                        Some(options.as_merk_options()),
                        grove_version,
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
