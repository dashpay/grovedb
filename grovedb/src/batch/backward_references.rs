//! The backward-references batch preprocessor (batching milestones M2–M4).
//!
//! When [`super::BatchApplyOptions::propagate_backward_references`] is set,
//! user operations touching the backward-references family — the three ITEM
//! variants and `BidirectionalReference` itself — expand into the derived
//! operations the live flagged flow would perform. The decisions come from
//! the shared semantic core in
//! [`crate::bidirectional_references::semantics`] — the same planners the
//! `MerkCache` driver uses — so live and batched semantics cannot drift.
//!
//! # Sequential simulation over an overlay
//!
//! A batch is an unordered set (one op per position), but the family's
//! bookkeeping is inherently sequential: a reference can target an element
//! the same batch creates, and an overwrite's propagation must see earlier
//! registrations. The preprocessor therefore simulates ONE canonical
//! sequential order and expands the batch into the ops that produce exactly
//! that order's outcome:
//!
//! 1. every non-reference op, in user order (staging its effect into an
//!    overlay of pending position states and planning item-family and
//!    bidi-position bookkeeping against DB-plus-overlay), then
//! 2. every `BidirectionalReference` op, in topological order — targets
//!    before their referrers — so in-batch chains resolve, registrations
//!    land on pending elements, and the hop/component budgets are validated
//!    against the prospective POST-batch state.
//!
//! Planners read through [`OverlayChainStore`]: staged pending state first,
//! the transaction's pre-batch DB state otherwise.
//!
//! Derived writes carry their final node value hash (the two-layer
//! combine), computed here exactly as the live applier computes it, and
//! execute through [`super::GroveOp::ReplaceBackwardReferenceFamilyMember`].
//! A user `BidirectionalReference` op is itself converted into that derived
//! form (its end hash resolved against the overlay); an identical-edge
//! re-insert converts into nothing, mirroring the live no-op.
//!
//! # Conflict rules (milestone M4) — fail closed
//!
//! Derived mutations merge into the batch only where the semantics are
//! unambiguous; everything else is an error:
//!
//! - a bidirectional reference inserted in the same batch that deletes its
//!   target → error (checked explicitly for the first hop; deeper links
//!   surface as missing-reference resolution errors);
//! - a cascade deletion hitting a position another op writes or deletes →
//!   error;
//! - a propagation/registration rewrite hitting a position whose user op is
//!   a delete or a `RefreshReference` → error;
//! - `RefreshReference` on a position holding a bidirectional reference →
//!   rejected (re-insert the reference through a flagged op instead);
//! - registrations onto targets written in the same batch merge into the
//!   target op's element — the one merge the design requires.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
};

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    element::{get::ElementFetchFromStorageExtensions, ElementExt},
    CryptoHash,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::{key_info::KeyInfo, GroveOp, KeyInfoPath, QualifiedGroveDbOp};
use crate::{
    bidirectional_references::semantics::{
        plan_element_update, plan_reference_insertion, ChainStore, DerivedMutation, Position,
        ResolvedPosition,
    },
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{path_from_reference_path_type, ReferencePathType},
    util::TxRef,
    Element, Error, GroveDb, Transaction,
};

/// [`ChainStore`] over the batch's prospective state: an overlay of staged
/// pending position states (what the batch has decided each position will
/// hold) falling back to the database at the batch's transaction snapshot.
/// Hashes are derived from element bytes via the logical-hash convention,
/// so no merk node reads are required.
pub(super) struct OverlayChainStore<'db, 'g> {
    db: &'g GroveDb,
    tx: &'db Transaction<'db>,
    version: &'g GroveVersion,
    /// Staged pending state: `Some(element)` = the batch writes this,
    /// `None` = the batch deletes the position. Absent = untouched.
    overlay: RefCell<HashMap<Position, Option<Element>>>,
}

impl<'db, 'g> OverlayChainStore<'db, 'g> {
    fn new(db: &'g GroveDb, tx: &'db Transaction<'db>, version: &'g GroveVersion) -> Self {
        Self {
            db,
            tx,
            version,
            overlay: RefCell::new(HashMap::new()),
        }
    }

    /// Stage the pending state of a position.
    fn stage(&self, position: Position, element: Option<Element>) {
        self.overlay.borrow_mut().insert(position, element);
    }

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

impl<'db, 'g> ChainStore for OverlayChainStore<'db, 'g> {
    fn element_at(&self, path: &[Vec<u8>], key: &[u8]) -> CostResult<Option<Element>, Error> {
        if let Some(staged) = self
            .overlay
            .borrow()
            .get(&(path.to_vec(), key.to_vec()))
            .cloned()
        {
            return Ok(staged).wrap_with_cost(OperationCost::default());
        }
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
        let mut visited: HashSet<Position> = Default::default();
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

/// The expansion state: the user ops (slots emptied when an op is dropped
/// or converted), the derived ops keyed by position, and the overlay store
/// everything is planned against.
struct Expansion<'db, 'g> {
    store: OverlayChainStore<'db, 'g>,
    /// User ops; a slot becomes `None` when the op is dropped (an inert
    /// insert, an identical-edge no-op) or converted into a derived op.
    ops: Vec<Option<QualifiedGroveDbOp>>,
    /// Position of every KEYED user op, whether retained or not.
    user_index_by_position: HashMap<Position, usize>,
    /// Positions the user's own ops delete (used for the explicit
    /// ref-plus-target-delete conflict rule).
    user_deleted_positions: HashSet<Position>,
    /// Derived ops by position; a write is upserted (recomputed hash), a
    /// cascade delete replaces a pending derived write.
    derived: BTreeMap<Position, QualifiedGroveDbOp>,
    validate_insertion_does_not_override: bool,
}

impl<'db, 'g> Expansion<'db, 'g> {
    fn op_position(op: &QualifiedGroveDbOp) -> Option<Position> {
        op.key
            .as_ref()
            .map(|key| (op.path.to_path(), key.get_key_clone()))
    }

    /// Apply a plan's mutations: stage each into the overlay and merge it
    /// into the batch per the M4 conflict rules.
    fn apply_mutations(&mut self, mutations: Vec<DerivedMutation>) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        for mutation in mutations {
            match mutation {
                DerivedMutation::Write {
                    path,
                    key,
                    element,
                    end_hash,
                    ..
                } => {
                    let position = (path.clone(), key.clone());
                    let retained_user_op = self
                        .user_index_by_position
                        .get(&position)
                        .copied()
                        .filter(|i| self.ops[*i].is_some());
                    if let Some(index) = retained_user_op {
                        let user_op = self.ops[index].as_mut().expect("filtered above");
                        match &mut user_op.op {
                            // A registration or propagation rewrite landing
                            // on a position whose element the batch itself
                            // writes:
                            // - an unprocessed BidirectionalReference op
                            //   keeps the user's payload (its own planning
                            //   will resolve everything freshly); the
                            //   rewrite is staged as a derived op which
                            //   that planning later replaces (or keeps, on
                            //   an identical-edge no-op);
                            // - a family-item write absorbs the rewrite:
                            //   the mutation's element IS the op's element
                            //   plus the registration/cleanup, so it
                            //   replaces the op's payload in place.
                            GroveOp::InsertOrReplace {
                                element: op_element,
                            }
                            | GroveOp::Replace {
                                element: op_element,
                            }
                            | GroveOp::Patch {
                                element: op_element,
                                ..
                            }
                            | GroveOp::InsertIfNotExists {
                                element: op_element,
                                ..
                            }
                            | GroveOp::InsertWithKnownToNotAlreadyExist {
                                element: op_element,
                            } => {
                                if matches!(op_element, Element::BidirectionalReference(..)) {
                                    let node_value_hash = cost_return_on_error!(
                                        &mut cost,
                                        derived_node_value_hash(
                                            &element,
                                            end_hash,
                                            self.store.version
                                        )
                                    );
                                    self.derived.insert(
                                        position.clone(),
                                        QualifiedGroveDbOp {
                                            path: KeyInfoPath::from_known_owned_path(path),
                                            key: Some(KeyInfo::KnownKey(key)),
                                            op: GroveOp::ReplaceBackwardReferenceFamilyMember {
                                                element: element.clone(),
                                                node_value_hash,
                                            },
                                        },
                                    );
                                } else {
                                    *op_element = element.clone();
                                }
                            }
                            _ => {
                                return Err(Error::InvalidBatchOperation(
                                    "a derived backward-references rewrite conflicts with \
                                     another operation in the batch",
                                ))
                                .wrap_with_cost(cost);
                            }
                        }
                        self.store.stage(position, Some(element));
                    } else {
                        let node_value_hash = cost_return_on_error!(
                            &mut cost,
                            derived_node_value_hash(&element, end_hash, self.store.version)
                        );
                        self.derived.insert(
                            position.clone(),
                            QualifiedGroveDbOp {
                                path: KeyInfoPath::from_known_owned_path(path),
                                key: Some(KeyInfo::KnownKey(key)),
                                op: GroveOp::ReplaceBackwardReferenceFamilyMember {
                                    element: element.clone(),
                                    node_value_hash,
                                },
                            },
                        );
                        self.store.stage(position, Some(element));
                    }
                }
                DerivedMutation::Delete { path, key } => {
                    let position = (path.clone(), key.clone());
                    let touched_by_user = self
                        .user_index_by_position
                        .get(&position)
                        .copied()
                        .filter(|i| self.ops[*i].is_some())
                        .is_some();
                    if touched_by_user {
                        // M4: a cascade may not delete a position another
                        // op in the batch touches — whether it writes it
                        // (order-dependent outcome) or deletes it
                        // (double delete).
                        return Err(Error::InvalidBatchOperation(
                            "a backward-references cascade would delete a position another \
                             operation in the batch touches",
                        ))
                        .wrap_with_cost(cost);
                    }
                    self.derived
                        .insert(position.clone(), QualifiedGroveDbOp::delete_op(path, key));
                    self.store.stage(position, None);
                }
            }
        }
        Ok(()).wrap_with_cost(cost)
    }
}

/// Expand `ops` with the derived operations the backward-references rules
/// require, per the module documentation.
pub(super) fn expand_backward_references_ops(
    db: &GroveDb,
    tx: &TxRef<'_, '_>,
    ops: Vec<QualifiedGroveDbOp>,
    validate_insertion_does_not_override: bool,
    grove_version: &GroveVersion,
) -> CostResult<Vec<QualifiedGroveDbOp>, Error> {
    let mut cost = OperationCost::default();

    let mut expansion = Expansion {
        store: OverlayChainStore::new(db, tx.as_ref(), grove_version),
        user_index_by_position: HashMap::new(),
        user_deleted_positions: HashSet::new(),
        derived: BTreeMap::new(),
        validate_insertion_does_not_override,
        ops: Vec::new(),
    };

    for (index, op) in ops.iter().enumerate() {
        if let Some(position) = Expansion::op_position(op) {
            // Consistency checking has already rejected duplicate
            // positions; a stray duplicate would silently lose an op here,
            // so refuse it outright.
            if expansion
                .user_index_by_position
                .insert(position.clone(), index)
                .is_some()
            {
                return Err(Error::InvalidBatchOperation(
                    "batch operations fail consistency checks",
                ))
                .wrap_with_cost(cost);
            }
            if matches!(op.op, GroveOp::Delete | GroveOp::DeleteTree(..)) {
                expansion.user_deleted_positions.insert(position);
            }
        }
    }
    expansion.ops = ops.into_iter().map(Some).collect();

    // Pass 1: every non-reference op in user order. Each op's effect is
    // staged into the overlay; item-family and bidi-position bookkeeping is
    // planned against DB-plus-overlay. `BidirectionalReference` ops are
    // deferred to pass 2.
    let mut bidi_op_indices: Vec<usize> = Vec::new();

    for index in 0..expansion.ops.len() {
        let Some((path, key, op_kind)) = expansion.ops[index].as_ref().and_then(|op| {
            op.key
                .as_ref()
                .map(|k| (op.path.to_path(), k.get_key_clone(), op.op.clone()))
        }) else {
            continue;
        };
        let position = (path.clone(), key.clone());

        match &op_kind {
            GroveOp::InsertOrReplace { element }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                if matches!(element, Element::BidirectionalReference(..)) {
                    bidi_op_indices.push(index);
                    continue;
                }
                let mut element = element.clone();
                let writes_over_existing = matches!(
                    op_kind,
                    GroveOp::InsertOrReplace { .. }
                        | GroveOp::Replace { .. }
                        | GroveOp::Patch { .. }
                );
                let (is_insert_if_not_exists, error_if_exists) = match &op_kind {
                    GroveOp::InsertIfNotExists {
                        error_if_exists, ..
                    } => (true, *error_if_exists),
                    _ => (false, false),
                };
                let is_known_new =
                    matches!(op_kind, GroveOp::InsertWithKnownToNotAlreadyExist { .. });

                let previous =
                    cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key));

                if is_family_item(&element) {
                    // The stored referrer list is authoritative; whatever
                    // the caller supplied is not theirs to claim.
                    if let Some(refs) = element.backward_references_mut() {
                        *refs = previous
                            .as_ref()
                            .and_then(|p| p.backward_references())
                            .map(|p| p.to_vec())
                            .unwrap_or_default();
                    }
                    // Update the op in place so execution writes the
                    // authoritative list.
                    if let Some(user_op) = expansion.ops[index].as_mut() {
                        match &mut user_op.op {
                            GroveOp::InsertOrReplace { element: e }
                            | GroveOp::Replace { element: e }
                            | GroveOp::Patch { element: e, .. }
                            | GroveOp::InsertIfNotExists { element: e, .. }
                            | GroveOp::InsertWithKnownToNotAlreadyExist { element: e } => {
                                *e = element.clone();
                            }
                            _ => unreachable!("matched an insert variant above"),
                        }
                    }
                }

                if let Some(previous) = previous {
                    if is_insert_if_not_exists {
                        if error_if_exists || expansion.validate_insertion_does_not_override {
                            return Err(Error::InvalidBatchOperation(
                                "attempting to insert element that already exists",
                            ))
                            .wrap_with_cost(cost);
                        }
                        // InsertIfNotExists over an existing key writes
                        // nothing. Drop the op so a later derived rewrite
                        // of the position (e.g. a registration on the
                        // stored element) doesn't collide with an op that
                        // never lands.
                        if is_family_item(&element) {
                            expansion.ops[index] = None;
                        }
                        continue;
                    }
                    if is_known_new && is_family_item(&element) {
                        // The caller's not-exists assertion is false. The
                        // plain-element path skips the existence check by
                        // design, but a blind family overwrite would skip
                        // the bookkeeping below — refuse instead.
                        return Err(Error::InvalidBatchOperation(
                            "attempting to insert element that already exists",
                        ))
                        .wrap_with_cost(cost);
                    }
                    if !writes_over_existing {
                        continue;
                    }

                    let previous_needs_bookkeeping =
                        matches!(previous, Element::BidirectionalReference(..))
                            || previous
                                .backward_references()
                                .map(|refs| !refs.is_empty())
                                .unwrap_or(false);
                    expansion
                        .store
                        .stage(position.clone(), Some(element.clone()));
                    if !previous_needs_bookkeeping {
                        continue;
                    }
                    if element == previous {
                        // No logical change (the referrer list was just
                        // merged from the stored element, so the comparison
                        // covers the full stored form): nothing propagates —
                        // mirroring the live flow's delta gate.
                        continue;
                    }
                    let plan = cost_return_on_error!(
                        &mut cost,
                        plan_element_update(
                            &expansion.store,
                            &path,
                            &key,
                            previous,
                            Some(element.clone())
                        )
                    );
                    cost_return_on_error!(&mut cost, expansion.apply_mutations(plan.mutations));
                } else {
                    // Fresh insert: no bookkeeping (fresh family items get
                    // their empty authoritative list above).
                    expansion.store.stage(position, Some(element));
                }
            }
            GroveOp::Delete | GroveOp::DeleteTree(..) => {
                let previous =
                    cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key));
                expansion.store.stage(position, None);
                let Some(previous) = previous else { continue };
                let needs_bookkeeping = matches!(previous, Element::BidirectionalReference(..))
                    || (is_family_item(&previous)
                        && previous
                            .backward_references()
                            .map(|refs| !refs.is_empty())
                            .unwrap_or(false));
                if !needs_bookkeeping {
                    continue;
                }
                let plan = cost_return_on_error!(
                    &mut cost,
                    plan_element_update(&expansion.store, &path, &key, previous, None)
                );
                cost_return_on_error!(&mut cost, expansion.apply_mutations(plan.mutations));
            }
            GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode,
                flags,
                ..
            } => {
                let previous =
                    cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key));
                if matches!(previous, Some(Element::BidirectionalReference(..))) {
                    // M4: refreshing a bidirectional reference is rejected —
                    // a refresh rewrites the node without the registration /
                    // propagation bookkeeping. Re-insert the reference
                    // through a flagged op instead.
                    return Err(Error::NotSupported(
                        "RefreshReference cannot target a bidirectional reference; re-insert \
                         the reference through a flagged batch operation instead"
                            .to_owned(),
                    ))
                    .wrap_with_cost(cost);
                }
                // Trusted variants overwrite the stored path; stage the
                // rebuilt shape so later resolutions follow the new edge.
                // Untrusted variants keep the stored path — the DB state
                // is already what resolution should see.
                use super::RefreshReferenceMode;
                match mode {
                    RefreshReferenceMode::PlainReferenceTrusted => {
                        expansion.store.stage(
                            position,
                            Some(Element::Reference(
                                reference_path_type.clone(),
                                *max_reference_hop,
                                flags.clone(),
                            )),
                        );
                    }
                    RefreshReferenceMode::SumItemReferenceTrusted(sum) => {
                        expansion.store.stage(
                            position,
                            Some(Element::ReferenceWithSumItem(
                                reference_path_type.clone(),
                                *max_reference_hop,
                                *sum,
                                flags.clone(),
                            )),
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Pass 2: `BidirectionalReference` ops, targets before referrers, so
    // in-batch chains resolve through the overlay and every insertion's
    // component budget is measured against the prospective post-batch
    // state. (Cycles among pending references get an arbitrary order; their
    // planning then fails on resolution, which is the right outcome.)
    let ordered_bidi_ops = order_reference_ops_targets_first(&expansion.ops, &bidi_op_indices);

    for index in ordered_bidi_ops {
        let Some((path, key, op_kind)) = expansion.ops[index].as_ref().and_then(|op| {
            op.key
                .as_ref()
                .map(|k| (op.path.to_path(), k.get_key_clone(), op.op.clone()))
        }) else {
            continue;
        };

        let reference = match &op_kind {
            GroveOp::InsertOrReplace { element }
            | GroveOp::Replace { element }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                let Element::BidirectionalReference(reference) = element else {
                    unreachable!("collected as a bidirectional-reference op");
                };
                reference.clone()
            }
            GroveOp::Patch { .. } => {
                return Err(Error::NotSupported(
                    "Patch operations cannot carry bidirectional references".to_owned(),
                ))
                .wrap_with_cost(cost);
            }
            _ => unreachable!("collected as a bidirectional-reference op"),
        };

        // Per-kind gating against the sequential previous state.
        let previous = cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key));
        match &op_kind {
            GroveOp::InsertIfNotExists {
                error_if_exists, ..
            } if previous.is_some() => {
                if *error_if_exists || expansion.validate_insertion_does_not_override {
                    return Err(Error::InvalidBatchOperation(
                        "attempting to insert element that already exists",
                    ))
                    .wrap_with_cost(cost);
                }
                // Writes nothing; the op dissolves.
                expansion.ops[index] = None;
                continue;
            }
            GroveOp::InsertWithKnownToNotAlreadyExist { .. } if previous.is_some() => {
                return Err(Error::InvalidBatchOperation(
                    "attempting to insert element that already exists",
                ))
                .wrap_with_cost(cost);
            }
            GroveOp::InsertOrReplace { .. } | GroveOp::Replace { .. }
                if previous.is_some() && expansion.validate_insertion_does_not_override =>
            {
                return Err(Error::InvalidBatchOperation(
                    "attempting to insert element that already exists",
                ))
                .wrap_with_cost(cost);
            }
            GroveOp::Replace { .. } if previous.is_none() => {
                return Err(Error::InvalidBatchOperation(
                    "attempting to replace an element that does not exist",
                ))
                .wrap_with_cost(cost);
            }
            _ => {}
        }

        // M4: a reference and its target's deletion cannot share a batch.
        if let Some(target_position) =
            first_hop_position(&reference.forward_reference_path, &path, &key)
            && expansion.user_deleted_positions.contains(&target_position)
        {
            return Err(Error::InvalidBatchOperation(
                "a bidirectional reference cannot be inserted in the same batch that deletes \
                 its target",
            ))
            .wrap_with_cost(cost);
        }

        let plan = cost_return_on_error!(
            &mut cost,
            plan_reference_insertion(&expansion.store, &path, &key, reference)
        );

        // The op is consumed either way: an identical edge dissolves (any
        // derived rewrite staged earlier for this position stays — it
        // carries a propagated end hash); a real plan replaces it with the
        // derived form via its primary write.
        expansion.ops[index] = None;

        if let Some(plan) = plan {
            cost_return_on_error!(&mut cost, expansion.apply_mutations(plan.mutations));
        }
    }

    let mut expanded: Vec<QualifiedGroveDbOp> = expansion.ops.into_iter().flatten().collect();
    expanded.extend(expansion.derived.into_values());
    Ok(expanded).wrap_with_cost(cost)
}

/// The qualified position of a reference's first hop, when the path type is
/// resolvable syntactically. `None` falls back to plan-time resolution
/// errors.
fn first_hop_position(
    reference_path: &ReferencePathType,
    path: &[Vec<u8>],
    key: &[u8],
) -> Option<Position> {
    let qualified = path_from_reference_path_type(reference_path.clone(), path, Some(key)).ok()?;
    let (target_key, target_path) = qualified.split_last()?;
    Some((target_path.to_vec(), target_key.clone()))
}

/// Order the pending `BidirectionalReference` ops so that every op whose
/// forward edge targets another pending op's position comes AFTER that op
/// (targets first). Cycles keep their relative user order.
fn order_reference_ops_targets_first(
    ops: &[Option<QualifiedGroveDbOp>],
    bidi_op_indices: &[usize],
) -> Vec<usize> {
    let mut position_to_index: HashMap<Position, usize> = HashMap::new();
    let mut forward_target: HashMap<usize, Option<Position>> = HashMap::new();

    for &index in bidi_op_indices {
        let Some(op) = ops[index].as_ref() else {
            continue;
        };
        let Some(position) = Expansion::op_position(op) else {
            continue;
        };
        let reference = match &op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => match element {
                Element::BidirectionalReference(reference) => Some(reference),
                _ => None,
            },
            _ => None,
        };
        let target = reference
            .and_then(|r| first_hop_position(&r.forward_reference_path, &position.0, &position.1));
        position_to_index.insert(position, index);
        forward_target.insert(index, target);
    }

    let mut ordered = Vec::with_capacity(bidi_op_indices.len());
    let mut state: HashMap<usize, u8> = HashMap::new(); // 0/absent = new, 1 = visiting, 2 = done

    for &start in bidi_op_indices {
        // Iterative DFS along forward edges; emit post-order (target before
        // referrer). A back-edge (cycle) is skipped — the member order then
        // stays the user order, and planning reports the cycle.
        let mut stack = vec![(start, false)];
        while let Some((index, children_done)) = stack.pop() {
            if children_done {
                state.insert(index, 2);
                ordered.push(index);
                continue;
            }
            match state.get(&index) {
                Some(1) | Some(2) => continue,
                _ => {}
            }
            state.insert(index, 1);
            stack.push((index, true));
            if let Some(Some(target)) = forward_target.get(&index)
                && let Some(&target_index) = position_to_index.get(target)
                && !matches!(state.get(&target_index), Some(1) | Some(2))
            {
                stack.push((target_index, false));
            }
        }
    }

    ordered
}
