//! The backward-references batch preprocessor (batching milestone M2).
//!
//! When [`super::BatchApplyOptions::propagate_backward_references`] is set,
//! user operations touching the backward-references ITEM family expand into
//! the derived operations the live flagged flow would perform: overwrites
//! of registered targets append referrer rewrites (and lazy referrer-list
//! cleanups), deletions and incompatible overwrites append consent-checked
//! cascade deletions. The decisions come from the shared semantic core in
//! [`crate::bidirectional_references::semantics`] — the same planners the
//! `MerkCache` driver uses — over a read-only view of the pre-batch state,
//! so live and batched semantics cannot drift.
//!
//! Derived writes carry their final node value hash (the two-layer
//! combine), computed here exactly as the live applier computes it, and
//! execute through [`super::GroveOp::ReplaceBackwardReferenceFamilyMember`].
//!
//! Scope (fail closed beyond it):
//! - `BidirectionalReference` ELEMENT ops stay rejected (milestone M3).
//! - In-batch chain composition (an expansion reading a position another op
//!   writes) surfaces as a batch-consistency conflict rather than being
//!   resolved (milestone M4 specifies the merge rules).

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    element::{get::ElementFetchFromStorageExtensions, ElementExt},
    CryptoHash,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::{GroveOp, QualifiedGroveDbOp};
use crate::{
    bidirectional_references::semantics::{
        plan_element_update, ChainStore, DerivedMutation, Position, ResolvedPosition,
    },
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{path_from_reference_path_type, ReferencePathType},
    util::TxRef,
    Element, Error, GroveDb, Transaction,
};

/// Read-only [`ChainStore`] over the database at the batch's transaction
/// snapshot (pre-batch state). Hashes are derived from element bytes via
/// the logical-hash convention, so no merk node reads are required.
pub(super) struct DbChainStore<'db, 'g> {
    db: &'g GroveDb,
    tx: &'db Transaction<'db>,
    version: &'g GroveVersion,
}

impl<'db, 'g> DbChainStore<'db, 'g> {
    fn resolve_position(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
        cost: &mut OperationCost,
    ) -> Result<(Position, Element), Error> {
        let qualified = path_from_reference_path_type(reference_path, path, Some(key))?;
        let (target_key, target_path) = qualified
            .split_last()
            .ok_or(Error::CorruptedPath("empty reference".to_string()))?;
        let position = (target_path.to_vec(), target_key.clone());
        let element = self
            .element_at(&position.0, &position.1)
            .unwrap_add_cost(cost)?
            .ok_or_else(|| {
                Error::CorruptedReferencePathKeyNotFound(format!(
                    "batch backward-references expansion: missing element at {}",
                    hex::encode(target_key)
                ))
            })?;
        Ok((position, element))
    }
}

impl<'db, 'g> ChainStore for DbChainStore<'db, 'g> {
    fn element_at(&self, path: &[Vec<u8>], key: &[u8]) -> CostResult<Option<Element>, Error> {
        let mut cost = OperationCost::default();
        let path_slices: Vec<&[u8]> = path.iter().map(|p| p.as_slice()).collect();
        let merk = cost_return_on_error!(
            &mut cost,
            self.db.open_transactional_merk_at_path(
                SubtreePath::from(path_slices.as_slice()),
                self.tx,
                None,
                self.version,
            )
        );
        Element::get_optional(&merk, key, true, self.version)
            .map_err(Error::MerkError)
            .add_cost(cost)
    }

    fn resolve_once(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
    ) -> CostResult<ResolvedPosition, Error> {
        let mut cost = OperationCost::default();
        let result = self
            .resolve_position(path, key, reference_path, &mut cost)
            .and_then(|((path, key), element)| {
                let node_value_hash = element.logical_value_hash(self.version).unwrap()?;
                Ok(ResolvedPosition {
                    path,
                    key,
                    element,
                    node_value_hash,
                    hops: 1,
                })
            });
        result.wrap_with_cost(cost)
    }

    fn resolve_chain(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
        reference_path: ReferencePathType,
    ) -> CostResult<ResolvedPosition, Error> {
        let mut cost = OperationCost::default();
        let mut visited: std::collections::HashSet<Position> = Default::default();
        visited.insert((path.to_vec(), key.to_vec()));
        let mut current = (path.to_vec(), key.to_vec(), reference_path);
        let mut hops = 0usize;
        loop {
            hops += 1;
            if hops > MAX_REFERENCE_HOPS {
                return Err(Error::ReferenceLimit).wrap_with_cost(cost);
            }
            let (position, element) =
                match self.resolve_position(&current.0, &current.1, current.2.clone(), &mut cost) {
                    Ok(resolved) => resolved,
                    Err(e) => return Err(e).wrap_with_cost(cost),
                };
            if !visited.insert(position.clone()) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            match element {
                Element::BidirectionalReference(reference) => {
                    current = (position.0, position.1, reference.forward_reference_path);
                }
                Element::Reference(reference_path, ..)
                | Element::ReferenceWithSumItem(reference_path, ..) => {
                    current = (position.0, position.1, reference_path);
                }
                element => {
                    let node_value_hash = match element.logical_value_hash(self.version).unwrap() {
                        Ok(hash) => hash,
                        Err(e) => return Err(e.into()).wrap_with_cost(cost),
                    };
                    return Ok(ResolvedPosition {
                        path: position.0,
                        key: position.1,
                        element,
                        node_value_hash,
                        hops,
                    })
                    .wrap_with_cost(cost);
                }
            }
        }
    }

    fn version(&self) -> &GroveVersion {
        self.version
    }
}

fn is_family_item(element: &Element) -> bool {
    matches!(
        element,
        Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)
            | Element::ItemWithSumItemWithBackwardsReferences(..)
    )
}

/// The node value hash a derived write installs: for a bidirectional
/// reference `combine(combined, end_hash)`, for the item variants the
/// element's own combined hash — exactly what the live applier's writes
/// produce.
fn derived_node_value_hash(
    element: &Element,
    end_hash: Option<CryptoHash>,
    version: &GroveVersion,
) -> CostResult<CryptoHash, Error> {
    let mut cost = OperationCost::default();
    let hashes = match element
        .backward_references_hashes(version)
        .unwrap_add_cost(&mut cost)
    {
        Ok(Some(hashes)) => hashes,
        Ok(None) => {
            return Err(Error::CorruptedCodeExecution(
                "derived write for an element outside the backward-references family",
            ))
            .wrap_with_cost(cost)
        }
        Err(e) => return Err(e.into()).wrap_with_cost(cost),
    };
    match (element, end_hash) {
        (Element::BidirectionalReference(..), Some(end_hash)) => {
            let combined = grovedb_merk::tree::hash::combine_hash(&hashes.combined, &end_hash)
                .unwrap_add_cost(&mut cost);
            Ok(combined).wrap_with_cost(cost)
        }
        (Element::BidirectionalReference(..), None) => Err(Error::CorruptedCodeExecution(
            "a derived bidirectional-reference rewrite requires its end hash",
        ))
        .wrap_with_cost(cost),
        _ => Ok(hashes.combined).wrap_with_cost(cost),
    }
}

/// Expand `ops` with the derived operations the backward-references rules
/// require. User ops carrying family items get their referrer list replaced
/// with the stored one (or emptied — the list is bookkeeping, never the
/// caller's to claim); overwrites and deletes of registered targets append
/// propagation rewrites, lazy cleanups, and consent-checked cascades.
pub(super) fn expand_backward_references_ops(
    db: &GroveDb,
    tx: &TxRef<'_, '_>,
    mut ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
    let mut cost = OperationCost::default();
    let store = DbChainStore {
        db,
        tx: tx.as_ref(),
        version: grove_version,
    };

    let mut derived: Vec<QualifiedGroveDbOp> = Vec::new();

    for op in ops.iter_mut() {
        let Some(key) = op.key.as_ref().map(|k| k.get_key_clone()) else {
            continue;
        };
        let path = op.path.to_path();

        let writes_over_existing = matches!(
            op.op,
            GroveOp::InsertOrReplace { .. } | GroveOp::Replace { .. } | GroveOp::Patch { .. }
        );
        match &mut op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                let previous = cost_return_on_error!(&mut cost, store.element_at(&path, &key));

                if is_family_item(element) {
                    // The stored referrer list is authoritative; whatever
                    // the caller supplied is not theirs to claim.
                    if let Some(refs) = element.backward_references_mut() {
                        *refs = previous
                            .as_ref()
                            .and_then(|p| p.backward_references())
                            .map(|p| p.to_vec())
                            .unwrap_or_default();
                    }
                }

                let Some(previous) = previous else { continue };
                if !writes_over_existing {
                    // InsertIfNotExists over an existing key writes nothing
                    // (and KnownToNotAlreadyExist will fail at execution);
                    // no bookkeeping follows from a write that never lands.
                    continue;
                }
                let previous_has_registrations = previous
                    .backward_references()
                    .map(|refs| !refs.is_empty())
                    .unwrap_or(false);
                if !previous_has_registrations
                    || matches!(previous, Element::BidirectionalReference(..))
                {
                    // Bidirectional-reference positions keep their current
                    // batch semantics until M3; unregistered targets need
                    // no bookkeeping.
                    continue;
                }
                if *element == previous {
                    // No logical change (the referrer list was just merged
                    // from the stored element, so the comparison covers the
                    // full stored form): nothing propagates — mirroring the
                    // live flow's delta gate.
                    continue;
                }

                let plan = cost_return_on_error!(
                    &mut cost,
                    plan_element_update(&store, &path, &key, previous, Some(element.clone()))
                );
                cost_return_on_error!(
                    &mut cost,
                    translate_plan_mutations(plan.mutations, &mut derived, grove_version)
                        .wrap_with_cost(OperationCost::default())
                );
            }
            GroveOp::Delete => {
                let previous = cost_return_on_error!(&mut cost, store.element_at(&path, &key));
                let Some(previous) = previous else { continue };
                if !is_family_item(&previous)
                    || previous
                        .backward_references()
                        .map(|refs| refs.is_empty())
                        .unwrap_or(true)
                {
                    continue;
                }
                let plan = cost_return_on_error!(
                    &mut cost,
                    plan_element_update(&store, &path, &key, previous, None)
                );
                cost_return_on_error!(
                    &mut cost,
                    translate_plan_mutations(plan.mutations, &mut derived, grove_version)
                        .wrap_with_cost(OperationCost::default())
                );
            }
            _ => {}
        }
    }

    ops.extend(derived);
    Ok(ops).wrap_with_cost(cost)
}

fn translate_plan_mutations(
    mutations: Vec<DerivedMutation>,
    derived: &mut Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    for mutation in mutations {
        match mutation {
            DerivedMutation::Write {
                path,
                key,
                element,
                end_hash,
                ..
            } => {
                let node_value_hash =
                    derived_node_value_hash(&element, end_hash, grove_version).unwrap()?;
                derived.push(QualifiedGroveDbOp {
                    path: crate::batch::KeyInfoPath::from_known_owned_path(path),
                    key: Some(crate::batch::key_info::KeyInfo::KnownKey(key)),
                    op: GroveOp::ReplaceBackwardReferenceFamilyMember {
                        element,
                        node_value_hash,
                    },
                });
            }
            DerivedMutation::Delete { path, key } => {
                derived.push(QualifiedGroveDbOp::delete_op(path, key));
            }
        }
    }
    Ok(())
}
