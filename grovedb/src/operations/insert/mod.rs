//! Insert operations

use std::{collections::HashMap, option::Option::None};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_element::reference_path::path_from_reference_path_type;
use grovedb_merk::{
    element::{costs::ElementCostExtensions, insert::ElementInsertToStorageExtensions, ElementExt},
    tree::NULL_HASH,
    Merk, MerkOptions,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch};
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{util::TxRef, Element, Error, GroveDb, Transaction, TransactionArg};

#[derive(Clone)]
/// Insert options
pub struct InsertOptions {
    /// Validate insertion does not override
    pub validate_insertion_does_not_override: bool,
    /// Validate insertion does not override tree
    pub validate_insertion_does_not_override_tree: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
}

impl Default for InsertOptions {
    fn default() -> Self {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: true,
            base_root_storage_is_free: true,
        }
    }
}

impl InsertOptions {
    fn checks_for_override(&self) -> bool {
        self.validate_insertion_does_not_override_tree || self.validate_insertion_does_not_override
    }

    fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}

/// Maximum key length in bytes. Merk link encoding stores the key length as a
/// single `u8`, so keys longer than 255 bytes would corrupt the encoding.
const MAX_KEY_LENGTH: usize = u8::MAX as usize;

fn validate_key_length(key: &[u8]) -> CostResult<(), Error> {
    if key.len() > MAX_KEY_LENGTH {
        return Err(Error::InvalidInput("key length must be at most 255 bytes"))
            .wrap_with_cost(OperationCost::default());
    }
    Ok(()).wrap_with_cost(OperationCost::default())
}

impl GroveDb {
    /// Insert a GroveDB element given a path to the subtree and the key to
    /// insert at
    pub fn insert<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        options: Option<InsertOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert",
            grove_version.grovedb_versions.operations.insert.insert
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));

        let subtree_path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();

        let tx = TxRef::new(&self.db, transaction);

        cost_return_on_error!(
            &mut cost,
            self.insert_on_transaction(
                subtree_path,
                key,
                element,
                options.unwrap_or_default(),
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_on_transaction
        );

        let mut cost = OperationCost::default();

        let mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();

        let merk = cost_return_on_error!(
            &mut cost,
            self.add_element_on_transaction(
                path.clone(),
                key,
                element,
                options,
                transaction,
                batch,
                grove_version
            )
        );
        merk_cache.insert(path.clone(), merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                path,
                transaction,
                batch,
                grove_version
            )
        );

        Ok(()).wrap_with_cost(cost)
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_element_on_transaction<'db, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        check_grovedb_v0_with_cost!(
            "add_element_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .add_element_on_transaction
        );

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

        // Inserting an item directly into a CountIndexedTree primary
        // via `db.insert()` would update the primary's content but
        // leave the secondary stale — `add_element_on_transaction`
        // has no secondary-mirror hook. The dedicated
        // `insert_into_count_indexed_tree` exists precisely for this
        // case and handles the dual-Merk write. Reject early with a
        // pointer to the right API to prevent silent data drift.
        // (References — which carry no count change at the cidx
        // level — are still allowed; the cidx primary handles them via
        // `insert_into_count_indexed_tree`'s reference resolution
        // path, but the path used here goes through generic merk
        // insertion only and does not need the cidx mirror.)
        if subtree_to_insert_into.tree_type.is_count_indexed_primary() {
            return Err(Error::NotSupported(
                "direct `db.insert` into a CountIndexedTree primary is not supported \
                 (the secondary index would not be mirrored). Use \
                 `db.insert_into_count_indexed_tree(...)` for element insertion or \
                 `db.delete_from_count_indexed_tree(...)` for removal"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

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
            // CountIndexedTree / ProvableCountIndexedTree own two child Merks
            // (primary + secondary). The H1-A combined value hash needs the
            // actual root hashes from both child Merks; for the empty case
            // (both root keys = None, count = 0) those are `NULL_HASH`,
            // otherwise we open the existing child Merks and read their
            // current root hashes so the parent's value_hash is consistent
            // with on-disk state (migration / restore-from-backup path).
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
                        let (p_hash, p_root_key, _) = cost_return_on_error!(
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
            // ProvableSumIndexedTree owns two child Merks (primary +
            // sum-ordered secondary). The H1-A combined value hash is
            // `combine_hash_three(value_hash, primary_root_hash,
            // secondary_root_hash)`. For the empty case (both root keys
            // = None, sum = 0) both child root hashes are NULL_HASH.
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
                        let (p_hash, p_root_key, _) = cost_return_on_error!(
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
            // ProvableCountProvableSumIndexedTree owns a primary Merk
            // and 1..=3 per-axis secondaries. The H1-A combined value
            // hash is `combine_hash_three(value_hash, primary_root_hash,
            // axes_digest)`, where `axes_digest` is the canonical
            // digest over the (axis_tag, secondary_root_hash) TLV.
            //
            // The "empty creation" case is: primary_root_key = None,
            // count = 0, sum = 0, AND every axis entry has
            // secondary_root_key = None (the axes list itself may be
            // non-empty — it carries the schema). For an empty
            // creation, each axis contributes NULL_HASH to the
            // axes_digest payload.
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
                    let digest = grovedb_merk::tree::axes_digest(&zero_axes).unwrap();
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
                    let (p_hash, p_root_key, _) = cost_return_on_error!(
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
                    let digest = grovedb_merk::tree::axes_digest(&axis_hashes).unwrap();
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

    /// Insert if not exists
    /// Insert if not exists
    ///
    /// Inserts an element at the specified path and key if it does not already
    /// exist.
    ///
    /// # Arguments
    ///
    /// * `path` - The path where the element should be inserted.
    /// * `key` - The key under which the element should be inserted.
    /// * `element` - The element to insert.
    /// * `transaction` - The transaction argument, if any.
    /// * `grove_version` - The GroveDB version.
    ///
    /// # Returns
    ///
    /// Returns a `CostResult<bool, Error>` indicating whether the element was
    /// inserted (`true`) or already existed (`false`).
    pub fn insert_if_not_exists<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_not_exists
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));
        let subtree_path: SubtreePath<_> = path.into();

        if cost_return_on_error!(
            &mut cost,
            self.has_raw(subtree_path.clone(), key, transaction, grove_version)
        ) {
            Ok(false).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| true)
                .add_cost(cost)
        }
    }

    /// Insert if not exists
    /// If the item does exist return it
    ///
    /// Inserts an element at the given `path` and `key` if it does not exist.
    /// If the element already exists, returns the existing element.
    ///
    /// # Arguments
    ///
    /// * `path` - The path where the element should be inserted.
    /// * `key` - The key under which the element should be inserted.
    /// * `element` - The element to insert.
    /// * `transaction` - The transaction argument, if any.
    /// * `grove_version` - The GroveDB version.
    ///
    /// # Returns
    ///
    /// Returns a `CostResult<Option<Element>, Error>`, where
    /// `Ok(Some(element))` is the existing element if it was found, or
    /// `Ok(None)` if the new element was inserted.
    pub fn insert_if_not_exists_return_existing_element<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists_return_existing_element",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_not_exists_return_existing_element
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));
        let subtree_path: SubtreePath<_> = path.into();

        let previous_element = cost_return_on_error!(
            &mut cost,
            self.get_raw_optional(subtree_path.clone(), key, transaction, grove_version)
        );
        if previous_element.is_some() {
            Ok(previous_element).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| None)
                .add_cost(cost)
        }
    }

    /// Insert if the value changed
    /// We return if the value was inserted
    /// If the value was changed then we return the previous element
    pub fn insert_if_changed_value<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_changed_value
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));
        let subtree_path: SubtreePath<B> = path.into();

        let previous_element = cost_return_on_error!(
            &mut cost,
            self.get_raw_optional(subtree_path.clone(), key, transaction, grove_version)
        );
        let needs_insert = match &previous_element {
            None => true,
            Some(previous_element) => previous_element != &element,
        };
        if !needs_insert {
            Ok((false, None)).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| (true, previous_element))
                .add_cost(cost)
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use grovedb_version::version::GroveVersion;
    use pretty_assertions::assert_eq;

    use crate::{
        operations::insert::InsertOptions,
        tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn test_non_root_insert_item_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful insert");
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key", None, grove_version)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_subtree_then_insert_item_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());

        // Insert a subtree first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_item_with_transaction() {
        let grove_version = GroveVersion::latest();
        let item_key = b"key3";

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let element1 = Element::new_item(b"ayy".to_vec());

        db.insert(
            [TEST_LEAF].as_ref(),
            item_key,
            element1,
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        // The key was inserted inside the transaction, so it shouldn't be
        // possible to get it back without committing or using transaction
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
        // Check that the element can be retrieved when transaction is passed
        let result_with_transaction = db
            .get(
                [TEST_LEAF].as_ref(),
                item_key,
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::new_item(b"ayy".to_vec()));

        // Test that commit works
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check that the change was committed
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::new_item(b"ayy".to_vec()));
    }

    #[test]
    fn test_non_root_insert_subtree_with_transaction() {
        let grove_version = GroveVersion::latest();
        let subtree_key = b"subtree_key";

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        db.insert(
            [TEST_LEAF].as_ref(),
            subtree_key,
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let result_with_transaction = db
            .get(
                [TEST_LEAF].as_ref(),
                subtree_key,
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::empty_tree());

        db.commit_transaction(transaction).unwrap().unwrap();

        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::empty_tree());
    }

    #[test]
    fn test_insert_if_not_exists() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert twice at the same path
        assert!(db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version
            )
            .unwrap()
            .expect("Provided valid path"));
        assert!(!db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version
            )
            .unwrap()
            .expect("Provided valid path"));

        // Should propagate errors from insertion
        let result = db
            .insert_if_not_exists(
                [TEST_LEAF, b"unknown"].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidParentLayerPath(_))));
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let element_key = b"key1";
        let new_element = Element::new_item(b"new_value".to_vec());

        // Insert a new element and check if it returns None
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                new_element.clone(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("Expected insertion of new element");

        assert_eq!(result, None);

        // Try inserting the same element again and expect it to return the existing
        // element
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                Element::new_item(b"another_value".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("Expected to return existing element");

        assert_eq!(result, Some(new_element.clone()));

        // Check if the existing element is still the original one and not replaced
        let fetched_element = db
            .get([TEST_LEAF].as_ref(), element_key, None, grove_version)
            .unwrap()
            .expect("Expected to retrieve the existing element");

        assert_eq!(fetched_element, new_element);
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element_with_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let element_key = b"key2";
        let new_element = Element::new_item(b"transaction_value".to_vec());
        let transaction = db.start_transaction();

        // Insert a new element within a transaction and check if it returns None
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                new_element.clone(),
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected insertion of new element in transaction");

        assert_eq!(result, None);

        // Try inserting the same element again within the transaction
        // and expect it to return the existing element
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                Element::new_item(b"another_transaction_value".to_vec()),
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to return existing element in transaction");

        assert_eq!(result, Some(new_element.clone()));

        // Commit the transaction
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check if the element is still the original one and not replaced
        let fetched_element = db
            .get([TEST_LEAF].as_ref(), element_key, None, grove_version)
            .unwrap()
            .expect("Expected to retrieve the existing element after transaction commit");

        assert_eq!(fetched_element, new_element);
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element_invalid_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Try inserting to an invalid path and expect an error
        let result = db.insert_if_not_exists_return_existing_element(
            [b"invalid_path"].as_ref(),
            b"key",
            Element::new_item(b"value".to_vec()),
            None,
            grove_version,
        );

        assert!(matches!(
            result.unwrap(),
            Err(Error::InvalidParentLayerPath(_))
        ));
    }

    #[test]
    fn test_one_insert_item_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 for Basic merk
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Basic Merk 1
        // Child Heights 2

        // Total 37 + 72 + 40 = 149

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 149,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_in_sum_tree_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key1",
                Element::new_sum_item(5),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        //   1 for the flag option (but no flags)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 84, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 156,
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_under_sum_item_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        db.insert(
            [b"s".as_slice()].as_ref(),
            b"key1",
            Element::new_sum_item(5),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key2",
                Element::new_sum_item(6),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170

        // replaced bytes
        // 133 for key1 (higher node/same merk level)
        // ?

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 217,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 232,
                hash_node_calls: 10,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_bigger_sum_item_under_sum_item_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        db.insert(
            [b"s".as_slice()].as_ref(),
            b"key1",
            Element::new_sum_item(126),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        // the cost of the varint goes up by 2 after 126 and another 2 at 32768
        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key2",
                Element::new_sum_item(32768),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 81)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 217, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 237,
                hash_node_calls: 10,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"dog".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 76
        //   1 for the flag option
        //   3 for flags
        //   1 for flags length
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 76 + 40 = 153

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 153,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_tree_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 38
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 38 + 40 = 115

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 115,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3, // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_sum_tree_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_sum_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 47
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        //   9 bytes for sum
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 47 + 40 = 124

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 124,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3, // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_tree_cost_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(b"cat".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 42
        //   1 for the flag option
        //   1 byte for flag size
        //   3 bytes for flags
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 42 + 40 = 119

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 119,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_under_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 73
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 72)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 73 + 40 = 150

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 150,
                    replaced_bytes: 78,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_under_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_count_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 81
        //   1 for the enum type item
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for the flag option (but no flags)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Count node
        // 1 byte for the value_size (required space for 1)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Count Merk 9
        // Child Heights 2

        // Total 37 + 81 + 48 = 166

        // Explanation for replaced bytes

        // Replaced parent Value -> 86
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for the count merk
        //   9 for the count
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 166,
                    replaced_bytes: 87,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 162, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_apple_flags_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 79 + 40 = 156

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for the basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 78)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 79 + 40 = 156

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size

        // Hash node calls
        // 1 for getting the merk
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 2 for the node hash

        // on the level above
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 78,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 78)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 79 + 40 = 156

        // Explanation for replaced bytes

        // Replaced parent Value -> 82
        //   1 for the flag option
        //   3 bytes for flags
        //   1 for flags size
        //   1 for the enum type
        //   1 for an empty option
        //   1 for basic merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for the child to parent hook
        // 2 byte for the value_size (required space)

        // Hash node calls
        // 1 for getting the merk
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 2 for the node hash

        // on the level above
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 82,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 160, // todo: verify this
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_item_same_cost_at_root() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 110 replaced bytes

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 71)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // 72 + 40 = 112

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 112,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 77,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 190,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 230, // todo verify this
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_bigger_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(15),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(1000000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 248,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 266, // todo verify this
                hash_node_calls: 9,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_bigger_sum_item_parent_sum_tree_already_big(
    ) {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [1; 32].as_slice(),
            Element::new_sum_item(1000000),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(15),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(1000000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 9, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 409, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 487, // todo verify this
                hash_node_calls: 11,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_smaller_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(1000000),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(15),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 248,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 276, // todo verify this
                hash_node_calls: 9,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_bigger_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item(b"test".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test1".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 1,
                    replaced_bytes: 191, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 231,
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_tree_bigger_cost_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_tree(None),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_tree_with_flags(None, Some(b"cat".to_vec())),
                Some(InsertOptions {
                    validate_insertion_does_not_override: false,
                    validate_insertion_does_not_override_tree: false,
                    base_root_storage_is_free: true,
                }),
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 4 added bytes

        // 1 for size of "cat" flags
        // 3 for bytes

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for child to parent hook
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Replaced current tree -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for child to parent hook
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 4,
                    replaced_bytes: 156,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 227,
                hash_node_calls: 9, // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn insert_accepts_255_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xAA; 255];
        db.insert(
            [TEST_LEAF].as_ref(),
            &key,
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("255-byte key should be accepted");
    }

    #[test]
    fn insert_rejects_256_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xBB; 256];
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(b"val".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(..))));
    }

    #[test]
    fn insert_if_not_exists_rejects_256_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xBB; 256];
        let result = db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(b"val".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(..))));
    }

    #[test]
    fn insert_if_changed_value_rejects_256_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xBB; 256];
        let result = db
            .insert_if_changed_value(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(b"val".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(..))));
    }
}
