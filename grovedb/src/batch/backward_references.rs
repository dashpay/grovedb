//! The backward-references batch preprocessor (batching milestones M2–M4).
//!
//! With the default [`crate::BackwardsReferences::Check`],
//! user operations touching the backward-references family — the three ITEM
//! variants and `BidirectionalReference` itself — expand into the derived
//! operations the live flow would perform. The decisions come from
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
//! Planning commits referrers to the staged, caller-supplied bytes of their
//! terminal item. A third pass then settles those end hashes against the
//! bytes the apply will actually store, since the apply runs the caller's
//! flags update on every write over a stored value (see
//! [`Expansion::settle_end_hashes`]).
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
//!   rejected (re-insert the reference through an insert/replace op instead);
//! - registrations onto targets written in the same batch merge into the
//!   target op's element — but ONLY into an already-processed,
//!   guaranteed-to-execute family write (`InsertIfNotExists` over an
//!   existing key writes nothing and is dropped outright, so no rewrite
//!   can be folded into an op that never lands);
//! - a rewrite hitting a write op LATER in the canonical order is kept as
//!   a derived op and superseded when that op's own turn comes — its
//!   processing drops the pending derived op and plans the bookkeeping its
//!   own overwrite requires, preserving sequential semantics.

use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{removal::StorageRemovedBytes, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{costs::ElementCostExtensions, ElementExt},
    tree_type::TreeType,
    CryptoHash, Merk,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch};
use grovedb_version::version::GroveVersion;

pub(super) type PreparedMerks<'db> =
    HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext<'db>>>;

use super::{
    just_in_time_value_update::predict_provided_value_hash_put, key_info::KeyInfo, GroveOp,
    KeyInfoPath, QualifiedGroveDbOp,
};
use crate::{
    bidirectional_references::semantics::{
        plan_element_update, plan_propagation, plan_reference_insertion, ChainStore,
        DerivedMutation, Plan, Position, ResolvedPosition,
    },
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{path_from_reference_path_type, ReferencePathType},
    Element, ElementFlags, Error, GroveDb, Transaction,
};

/// Rounds the end-hash settling pass may take. A round re-propagates an
/// item only when its landed hash moved. Besides the referrer chain (which
/// holds no items), a propagation rewrites only the item itself, and only
/// to drop dangling referrer entries; that can move its landed hash once
/// more, and then nothing is left to drop. So three rounds settle any
/// batch (re-propagate, re-propagate after the drop, confirm); the rest is
/// margin before failing closed.
const MAX_END_HASH_SETTLING_ROUNDS: usize = 8;

/// [`ChainStore`] over the batch's prospective state: an overlay of staged
/// pending position states (what the batch has decided each position will
/// hold) falling back to the database at the batch's transaction snapshot.
/// Logical hashes come from the observed element bytes, avoiding a separate
/// value-hash lookup. Observed Merks remain cached for execution.
pub(super) struct OverlayChainStore<'db, 'g> {
    db: &'db GroveDb,
    batch: &'db StorageBatch,
    merks: RefCell<PreparedMerks<'db>>,
    /// Committed (pre-batch) state read so far: the element and the exact
    /// bytes it is stored as.
    observed: RefCell<HashMap<Position, Option<(Element, Vec<u8>)>>>,
    tx: &'db Transaction<'db>,
    version: &'g GroveVersion,
    /// Staged pending state: `Some(element)` = the batch writes this,
    /// `None` = the batch deletes the position. Absent = untouched.
    overlay: RefCell<HashMap<Position, Option<Element>>>,
    /// Qualified paths of subtrees whose prospective content is defined by
    /// the batch alone: a tree element written where committed storage held
    /// no tree. Reads beneath them must not touch committed storage — the
    /// staged parent does not exist there yet, and opening it would fail
    /// the whole (otherwise valid) batch.
    fresh_subtrees: RefCell<HashSet<Vec<Vec<u8>>>>,
    /// DECLARED final edges of `BidirectionalReference` ops deferred to
    /// pass 2 and not yet planned: the prospective component budget must
    /// validate against these (a stored ancestor's budget may be raised,
    /// or its edge retargeted away, in the same unordered batch). Entries
    /// are removed as their ops get planned (or dissolve), after which the
    /// overlay carries the authoritative staged state.
    pending_references: RefCell<HashMap<Position, (ReferencePathType, Option<u8>)>>,
}

impl<'db, 'g> OverlayChainStore<'db, 'g> {
    fn new(
        db: &'db GroveDb,
        tx: &'db Transaction<'db>,
        batch: &'db StorageBatch,
        version: &'g GroveVersion,
    ) -> Self {
        Self {
            db,
            batch,
            merks: RefCell::new(HashMap::new()),
            observed: RefCell::new(HashMap::new()),
            tx,
            version,
            overlay: RefCell::new(HashMap::new()),
            fresh_subtrees: RefCell::new(HashSet::new()),
            pending_references: RefCell::new(HashMap::new()),
        }
    }

    /// Record the declared final edge of a pass-2 reference op.
    fn stage_pending_reference(
        &self,
        position: Position,
        forward: ReferencePathType,
        max_hop: Option<u8>,
    ) {
        self.pending_references
            .borrow_mut()
            .insert(position, (forward, max_hop));
    }

    /// Remove a pending declaration once its op is planned or dissolves.
    fn clear_pending_reference(&self, position: &Position) {
        self.pending_references.borrow_mut().remove(position);
    }

    /// Stage the pending state of a position.
    fn stage(&self, position: Position, element: Option<Element>) {
        self.overlay.borrow_mut().insert(position, element);
    }

    /// Record that the subtree at `qualified` is created by this batch with
    /// no committed counterpart (fresh — its prospective content is only
    /// what later ops stage under it).
    fn stage_fresh_subtree(&self, qualified: Vec<Vec<u8>>) {
        self.fresh_subtrees.borrow_mut().insert(qualified);
    }

    /// Whether `path` lies at or below a batch-created fresh subtree.
    fn under_fresh_subtree(&self, path: &[Vec<u8>]) -> bool {
        let fresh = self.fresh_subtrees.borrow();
        (1..=path.len()).any(|i| fresh.contains(&path[..i]))
    }

    /// The committed (pre-batch) element at a position and the exact bytes
    /// it is stored as — what the apply finds there, since a batch writes
    /// each position once. `None` for an absent key, and beneath a subtree
    /// this batch creates (committed storage holds nothing there).
    fn committed_at(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
    ) -> CostResult<Option<(Element, Vec<u8>)>, Error> {
        if self.under_fresh_subtree(path) {
            return Ok(None).wrap_with_cost(OperationCost::default());
        }
        let position = (path.to_vec(), key.to_vec());
        if let Some(observed) = self.observed.borrow().get(&position) {
            return Ok(observed.clone()).wrap_with_cost(OperationCost::default());
        }
        let mut cost = OperationCost::default();
        let mut merks = self.merks.borrow_mut();
        if !merks.contains_key(path) {
            let merk = cost_return_on_error!(
                &mut cost,
                self.db.open_batch_transactional_merk_at_path(
                    self.batch,
                    SubtreePath::from(path),
                    self.tx,
                    false,
                    self.version,
                )
            );
            merks.insert(path.to_vec(), merk);
        }
        let merk = merks.get(path).expect("opened above");
        let mut observed = None;
        cost_return_on_error!(
            &mut cost,
            merk.observe_old_value(
                key,
                Some(&Element::value_defined_cost_for_serialized_value),
                &mut |_, bytes| {
                    observed = Some(
                        Element::deserialize(bytes, self.version)
                            .map(|element| (element, bytes.to_vec())),
                    );
                },
                self.version,
            )
            .map_err(Error::MerkError)
        );
        let observed = match observed.transpose() {
            Ok(value) => value,
            Err(error) => return Err(error.into()).wrap_with_cost(cost),
        };
        self.observed
            .borrow_mut()
            .insert(position, observed.clone());
        Ok(observed).wrap_with_cost(cost)
    }

    /// The tree type of the subtree at `path`, when planning opened its
    /// Merk (the one the apply reuses).
    fn opened_tree_type(&self, path: &[Vec<u8>]) -> Option<TreeType> {
        self.merks.borrow().get(path).map(|merk| merk.tree_type)
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
        // Below a subtree this batch creates, committed storage has nothing
        // — not even the parent tree. Everything that exists there is in
        // the overlay (checked above).
        self.committed_at(path, key)
            .map_ok(|committed| committed.map(|(element, _)| element))
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
                let node_value_hash = element
                    .logical_value_hash(self.version)
                    .unwrap_add_cost(&mut cost)?;
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
                Element::BidirectionalReference(reference, _) => {
                    current = (position.0, position.1, reference.forward_reference_path);
                }
                Element::Reference(reference_path, ..)
                | Element::ReferenceWithSumItem(reference_path, ..) => {
                    current = (position.0, position.1, reference_path);
                }
                element => {
                    let node_value_hash = match element
                        .logical_value_hash(self.version)
                        .unwrap_add_cost(&mut cost)
                    {
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

    fn pending_reference_at(
        &self,
        path: &[Vec<u8>],
        key: &[u8],
    ) -> Option<(ReferencePathType, Option<u8>)> {
        self.pending_references
            .borrow()
            .get(&(path.to_vec(), key.to_vec()))
            .cloned()
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
    /// into the batch per the M4 conflict rules. `current_index` is the
    /// position in the canonical sequential order of the op whose plan
    /// these mutations come from (`usize::MAX` in pass 2, where every
    /// non-reference op has been processed).
    fn apply_mutations(
        &mut self,
        mutations: Vec<DerivedMutation>,
        current_index: usize,
    ) -> CostResult<(), Error> {
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
                    // A rewrite may fold into the user op's payload ONLY
                    // when that op has already been processed in the
                    // canonical order, is guaranteed to execute, and holds
                    // a family element the rewrite semantically extends
                    // (op element + registration/cleanup). Everything else
                    // stays a separate derived op:
                    // - an unprocessed op (a later overwrite, or a pass-2
                    //   BidirectionalReference) supersedes the rewrite when
                    //   its own turn comes — folding would either discard
                    //   the caller's payload or resurrect a write the
                    //   caller replaced;
                    // - conditional inserts that turned out not to execute
                    //   are dropped at processing time, so a retained op
                    //   here always writes.
                    //
                    // The CURRENTLY processed op counts as processed
                    // (`<=`): its own plan may clean a stale referrer
                    // entry off the very element it writes (a dangling
                    // registration on the overwritten family item), and
                    // that cleanup must fold into the op rather than
                    // become a second op on the same position.
                    let mergeable_into_user_op = retained_user_op
                        .filter(|&index| index <= current_index)
                        .filter(|&index| {
                            let user_op = self.ops[index].as_ref().expect("retained above");
                            match &user_op.op {
                                GroveOp::InsertOrReplace {
                                    element: op_element,
                                }
                                | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences {
                                    element: op_element,
                                }
                                | GroveOp::Replace {
                                    element: op_element,
                                }
                                | GroveOp::ReplaceDontCheckForBackwardsReferences {
                                    element: op_element,
                                }
                                | GroveOp::Patch {
                                    element: op_element,
                                    ..
                                }
                                | GroveOp::PatchDontCheckForBackwardsReferences {
                                    element: op_element,
                                    ..
                                }
                                | GroveOp::InsertIfNotExists {
                                    element: op_element,
                                    ..
                                }
                                | GroveOp::InsertWithKnownToNotAlreadyExist {
                                    element: op_element,
                                } => is_family_item(op_element),
                                _ => false,
                            }
                        });
                    if let Some(index) = mergeable_into_user_op {
                        let user_op = self.ops[index].as_mut().expect("retained above");
                        match &mut user_op.op {
                            GroveOp::InsertOrReplace {
                                element: op_element,
                            }
                            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences {
                                element: op_element,
                            }
                            | GroveOp::Replace {
                                element: op_element,
                            }
                            | GroveOp::ReplaceDontCheckForBackwardsReferences {
                                element: op_element,
                            }
                            | GroveOp::Patch {
                                element: op_element,
                                ..
                            }
                            | GroveOp::PatchDontCheckForBackwardsReferences {
                                element: op_element,
                                ..
                            }
                            | GroveOp::InsertIfNotExists {
                                element: op_element,
                                ..
                            }
                            | GroveOp::InsertWithKnownToNotAlreadyExist {
                                element: op_element,
                            } => *op_element = element.clone(),
                            _ => unreachable!("filtered to family-write kinds above"),
                        }
                        self.store.stage(position, Some(element));
                    } else if let Some(index) = retained_user_op {
                        // Retained but not mergeable: allowed only for
                        // write kinds whose own processing supersedes this
                        // rewrite (unprocessed writes and pass-2 reference
                        // ops). Deletes and refreshes stay fail-closed.
                        let user_op = self.ops[index].as_ref().expect("retained above");
                        let (colliding_write, payload_is_bidi) = match &user_op.op {
                            GroveOp::InsertOrReplace { element }
                            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { element }
                            | GroveOp::Replace { element }
                            | GroveOp::ReplaceDontCheckForBackwardsReferences { element }
                            | GroveOp::Patch { element, .. }
                            | GroveOp::PatchDontCheckForBackwardsReferences { element, .. }
                            | GroveOp::InsertIfNotExists { element, .. }
                            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                                (true, matches!(element, Element::BidirectionalReference(..)))
                            }
                            _ => (false, false),
                        };
                        // A BidirectionalReference payload is processed in
                        // pass 2 whatever its index, so it always counts as
                        // still-to-come here.
                        let processed = index < current_index && !payload_is_bidi;
                        if !colliding_write || processed {
                            return Err(Error::InvalidBatchOperation(
                                "a derived backward-references rewrite conflicts with \
                                 another operation in the batch",
                            ))
                            .wrap_with_cost(cost);
                        }
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
                                    end_hash,
                                },
                            },
                        );
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
                                    end_hash,
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

impl<'db, 'g> Expansion<'db, 'g> {
    /// Settle every referrer's end hash against the bytes its terminal item
    /// will actually store.
    ///
    /// The planners commit referrers to the logical hash of the item AS
    /// STAGED — the bytes the caller supplied. When the apply writes a
    /// flagged item over a stored value, it first runs the caller's
    /// just-in-time flags update, which may rewrite the flags and with
    /// them the logical hash (Drive's epoch-based storage flags do on
    /// every cross-epoch update: reset to the old flags on a same-size
    /// write, merge into a multi-epoch map when it grows, combine removed
    /// bytes when it shrinks). Merk rehashes the item's own node from its
    /// final bytes, but its referrers would keep the supplied hash.
    ///
    /// So once planning is complete — every registration folded in, since
    /// the flags a callback derives can depend on the item's full size —
    /// this pass predicts each such item's final bytes through the same
    /// update Merk runs, and re-propagates the landed logical hash along
    /// its referrer chains wherever it differs from the one committed to.
    fn settle_end_hashes<G, SR>(
        &mut self,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<(), Error>
    where
        G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();
        let version = self.store.version;
        // Per re-propagated item: the hash its referrers now commit to.
        let mut settled: HashMap<Position, CryptoHash> = HashMap::new();
        for _ in 0..MAX_END_HASH_SETTLING_ROUNDS {
            // Only a flagged item with referrers can move a committed hash:
            // the update leaves unflagged bytes as supplied.
            let mut items: Vec<(Position, Element)> = self
                .store
                .overlay
                .borrow()
                .iter()
                .filter_map(|(position, staged)| match staged {
                    Some(element)
                        if is_family_item(element)
                            && element.get_flags().is_some()
                            && element
                                .backward_references()
                                .is_some_and(|refs| !refs.is_empty()) =>
                    {
                        Some((position.clone(), element.clone()))
                    }
                    _ => None,
                })
                .collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));

            let mut repropagated = false;
            for (position, element) in items {
                let (path, key) = &position;
                // No stored predecessor: the apply runs no just-in-time
                // update and stores the bytes as supplied.
                let Some((_, old_bytes)) =
                    cost_return_on_error!(&mut cost, self.store.committed_at(path, key))
                else {
                    continue;
                };
                let in_tree_type = cost_return_on_error_no_add!(
                    cost,
                    self.store
                        .opened_tree_type(path)
                        .ok_or(Error::CorruptedCodeExecution(
                            "a committed backward-references item's subtree was not opened",
                        ))
                );
                let landed = cost_return_on_error_no_add!(
                    cost,
                    predict_provided_value_hash_put(
                        key,
                        &old_bytes,
                        &element,
                        in_tree_type,
                        flags_update,
                        split_removal_bytes,
                        version,
                    )
                );
                let already_settled = settled.get(&position).copied();
                if already_settled.is_none() && landed == element {
                    // Stored as staged: the planned hashes hold.
                    continue;
                }
                let landed_hash = cost_return_on_error!(
                    &mut cost,
                    landed.logical_value_hash(version).map_err(Error::from)
                );
                let committed_hash = match already_settled {
                    Some(hash) => hash,
                    None => cost_return_on_error!(
                        &mut cost,
                        element.logical_value_hash(version).map_err(Error::from)
                    ),
                };
                if landed_hash == committed_hash {
                    continue;
                }
                // Re-propagate from the STAGED element: the referrer list
                // is what the apply writes (the update only touches flags).
                let mut plan = Plan::default();
                cost_return_on_error!(
                    &mut cost,
                    plan_propagation(&self.store, &mut plan, path, key, element, landed_hash)
                );
                cost_return_on_error!(&mut cost, self.apply_mutations(plan.mutations, usize::MAX));
                settled.insert(position, landed_hash);
                repropagated = true;
            }
            if !repropagated {
                return Ok(()).wrap_with_cost(cost);
            }
        }
        Err(Error::CorruptedCodeExecution(
            "backward-references end hashes did not settle against the flags update",
        ))
        .wrap_with_cost(cost)
    }
}

/// Expand `ops` with the derived operations the backward-references rules
/// require, per the module documentation.
///
/// `flags_update` and `split_removal_bytes` are the caller's just-in-time
/// callbacks the apply will run; the expansion consults them to predict the
/// bytes a flagged family item finally stores (see
/// [`Expansion::settle_end_hashes`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn expand_backward_references_ops<'db, G, SR>(
    db: &'db GroveDb,
    tx: &'db Transaction<'db>,
    batch: &'db StorageBatch,
    ops: Vec<QualifiedGroveDbOp>,
    validate_insertion_does_not_override: bool,
    flags_update: &mut G,
    split_removal_bytes: &mut SR,
    grove_version: &GroveVersion,
) -> CostResult<(Vec<QualifiedGroveDbOp>, PreparedMerks<'db>), Error>
where
    G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
    SR: FnMut(
        &mut ElementFlags,
        u32,
        u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
{
    let mut cost = OperationCost::default();

    let mut expansion = Expansion {
        store: OverlayChainStore::new(db, tx, batch, grove_version),
        user_index_by_position: HashMap::new(),
        user_deleted_positions: HashSet::new(),
        derived: BTreeMap::new(),
        validate_insertion_does_not_override,
        ops: Vec::new(),
    };

    expansion.ops = ops.into_iter().map(Some).collect();

    // Fresh-subtree pre-scan: a tree written where committed storage holds
    // no tree defines a subtree whose prospective content exists only in
    // the overlay. Marked BEFORE any op processing — shallowest paths
    // first, so a nested new tree sees its parent already marked — because
    // the batch is unordered: an op under such a subtree may appear before
    // the op creating it, and its previous-state read must not touch
    // committed storage (the parent does not exist there).
    let mut tree_write_positions: Vec<Position> = expansion
        .ops
        .iter()
        .flatten()
        .filter_map(|op| match &op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Replace { element }
            | GroveOp::ReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::PatchDontCheckForBackwardsReferences { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element }
                if element.is_any_tree() =>
            {
                Expansion::op_position(op)
            }
            _ => None,
        })
        .collect();
    tree_write_positions.sort_by_key(|(path, _)| path.len());
    for (path, key) in tree_write_positions {
        let previous_is_tree = if expansion.store.under_fresh_subtree(&path) {
            false
        } else {
            cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key))
                .map(|p| p.is_any_tree())
                .unwrap_or(false)
        };
        if !previous_is_tree {
            let mut qualified = path;
            qualified.push(key);
            expansion.store.stage_fresh_subtree(qualified);
        }
    }

    // Observe first, without applying reference-specific conflict or conditional
    // write rules. Ordinary batches keep the executor's original operation set,
    // including its behavior when consistency checking is disabled.
    let mut needs_reference_planning = false;
    let user_deleted_positions: HashSet<_> = expansion
        .ops
        .iter()
        .flatten()
        .filter(|op| {
            matches!(
                op.op,
                GroveOp::Delete
                    | GroveOp::DeleteDontCheckForBackwardsReferences
                    | GroveOp::DeleteTree(..)
                    | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..)
            )
        })
        .filter_map(Expansion::op_position)
        .collect();
    for op in expansion.ops.iter().flatten() {
        let Some((path, key)) = Expansion::op_position(op) else {
            continue;
        };
        let new_element = match &op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Replace { element }
            | GroveOp::ReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::PatchDontCheckForBackwardsReferences { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => Some(element),
            GroveOp::Delete
            | GroveOp::DeleteDontCheckForBackwardsReferences
            | GroveOp::DeleteTree(..)
            | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..)
            | GroveOp::RefreshReference { .. } => None,
            _ => continue,
        };
        let previous = cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key));
        // The displaced value is in hand, so a `DontCheck` claim costs
        // nothing to check and fails closed.
        let previous_participates = previous
            .as_ref()
            .is_some_and(Element::supports_backward_references);
        // A conditional insert over an existing key writes nothing (or fails
        // for existing), so it displaces nothing to check.
        if previous_participates
            && !op.op.backwards_references().should_check()
            && !matches!(op.op, GroveOp::InsertIfNotExists { .. })
        {
            return Err(Error::NotSupported(
                "a DontCheckForBackwardsReferences op displaces a value that takes part in backward references; use \
                 the checked op for maintenance"
                    .to_owned(),
            ))
            .wrap_with_cost(cost);
        }
        needs_reference_planning |=
            new_element.is_some_and(Element::supports_backward_references) || previous_participates;
        // A removal whose contents nothing in this batch reads is scanned for
        // participants when the op declares there may be some: `Delete` on a
        // populated tree and a tree replacement. Every `DeleteTree` is
        // exempt: `DeleteChildren`, `Error` and `Skip` are checked on the
        // cleanup walk that already decodes the contents, and
        // `DontCheckWithNoCleanup` declares that the batch's own deletes,
        // each planned here, emptied the subtree.
        let removes_unread_subtree = op.op.backwards_references().should_check()
            && !matches!(
                op.op,
                GroveOp::InsertIfNotExists { .. }
                    | GroveOp::RefreshReference { .. }
                    | GroveOp::DeleteTree(..)
                    | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..)
            );
        if removes_unread_subtree
            && previous.as_ref().is_some_and(|old| {
                old.is_any_tree()
                    && !old.uses_non_merk_data_storage()
                    && old
                        .root_key_and_tree_type()
                        .is_some_and(|(root, _)| root.is_some())
            })
        {
            let mut qualified = path;
            qualified.push(key);
            let participants = cost_return_on_error!(
                &mut cost,
                db.backward_reference_participants(&qualified, tx, grove_version)
            );
            if matches!(
                op.op,
                GroveOp::Delete | GroveOp::DeleteDontCheckForBackwardsReferences
            ) {
                if participants.iter().any(|(path, key, _)| {
                    !user_deleted_positions.contains(&(path.clone(), key.clone()))
                }) {
                    return Err(Error::NotSupported(
                        "a batch subtree deletion must explicitly delete its backward-reference participants; use live delete for recursive maintenance".to_owned(),
                    )).wrap_with_cost(cost);
                }
            } else if !participants.is_empty() {
                return Err(Error::NotSupported(
                    "delete a subtree containing backward-reference participants before replacing it".to_owned(),
                )).wrap_with_cost(cost);
            }
        }
    }
    if !needs_reference_planning {
        return Ok((
            expansion.ops.into_iter().flatten().collect(),
            expansion.store.merks.into_inner(),
        ))
        .wrap_with_cost(cost);
    }

    for (index, op) in expansion.ops.iter().flatten().enumerate() {
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
            if matches!(
                op.op,
                GroveOp::Delete
                    | GroveOp::DeleteDontCheckForBackwardsReferences
                    | GroveOp::DeleteTree(..)
                    | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..)
            ) {
                expansion.user_deleted_positions.insert(position);
            }
        }
    }
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
            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Replace { element }
            | GroveOp::ReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::PatchDontCheckForBackwardsReferences { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                if let Element::BidirectionalReference(reference, _) = element {
                    // A conditional insert whose gate will SKIP it must not
                    // advertise a pending edge: `InsertIfNotExists` over an
                    // existing position writes nothing, so the STORED edge
                    // stays authoritative for prospective-component checks.
                    // (One op per position, and cascades refuse user-op
                    // positions, so existence here is stable through
                    // planning.) An erroring gate fails the whole batch
                    // anyway, making its staleness irrelevant.
                    let will_write = if matches!(
                        op_kind,
                        GroveOp::InsertIfNotExists { .. }
                            | GroveOp::InsertWithKnownToNotAlreadyExist { .. }
                    ) {
                        cost_return_on_error!(&mut cost, expansion.store.element_at(&path, &key))
                            .is_none()
                    } else {
                        true
                    };
                    if will_write {
                        expansion.store.stage_pending_reference(
                            position.clone(),
                            reference.forward_reference_path.clone(),
                            reference.max_hop,
                        );
                    }
                    bidi_op_indices.push(index);
                    continue;
                }
                let mut element = element.clone();
                let writes_over_existing = matches!(
                    op_kind,
                    GroveOp::InsertOrReplace { .. }
                        | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { .. }
                        | GroveOp::Replace { .. }
                        | GroveOp::ReplaceDontCheckForBackwardsReferences { .. }
                        | GroveOp::Patch { .. }
                        | GroveOp::PatchDontCheckForBackwardsReferences { .. }
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
                            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences {
                                element: e,
                            }
                            | GroveOp::Replace { element: e }
                            | GroveOp::ReplaceDontCheckForBackwardsReferences { element: e }
                            | GroveOp::Patch { element: e, .. }
                            | GroveOp::PatchDontCheckForBackwardsReferences {
                                element: e, ..
                            }
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
                            if !element.supports_backward_references() {
                                // Let the ordinary executor preserve its type-specific refusal.
                                continue;
                            }
                            return Err(Error::InvalidBatchOperation(
                                "attempting to insert element that already exists",
                            ))
                            .wrap_with_cost(cost);
                        }
                        // InsertIfNotExists over an existing key writes
                        // nothing — for ANY payload. Drop the op entirely
                        // so a later derived rewrite of the position (e.g.
                        // a registration on the stored element) lands as a
                        // derived op instead of being folded into an op
                        // that never executes and silently swallowed.
                        expansion.ops[index] = None;
                        continue;
                    }
                    if is_known_new
                        && (element.supports_backward_references()
                            || previous.supports_backward_references())
                    {
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
                    // This write comes LATER in the canonical order than
                    // any derived rewrite already recorded for the
                    // position (an earlier op's propagation): the write
                    // supersedes it, and this op's own planning below
                    // handles the bookkeeping the displaced element needs.
                    expansion.derived.remove(&position);
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
                    cost_return_on_error!(
                        &mut cost,
                        expansion.apply_mutations(plan.mutations, index)
                    );
                } else {
                    // Fresh insert: no bookkeeping (fresh family items get
                    // their empty authoritative list above).
                    expansion.store.stage(position, Some(element));
                }
            }
            GroveOp::Delete
            | GroveOp::DeleteDontCheckForBackwardsReferences
            | GroveOp::DeleteTree(..)
            | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..) => {
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
                cost_return_on_error!(&mut cost, expansion.apply_mutations(plan.mutations, index));
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
                if previous
                    .as_ref()
                    .is_some_and(Element::supports_backward_references)
                {
                    // M4: refreshing a bidirectional reference is rejected —
                    // a refresh rewrites the node without the registration /
                    // propagation bookkeeping. Re-insert the reference
                    // through an insert/replace op instead.
                    return Err(Error::NotSupported(
                        "RefreshReference cannot target a bidirectional reference; re-insert \
                         the reference through a batch insert/replace operation instead"
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

        let (reference, reference_flags) = match &op_kind {
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Replace { element }
            | GroveOp::ReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                let Element::BidirectionalReference(reference, flags) = element else {
                    unreachable!("collected as a bidirectional-reference op");
                };
                (reference.clone(), flags.clone())
            }
            GroveOp::Patch { .. } | GroveOp::PatchDontCheckForBackwardsReferences { .. } => {
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
                // Writes nothing; the op dissolves — the STORED edge is
                // authoritative again for prospective-component checks.
                expansion.store.clear_pending_reference(&(path, key));
                expansion.ops[index] = None;
                continue;
            }
            GroveOp::InsertWithKnownToNotAlreadyExist { .. } if previous.is_some() => {
                return Err(Error::InvalidBatchOperation(
                    "attempting to insert element that already exists",
                ))
                .wrap_with_cost(cost);
            }
            GroveOp::InsertOrReplace { .. }
            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { .. }
            | GroveOp::Replace { .. }
            | GroveOp::ReplaceDontCheckForBackwardsReferences { .. }
                if previous.is_some() && expansion.validate_insertion_does_not_override =>
            {
                return Err(Error::InvalidBatchOperation(
                    "attempting to insert element that already exists",
                ))
                .wrap_with_cost(cost);
            }
            GroveOp::Replace { .. } | GroveOp::ReplaceDontCheckForBackwardsReferences { .. }
                if previous.is_none() =>
            {
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

        // The op is being planned NOW: its declaration graduates from
        // "pending" — its own upstream walk must read the state around it,
        // and after planning the overlay carries its staged element.
        expansion
            .store
            .clear_pending_reference(&(path.clone(), key.clone()));
        let plan = cost_return_on_error!(
            &mut cost,
            plan_reference_insertion(&expansion.store, &path, &key, reference, reference_flags)
        );

        // The op is consumed either way: an identical edge dissolves (any
        // derived rewrite staged earlier for this position stays — it
        // carries a propagated end hash); a real plan replaces it with the
        // derived form via its primary write.
        expansion.ops[index] = None;

        if let Some(plan) = plan {
            cost_return_on_error!(
                &mut cost,
                expansion.apply_mutations(plan.mutations, usize::MAX)
            );
        }
    }

    // Pass 3: every end hash above was planned against the STAGED bytes;
    // settle them against what the apply's flags update will store.
    cost_return_on_error!(
        &mut cost,
        expansion.settle_end_hashes(flags_update, split_removal_bytes)
    );

    let mut expanded: Vec<QualifiedGroveDbOp> = expansion.ops.into_iter().flatten().collect();
    expanded.extend(expansion.derived.into_values());
    Ok((expanded, expansion.store.merks.into_inner())).wrap_with_cost(cost)
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
            | GroveOp::InsertOrReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Replace { element }
            | GroveOp::ReplaceDontCheckForBackwardsReferences { element }
            | GroveOp::Patch { element, .. }
            | GroveOp::PatchDontCheckForBackwardsReferences { element, .. }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => match element {
                Element::BidirectionalReference(reference, _) => Some(reference),
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
