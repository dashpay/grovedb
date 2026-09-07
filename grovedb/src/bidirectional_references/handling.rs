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

/// The caller's sectioned-removal policy (flags → how removed key/value
/// bytes are accounted), threaded through every deletion a plan performs so
/// cascaded referrers get the same refund allocation — and the same
/// callback errors — as the element the caller deleted directly.
pub(crate) type SectionedRemovalFn<'a> =
    &'a mut dyn FnMut(
        &Vec<u8>,
        u32,
        u32,
    )
        -> Result<(StorageRemovedBytes, StorageRemovedBytes), grovedb_merk::Error>;

/// The default policy for flows that carry no caller callback (inserts):
/// plain basic removal accounting.
pub(crate) fn basic_sectioned_removal() -> impl FnMut(
    &Vec<u8>,
    u32,
    u32,
) -> Result<
    (StorageRemovedBytes, StorageRemovedBytes),
    grovedb_merk::Error,
> {
    |_flags: &Vec<u8>, removed_key_bytes: u32, removed_value_bytes: u32| {
        Ok((
            StorageRemovedBytes::BasicStorageRemoval(removed_key_bytes),
            StorageRemovedBytes::BasicStorageRemoval(removed_value_bytes),
        ))
    }
}

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
/// primary write only (the user-visible element the plan was derived from);
/// `sectioned_removal` is the caller's removal-accounting policy, applied to
/// every deletion the plan performs.
fn apply_plan<'b, B: AsRef<[u8]>>(
    merk_cache: &MerkCache<'_, 'b, B>,
    plan: Plan,
    primary_options: Option<InsertOptions>,
    sectioned_removal: SectionedRemovalFn<'_>,
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
                            &mut |flags: &Vec<u8>, removed_key_bytes, removed_value_bytes| {
                                sectioned_removal(flags, removed_key_bytes, removed_value_bytes)
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
    // Insertion plans never delete, so no caller removal policy applies.
    apply_plan(merk_cache, plan, options, &mut basic_sectioned_removal()).add_cost(cost)
}

/// Post-processing of possible backward references relationships after
/// insertion of anything but bidirectional reference (because there is
/// [process_bidirectional_reference_insertion] for that).
///
/// `sectioned_removal` is the caller's removal-accounting policy; it governs
/// every referrer a cascade deletes, exactly as it governed the element the
/// caller updated.
pub(crate) fn process_update_element_with_backward_references<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    merk: MerkHandle<'db, 'c>,
    path: SubtreePathBuilder<'b, B>,
    key: &[u8],
    delta: Delta,
    sectioned_removal: SectionedRemovalFn<'_>,
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
    apply_plan(merk_cache, plan, None, sectioned_removal).add_cost(cost)
}
