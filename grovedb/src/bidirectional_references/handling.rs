//! The `MerkCache` driver for backward-references bookkeeping.
//!
//! All decisions live in the pure planners of [`super::semantics`]; this
//! module supplies the two halves the planners abstract over:
//! - [`MerkCacheChainStore`], the read-only [`ChainStore`] view backed by
//!   the transaction's `MerkCache` (so planning sees uncommitted writes of
//!   EARLIER operations), and
//! - [`apply_plan`], which executes a [`Plan`]'s mutations through the same
//!   cache in order.
//!
//! Backward references live ON their target element and are covered by the
//! node hash through the two-layer scheme described in
//! `grovedb_element::bidirectional_reference`: forward references commit to
//! the target's INNER (stripped) hash, so registering or removing a
//! referrer rewrites only the target itself — never the hashes stored by
//! other referrers.

use grovedb_costs::{
    cost_return_on_error, storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt,
};
use grovedb_merk::{
    element::{
        delete::ElementDeleteFromStorageExtensions,
        get::ElementFetchFromStorageExtensions,
        insert::{Delta, ElementInsertToStorageExtensions},
    },
    CryptoHash,
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};

use super::{
    semantics::{
        plan_element_update, plan_reference_insertion, ChainStore, DerivedMutation, Plan,
        ResolvedPosition,
    },
    BidirectionalReference,
};
use crate::{
    merk_cache::MerkCache,
    merk_cache::MerkHandle,
    operations::insert::InsertOptions,
    reference_path::{follow_reference, follow_reference_once, ReferencePathType},
    Element, Error,
};

/// [`ChainStore`] over the transaction's `MerkCache`.
struct MerkCacheChainStore<'c, 'db, 'b, B: AsRef<[u8]>>(&'c MerkCache<'db, 'b, B>);

impl<'c, 'db, 'b, B: AsRef<[u8]>> MerkCacheChainStore<'c, 'db, 'b, B> {
    fn builder(&self, path: &[Vec<u8>]) -> SubtreePathBuilder<'b, B> {
        SubtreePathBuilder::owned_from_iter(path)
    }
}

impl<'c, 'db, 'b, B: AsRef<[u8]>> ChainStore for MerkCacheChainStore<'c, 'db, 'b, B> {
    fn element_at(&self, path: &[Vec<u8>], key: &[u8]) -> CostResult<Option<Element>, Error> {
        let mut cost = Default::default();
        let mut merk = cost_return_on_error!(&mut cost, self.0.get_merk(self.builder(path)));
        merk.for_merk(|m| {
            Element::get_optional(m, key, true, self.0.version).map_err(Error::MerkError)
        })
        .add_cost(cost)
    }

    fn resolve_once(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
    ) -> CostResult<ResolvedPosition, Error> {
        follow_reference_once(self.0, self.builder(path), key, reference_path).map_ok(|resolved| {
            ResolvedPosition {
                path: resolved.target_path.to_vec(),
                key: resolved.target_key,
                element: resolved.target_element,
                node_value_hash: resolved.target_node_value_hash,
                hops: resolved.hops,
            }
        })
    }

    fn resolve_chain(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
    ) -> CostResult<ResolvedPosition, Error> {
        follow_reference(self.0, self.builder(path), key, reference_path).map_ok(|resolved| {
            ResolvedPosition {
                path: resolved.target_path.to_vec(),
                key: resolved.target_key,
                element: resolved.target_element,
                node_value_hash: resolved.target_node_value_hash,
                hops: resolved.hops,
            }
        })
    }

    fn version(&self) -> &grovedb_version::version::GroveVersion {
        self.0.version
    }
}

/// Execute a single derived write through the cache. Bidirectional
/// references are written through `insert_reference` with their resolved
/// end hash; the item variants derive their combined hash from the bytes.
fn apply_write(
    merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
    element: Element,
    end_hash: Option<CryptoHash>,
    options: Option<InsertOptions>,
    version: &grovedb_version::version::GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = Default::default();
    match (&element, end_hash) {
        (Element::BidirectionalReference(..), Some(end_hash)) => {
            cost_return_on_error!(
                &mut cost,
                merk.for_merk(|m| {
                    element
                        .insert_reference(
                            m,
                            key,
                            end_hash,
                            options.map(|o| o.as_merk_options()),
                            version,
                        )
                        .map_err(Error::MerkError)
                })
            );
        }
        (Element::BidirectionalReference(..), None) => {
            return Err(Error::InternalError(
                "rewriting a bidirectional reference requires its resolved end hash".to_owned(),
            ))
            .wrap_with_cost(cost);
        }
        _ => {
            cost_return_on_error!(
                &mut cost,
                merk.for_merk(|m| {
                    element
                        .insert(m, key, options.map(|o| o.as_merk_options()), version)
                        .map_err(Error::MerkError)
                })
            );
        }
    }
    Ok(()).wrap_with_cost(cost)
}

/// Apply a plan's mutations in order through the `MerkCache`.
/// `primary_options` are the caller's insert options, applied to the plan's
/// primary write only (the user-visible element the plan was derived from).
fn apply_plan<'b, B: AsRef<[u8]>>(
    merk_cache: &MerkCache<'_, 'b, B>,
    plan: Plan,
    primary_options: Option<InsertOptions>,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    for mutation in plan.mutations {
        match mutation {
            DerivedMutation::Write {
                path,
                key,
                element,
                end_hash,
                is_primary,
            } => {
                let mut merk = cost_return_on_error!(
                    &mut cost,
                    merk_cache.get_merk(SubtreePathBuilder::owned_from_iter(&path))
                );
                let options = if is_primary {
                    primary_options.clone()
                } else {
                    None
                };
                cost_return_on_error!(
                    &mut cost,
                    apply_write(
                        &mut merk,
                        &key,
                        element,
                        end_hash,
                        options,
                        merk_cache.version
                    )
                );
            }
            DerivedMutation::Delete { path, key } => {
                let mut merk = cost_return_on_error!(
                    &mut cost,
                    merk_cache.get_merk(SubtreePathBuilder::owned_from_iter(&path))
                );
                cost_return_on_error!(
                    &mut cost,
                    merk.for_merk(|m| {
                        Element::delete_with_sectioned_removal_bytes(
                            m,
                            &key,
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
    }

    Ok(()).wrap_with_cost(cost)
}

/// Insert bidirectional reference at specified location performing required
/// checks and updates.
pub(crate) fn process_bidirectional_reference_insertion<'b, B: AsRef<[u8]>>(
    merk_cache: &MerkCache<'_, 'b, B>,
    path: SubtreePath<'b, B>,
    key: &[u8],
    reference: BidirectionalReference,
    flags: Option<crate::element::ElementFlags>,
    options: Option<InsertOptions>,
) -> CostResult<(), Error> {
    let mut cost = Default::default();

    let store = MerkCacheChainStore(merk_cache);
    let plan = cost_return_on_error!(
        &mut cost,
        plan_reference_insertion(&store, &path.to_vec(), key, reference, flags)
    );
    let Some(plan) = plan else {
        // Identical logical edge: a true no-op.
        return Ok(()).wrap_with_cost(cost);
    };
    apply_plan(merk_cache, plan, options).add_cost(cost)
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
    let _ = merk;

    // On no changes no propagations shall happen:
    if !delta.has_changed() {
        return Ok(()).wrap_with_cost(cost);
    }

    // If there was no overwrite we short-circuit as well:
    let Some(old) = delta.old else {
        return Ok(()).wrap_with_cost(cost);
    };

    let store = MerkCacheChainStore(merk_cache);
    let plan = cost_return_on_error!(
        &mut cost,
        plan_element_update(&store, &path.to_vec(), key, old, delta.new.cloned())
    );
    apply_plan(merk_cache, plan, None).add_cost(cost)
}
