//! Get operations and costs

#[cfg(feature = "estimated_costs")]
mod average_case;

mod aggregate_per_key;
mod query;
mod run_path_query;
use grovedb_storage::Storage;
pub use query::QueryItemOrSumReturnType;
pub use run_path_query::{AxisAggregateValue, PathQueryRun};
#[cfg(feature = "estimated_costs")]
mod worst_case;

use std::collections::HashSet;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{get::ElementFetchFromStorageExtensions, ElementExt},
    error::MerkErrorExt,
    tree::combine_hash,
    CryptoHash,
};
use grovedb_path::SubtreePath;
use grovedb_storage::StorageContext;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{
    reference_path::{path_from_reference_path_type, path_from_reference_qualified_path_type},
    util::TxRef,
    Element, Error, GroveDb, Transaction, TransactionArg,
};

/// Limit of possible indirections
pub const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    /// Get an element from the backing store
    /// Merk Caching is on by default
    /// use get_caching_optional if no caching is desired
    pub fn get<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!("get", grove_version.grovedb_versions.operations.get.get);

        self.get_caching_optional(path.into(), key, true, transaction, grove_version)
    }

    /// Get an element from the backing store
    /// Merk Caching can be set
    pub fn get_caching_optional<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!(
            "get_caching_optional",
            grove_version
                .grovedb_versions
                .operations
                .get
                .get_caching_optional
        );

        let mut cost = OperationCost::default();

        // Look through `NonCounted` so a wrapped reference still resolves.
        // The wrapper is transparent at the get/query layer.
        // `ReferenceWithSumItem` follows the same chain as `Reference` — the
        // carried sum is a parent-aggregation property, not a per-hop value.
        match cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(
                path.clone(),
                key,
                allow_cache,
                transaction,
                grove_version
            )
        )
        .into_underlying()
        {
            Element::Reference(reference_path, ..)
            | Element::ReferenceWithSumItem(reference_path, ..) => {
                let path_owned = cost_return_on_error_into!(
                    &mut cost,
                    path_from_reference_path_type(reference_path, &path.to_vec(), Some(key))
                        .wrap_with_cost(OperationCost::default())
                );
                self.follow_reference(
                    path_owned.as_slice().into(),
                    allow_cache,
                    transaction,
                    grove_version,
                )
                .add_cost(cost)
            }
            other => Ok(other).wrap_with_cost(cost),
        }
    }

    /// Return the Element that a reference points to, **for presentation**.
    /// If the reference points to another reference, keep following until
    /// base element is reached.
    ///
    /// The terminal is returned looked-through: a `NonCounted`-wrapped
    /// terminal comes back as its inner element, which is what `get` /
    /// query callers want. Anything that must reproduce the reference's
    /// *commitment* — the hash a reference node binds — must start with
    /// [`Self::follow_reference_as_stored`] instead. GROVE_V4 writes commit to
    /// the terminal's stored bytes, wrapper included. Readers of existing nodes
    /// must also account for legacy direct writes that hashed the inner value.
    pub fn follow_reference<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        self.follow_reference_as_stored(path, allow_cache, transaction, grove_version)
            .map_ok(Element::into_underlying)
    }

    /// Return the terminal Element of a reference chain **exactly as it is
    /// stored**, i.e. commitment-preserving.
    ///
    /// Wrappers are looked through only to decide whether to keep hopping
    /// (a `NonCounted(Reference)` is followed like a bare `Reference`); the
    /// terminal itself is returned with its wrapper intact. This is the
    /// element whose serialized bytes hash to the merk-stored `value_hash`
    /// of the terminal node, so `terminal.value_hash()` is the value a
    /// reference node must combine into its own hash. The batch reference
    /// resolver (`process_reference` in `batch/mod.rs`) commits to the same
    /// representation — it either reads the terminal's stored value hash
    /// directly or hashes the outer element's bytes. Direct writes on GROVE_V4
    /// use that representation too. Proof generation and integrity checks
    /// additionally select the legacy unwrapped representation when it matches
    /// an existing reference's committed hash; the current read version does
    /// not identify which write path originally produced the node.
    ///
    /// Use [`Self::follow_reference`] when the caller only needs the value
    /// the reference denotes.
    pub fn follow_reference_as_stored<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        self.follow_reference_as_stored_visiting(
            path,
            HashSet::new(),
            allow_cache,
            transaction,
            grove_version,
        )
    }

    /// [`Self::follow_reference_as_stored`] for a reference that is *being
    /// written* at `referrer_qualified_path` (its parent path plus its key):
    /// the chain is refused with [`Error::CyclicReference`] the moment it
    /// reaches the referrer's own position.
    ///
    /// Resolving from the target alone cannot see that case. The position
    /// being written still holds its previous element in storage, so a chain
    /// that runs back to it reads that stale element — an item, say — and
    /// looks acyclic, while the state about to be committed is the cycle
    /// `referrer -> ... -> referrer`, which every later read of any key on it
    /// fails on. The MerkCache follower (`reference_path::follow_reference`)
    /// and the batch resolver (`follow_reference_get_value_hash`) already
    /// account for the referrer's position; this is the direct-write
    /// equivalent.
    pub(crate) fn follow_reference_as_stored_for_write<B: AsRef<[u8]>>(
        &self,
        referrer_qualified_path: Vec<Vec<u8>>,
        path: SubtreePath<B>,
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        self.follow_reference_as_stored_visiting(
            path,
            HashSet::from([referrer_qualified_path]),
            allow_cache,
            transaction,
            grove_version,
        )
    }

    /// The walk behind both `follow_reference_as_stored*` entry points.
    /// `visited` seeds the cycle check: the chain is refused as cyclic as
    /// soon as it reaches any qualified path already in the set.
    fn follow_reference_as_stored_visiting<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        mut visited: HashSet<Vec<Vec<u8>>>,
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!(
            "follow_reference",
            grove_version
                .grovedb_versions
                .operations
                .get
                .follow_reference
        );

        let mut cost = OperationCost::default();

        let mut hops_left = MAX_REFERENCE_HOPS;
        let mut current_element;
        // TODO, still have to do because of references handling
        let mut current_path = path.to_vec();

        while hops_left > 0 {
            if visited.contains(&current_path) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            if let Some((key, path_slice)) = current_path.split_last() {
                current_element = cost_return_on_error!(
                    &mut cost,
                    self.get_raw_caching_optional(
                        path_slice.into(),
                        key,
                        allow_cache,
                        transaction,
                        grove_version
                    )
                    .map_err(|e| match e {
                        Error::PathParentLayerNotFound(p) => {
                            Error::CorruptedReferencePathParentLayerNotFound(p)
                        }
                        Error::PathKeyNotFound(p) => {
                            Error::CorruptedReferencePathKeyNotFound(p)
                        }
                        Error::PathNotFound(p) => {
                            Error::CorruptedReferencePathNotFound(p)
                        }
                        _ => e,
                    })
                )
            } else {
                return Err(Error::CorruptedPath("empty path".to_string())).wrap_with_cost(cost);
            }
            visited.insert(current_path.clone());
            // Look through `NonCounted` so a chain that hops via a wrapped
            // reference is followed instead of being returned as a value.
            // `ReferenceWithSumItem` is also followed — the carried sum is
            // irrelevant to chain destination. The terminal is handed back
            // untouched (wrapper and all): it is the stored element.
            let next_hop = match current_element.underlying() {
                Element::Reference(reference_path, ..)
                | Element::ReferenceWithSumItem(reference_path, ..) => Some(reference_path.clone()),
                _ => None,
            };
            match next_hop {
                Some(reference_path) => {
                    current_path = cost_return_on_error_into!(
                        &mut cost,
                        path_from_reference_qualified_path_type(reference_path, &current_path)
                            .wrap_with_cost(OperationCost::default())
                    )
                }
                None => return Ok(current_element).wrap_with_cost(cost),
            }
            hops_left -= 1;
        }
        Err(Error::ReferenceLimit).wrap_with_cost(cost)
    }

    /// Select the terminal representation bound by an existing reference.
    ///
    /// Pre-V4 direct writes hashed the unwrapped terminal, while batch writes
    /// and V4 direct writes hash its stored bytes. Both can coexist, even after
    /// an upgrade, so select the legacy form only when its combined hash matches
    /// the reference's actual commitment. Otherwise retain the stored form:
    /// callers still detect a stale reference if neither representation matches.
    /// This is a read-side compatibility step and must not be used for writes.
    pub(crate) fn reference_terminal_as_committed(
        terminal: Element,
        reference_element_hash: &CryptoHash,
        committed_value_hash: &CryptoHash,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();
        if terminal.is_wrapped() {
            let legacy_terminal_hash = cost_return_on_error_into!(
                &mut cost,
                terminal.underlying().value_hash(grove_version)
            );
            let legacy_commitment = combine_hash(reference_element_hash, &legacy_terminal_hash)
                .unwrap_add_cost(&mut cost);
            if &legacy_commitment == committed_value_hash {
                return Ok(terminal.into_underlying()).wrap_with_cost(cost);
            }
        }
        Ok(terminal).wrap_with_cost(cost)
    }

    /// Get Element at specified path and key
    /// If element is a reference return as is, don't follow
    pub fn get_raw<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!(
            "get_raw",
            grove_version.grovedb_versions.operations.get.get_raw
        );

        self.get_raw_caching_optional(path, key, true, transaction, grove_version)
    }

    /// Get tree item without following references
    pub fn get_raw_caching_optional<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        check_grovedb_v0_with_cost!(
            "get_raw_caching_optional",
            grove_version
                .grovedb_versions
                .operations
                .get
                .get_raw_caching_optional
        );

        let tx = TxRef::new(&self.db, transaction);

        self.get_raw_on_transaction_caching_optional(
            path,
            key,
            allow_cache,
            tx.as_ref(),
            grove_version,
        )
    }

    /// Get Element at specified path and key
    /// If element is a reference return as is, don't follow
    /// Return None if element is not found
    pub fn get_raw_optional<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error> {
        check_grovedb_v0_with_cost!(
            "get_raw_optional",
            grove_version
                .grovedb_versions
                .operations
                .get
                .get_raw_optional
        );

        self.get_raw_optional_caching_optional(path, key, true, transaction, grove_version)
    }

    /// Get tree item without following references
    pub fn get_raw_optional_caching_optional<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        allow_cache: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error> {
        check_grovedb_v0_with_cost!(
            "get_raw_optional_caching_optional",
            grove_version
                .grovedb_versions
                .operations
                .get
                .get_raw_optional_caching_optional
        );

        let tx = TxRef::new(&self.db, transaction);

        self.get_raw_optional_on_transaction_caching_optional(
            path,
            key,
            allow_cache,
            tx.as_ref(),
            grove_version,
        )
    }

    /// Get tree item without following references
    pub(crate) fn get_raw_on_transaction_caching_optional<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        allow_cache: bool,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();

        let merk_to_get_from = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path.clone(), transaction, None, grove_version)
                .map_err(|e| match e {
                    Error::InvalidParentLayerPath(s) => {
                        Error::PathParentLayerNotFound(s)
                    }
                    _ => e,
                })
        );

        Element::get(&merk_to_get_from, key, allow_cache, grove_version)
            .add_context(format!("path is {}", path))
            .map_err(|e| e.into())
            .add_cost(cost)
    }

    /// Get tree item without following references
    pub(crate) fn get_raw_optional_on_transaction_caching_optional<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        allow_cache: bool,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error> {
        let mut cost = OperationCost::default();
        let merk_result = self
            .open_transactional_merk_at_path(path, transaction, None, grove_version)
            .map_err(|e| match e {
                Error::InvalidParentLayerPath(s) => Error::PathParentLayerNotFound(s),
                _ => e,
            })
            .unwrap_add_cost(&mut cost);
        let merk = cost_return_on_error_no_add!(
            cost,
            match merk_result {
                Ok(result) => Ok(Some(result)),
                Err(Error::PathParentLayerNotFound(_)) | Err(Error::InvalidParentLayerPath(_)) =>
                    Ok(None),
                Err(e) => Err(e),
            }
        );

        if let Some(merk_to_get_from) = merk {
            Element::get_optional(&merk_to_get_from, key, allow_cache, grove_version)
                .map_err(|e| e.into())
                .add_cost(cost)
        } else {
            Ok(None).wrap_with_cost(cost)
        }
    }

    /// Does tree element exist without following references
    /// There is no cache for has_raw
    pub fn has_raw<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "has_raw",
            grove_version.grovedb_versions.operations.get.has_raw
        );

        let tx = TxRef::new(&self.db, transaction);

        // Merk's items should be written into data storage and checked accordingly
        self.db
            .get_transactional_storage_context(path.into(), None, tx.as_ref())
            .flat_map(|s| s.get(key).map_err(|e| e.into()).map_ok(|x| x.is_some()))
    }

    fn check_subtree_exists<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        transaction: &Transaction,
        error_fn: impl FnOnce() -> Error,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if let Some((parent_path, parent_key)) = path.derive_parent() {
            let element = {
                let merk_to_get_from = cost_return_on_error!(
                    &mut cost,
                    self.open_transactional_merk_at_path(
                        parent_path,
                        transaction,
                        None,
                        grove_version
                    )
                );

                Element::get(&merk_to_get_from, parent_key, true, grove_version)
                    .add_context(format!("path is {}", path))
                    .map_err(|e| e.into())
            }
            .unwrap_add_cost(&mut cost);
            // `is_any_tree()` is the single source of truth for "this
            // element is a subtree" (every Merk and non-Merk tree variant),
            // and it looks through `NonCounted` itself, so a parent stored
            // as `NonCounted(Tree)` (or any wrapped tree variant) still
            // validates. Enumerating variants here instead has repeatedly
            // drifted behind new tree types (#710, #657, #787).
            match element {
                Ok(ref e) if e.is_any_tree() => Ok(()).wrap_with_cost(cost),
                Ok(_) | Err(Error::PathKeyNotFound(_)) => Err(error_fn()).wrap_with_cost(cost),
                Err(e) => Err(e).wrap_with_cost(cost),
            }
        } else {
            Ok(()).wrap_with_cost(cost)
        }
    }

    /// Check that subtree exists with path not found error
    pub(crate) fn check_subtree_exists_path_not_found<'b, B>(
        &self,
        path: SubtreePath<'b, B>,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.check_subtree_exists(
            path.clone(),
            transaction,
            || {
                Error::PathNotFound(format!(
                    "subtree doesn't exist at path {:?}",
                    path.to_vec()
                        .into_iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                ))
            },
            grove_version,
        )
    }

    /// Check subtree exists with invalid path error
    pub fn check_subtree_exists_invalid_path<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "check_subtree_exists_invalid_path",
            grove_version
                .grovedb_versions
                .operations
                .get
                .check_subtree_exists_invalid_path
        );

        let tx = TxRef::new(&self.db, transaction);

        self.check_subtree_exists(
            path,
            tx.as_ref(),
            || Error::InvalidPath("subtree doesn't exist".to_owned()),
            grove_version,
        )
    }
}
