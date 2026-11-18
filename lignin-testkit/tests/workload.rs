//! Acceptance tests for the workload generators (BASE-01).
//!
//! - The same seed gives the same stream on any machine:
//!   `streams_are_pinned`, `a_suite_is_pinned_for_every_grove_version` and
//!   `the_reference_documents_stream_is_pinned` compare stream
//!   digests with values written down here.
//! - Every `Element` variant and `TreeType` is produced at least once:
//!   `the_suite_produces_every_element_variant_and_tree_type`. The lists it
//!   checks against come from an exhaustive `match`, so a variant added to
//!   GroveDB stops the crate compiling until it is named and then fails that
//!   test until a workload produces it.
//! - Each operation that reaches storage in a way of its own is produced at
//!   least once: `the_suite_produces_every_operation`, for the suite of the
//!   latest grove version and, for every other grove version, for exactly
//!   the operations that version has.
//! - The same holds for the remaining operations that charge a cost of
//!   their own, which are part of the same list. `tests/surface.rs` checks
//!   the list that names every public GroveDB operation as produced or as
//!   left out.
//! - The streams are ones GroveDB accepts:
//!   `every_workload_applies_on_every_grove_version`, and for more seeds
//!   `the_operation_workloads_apply_for_many_seeds`. What an operation
//!   returns is checked on the way: see [`run`].

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    thread,
};

use grovedb::{
    operations::{
        proof::{SumBudgetStop, VerifiedPathQuery},
        QueryItemOrSumReturnType,
    },
    query_result_type::{PathKeyElementTrio, QueryResultType},
    AxisAggregateValue, Element, GroveDb, LeafInfo, PathBranchChunkQuery, PathQuery, PathQueryRun,
    PathTrunkChunkQuery, Transaction,
};
use grovedb_commitment_tree::{ciphertext_payload_size, DashMemo};
use grovedb_costs::{
    storage_cost::removal::{StorageRemovedBytes, UNKNOWN_EPOCH},
    OperationCost,
};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_merk::{
    element::costs::ElementCostExtensions, merk::NodeType, proofs::query::ReadMode, tree::kv::KV,
};
use grovedb_storage::{rocksdb_storage::RocksDbStorage, Storage, StorageContext};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::{
    cost_model::rules,
    workload::{
        adversarial::{
            ChunkProofs, DeepNesting, DeleteHeavy, FlagEpochs, FlatDrops, IndexedWrites,
            LargeValues, LongKeys, Moves, Pagination, PartialBatches, ReferenceChains,
            TinySubtrees, TreeDeletes, TypedReads, MAX_LADDER_RUNGS, MAX_NESTING_DEPTH,
            MIN_CHUNKED_ROWS, MIN_TREE_DELETE_ITEMS,
        },
        apply,
        coverage::{
            Operation, ELEMENT_VARIANTS, OPERATION_GROUPS, REFERENCE_PATH_TYPES, TREE_TYPES,
        },
        drive::{
            Balances, BlockExecution, ConditionalWrites, Documents, Expiration, IdentityKeys,
            IdentityReads, IndexAggregates, IndexRekey, PlatformState, StorageFees, Tokens,
        },
        flags::{Histories, Seen},
        zoo::{ElementZoo, COMMITMENT_PAYLOAD_LEN},
        BatchOp, Coverage, Features, Fetch, HandoverBatch, Op, Outcome, PartialBatch, Prove, Query,
        Read, StreamDigest, TransactionOp, Transactions, TreeRead, Workload, Write,
    },
};

/// The group of the operations every workload is made of, which is all the
/// first unit's workloads produce.
const FIRST_UNIT_GROUP: &str = "writes, batches and reads";

/// The groups of the operations the second unit added.
const SECOND_UNIT_GROUPS: [&str; 8] = [
    "patch",
    "move",
    "partial batches",
    "aux records",
    "meta records",
    "query offsets",
    "conditional subqueries",
    "aggregate queries",
];

/// The group of the operations the fourth unit added.
const FLAG_CALLBACKS_GROUP: &str = "storage-flag callbacks";

/// The group of what a transaction did to a tree before it deleted it
/// whole. These are states of trees the other groups' operations delete,
/// so any workload that deletes a filled tree produces one.
const TREE_DELETES_GROUP: &str = "deletes of filled trees";

const SEED: u64 = 1;
const OTHER_SEED: u64 = 0x4C49_474E_494E;

/// Deep nesting recurses once per level inside GroveDB, further than a test
/// thread's stack allows in an unoptimized build.
const STACK_BYTES: usize = 64 << 20;

fn digest(ops: impl Iterator<Item = Op>) -> (u64, String) {
    let mut digest = StreamDigest::new();
    for op in ops {
        digest.update(&op);
    }
    (digest.ops(), digest.hex())
}

/// What [`run`] applied and what it could check on the way.
#[derive(Clone, Copy, Debug, Default)]
struct Applied {
    /// How many operations were applied.
    ops: u64,
    /// How many snapshot reads returned something else than the committed
    /// state held when they ran.
    snapshot_reads_of_older_state: u64,
    /// How many branch chunks were verified against the hash a trunk chunk
    /// gave for their key.
    branches_verified: u64,
    /// What the operations that run Drive's callbacks did.
    flags: FlagsApplied,
}

/// What [`run`] saw the operations that run Drive's callbacks do.
#[derive(Clone, Copy, Debug, Default)]
struct FlagsApplied {
    /// How many were applied, each with the flags it stored checked.
    ops: u64,
    /// For how many of them the bytes the cost names as freed were
    /// predicted, owner by owner and epoch by epoch.
    freed_bytes_predicted: u64,
    /// How many freed bytes of one owner in more than one epoch.
    freed_across_epochs: u64,
    /// How many stored flags that hold more epochs than they held.
    recorded_an_epoch: u64,
    /// How many updates accounted as a replacement: full handovers.
    replaced: u64,
}

/// Applies a stream to `db`, one transaction per block, and returns how many
/// operations it applied. Panics on the first refused operation, on a read
/// or a flush that does not follow a commit, on a query whose proof does not
/// verify to what the query returned, on a query marked as not proven that
/// GroveDB proves, and on a flush that does not reclaim every subtree
/// dropped since the flush before it.
///
/// It also panics when a conditional write does not return what the state
/// it was applied to calls for, when a rollback does not restore the root
/// hash, when a read through a snapshot does not return what was committed
/// when the snapshot was opened, when two readers of the same data
/// disagree, and when a proof returned by a read does not verify.
///
/// An operation that runs Drive's callbacks is checked against
/// [`FlagsModel`]: the flags it must have stored and the bytes its cost must
/// name as freed.
fn run(
    name: &str,
    db: &GroveDb,
    mut ops: impl Iterator<Item = Op>,
    grove_version: &GroveVersion,
) -> Applied {
    let version = grove_version.protocol_version;
    let root_hash = |transaction: Option<&Transaction>| {
        db.root_hash(transaction, grove_version)
            .unwrap()
            .expect("root hash")
    };
    let mut transactions = Transactions::new(db);
    let mut ahead: VecDeque<Op> = VecDeque::new();
    let mut applied = Applied::default();
    let mut committed = true;
    let mut dropped = 0u64;
    let mut savepoints: Vec<[u8; 32]> = Vec::new();
    let mut snapshot_answers: VecDeque<String> = VecDeque::new();
    let mut trunks: BTreeMap<Vec<Vec<u8>>, BTreeMap<Vec<u8>, LeafInfo>> = BTreeMap::new();
    let mut committed_root = root_hash(None);
    let mut histories = Histories::new();
    let mut index = 0usize;
    while let Some(op) = ahead.pop_front().or_else(|| ops.next()) {
        let context = format!("{name} on grove v{version}: op {index}\n{op:?}");
        if op.follows_a_commit() {
            assert!(committed, "{context}\ndoes not follow a commit");
        }
        dropped += flat_drops(&op);
        if std::env::var_os("WORKLOAD_TRACE").is_some() {
            eprintln!("{name} v{version} #{index}: {op:?}");
        }
        if matches!(op, Op::Transaction(TransactionOp::OpenSnapshot)) {
            // What the reads through this snapshot must return is what
            // they return now, against committed state.
            assert!(ahead.is_empty(), "{context}\nopens a second snapshot");
            snapshot_answers.clear();
            for next in ops.by_ref() {
                let ends = matches!(
                    next,
                    Op::Transaction(TransactionOp::CloseSnapshot | TransactionOp::OpenSnapshot)
                );
                if let Op::Transaction(TransactionOp::SnapshotRead(read)) = &next {
                    snapshot_answers.push_back(answer(db, read, None, grove_version));
                }
                ahead.push_back(next);
                if ends {
                    break;
                }
            }
        }
        let expected = expected_outcome(db, &op, transactions.block(), grove_version);
        let replaced = match &op {
            Op::Write(
                Write::InsertIfNotExists { path, key, .. }
                | Write::InsertIfChanged { path, key, .. },
            ) => stored(db, path, key, Some(transactions.block()), grove_version),
            _ => None,
        };
        let flags_model = FlagsModel::before(db, &op, transactions.block(), grove_version);
        let result = transactions.apply(&op, grove_version);
        let cost = result.cost;
        let outcome = result
            .value
            .unwrap_or_else(|error| panic!("{context}\nrefused: {error:?}"));
        let seen = histories.record(&op);
        if let Some(model) = flags_model {
            model.check(
                &context,
                db,
                &outcome,
                &cost,
                (seen, &histories),
                transactions.block(),
                grove_version,
                &mut applied.flags,
            );
        }
        if let Some(expected) = expected {
            assert_eq!(format!("{outcome:?}"), expected, "{context}");
        }
        if matches!(op, Op::Commit) {
            committed_root = root_hash(None);
        }
        if op.ends_transaction() {
            committed = true;
            savepoints.clear();
        } else if !op.keeps_transaction() {
            committed = false;
        }
        if !op.keeps_transaction() {
            trunks.clear();
        }
        let block = transactions.block();
        match (&op, outcome) {
            (Op::Read(Read::Query(query)), Outcome::Rows(rows)) => {
                prove(
                    name,
                    index,
                    db,
                    query,
                    rows.to_path_key_elements(),
                    grove_version,
                );
            }
            (Op::Read(read), Outcome::Run(answer)) => {
                check_page(name, index, db, read, &answer, grove_version);
                if let Read::PathQuery { query, proven } = read {
                    prove_path_query(name, index, db, query, *proven, answer, grove_version);
                }
            }
            (Op::FlushPendingDrops, Outcome::Reclaimed(report)) => {
                assert_eq!(
                    (
                        report.reclaimed_records,
                        report.skipped_live,
                        report.unresolved_records
                    ),
                    (dropped, 0, 0),
                    "{context}\ndid not reclaim the {dropped} dropped subtrees"
                );
                dropped = 0;
            }
            (Op::Read(read), outcome) => {
                check_answer(&context, db, read, &outcome, None, grove_version);
                applied.branches_verified +=
                    check_proof(&context, db, read, &outcome, &mut trunks, grove_version);
            }
            (Op::Write(write), outcome) => check_write(
                &context,
                db,
                write,
                &outcome,
                replaced,
                block,
                grove_version,
            ),
            (Op::Transaction(step), outcome) => match step {
                TransactionOp::SetSavepoint => savepoints.push(root_hash(Some(block))),
                TransactionOp::RollbackToSavepoint => assert_eq!(
                    Some(root_hash(Some(block))),
                    savepoints.pop(),
                    "{context}\ndid not restore the root hash of its savepoint"
                ),
                TransactionOp::Rollback | TransactionOp::Abandon => assert_eq!(
                    (root_hash(Some(block)), root_hash(None)),
                    (committed_root, committed_root),
                    "{context}\ndid not restore the root hash of the last commit"
                ),
                TransactionOp::BlockRead(read) => {
                    check_answer(&context, db, read, &outcome, Some(block), grove_version)
                }
                TransactionOp::SnapshotRead(read) => {
                    assert_eq!(
                        Some(format!("{outcome:?}")),
                        snapshot_answers.pop_front(),
                        "{context}\ndid not return what was committed when its snapshot opened"
                    );
                    check_answer(
                        &context,
                        db,
                        read,
                        &outcome,
                        transactions.snapshot(),
                        grove_version,
                    );
                    if format!("{outcome:?}") != answer(db, read, None, grove_version) {
                        applied.snapshot_reads_of_older_state += 1;
                    }
                }
                TransactionOp::OpenSnapshot | TransactionOp::CloseSnapshot => {}
            },
            _ => {}
        }
        applied.ops += 1;
        index += 1;
    }
    assert!(committed, "{name}: the stream does not end on a commit");
    assert_eq!(dropped, 0, "{name}: the stream leaves dropped subtrees");
    assert!(
        transactions.snapshot().is_none(),
        "{name}: the stream leaves a snapshot open"
    );
    applied
}

/// What the flags of a stored element say, as Drive reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Epochs {
    owner: Option<[u8; 32]>,
    /// The epoch the element was created in.
    first: u16,
    /// The bytes each later epoch added.
    later: BTreeMap<u16, u32>,
}

impl Epochs {
    fn of(element: &Element) -> Option<Self> {
        let flags = StorageFlags::from_element_flags_ref(element.get_flags().as_ref()?);
        let flags = flags.expect("flags Drive can read")?;
        Some(Epochs {
            owner: flags.owner_id().copied(),
            first: *flags.base_epoch(),
            later: flags.epoch_index_map().cloned().unwrap_or_default(),
        })
    }

    fn to_flags(&self) -> Vec<u8> {
        match (self.owner, self.later.is_empty()) {
            (None, true) => StorageFlags::SingleEpoch(self.first),
            (Some(owner), true) => StorageFlags::SingleEpochOwned(self.first, owner),
            (None, false) => StorageFlags::MultiEpoch(self.first, self.later.clone()),
            (Some(owner), false) => {
                StorageFlags::MultiEpochOwned(self.first, self.later.clone(), owner)
            }
        }
        .to_element_flags()
    }
}

/// Freed bytes by owner and epoch, and those no callback was asked about:
/// of elements without flags and of what a deleted tree held. Among bytes
/// named by owner and epoch, GroveDB names those by the owner of 32 zero
/// bytes and `UNKNOWN_EPOCH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Freed {
    by_epoch: BTreeMap<([u8; 32], u16), u32>,
    unsplit: u32,
}

impl Freed {
    fn of(removed: &StorageRemovedBytes) -> Self {
        let mut freed = Freed::default();
        match removed {
            StorageRemovedBytes::NoStorageRemoval => {}
            StorageRemovedBytes::BasicStorageRemoval(bytes) => freed.unsplit = *bytes,
            StorageRemovedBytes::SectionedStorageRemoval(sections) => {
                for (owner, epochs) in sections {
                    for (epoch, bytes) in epochs.iter() {
                        freed.add(Some(*owner), epoch, *bytes);
                    }
                }
                let unknown = ([0; 32], UNKNOWN_EPOCH);
                freed.unsplit = freed.by_epoch.remove(&unknown).unwrap_or(0);
            }
        }
        freed
    }

    fn add(&mut self, owner: Option<[u8; 32]>, epoch: u16, bytes: u32) {
        if bytes > 0 {
            *self
                .by_epoch
                .entry((owner.unwrap_or_default(), epoch))
                .or_default() += bytes;
        }
    }

    /// Frees `key_bytes` and `value_bytes` of an element stored with
    /// `flags`: the key goes back to the epoch the element was created in,
    /// and the value to the latest epochs first, each of which keeps the
    /// three bytes its own entry in the flags takes, with what is left to
    /// the first epoch. Returns what each later epoch gave back.
    fn free(
        &mut self,
        flags: Option<&Epochs>,
        key_bytes: u32,
        value_bytes: u32,
    ) -> BTreeMap<u16, u32> {
        let mut given_back = BTreeMap::new();
        let Some(flags) = flags else {
            self.unsplit += key_bytes + value_bytes;
            return given_back;
        };
        let mut left = value_bytes;
        for (epoch, added) in flags.later.iter().rev() {
            if left == 0 {
                break;
            }
            let frees = if *added <= left + 3 {
                added.saturating_sub(3)
            } else {
                left
            };
            given_back.insert(*epoch, frees);
            self.add(flags.owner, *epoch, frees);
            left -= frees;
        }
        self.add(flags.owner, flags.first, key_bytes + left);
        given_back
    }

    /// In how many epochs the owner with the most of them gets bytes back.
    fn most_epochs_of_an_owner(&self) -> usize {
        let mut epochs: BTreeMap<[u8; 32], usize> = BTreeMap::new();
        for (owner, _) in self.by_epoch.keys() {
            *epochs.entry(*owner).or_default() += 1;
        }
        epochs.into_values().max().unwrap_or(0)
    }
}

/// What an operation that runs Drive's callbacks does to one key.
enum FlagsWrite {
    /// The element is stored, over what the key holds.
    Put(Element),
    /// The element is stored unless the key holds one.
    PutIfAbsent(Element),
    /// The key is deleted.
    Delete,
    /// A tree above the deleted key, deleted if the delete left it empty.
    DeleteIfLeftEmpty,
    /// The reference the key holds is written again as it is.
    Refresh,
}

/// A key an operation that runs Drive's callbacks writes.
struct KeyWritten {
    path: Vec<Vec<u8>>,
    key: Vec<u8>,
    write: FlagsWrite,
    /// The element the key holds before the operation.
    held: Option<Element>,
}

/// The model the operations that run Drive's callbacks are checked
/// against.
///
/// It restates, from what the stream says and what the database held
/// before the operation, what Drive's callbacks do:
///
/// - **The flags stored.** An element stored where none was, one written
///   without flags, and one written over an element without flags keep the
///   flags written. Otherwise the flags stored are the stored ones with
///   this changed: an update that makes the element longer names the owner
///   written, if it names one, and records the bytes it added under its
///   epoch when that is later than the element's first. One that makes it
///   shorter in a later epoch names the owner written and takes the freed
///   bytes off the epochs recorded, dropping one left with no more than the
///   three bytes of its own entry, unless the flags record no later epoch,
///   in which case they stay as they are. One that keeps the length keeps
///   them, in a batch, and stores the flags written, in a partial batch.
///   A batch that hands elements over then stores the owner written in
///   every case. Where that owner or the stored one is an owner it hands
///   over fully to or from, and the two differ, the element is stored as
///   written instead, and the stored one is freed whole.
/// - **The bytes freed.** Whatever an update or a delete frees goes back to
///   the owner in the stored flags: to the latest epochs first, and what is
///   left to the epoch the element was created in, with the bytes of a
///   deleted key.
///
/// "Longer" and "shorter" compare the element written with the element
/// stored, flags aside. The sizes are those GroveDB charges for the stored
/// bytes: the model takes them from `Element::specialized_costs_for_key_value`.
struct FlagsModel {
    partial: bool,
    /// For a batch that hands elements over, the owners a handover to or
    /// from which is a full one.
    handover: Option<Vec<[u8; 32]>>,
    /// Every key the operation writes.
    writes: Vec<KeyWritten>,
    /// Whether the operation deletes a tree that holds something it does
    /// not delete key by key. What the tree holds goes with it, and no
    /// callback is asked about those bytes.
    deletes_a_populated_tree: bool,
    /// Whether the model covers all the operation does.
    whole: bool,
}

impl FlagsModel {
    /// The model of `op`, if it runs Drive's callbacks, with what its keys
    /// hold before it is applied.
    fn before(
        db: &GroveDb,
        op: &Op,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> Option<Self> {
        let mut whole = true;
        let mut keys: Vec<(Vec<Vec<u8>>, Vec<u8>, FlagsWrite)> = Vec::new();
        match op {
            Op::FlagsBatch(_) | Op::FlagsPartialBatch(_) | Op::FlagsHandoverBatch(_) => {
                for batch_op in op.batch_ops() {
                    let (path, key, write) = match batch_op {
                        BatchOp::InsertOrReplace { path, key, element }
                        | BatchOp::InsertOnly { path, key, element }
                        | BatchOp::Replace { path, key, element } => {
                            (path, key, FlagsWrite::Put(element.clone()))
                        }
                        BatchOp::InsertIfNotExists { path, key, element } => {
                            (path, key, FlagsWrite::PutIfAbsent(element.clone()))
                        }
                        BatchOp::Delete { path, key } | BatchOp::DeleteTree { path, key, .. } => {
                            (path, key, FlagsWrite::Delete)
                        }
                        BatchOp::RefreshReference { path, key, .. } => {
                            (path, key, FlagsWrite::Refresh)
                        }
                        BatchOp::Append { .. }
                        | BatchOp::Patch { .. }
                        | BatchOp::Move { .. }
                        | BatchOp::DropFlat { .. } => {
                            whole = false;
                            continue;
                        }
                    };
                    keys.push((path.clone(), key.clone(), write));
                }
            }
            Op::Write(Write::SectionalDelete { path, key }) => {
                keys.push((path.clone(), key.clone(), FlagsWrite::Delete));
            }
            Op::Write(Write::SectionalDeleteUpTree { path, key, .. }) => {
                keys.push((path.clone(), key.clone(), FlagsWrite::Delete));
                for level in (0..path.len()).rev() {
                    keys.push((
                        path[..level].to_vec(),
                        path[level].clone(),
                        FlagsWrite::DeleteIfLeftEmpty,
                    ));
                }
            }
            _ => return None,
        }
        let places: BTreeSet<_> = keys.iter().map(|(path, key, _)| (path, key)).collect();
        whole &= places.len() == keys.len();
        // The subtrees the operation writes into: a tree it deletes after
        // it has deleted what the tree holds key by key is empty by then.
        let written_into: BTreeSet<Vec<Vec<u8>>> =
            keys.iter().map(|(path, ..)| path.clone()).collect();
        let mut deletes_a_populated_tree = false;
        let writes = keys
            .into_iter()
            .map(|(path, key, write)| {
                let held = stored(db, &path, &key, Some(transaction), grove_version);
                let deletes = matches!(write, FlagsWrite::Delete);
                if deletes && held.as_ref().is_some_and(Element::is_any_tree) {
                    let mut tree = path.clone();
                    tree.push(key.clone());
                    let emptied = written_into.contains(&tree);
                    deletes_a_populated_tree |= !emptied
                        && !db
                            .is_empty_tree(tree.as_slice(), Some(transaction), grove_version)
                            .unwrap()
                            .expect("a tree");
                }
                KeyWritten {
                    path,
                    key,
                    write,
                    held,
                }
            })
            .collect();
        Some(FlagsModel {
            partial: matches!(op, Op::FlagsPartialBatch(_)),
            handover: match op {
                Op::FlagsHandoverBatch(batch) => {
                    Some(batch.full_handover_owners.iter().copied().collect())
                }
                _ => None,
            },
            writes,
            deletes_a_populated_tree,
            whole,
        })
    }

    /// Checks what the operation left against the model.
    #[allow(clippy::too_many_arguments)]
    fn check(
        self,
        context: &str,
        db: &GroveDb,
        outcome: &Outcome,
        cost: &OperationCost,
        (seen, histories): (Seen, &Histories),
        transaction: &Transaction,
        grove_version: &GroveVersion,
        applied: &mut FlagsApplied,
    ) {
        let size = |key: &[u8], element: &Element| {
            let value = element.serialize(grove_version).expect("an element");
            Element::specialized_costs_for_key_value(
                key,
                &value,
                NodeType::NormalNode,
                grove_version,
            )
            .expect("the size of a value")
        };
        let deleted_above = match outcome {
            Outcome::Deleted(deleted) => usize::from(*deleted) - 1,
            _ => 0,
        };
        let mut whole = self.whole;
        let mut freed = Freed::default();
        let mut added_whole = 0;
        let mut above = 0;
        for written in &self.writes {
            let KeyWritten {
                path,
                key,
                write,
                held,
            } = written;
            let place = format!("{context}\nat {path:?} {key:?}");
            let now = stored(db, path, key, Some(transaction), grove_version);
            let held_flags = held.as_ref().and_then(Epochs::of);
            let written = match write {
                FlagsWrite::Put(element) => element,
                FlagsWrite::PutIfAbsent(element) if held.is_none() => element,
                FlagsWrite::PutIfAbsent(_) | FlagsWrite::Refresh => {
                    assert_eq!(now, *held, "{place}\ndid not stay as it was");
                    continue;
                }
                FlagsWrite::Delete | FlagsWrite::DeleteIfLeftEmpty => {
                    if matches!(write, FlagsWrite::DeleteIfLeftEmpty) {
                        above += 1;
                        if above > deleted_above {
                            continue;
                        }
                    }
                    assert_eq!(now, None, "{place}\nwas not deleted");
                    let held = held.as_ref().expect("a delete of a stored key");
                    freed.free(
                        held_flags.as_ref(),
                        KV::node_key_byte_cost_size(key.len() as u32),
                        size(key, held),
                    );
                    continue;
                }
            };
            let now = now.unwrap_or_else(|| panic!("{place}\nholds nothing"));
            let Some(held) = held else {
                // A tree is stored with the root key it has by then.
                if written.is_any_tree() {
                    assert!(now.is_any_tree(), "{place}\nstored {now:?}");
                    assert_eq!(now.get_flags(), written.get_flags(), "{place}");
                } else {
                    assert_eq!(now, *written, "{place}\nwas not stored as written");
                }
                continue;
            };
            if held.is_any_tree() || written.is_any_tree() {
                whole = false;
                continue;
            }
            let (was, is) = (size(key, held), size(key, &now));
            let written_flags = Epochs::of(written);
            if self.is_full_handover(held_flags.as_ref(), written_flags.as_ref()) {
                // The stored element is freed whole, as a delete frees it,
                // and the element written is stored as it is and charged
                // whole, key included.
                assert_eq!(now, *written, "{place}\nwas not replaced");
                added_whole += KV::node_key_byte_cost_size(key.len() as u32) + is;
                freed.free(
                    held_flags.as_ref(),
                    KV::node_key_byte_cost_size(key.len() as u32),
                    was,
                );
                applied.replaced += 1;
                continue;
            }
            let given_back = if is < was {
                freed.free(held_flags.as_ref(), 0, was - is)
            } else {
                BTreeMap::new()
            };
            let expected = match (&held_flags, written_flags) {
                (Some(held_flags), Some(written_flags)) => {
                    // Longer or shorter is told with the flags the element
                    // had, as GroveDB measures it first.
                    let mut measured = written.clone();
                    measured.set_flags(held.get_flags().clone());
                    let mut expected = written.clone();
                    expected.set_flags(Some(
                        self.merged(
                            held_flags,
                            &written_flags,
                            size(key, &measured).cmp(&was),
                            is.saturating_sub(was),
                            &given_back,
                        )
                        .to_flags(),
                    ));
                    expected
                }
                _ => written.clone(),
            };
            assert_eq!(
                now,
                expected,
                "{place}\nstored {:?}, not {:?}, over {held_flags:?}",
                Epochs::of(&now),
                Epochs::of(&expected)
            );
            let epochs = |flags: Option<Epochs>| flags.map_or(0, |flags| flags.later.len());
            if epochs(Epochs::of(&now)) > epochs(held_flags) {
                applied.recorded_an_epoch += 1;
            }
        }
        // What the stream alone says the flags hold is what they hold.
        for KeyWritten { path, key, .. } in &self.writes {
            let Some(history) = histories.get(path, key) else {
                continue;
            };
            let flags = stored(db, path, key, Some(transaction), grove_version)
                .as_ref()
                .and_then(Epochs::of)
                .unwrap_or_else(|| panic!("{context}\n{path:?} {key:?} is followed, not stored"));
            assert_eq!(
                (flags.owner, flags.first, &flags.later),
                (history.owner, history.base_epoch, &history.later),
                "{context}\nthe stream says otherwise of {path:?} {key:?}"
            );
        }
        applied.ops += 1;
        assert!(
            cost.storage_cost.added_bytes >= added_whole,
            "{context}\nadded {} bytes, and replaced elements of {added_whole}",
            cost.storage_cost.added_bytes
        );
        let named = Freed::of(&cost.storage_cost.removed_bytes);
        if whole {
            assert_eq!(
                named.by_epoch, freed.by_epoch,
                "{context}\nfreed other bytes"
            );
            if self.deletes_a_populated_tree {
                assert!(named.unsplit > freed.unsplit, "{context}\n{named:?}");
            } else {
                assert_eq!(named.unsplit, freed.unsplit, "{context}\nfreed other bytes");
            }
            applied.freed_bytes_predicted += 1;
        }
        let across = named.most_epochs_of_an_owner() > 1;
        assert!(
            across || !(seen.update.shrank_across_epochs || seen.deleted_several_epochs),
            "{context}\nfreed bytes of one epoch: {named:?}"
        );
        applied.freed_across_epochs += u64::from(across);
    }

    /// The flags an update stores over `held`, given how the element
    /// written compares with the one stored, the bytes the update added and
    /// the bytes each later epoch gave back.
    fn merged(
        &self,
        held: &Epochs,
        written: &Epochs,
        change: std::cmp::Ordering,
        added: u32,
        given_back: &BTreeMap<u16, u32>,
    ) -> Epochs {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let mut merged = Epochs {
            owner: written.owner.or(held.owner),
            ..held.clone()
        };
        match change {
            Equal if self.partial => return written.clone(),
            Equal => merged = held.clone(),
            Less if written.first > held.first && held.later.is_empty() => merged = held.clone(),
            Greater if written.first > held.first => {
                *merged.later.entry(written.first).or_default() += added;
            }
            Less if written.first > held.first => {
                for (epoch, bytes) in given_back {
                    let recorded = merged.later.get_mut(epoch).expect("a recorded epoch");
                    *recorded -= bytes;
                    if *recorded <= 3 {
                        merged.later.remove(epoch);
                    }
                }
            }
            Greater | Less => {}
        }
        // A batch that hands elements over stores the owner written,
        // whatever the merge kept.
        if self.handover.is_some() && written.owner.is_some() {
            merged.owner = written.owner;
        }
        merged
    }

    /// Whether an update of `held` by `written` is a full handover: the
    /// two name different owners, and one of them is an owner the batch
    /// hands over fully to or from.
    fn is_full_handover(&self, held: Option<&Epochs>, written: Option<&Epochs>) -> bool {
        let owner = |epochs: Option<&Epochs>| epochs.and_then(|epochs| epochs.owner);
        let (Some(full), Some(held), Some(written)) = (&self.handover, owner(held), owner(written))
        else {
            return false;
        };
        held != written && (full.contains(&held) || full.contains(&written))
    }
}

/// What `read` returns in `transaction`, as text.
fn answer(
    db: &GroveDb,
    read: &Read,
    transaction: Option<&Transaction>,
    grove_version: &GroveVersion,
) -> String {
    let outcome = apply(db, &Op::Read(read.clone()), transaction, grove_version)
        .unwrap()
        .unwrap_or_else(|error| panic!("{read:?} refused: {error:?}"));
    format!("{outcome:?}")
}

fn stored(
    db: &GroveDb,
    path: &[Vec<u8>],
    key: &[u8],
    transaction: Option<&Transaction>,
    grove_version: &GroveVersion,
) -> Option<Element> {
    db.get_raw_optional(path.into(), key, transaction, grove_version)
        .unwrap()
        .expect("a raw read")
}

/// What a conditional write must return, given the state it is about to be
/// applied to. `None` for an operation whose outcome does not depend on it.
fn expected_outcome(
    db: &GroveDb,
    op: &Op,
    transaction: &Transaction,
    grove_version: &GroveVersion,
) -> Option<String> {
    let Op::Write(write) = op else {
        return None;
    };
    let stored =
        |path: &[Vec<u8>], key: &[u8]| stored(db, path, key, Some(transaction), grove_version);
    let outcome = match write {
        Write::InsertIfNotExists {
            path,
            key,
            return_existing,
            ..
        } => {
            let existing = stored(path, key);
            if *return_existing {
                Outcome::Existing(existing)
            } else {
                Outcome::Flag(existing.is_none())
            }
        }
        Write::InsertIfChanged { path, key, element } => {
            let previous = stored(path, key);
            let changed = previous.as_ref() != Some(element);
            Outcome::Changed {
                changed,
                previous: previous.filter(|_| changed),
            }
        }
        Write::DeleteIfEmptyTree { path, key } => {
            let mut tree = path.clone();
            tree.push(key.clone());
            let empty = db
                .is_empty_tree(tree.as_slice(), Some(transaction), grove_version)
                .unwrap()
                .expect("the key holds a tree");
            Outcome::Flag(empty)
        }
        Write::DeleteBuilt { path, key } | Write::IndexedDelete { path, key, .. } => {
            Outcome::Flag(stored(path, key).is_some())
        }
        _ => return None,
    };
    Some(format!("{outcome:?}"))
}

/// Checks what a write left behind.
fn check_write(
    context: &str,
    db: &GroveDb,
    write: &Write,
    outcome: &Outcome,
    replaced: Option<Element>,
    transaction: &Transaction,
    grove_version: &GroveVersion,
) {
    let stored =
        |path: &[Vec<u8>], key: &[u8]| stored(db, path, key, Some(transaction), grove_version);
    match (write, outcome) {
        // A key that was taken keeps what it held.
        (
            Write::InsertIfNotExists {
                path, key, element, ..
            },
            _,
        ) => assert_eq!(
            stored(path, key),
            replaced.or_else(|| Some(element.clone())),
            "{context}\nleft another element"
        ),
        (Write::InsertIfChanged { path, key, element }, _) => assert_eq!(
            stored(path, key).as_ref(),
            Some(element),
            "{context}\nleft another element"
        ),
        (
            Write::DeleteBuilt { path, key }
            | Write::DeleteIfEmptyTree { path, key }
            | Write::IndexedDelete { path, key, .. },
            Outcome::Flag(true),
        ) => assert!(stored(path, key).is_none(), "{context}\ndeleted nothing"),
        (
            Write::DeleteUpTree { path, key, .. } | Write::SectionalDeleteUpTree { path, key, .. },
            Outcome::Deleted(deleted),
        ) => {
            // The key went, then the tree it was in, and so on up: each
            // level above the first is a segment of the path.
            let deleted = usize::from(*deleted);
            assert!((1..=path.len()).contains(&deleted), "{context}");
            assert!(stored(path, key).is_none(), "{context}\ndeleted nothing");
            for level in 1..=deleted {
                let (parent, tree) = path.split_at(path.len() - level);
                let gone = stored(parent, &tree[0]).is_none();
                assert_eq!(
                    gone,
                    level < deleted,
                    "{context}\ndeleted {deleted}, and level {level} is the other way"
                );
            }
        }
        (
            Write::IndexedInsert {
                path, key, element, ..
            },
            _,
        ) => {
            let stored = stored(path, key).expect("an indexed insert stores its element");
            // A tree is stored with the root key and aggregates it has by
            // then, a value as it was given.
            if element.is_any_tree() {
                assert!(stored.is_any_tree(), "{context}\nstored {stored:?}");
            } else {
                assert_eq!(stored, *element, "{context}\nstored another element");
            }
        }
        (Write::CommitmentInsert { path, .. }, Outcome::Appended { position, .. }) => {
            let (key, parent) = path.split_last().expect("the path of a tree");
            let count = db
                .commitment_tree_count(parent, key, Some(transaction), grove_version)
                .unwrap()
                .expect("a commitment tree");
            assert_eq!(count, position + 1, "{context}\nis not the newest note");
        }
        _ => {}
    }
}

/// The rows a path query selects, read by `GroveDb::query_raw`, or by
/// `GroveDb::query` to have references followed.
fn rows(
    db: &GroveDb,
    query: &PathQuery,
    raw: bool,
    transaction: Option<&Transaction>,
    grove_version: &GroveVersion,
) -> (Vec<PathKeyElementTrio>, u16) {
    let read = Op::Read(Read::Rows {
        query: Box::new(query.clone()),
        raw,
    });
    match apply(db, &read, transaction, grove_version).unwrap() {
        Ok(Outcome::Run(PathQueryRun::Elements { elements, skipped })) => {
            (elements.to_path_key_elements(), skipped)
        }
        other => panic!("{query:?} does not select rows: {other:?}"),
    }
}

/// Checks a read against another reader of the same data.
fn check_answer(
    context: &str,
    db: &GroveDb,
    read: &Read,
    outcome: &Outcome,
    transaction: Option<&Transaction>,
    grove_version: &GroveVersion,
) {
    let stored = |path: &[Vec<u8>], key: &[u8]| stored(db, path, key, transaction, grove_version);
    match (read, outcome) {
        (Read::HasRaw { path, key }, Outcome::Flag(has)) => {
            assert_eq!(*has, stored(path, key).is_some(), "{context}")
        }
        (Read::IsEmptyTree { path }, Outcome::Flag(empty)) => {
            // An append-only tree says in its element how much it holds.
            let entries = path.split_last().and_then(|(key, parent)| {
                stored(parent, key)
                    .expect("the tree is stored")
                    .non_merk_entry_count()
            });
            let expected = entries.map_or_else(
                || {
                    let all =
                        PathQuery::new_unsized(path.clone(), grovedb::Query::new_range_full());
                    rows(db, &all, true, transaction, grove_version)
                        .0
                        .is_empty()
                },
                |entries| entries == 0,
            );
            assert_eq!(*empty, expected, "{context}");
        }
        (
            Read::Fetch {
                path, key, fetch, ..
            },
            outcome,
        ) => {
            let raw = stored(path, key);
            let expected = match fetch {
                Fetch::RawOptional => Outcome::Existing(raw),
                Fetch::Raw => Outcome::Element(raw.expect("the key is stored")),
                Fetch::Resolved => Outcome::Element(
                    db.get(path.as_slice(), key, transaction, grove_version)
                        .unwrap()
                        .expect("the key is stored"),
                ),
            };
            assert_eq!(format!("{outcome:?}"), format!("{expected:?}"), "{context}");
        }
        (Read::ItemValues { query, or_sums }, outcome) => {
            let mut unpaged = (**query).clone();
            unpaged.query.offset = None;
            unpaged.query.limit = None;
            let (rows, _) = rows(db, &unpaged, false, transaction, grove_version);
            let skip = usize::from(query.query.offset.unwrap_or(0));
            let limit = query.query.limit.map_or(usize::MAX, usize::from);
            let rows = rows.into_iter().skip(skip).take(limit);
            let expected: Vec<QueryItemOrSumReturnType> = rows
                .map(|(_, _, element)| match element {
                    Element::Item(value, _) => QueryItemOrSumReturnType::ItemData(value),
                    Element::SumItem(sum, _) => QueryItemOrSumReturnType::SumValue(sum),
                    other => panic!("{context}\nselects {other:?}"),
                })
                .collect();
            match outcome {
                Outcome::ItemValuesOrSums { values, .. } => {
                    assert!(or_sums, "{context}");
                    assert_eq!(*values, expected, "{context}");
                }
                Outcome::ItemValues { values, .. } => {
                    let values: Vec<_> = values
                        .iter()
                        .cloned()
                        .map(QueryItemOrSumReturnType::ItemData)
                        .collect();
                    assert_eq!(values, expected, "{context}");
                }
                other => panic!("{context}\nreturned {other:?}"),
            }
        }
        (Read::SumValues { query }, Outcome::SumValues { sums, .. }) => {
            let (rows, _) = rows(db, query, false, transaction, grove_version);
            let expected: Vec<i64> = rows
                .into_iter()
                .map(|(_, _, element)| element.as_sum_item_value().expect("a sum item"))
                .collect();
            assert_eq!(*sums, expected, "{context}");
        }
        (Read::Many { queries }, Outcome::Rows(returned)) => {
            let merged = PathQuery::merge(queries.iter().collect(), grove_version)
                .expect("the queries merge");
            let (rows, _) = rows(db, &merged, true, transaction, grove_version);
            assert_eq!(returned.clone().to_path_key_elements(), rows, "{context}");
        }
        _ => {}
    }
}

/// The byte strings of a state sync answer: a 32-bit big-endian count, then
/// each string behind a 32-bit big-endian length.
fn unpacked(context: &str, packed: &[u8]) -> Vec<Vec<u8>> {
    fn number(rest: &mut &[u8]) -> Option<usize> {
        let (number, tail) = rest.split_first_chunk::<4>()?;
        *rest = tail;
        Some(u32::from_be_bytes(*number) as usize)
    }
    fn strings(rest: &mut &[u8]) -> Option<Vec<Vec<u8>>> {
        let mut strings = Vec::new();
        for _ in 0..number(rest)? {
            let length = number(rest)?;
            let (string, tail) = rest.split_at_checked(length)?;
            *rest = tail;
            strings.push(string.to_vec());
        }
        Some(strings)
    }
    let mut rest = packed;
    let strings = strings(&mut rest).unwrap_or_else(|| panic!("{context}\nis cut short"));
    assert!(rest.is_empty(), "{context}\nholds more than it says");
    strings
}

/// A key, its count where one was asked for, and its sum.
type BigSumsPerKey = Vec<(Vec<u8>, Option<u64>, i128)>;

/// A row as a proof gives it: a reference stands for the value it points
/// at.
fn resolved(
    db: &GroveDb,
    (path, key, element): PathKeyElementTrio,
    grove_version: &GroveVersion,
) -> PathKeyElementTrio {
    let element = if element.underlying().is_reference() {
        db.get(path.as_slice(), &key, None, grove_version)
            .unwrap()
            .expect("a stored reference resolves")
    } else {
        element
    };
    (path, key, element)
}

/// What a verifier proved, once it has verified the proof to `root`.
fn verified<T>(
    context: &str,
    root: [u8; 32],
    verification: Result<([u8; 32], T), grovedb::Error>,
) -> T {
    let (proved_root, proved) =
        verification.unwrap_or_else(|error| panic!("{context}\nnot verified: {error:?}"));
    assert_eq!(proved_root, root, "{context}\nverifies to another root");
    proved
}

/// Proves a read GroveDB proves, or verifies the proof a read returned,
/// against committed state. Returns how many branch chunks it verified
/// against the hash a trunk chunk gave.
fn check_proof(
    context: &str,
    db: &GroveDb,
    read: &Read,
    outcome: &Outcome,
    trunks: &mut BTreeMap<Vec<Vec<u8>>, BTreeMap<Vec<u8>, LeafInfo>>,
    grove_version: &GroveVersion,
) -> u64 {
    let root = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    let prove = |query: &PathQuery| {
        db.prove_query(query, None, grove_version)
            .unwrap()
            .unwrap_or_else(|error| panic!("{context}\nnot provable: {error:?}"))
    };
    match (read, outcome) {
        (Read::KeysOptional { query, .. }, Outcome::OptionalRows(returned)) => {
            let mut proved = verified(
                context,
                root,
                GroveDb::verify_query_with_absence_proof(&prove(query), query, grove_version),
            );
            let mut expected: Vec<_> = returned
                .iter()
                .cloned()
                .map(|(path, key, element)| match element {
                    Some(element) => {
                        let (path, key, element) =
                            resolved(db, (path, key, element), grove_version);
                        (path, key, Some(element))
                    }
                    None => (path, key, None),
                })
                .collect();
            let by_key = |row: &(Vec<Vec<u8>>, Vec<u8>, _)| (row.0.clone(), row.1.clone());
            proved.sort_by_key(by_key);
            expected.sort_by_key(by_key);
            assert_eq!(proved, expected, "{context}");
            assert_eq!(
                returned.len(),
                query.query.query.items.len(),
                "{context}\ndoes not answer for every key"
            );
        }
        (Read::BigSum { query, .. }, Outcome::BigSum { count: None, sum }) => {
            let proved = verified(
                context,
                root,
                GroveDb::verify_aggregate_big_sum_query(&prove(query), query, grove_version),
            );
            assert_eq!(proved, *sum, "{context}");
        }
        (
            Read::BigSum { query, .. },
            Outcome::BigSum {
                count: Some(count),
                sum,
            },
        ) => {
            let verification = GroveDb::verify_aggregate_count_and_big_sum_query(
                &prove(query),
                query,
                grove_version,
            )
            .map(|(proved_root, count, sum)| (proved_root, (count, sum)));
            let proved = verified(context, root, verification);
            assert_eq!(proved, (*count, *sum), "{context}");
        }
        (
            Read::BigSum {
                query, with_count, ..
            },
            Outcome::BigSumPerKey(per_key),
        ) => {
            let proof = prove(query);
            let proved: BigSumsPerKey = if *with_count {
                let verification = GroveDb::verify_aggregate_count_and_big_sum_query_per_key(
                    &proof,
                    query,
                    grove_version,
                );
                verified(context, root, verification)
                    .into_iter()
                    .map(|(key, count, sum)| (key, Some(count), sum))
                    .collect()
            } else {
                let verification =
                    GroveDb::verify_aggregate_big_sum_query_per_key(&proof, query, grove_version);
                verified(context, root, verification)
                    .into_iter()
                    .map(|(key, sum)| (key, None, sum))
                    .collect()
            };
            assert_eq!(proved, *per_key, "{context}");
        }
        (Read::Prove(prove), Outcome::Proof(proof)) => match prove {
            Prove::PathQuery(query) => {
                let proved = verified(
                    context,
                    root,
                    GroveDb::verify_query_raw(proof, query, grove_version),
                );
                let proved: Vec<PathKeyElementTrio> = proved
                    .into_iter()
                    .map(|row| {
                        let element = Element::deserialize(&row.value, grove_version)
                            .expect("a proved value is an element");
                        (row.path, row.key, element)
                    })
                    .collect();
                let (rows, _) = rows(db, query, true, None, grove_version);
                let expected: Vec<_> = rows
                    .into_iter()
                    .map(|row| resolved(db, row, grove_version))
                    .collect();
                assert_eq!(proved, expected, "{context}");
            }
            Prove::Many(queries) => {
                let merged = PathQuery::merge(queries.iter().collect(), grove_version)
                    .expect("the queries merge");
                let proved = verified(
                    context,
                    root,
                    GroveDb::verify_query_raw(proof, &merged, grove_version),
                );
                let proved: Vec<PathKeyElementTrio> = proved
                    .into_iter()
                    .map(|row| {
                        let element = Element::deserialize(&row.value, grove_version)
                            .expect("a proved value is an element");
                        (row.path, row.key, element)
                    })
                    .collect();
                let (rows, _) = rows(db, &merged, true, None, grove_version);
                let expected: Vec<_> = rows
                    .into_iter()
                    .map(|row| resolved(db, row, grove_version))
                    .collect();
                assert_eq!(proved, expected, "{context}");
            }
            Prove::BulkPositionRange {
                path,
                key,
                start,
                limit,
            } => {
                let verification = GroveDb::verify_bulk_position_range_proof(
                    proof,
                    path.clone(),
                    key,
                    *start,
                    *limit,
                    grove_version,
                );
                let proved = verified(context, root, verification);
                let tree = stored(db, path, key, None, grove_version).expect("the tree is stored");
                let (start, limit) = (*start, *limit);
                let range = match tree.underlying() {
                    Element::BulkAppendTree(..) => TreeRead::BulkRange { start, limit },
                    Element::CommitmentTree(..) => TreeRead::CommitmentRange { start, limit },
                    Element::PrivateDocumentStore(..) => {
                        TreeRead::PrivateDocumentRange { start, limit }
                    }
                    other => panic!("{context}\nproves a range of {other:?}"),
                };
                let mut tree_path = path.clone();
                tree_path.push(key.clone());
                let read = Read::Tree {
                    path: tree_path,
                    read: range,
                };
                assert_eq!(
                    format!("{:?}", Outcome::Page(proved)),
                    answer(db, &read, None, grove_version),
                    "{context}"
                );
            }
            Prove::TrunkChunk {
                path,
                max_depth,
                min_depth,
            } => {
                let query = PathTrunkChunkQuery {
                    path: path.clone(),
                    max_depth: *max_depth,
                    min_depth: *min_depth,
                };
                let trunk = verified(
                    context,
                    root,
                    GroveDb::verify_trunk_chunk_proof(proof, &query, grove_version),
                );
                assert!(!trunk.elements.is_empty(), "{context}\nholds no element");
                trunks.insert(path.clone(), trunk.leaf_keys);
            }
            Prove::BranchChunk { path, key, depth } => {
                assert!(!proof.is_empty(), "{context}");
                if let Some(leaf) = trunks.get(path).and_then(|leaves| leaves.get(key)) {
                    let query = PathBranchChunkQuery {
                        path: path.clone(),
                        key: key.clone(),
                        depth: *depth,
                    };
                    let branch =
                        GroveDb::verify_branch_chunk_proof(proof, &query, leaf.hash, grove_version)
                            .unwrap_or_else(|error| panic!("{context}\nnot verified: {error:?}"));
                    assert!(branch.elements.contains_key(key), "{context}");
                    return 1;
                }
            }
            Prove::Chunk { ids, .. } => {
                // One answer for the one subtree asked for, holding a chunk
                // for each chunk id, or the chunk at the root for none, or
                // one page of entries.
                let asked = match ids {
                    lignin_testkit::workload::ChunkIds::Merk(ids) => ids.len().max(1),
                    lignin_testkit::workload::ChunkIds::Entries { .. } => 1,
                };
                let subtrees = unpacked(context, proof);
                assert_eq!(subtrees.len(), 1, "{context}");
                let chunks = unpacked(context, &subtrees[0]);
                assert_eq!(chunks.len(), asked, "{context}");
                assert!(
                    chunks.iter().all(|chunk| !chunk.is_empty()),
                    "{context}\nserved an empty chunk"
                );
            }
        },
        _ => {}
    }
    0
}

/// How many subtrees an operation drops flat.
fn flat_drops(op: &Op) -> u64 {
    let in_batch = |ops: &[BatchOp]| {
        ops.iter()
            .filter(|op| matches!(op, BatchOp::DropFlat { .. }))
            .count() as u64
    };
    match op {
        Op::Write(Write::DropFlat { .. }) => 1,
        Op::Batch(ops) => in_batch(ops),
        Op::PartialBatch(batch) => in_batch(&batch.ops) + in_batch(&batch.add_on),
        _ => 0,
    }
}

/// Paths `verify_grovedb` reports. Grove versions 1 to 4 leave orphan rows
/// behind when an entry of a count-indexed tree is deleted, so this is
/// printed, not asserted.
fn report_verify_issues(name: &str, db: &GroveDb, grove_version: &GroveVersion) {
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify");
    if !issues.is_empty() {
        println!(
            "{name} on grove v{}: verify_grovedb reports {} paths",
            grove_version.protocol_version,
            issues.len(),
        );
    }
}

fn open() -> (tempfile::TempDir, GroveDb) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = GroveDb::open(dir.path()).expect("open");
    (dir, db)
}

/// Proves a query against committed state and checks that the proof
/// verifies to the database's root hash and to the rows the query returned:
/// the same paths, keys and elements in the same order.
///
/// A proof carries the value a reference points at, never the reference, so
/// a raw query's reference rows are compared with what `get` resolves them
/// to.
fn prove(
    name: &str,
    index: usize,
    db: &GroveDb,
    query: &Query,
    rows: Vec<PathKeyElementTrio>,
    grove_version: &GroveVersion,
) {
    let version = grove_version.protocol_version;
    let path_query = query.to_path_query();
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap_or_else(|error| {
            panic!("{name} on grove v{version}: query {index} not provable: {error:?}\n{query:?}")
        });
    let refused = |error| -> ! {
        panic!(
            "{name} on grove v{version}: proof of query {index} does not verify: {error:?}\n{query:?}"
        )
    };
    let (root_hash, proved): (_, Vec<PathKeyElementTrio>) = if query.raw {
        let (root_hash, proved) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .unwrap_or_else(|error| refused(error));
        let proved = proved
            .into_iter()
            .map(|row| {
                let element = Element::deserialize(&row.value, grove_version)
                    .expect("a proved value is an element");
                (row.path, row.key, element)
            })
            .collect();
        (root_hash, proved)
    } else {
        let (root_hash, proved) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .unwrap_or_else(|error| refused(error));
        let proved = proved
            .into_iter()
            .map(|(path, key, element)| (path, key, element.expect("a proved row has a value")))
            .collect();
        (root_hash, proved)
    };
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash"),
        "{name} on grove v{version}: proof of query {index} verifies to another root"
    );
    let expected: Vec<PathKeyElementTrio> = rows
        .into_iter()
        .map(|(path, key, element)| {
            let element = if element.underlying().is_reference() {
                db.get(path.as_slice(), &key, None, grove_version)
                    .unwrap()
                    .expect("a stored reference resolves")
            } else {
                element
            };
            (path, key, element)
        })
        .collect();
    assert!(
        proved == expected,
        "{name} on grove v{version}: query {index} proves other rows\n{query:?}\nproved: \
         {proved:?}\nexpected: {expected:?}"
    );
}

/// Proves a path query against committed state and checks that the proof
/// verifies to the database's root hash and to the answer the query gave. A
/// query the stream marks as not proven must be one GroveDB refuses to
/// prove.
fn prove_path_query(
    name: &str,
    index: usize,
    db: &GroveDb,
    query: &PathQuery,
    proven: bool,
    answer: PathQueryRun,
    grove_version: &GroveVersion,
) {
    let version = grove_version.protocol_version;
    let proof = db.prove_query(query, None, grove_version).unwrap();
    if !proven {
        assert!(
            proof.is_err(),
            "{name} on grove v{version}: query {index} is marked as not proven, but GroveDB \
             proves it\n{query:?}"
        );
        return;
    }
    let proof = proof.unwrap_or_else(|error| {
        panic!("{name} on grove v{version}: query {index} not provable: {error:?}\n{query:?}")
    });
    let verified =
        GroveDb::verify_path_query(&proof, query, grove_version).unwrap_or_else(|error| {
            panic!(
                "{name} on grove v{version}: proof of query {index} does not verify: \
                 {error:?}\n{query:?}"
            )
        });
    assert_eq!(
        *verified.root_hash(),
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash"),
        "{name} on grove v{version}: proof of query {index} verifies to another root"
    );
    let context =
        format!("{name} on grove v{version}: query {index} proves another answer\n{query:?}");
    match (answer, verified) {
        (
            PathQueryRun::Elements { elements, .. },
            VerifiedPathQuery::Elements {
                elements: proved, ..
            },
        ) => {
            let expected: Vec<_> = elements
                .to_path_key_elements()
                .into_iter()
                .map(|(path, key, element)| {
                    let element = if element.underlying().is_reference() {
                        db.get(path.as_slice(), &key, None, grove_version)
                            .unwrap()
                            .expect("a stored reference resolves")
                    } else {
                        element
                    };
                    (path, key, Some(element))
                })
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AggregateCount(count),
            VerifiedPathQuery::AggregateCount { count: proved, .. },
        ) => assert_eq!(proved, count, "{context}"),
        (PathQueryRun::AggregateSum(sum), VerifiedPathQuery::AggregateSum { sum: proved, .. }) => {
            assert_eq!(proved, sum, "{context}")
        }
        (
            PathQueryRun::AggregateCountAndSum { count, sum },
            VerifiedPathQuery::AggregateCountAndSum {
                count: proved_count,
                sum: proved_sum,
                ..
            },
        ) => assert_eq!((proved_count, proved_sum), (count, sum), "{context}"),
        (
            PathQueryRun::AggregateCountPerKey(per_key),
            VerifiedPathQuery::AggregatePerKey {
                per_key: proved, ..
            },
        ) => {
            let expected: Vec<_> = per_key
                .into_iter()
                .map(|(key, count)| (key, Some(count), None))
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AggregateSumPerKey(per_key),
            VerifiedPathQuery::AggregatePerKey {
                per_key: proved, ..
            },
        ) => {
            let expected: Vec<_> = per_key
                .into_iter()
                .map(|(key, sum)| (key, None, Some(sum)))
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AggregateCountAndSumPerKey(per_key),
            VerifiedPathQuery::AggregatePerKey {
                per_key: proved, ..
            },
        ) => {
            let expected: Vec<_> = per_key
                .into_iter()
                .map(|(key, count, sum)| (key, Some(count), Some(sum)))
                .collect();
            assert_eq!(proved, expected, "{context}");
        }
        (
            PathQueryRun::AxisEntries { entries, skipped },
            VerifiedPathQuery::AxisEntries {
                entries: proved,
                skipped: proved_skipped,
                ..
            },
        ) => assert_eq!((proved, proved_skipped), (entries, skipped), "{context}"),
        (
            PathQueryRun::AxisKeys { keys, skipped },
            VerifiedPathQuery::AxisEntries {
                entries: proved,
                skipped: proved_skipped,
                ..
            },
        ) => assert_eq!(
            (proved.to_keys(), proved_skipped),
            (keys, skipped),
            "{context}"
        ),
        (
            PathQueryRun::BranchedAxisEntries(branches),
            VerifiedPathQuery::BranchedAxisEntries {
                branches: proved, ..
            },
        ) => assert_eq!(proved, branches, "{context}"),
        (PathQueryRun::AxisRank(rank), VerifiedPathQuery::AxisRank { rank: proved, .. }) => {
            assert_eq!(proved, rank, "{context}")
        }
        (
            PathQueryRun::AxisAggregate(value),
            VerifiedPathQuery::AxisAggregate { value: proved, .. },
        ) => {
            let value = match value {
                AxisAggregateValue::Population(population) => i128::from(population),
                AxisAggregateValue::Total(total) => i128::from(total),
            };
            assert_eq!(proved, value, "{context}");
        }
        (
            PathQueryRun::SumBudget(sums),
            VerifiedPathQuery::SumBudget {
                matches,
                total,
                stop,
                ..
            },
        ) => {
            let Some(ReadMode::SumBudget(budget)) = query.query.query.read_mode.as_deref() else {
                panic!("{context}\nthe query has no sum budget");
            };
            let sum = sums
                .results
                .iter()
                .fold(0i64, |sum, (_, value)| sum.saturating_add(*value));
            let expected_stop = if sum >= budget.sum_limit as i64 {
                SumBudgetStop::BudgetReached
            } else if budget.match_limit.map(usize::from) == Some(sums.results.len()) {
                SumBudgetStop::MatchLimitReached
            } else if sums.hard_limit_reached {
                SumBudgetStop::HardScanCapReached
            } else {
                SumBudgetStop::Exhausted
            };
            assert_eq!(
                (matches, total, stop),
                (sums.results, sum, expected_stop),
                "{context}"
            );
        }
        (answer, verified) => panic!(
            "{name} on grove v{version}: query {index} was answered as {answer:?} and proved \
             as {verified:?}"
        ),
    }
}

/// Checks the page a query with an offset returned against the same query
/// run without offset or limit: the rows skipped are as many as the offset
/// asks for, or all there are, and the page is what follows them.
fn check_page(
    name: &str,
    index: usize,
    db: &GroveDb,
    read: &Read,
    answer: &PathQueryRun,
    grove_version: &GroveVersion,
) {
    let (query, raw) = match read {
        Read::Rows { query, raw } => (query, *raw),
        Read::PathQuery { query, .. } => (query, true),
        _ => return,
    };
    let (Some(offset), PathQueryRun::Elements { elements, skipped }) = (query.query.offset, answer)
    else {
        return;
    };
    let mut unpaged = (**query).clone();
    unpaged.query.offset = None;
    unpaged.query.limit = None;
    let unpaged = Op::Read(Read::Rows {
        query: Box::new(unpaged),
        raw,
    });
    let Outcome::Run(PathQueryRun::Elements { elements: all, .. }) =
        apply(db, &unpaged, None, grove_version)
            .unwrap()
            .expect("the query runs without its offset")
    else {
        panic!("{name}: a row reader returns rows");
    };
    let all = all.to_path_key_elements();
    let page = elements.clone().to_path_key_elements();
    let expected_skipped = usize::from(offset).min(all.len());
    let context = format!("{name}: query {index} pages wrongly\n{read:?}");
    assert_eq!(usize::from(*skipped), expected_skipped, "{context}");
    let rest = &all[expected_skipped..];
    assert!(page.len() <= rest.len(), "{context}");
    assert_eq!(page, rest[..page.len()], "{context}");
    let limit = query.query.limit.map_or(usize::MAX, usize::from);
    if query.query.query.has_subquery() {
        assert!(page.len() <= limit, "{context}");
    } else {
        assert_eq!(page.len(), rest.len().min(limit), "{context}");
    }
}

/// Runs `body` once per grove version, each on its own thread.
fn on_every_grove_version(body: impl Fn(&'static GroveVersion) + Sync) {
    let body = &body;
    thread::scope(|scope| {
        for grove_version in GROVE_VERSIONS {
            thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, move || body(grove_version))
                .expect("spawn");
        }
    });
}

#[test]
fn every_workload_applies_on_every_grove_version() {
    let only = std::env::var("WORKLOAD_ONLY").ok();
    let only = only.as_deref();
    on_every_grove_version(|grove_version| {
        let version = grove_version.protocol_version;
        let suite = Workload::suite(Features::for_grove_version(grove_version));
        let (_shared_dir, shared) = open();
        for workload in suite {
            let name = workload.name();
            if only.is_some_and(|only| only != name) {
                continue;
            }
            let (_dir, db) = open();
            let applied = run(name, &db, workload.stream(SEED), grove_version);
            report_verify_issues(name, &db, grove_version);
            println!(
                "grove v{version} {name}: {} operations applied",
                applied.ops
            );
            run(name, &shared, workload.stream(OTHER_SEED), grove_version);
        }
        report_verify_issues("suite", &shared, grove_version);
    });
}

/// The zoo's second pass picks what to replace, refresh and delete by seed,
/// so each seed meets other pairs of parent and child. `WORKLOAD_SEEDS` sets
/// how many seeds to try.
#[test]
fn the_zoo_applies_for_many_seeds() {
    let seeds: u64 = std::env::var("WORKLOAD_SEEDS")
        .ok()
        .map_or(12, |seeds| seeds.parse().expect("a number of seeds"));
    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        for seed in 2..2 + seeds {
            let (_dir, db) = open();
            let name = format!("element-zoo seed {seed}");
            run(
                &name,
                &db,
                ElementZoo { features }.stream(seed),
                grove_version,
            );
        }
    });
}

#[test]
fn the_smallest_workloads_apply() {
    let features = Features::ALL;
    let workloads = [
        Workload::Documents(Documents {
            documents: 1,
            owners: 1,
            deletes: 1,
            updates: 1,
            ..Documents::default()
        }),
        Workload::Documents(Documents {
            documents: 7,
            owners: u64::MAX,
            deletes: 7,
            transitions_per_block: 1,
            ..Documents::default()
        }),
        Workload::Balances(Balances {
            identities: 1,
            transfers: 5,
            ..Balances::default()
        }),
        Workload::Tokens(Tokens {
            tokens: 1,
            holders: 1,
            actions: 30,
            ..Tokens::default()
        }),
        Workload::DeepNesting(DeepNesting {
            depth: 1,
            chains: 4,
            leaves: 1,
        }),
        Workload::DeepNesting(DeepNesting {
            depth: 2,
            chains: 0,
            leaves: 1,
        }),
        Workload::LongKeys(LongKeys {
            keys: 0,
            nesting: 0,
        }),
        Workload::LongKeys(LongKeys {
            keys: 1,
            nesting: 1,
        }),
        Workload::LargeValues(LargeValues {
            max_value_size: 0,
            random_values: 0,
        }),
        Workload::TinySubtrees(TinySubtrees {
            subtrees: 0,
            features,
        }),
        Workload::TinySubtrees(TinySubtrees {
            subtrees: 1,
            features,
        }),
        Workload::DeleteHeavy(DeleteHeavy {
            keys: 1,
            rounds: 0,
            features,
        }),
        Workload::ReferenceChains(ReferenceChains {
            chains: 0,
            features,
        }),
        Workload::IdentityKeys(IdentityKeys {
            identities: 1,
            keys: 1,
            updates: 12,
            transitions_per_block: 1,
            features,
        }),
        Workload::PlatformState(PlatformState {
            masternodes: 0,
            validator_sets: 0,
            blocks: 5,
            blocks_per_rotation: 1,
        }),
        Workload::IndexRekey(IndexRekey {
            documents: 1,
            values: 1,
            twins: 1,
            moves_per_batch: 1,
            documents_after: 0,
            features,
        }),
        Workload::IndexRekey(IndexRekey {
            documents: 0,
            twins: 0,
            ..IndexRekey::default()
        }),
        Workload::Expiration(Expiration {
            blocks: 3,
            documents_per_block: 1,
            longest_life: 1,
            features,
        }),
        Workload::Expiration(Expiration {
            blocks: 2,
            documents_per_block: 0,
            ..Expiration::default()
        }),
        Workload::IndexAggregates(IndexAggregates {
            documents: 1,
            deletes: 1,
            owners: 1,
            transitions_per_block: 1,
            features,
        }),
        Workload::FlatDrops(FlatDrops {
            rounds: 0,
            features,
        }),
        Workload::Moves(Moves {
            rounds: 0,
            features,
        }),
        Workload::PartialBatches(PartialBatches {
            transitions: 0,
            transitions_per_block: 1,
        }),
        Workload::PartialBatches(PartialBatches {
            transitions: 9,
            transitions_per_block: 1,
        }),
        Workload::Pagination(Pagination {
            rows: 8,
            rounds: 1,
            features,
        }),
        Workload::ConditionalWrites(ConditionalWrites {
            transitions: 0,
            ..ConditionalWrites::default()
        }),
        Workload::ConditionalWrites(ConditionalWrites {
            transitions: 60,
            cities: 1,
            owners: 1,
            transitions_per_block: 1,
        }),
        Workload::IndexedWrites(IndexedWrites {
            rounds: 0,
            features,
        }),
        Workload::IndexedWrites(IndexedWrites {
            rounds: 1,
            features,
        }),
        Workload::TypedReads(TypedReads {
            rounds: 0,
            features,
        }),
        Workload::TypedReads(TypedReads {
            rounds: 1,
            features,
        }),
        Workload::IdentityReads(IdentityReads {
            identities: 1,
            updates: 12,
            transitions_per_block: 1,
            features,
        }),
        Workload::ChunkProofs(ChunkProofs {
            rows: MIN_CHUNKED_ROWS,
            rounds: 0,
            features,
        }),
        Workload::ChunkProofs(ChunkProofs {
            rows: MIN_CHUNKED_ROWS,
            rounds: 4,
            features,
        }),
        Workload::BlockExecution(BlockExecution {
            blocks: 0,
            ..BlockExecution::default()
        }),
        Workload::BlockExecution(BlockExecution {
            identities: 1,
            blocks: 13,
            transitions_per_block: 1,
        }),
        Workload::BlockExecution(BlockExecution {
            identities: 2,
            blocks: 8,
            transitions_per_block: 0,
        }),
        Workload::StorageFees(StorageFees {
            transitions: 0,
            ..StorageFees::default()
        }),
        Workload::StorageFees(StorageFees {
            transitions: 60,
            owners: 2,
            kinds: 1,
            transitions_per_block: 1,
            blocks_per_epoch: 1,
            features,
        }),
        Workload::FlagEpochs(FlagEpochs { rungs: 4, features }),
        Workload::TreeDeletes(TreeDeletes {
            rounds: 0,
            ..TreeDeletes::default()
        }),
        Workload::TreeDeletes(TreeDeletes {
            rounds: 1,
            items: MIN_TREE_DELETE_ITEMS,
        }),
    ];
    let grove_version = GroveVersion::latest();
    for workload in workloads {
        for seed in [0, u64::MAX] {
            let (_dir, db) = open();
            run(workload.name(), &db, workload.stream(seed), grove_version);
        }
    }
}

#[test]
fn the_deepest_nesting_applies_on_every_grove_version() {
    on_every_grove_version(|grove_version| {
        let deepest = DeepNesting {
            depth: MAX_NESTING_DEPTH,
            chains: 4,
            leaves: 2,
        };
        let (_dir, db) = open();
        run("deepest-nesting", &db, deepest.stream(SEED), grove_version);
    });
}

#[test]
fn the_largest_parameters_do_not_overflow() {
    let tokens = Tokens {
        tokens: 1,
        holders: u64::MAX,
        actions: 200,
        ..Tokens::default()
    };
    let (_dir, db) = open();
    run("tokens", &db, tokens.stream(SEED), GroveVersion::latest());

    let ladder = FlagEpochs {
        rungs: MAX_LADDER_RUNGS,
        features: Features::ALL,
    };
    assert!(ladder.stream(SEED).count() > MAX_LADDER_RUNGS as usize);
}

/// A benchmark can compare a workload with and without its reads: the
/// writes are the same.
#[test]
fn reads_do_not_change_the_writes() {
    fn writes(workload: Workload) -> (u64, String) {
        digest(
            workload
                .stream(SEED)
                .filter(|op| !matches!(op, Op::Read(_))),
        )
    }
    let pairs = [
        (
            Workload::Documents(Documents::default()),
            Workload::Documents(Documents {
                reads_per_block: 0,
                ..Documents::default()
            }),
        ),
        (
            Workload::Balances(Balances::default()),
            Workload::Balances(Balances {
                reads_per_block: 0,
                ..Balances::default()
            }),
        ),
        (
            Workload::Tokens(Tokens::default()),
            Workload::Tokens(Tokens {
                reads_per_block: 0,
                ..Tokens::default()
            }),
        ),
    ];
    for (with_reads, without_reads) in pairs {
        assert!(with_reads.stream(SEED).any(|op| matches!(op, Op::Read(_))));
        assert!(!without_reads
            .stream(SEED)
            .any(|op| matches!(op, Op::Read(_))));
        assert_eq!(
            writes(with_reads),
            writes(without_reads),
            "{}",
            with_reads.name()
        );
    }
}

/// The element variants and tree types no workload produces yet: added
/// upstream by #1055; the fourth BASE-01 unit produces them.
const NOT_YET_PRODUCED: [&str; 2] = ["CountBigSumTree", "ProvableBigSumTree"];

#[test]
fn the_suite_produces_every_element_variant_and_tree_type() {
    assert_eq!(
        Features::for_grove_version(GroveVersion::latest()),
        Features::ALL,
        "the suite applied to the latest grove version is not the one checked here"
    );
    let mut coverage = Coverage::new();
    for workload in Workload::suite(Features::ALL) {
        for op in workload.stream(SEED) {
            coverage.record(&op);
        }
    }
    assert_eq!(coverage.missing_elements(), NOT_YET_PRODUCED);
    assert_eq!(coverage.missing_tree_types(), NOT_YET_PRODUCED);
    assert_eq!(coverage.missing_reference_paths(), Vec::<&str>::new());
    println!(
        "{} element variants: {:?}",
        coverage.elements().len(),
        coverage.elements()
    );
    println!(
        "{} tree types: {:?}",
        coverage.tree_types().len(),
        coverage.tree_types()
    );
    println!(
        "{} reference path types: {:?}",
        coverage.reference_paths().len(),
        coverage.reference_paths()
    );

    let zoo: Vec<Op> = ElementZoo {
        features: Features::ALL,
    }
    .stream(OTHER_SEED)
    .collect();
    let zoo = Coverage::of(&zoo);
    assert_eq!(zoo.missing_elements(), NOT_YET_PRODUCED);
    assert_eq!(zoo.missing_tree_types(), NOT_YET_PRODUCED);
}

/// The workloads added for the operations that reach storage in a way of
/// their own.
fn operation_workloads(features: Features) -> Vec<Workload> {
    Workload::suite(features)
        .into_iter()
        .skip_while(|workload| workload.name() != "identity-keys")
        .collect()
}

/// Which rows a workload reads, pages and moves depends on the seed.
/// `WORKLOAD_SEEDS` sets how many seeds to try.
#[test]
fn the_operation_workloads_apply_for_many_seeds() {
    let seeds: u64 = std::env::var("WORKLOAD_SEEDS")
        .ok()
        .map_or(4, |seeds| seeds.parse().expect("a number of seeds"));
    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        let workloads = operation_workloads(features);
        assert_eq!(workloads.len(), 18);
        for workload in workloads {
            for seed in 2..2 + seeds {
                let (_dir, db) = open();
                let name = format!("{} seed {seed}", workload.name());
                run(&name, &db, workload.stream(seed), grove_version);
            }
        }
    });
}

fn suite_coverage(features: Features) -> Coverage {
    let mut coverage = Coverage::new();
    for workload in Workload::suite(features) {
        for op in workload.stream(SEED) {
            coverage.record(&op);
        }
    }
    coverage
}

#[test]
fn the_suite_produces_every_operation() {
    assert_eq!(
        Features::for_grove_version(GroveVersion::latest()),
        Features::ALL,
        "the suite applied to the latest grove version is not the one checked here"
    );
    let coverage = suite_coverage(Features::ALL);
    assert_eq!(coverage.missing_operations(), Vec::<Operation>::new());
    for group in OPERATION_GROUPS {
        let produced: Vec<&str> = coverage
            .operations()
            .iter()
            .filter(|operation| operation.group() == *group)
            .map(|operation| operation.name())
            .collect();
        assert!(!produced.is_empty(), "no workload produces {group}");
        println!("{group}: {produced:?}");
    }

    for grove_version in GROVE_VERSIONS {
        let features = Features::for_grove_version(grove_version);
        let unsupported: Vec<Operation> = Operation::ALL
            .iter()
            .copied()
            .filter(|operation| !features.supports(*operation))
            .collect();
        assert_eq!(
            suite_coverage(features).missing_operations(),
            unsupported,
            "grove v{}",
            grove_version.protocol_version
        );
        println!(
            "grove v{}: every operation but {unsupported:?}",
            grove_version.protocol_version
        );
    }

    let first_unit: Vec<Workload> = Workload::suite(Features::ALL)
        .into_iter()
        .take_while(|workload| workload.name() != "identity-keys")
        .collect();
    assert_eq!(first_unit.len(), 10);
    for workload in first_unit {
        let ops: Vec<Op> = workload.stream(SEED).collect();
        let beyond: Vec<Operation> = Coverage::of(&ops)
            .operations()
            .iter()
            .copied()
            .filter(|operation| {
                operation.group() != FIRST_UNIT_GROUP && operation.group() != TREE_DELETES_GROUP
            })
            .collect();
        assert!(
            beyond.is_empty(),
            "{} produces {beyond:?}, which the first unit did not have",
            workload.name()
        );
    }
}

#[test]
fn coverage_reports_what_a_stream_lacks() {
    let empty = Coverage::new();
    assert_eq!(empty.missing_operations(), Operation::ALL);
    let groups: BTreeSet<&str> = Operation::ALL
        .iter()
        .map(|operation| operation.group())
        .collect();
    assert_eq!(groups, OPERATION_GROUPS.iter().copied().collect());
    assert_eq!(OPERATION_GROUPS.len(), 18);
    let names: BTreeSet<&str> = Operation::ALL
        .iter()
        .map(|operation| operation.name())
        .collect();
    assert_eq!(names.len(), Operation::ALL.len());
    assert_eq!(empty.missing_elements(), ELEMENT_VARIANTS);
    assert_eq!(empty.missing_tree_types(), TREE_TYPES);
    assert_eq!(empty.missing_reference_paths(), REFERENCE_PATH_TYPES);

    let documents: Vec<Op> = Documents::default().stream(SEED).collect();
    let documents = Coverage::of(&documents);
    assert_eq!(
        documents.elements().iter().copied().collect::<Vec<_>>(),
        ["Item", "Reference", "Tree"]
    );
    assert_eq!(
        documents.tree_types().iter().copied().collect::<Vec<_>>(),
        ["NormalTree"]
    );
    assert!(documents.missing_elements().contains(&"MmrTree"));

    for names in [ELEMENT_VARIANTS, TREE_TYPES, REFERENCE_PATH_TYPES] {
        let unique: BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }
}

/// The digest of every workload's stream, for two seeds, generated for
/// [`Features::ALL`]: name, seed, operations, digest.
#[rustfmt::skip]
const PINNED_STREAMS: &[(&str, u64, u64, &str)] = &[
    ("documents", 0x1, 1208, "fbb70cc59ad376efbda8b7ce167df16b78191c23e91c92c6bd9fdbbb7190bf80"),
    ("documents", 0x4c49474e494e, 1208, "659032e87a47cf5a8bbf2f827276816645f1eba0ecdd51b03c87f806fd426d76"),
    ("balances", 0x1, 963, "5e9b74a10a9b77c52c6e3fbde4730dbcf5230cfeee582a82ebc54423dc1bd6b1"),
    ("balances", 0x4c49474e494e, 963, "bd2093107784baea22f85168ebc5fac77e5d36070a7c1f2431e7e7e2dcc7f508"),
    ("tokens", 0x1, 1013, "9c854f5c4192d2caff3a11e28a5c86191d8efd5b8adb6f3c8b750eeac081e92c"),
    ("tokens", 0x4c49474e494e, 1013, "42317281250ae58cdb2793ef11663af995a571e41096cf7eb64e403c330a461c"),
    ("element-zoo", 0x1, 2019, "52e9aa3fb0721dfbdd81865f0aab125dbf252db20b3b8d0d1ce942486d40ed0d"),
    ("element-zoo", 0x4c49474e494e, 2026, "3636474016c3c914098e1061a6acc4e40ad0771b8bf05d15faf132744a67bf05"),
    ("deep-nesting", 0x1, 377, "6461d8f8cf75990e32746b1e6f2bcbb5838bf1226e33ba916c9c53372146ea8a"),
    ("deep-nesting", 0x4c49474e494e, 377, "9ff40c4595ee15f9fd0111466e67b7c08de4391489b3f2a1239bb487f2c09d21"),
    ("long-keys", 0x1, 448, "8cabc4d420d4ec8a4a4c19c3cef115d06c07013aba75b78db11089cc6eb86560"),
    ("long-keys", 0x4c49474e494e, 511, "6d8c10161534851f5ec2c60573a9cac23265ef3dd61700913bcb1c711e5576db"),
    ("large-values", 0x1, 192, "1a72c05021d10874a99ca486d54a78d6e7319289088065d2ccfbc7b4d33cf483"),
    ("large-values", 0x4c49474e494e, 190, "620f3f31df09f43d601e055c37e694149b0763398263b798e10965444e7cb0d7"),
    ("tiny-subtrees", 0x1, 3640, "155c4fb8e06d7c8510b50dce0c7d158752648d0fbf3de2e67c5fc7b1e46c14c3"),
    ("tiny-subtrees", 0x4c49474e494e, 3710, "708de79761442cf04386ef9054fcdd0ea50aa034f3eabb1fd7c8d3e2ad57c437"),
    ("delete-heavy", 0x1, 6246, "7351393ef5fd04ef9b32b0a145e858465707c0cb7529a149c0be595b7278da26"),
    ("delete-heavy", 0x4c49474e494e, 6246, "2e9de83aa09c295c2cba188785c401fb2ba19136860685595a46c594ec411531"),
    ("reference-chains", 0x1, 460, "76d941fb245d7cb0be8005bbbc8aaf78bf04cf82fab6c233d0071865b95421b5"),
    ("reference-chains", 0x4c49474e494e, 460, "776d86bf9f6a7d87b8f7a5da2d14d27047dc6a9ab3439b7eec97092b75308e6b"),
    ("identity-keys", 0x1, 262, "4fd326146efeb547d2b9154c021c3d9e8f047bfb46f19ad145db54fba5d8c187"),
    ("identity-keys", 0x4c49474e494e, 262, "e715468ac182b86764efb2e69bde64c98ea31df041baf565c8292d1bafb9075e"),
    ("platform-state", 0x1, 227, "ad7a07360f8a524353908d5adea159ceaa5d3f0ed00c6fb2a190cf649a7c5db5"),
    ("platform-state", 0x4c49474e494e, 230, "e57620e2935db7f8e6454e61c97a85c60a9adf3b317833f6b91bbe3b74acf1e3"),
    ("index-rekey", 0x1, 320, "0d21d26fae26ed367c2eb28af967f53ea055edf5098bb6548db028fcd713c1f6"),
    ("index-rekey", 0x4c49474e494e, 320, "59feab58060ff927842bcf09a9f1e294ed7f914a86d518c536c887f081734f1c"),
    ("expiration", 0x1, 559, "a1e894ffe395c4d42af1510481d97d983dffe314e1c57fa15c27ab485b1cba0a"),
    ("expiration", 0x4c49474e494e, 563, "e35fd0fbaa598145529309a6daeff4d73e3427f177480b245316e0c823665f31"),
    ("index-aggregates", 0x1, 298, "cd048c0e6e76f57f042ed8df220f620cf96399479a7f1e239fbe134b31271731"),
    ("index-aggregates", 0x4c49474e494e, 298, "b547d3657cfbc1e1f01f0fd2d2a367fdd3c9e8bdbc7eb6003329c3fd8a0d409a"),
    ("flat-drops", 0x1, 179, "b080d17288c227df6bd39f7db4eaaf76036ff8b4a3eafff029c66812c8a56d0b"),
    ("flat-drops", 0x4c49474e494e, 179, "4f5039b35499b9f764c9326fd63bd5a1304371bbc1ee961473cfdb4641f09336"),
    ("moves", 0x1, 251, "5b3657e5db9ab8462294685a88559e3e7555ddcd8223e264a0d82edbe1d5eaf8"),
    ("moves", 0x4c49474e494e, 251, "4692bf1dead57fe5cec67d5fa8ff7073552b4491ea3a7f57309a15202324eae5"),
    ("partial-batches", 0x1, 229, "d74626348e1c61fa41fffb10d61a6a6be5859373cf6ae2e6d312d8b7a911461e"),
    ("partial-batches", 0x4c49474e494e, 229, "05a4bec0f5d876e8307ab29962158cded8eb28ed95004be37ecb5dc6d6a23ba5"),
    ("pagination", 0x1, 450, "c6f32bc26af5c7c640bd4a27c7f57a50b78e34b5384658ee0a3d97e6973722b4"),
    ("pagination", 0x4c49474e494e, 450, "8312d028f4fcd6c0ffe2cd4bc6282a8316c09acf0b69802a7d6501036371b075"),
    ("conditional-writes", 0x1, 889, "b79858ee32ef03eab31b71fdb88919f58bc1cd7b429def8188fc07821966de15"),
    ("conditional-writes", 0x4c49474e494e, 872, "d441efc8e5ae8b672dcc286427d29c0874c03b4e355ff2aea2f62c51dcbfd1fc"),
    ("indexed-writes", 0x1, 206, "8d9746f65dacd4b8b3538181acb20105b3e6454ad0fdd9e438000a3f4a22e0ec"),
    ("indexed-writes", 0x4c49474e494e, 206, "e452f11f12ac6e55d4b3cd1f9cf93a43d3190a27e15239c1d817377a5e08a24a"),
    ("typed-reads", 0x1, 289, "f915af2ee469465ef4b826d4bfbe2dc453a13077642eca9f817f9996eada82e6"),
    ("typed-reads", 0x4c49474e494e, 309, "ff49b183beed5f3bc5150102b56064f46e1d8af88f29f728b9402f8a137de1d8"),
    ("identity-reads", 0x1, 188, "9c7d4ac1552df91c50b967263e42b795b6c0db32f61b4c95635813b26cc3de38"),
    ("identity-reads", 0x4c49474e494e, 188, "186123bc3e879f82a40d47d86bffc63d07a7be6b174c02c6964e7b80505b6058"),
    ("chunk-proofs", 0x1, 262, "f341f5745226be883674d89660944dd66b7294ec7da07d658743a24d325df0a6"),
    ("chunk-proofs", 0x4c49474e494e, 267, "4bdd019fd5874626982eb8e99692ad40fcdb14062bd1631bd3b3a0d9700442a9"),
    ("block-execution", 0x1, 684, "d7b09d7424f2cfab718032e4933e9bd1af731e2c0d142df2c367217faf1f2235"),
    ("block-execution", 0x4c49474e494e, 687, "cbe8343d2304f8abbcfb2b3ff9377ba9d707d1caced7776c0cd1b0f69668818c"),
    ("storage-fees", 0x1, 439, "b18147d05baa8b710ebf2545dc381dfa8bf20c2e20381f5c52761b5b2250f266"),
    ("storage-fees", 0x4c49474e494e, 439, "169adad5c28b77dd42444150eb3af64fbc0f3f1be8b743d072b7a3f2da2c7220"),
    ("flag-epochs", 0x1, 149, "43baa2387a1cb98d8d65af296cbace04954f3b58c38aad5f7d74597481813dbc"),
    ("flag-epochs", 0x4c49474e494e, 149, "8b20523f2db8cd5765367eed3452262f595c6ee763baa1ab3d43f2f9f61c1a18"),
    ("tree-deletes", 0x1, 96, "b85e2cebee162a67848b7f9e51001329a58a24612782efd7ecd94ef926ea2a07"),
    ("tree-deletes", 0x4c49474e494e, 96, "1473dddd7a7f218f5a36e7ef7c0b54fa32333e6ebf739d85a1a3be882c1dfc76"),
];

#[test]
fn streams_are_pinned() {
    let mut actual = Vec::new();
    for workload in Workload::suite(Features::ALL) {
        for seed in [SEED, OTHER_SEED] {
            let (ops, hex) = digest(workload.stream(seed));
            assert_eq!(
                (ops, hex.clone()),
                digest(workload.stream(seed)),
                "{} gives two streams for seed {seed}",
                workload.name()
            );
            actual.push((workload.name(), seed, ops, hex));
        }
    }
    let actual: String = actual
        .iter()
        .map(|(name, seed, ops, hex)| format!("    ({name:?}, {seed:#x}, {ops}, {hex:?}),\n"))
        .collect();
    let expected: String = PINNED_STREAMS
        .iter()
        .map(|(name, seed, ops, hex)| format!("    ({name:?}, {seed:#x}, {ops}, {hex:?}),\n"))
        .collect();
    assert!(actual == expected, "the streams are now:\n{actual}");
}

/// The digest of the whole suite generated for each grove version's
/// features, seed [`SEED`]: protocol version, operations, digest.
#[rustfmt::skip]
const PINNED_SUITES: &[(u32, u64, &str)] = &[
    (1, 21716, "a137b573fa8e0e4189f3fbf8b68db6c9587358c316cba2fda9f101e0ff6049ae"),
    (2, 21716, "a137b573fa8e0e4189f3fbf8b68db6c9587358c316cba2fda9f101e0ff6049ae"),
    (3, 21740, "ba8f61b8ff5baa4e25652386a670b68af99e7a7c6a47bea7c0d628cbfcd420a9"),
    (4, 22228, "67acf0071537977173e5607cced138331aeeaac4d720665f1e51e5cd36356616"),
    (5, 22543, "a8212502aa2d25bf9e823907673ff7bba4a08f2fdfa0d9de04239212750b3975"),
];

#[test]
fn a_suite_is_pinned_for_every_grove_version() {
    let actual: String = GROVE_VERSIONS
        .iter()
        .map(|grove_version| {
            let suite = Workload::suite(Features::for_grove_version(grove_version));
            let (ops, hex) = digest(suite.iter().flat_map(|workload| workload.stream(SEED)));
            format!(
                "    ({}, {ops}, {hex:?}),\n",
                grove_version.protocol_version
            )
        })
        .collect();
    let expected: String = PINNED_SUITES
        .iter()
        .map(|(version, ops, hex)| format!("    ({version}, {ops}, {hex:?}),\n"))
        .collect();
    assert!(actual == expected, "the suites are now:\n{actual}");
}

#[test]
fn seeds_and_workloads_give_different_streams() {
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    for workload in Workload::suite(Features::ALL) {
        assert!(names.insert(workload.name()), "two workloads share a name");
        for seed in [0, SEED, 2, OTHER_SEED, u64::MAX] {
            let (_, hex) = digest(workload.stream(seed));
            assert!(
                seen.insert(hex),
                "{} repeats a stream at seed {seed}",
                workload.name()
            );
        }
    }
}

/// The first operations of the million-document stream the plan measures.
const REFERENCE_PREFIX_OPS: usize = 50_000;
const PINNED_REFERENCE_PREFIX: &str =
    "e212500dcfe1a4f8d491e8b0cfae9f19db17de9c7fcbaebed13d38037857f8e9";

#[test]
fn the_reference_documents_stream_is_pinned() {
    let reference = Documents::reference();
    assert_eq!(reference.documents, 1_000_000);
    assert_eq!(reference.document_size, 400);
    let (ops, hex) = digest(reference.stream(SEED).take(REFERENCE_PREFIX_OPS));
    assert_eq!(ops, REFERENCE_PREFIX_OPS as u64);
    assert_eq!(hex, PINNED_REFERENCE_PREFIX);
}

#[test]
fn the_commitment_payload_length_matches_grovedb() {
    assert_eq!(
        COMMITMENT_PAYLOAD_LEN as usize,
        ciphertext_payload_size::<DashMemo>()
    );
}

/// One operation of every kind the second unit added, and one query for
/// every construct of GroveDB's query grammar.
fn one_of_each_new_kind() -> Vec<Op> {
    use grovedb::{
        AggregateSumPathQuery, AxisProjection, AxisQuery, PathQuery, Query, QueryItem, SizedQuery,
        TreeType,
    };
    use grovedb_merk::proofs::query::{AggregateFold, AggregateSumQuery, IndexAxis};
    use lignin_testkit::workload::PartialBatch;

    let path = vec![b"tree".to_vec()];
    let key = b"key".to_vec();
    let bytes = |byte: u8| vec![byte; 2];
    let mut ops = vec![
        Op::Write(Write::PutAux {
            key: key.clone(),
            value: bytes(1),
        }),
        Op::Write(Write::DeleteAux { key: key.clone() }),
        Op::Write(Write::DropFlat {
            path: path.clone(),
            key: key.clone(),
        }),
        Op::Batch(vec![
            BatchOp::Patch {
                path: path.clone(),
                key: key.clone(),
                element: Element::new_item(bytes(2)),
                change_in_bytes: -3,
            },
            BatchOp::Move {
                path: path.clone(),
                key: key.clone(),
                new_key: bytes(3),
            },
            BatchOp::DropFlat {
                path: path.clone(),
                key: key.clone(),
                tree_type: TreeType::BulkAppendTree(4),
            },
        ]),
        Op::PartialBatch(PartialBatch {
            ops: vec![BatchOp::Delete {
                path: path.clone(),
                key: key.clone(),
            }],
            add_on: vec![BatchOp::InsertOnly {
                path: path.clone(),
                key: key.clone(),
                element: Element::new_sum_item(5),
            }],
            pause_height: Some(2),
        }),
        Op::PartialBatch(PartialBatch {
            ops: Vec::new(),
            add_on: Vec::new(),
            pause_height: None,
        }),
        Op::FlushPendingDrops,
        Op::Read(Read::GetAux { key: key.clone() }),
        Op::Read(Read::AuxByPrefix {
            prefix: key.clone(),
        }),
    ];
    for (skip_non_sum_items, ignore_references) in [(false, true), (true, false)] {
        ops.push(Op::Read(Read::SumBudget {
            query: Box::new(AggregateSumPathQuery::new(
                path.clone(),
                AggregateSumQuery {
                    items: vec![QueryItem::Key(key.clone())],
                    left_to_right: skip_non_sum_items,
                    sum_limit: 600,
                    limit_of_items_to_check: ignore_references.then_some(7),
                },
            )),
            skip_non_sum_items,
            ignore_references,
        }));
    }

    let items = [
        QueryItem::Key(bytes(1)),
        QueryItem::Range(bytes(1)..bytes(2)),
        QueryItem::RangeInclusive(bytes(1)..=bytes(2)),
        QueryItem::RangeFull(..),
        QueryItem::RangeFrom(bytes(1)..),
        QueryItem::RangeTo(..bytes(2)),
        QueryItem::RangeToInclusive(..=bytes(2)),
        QueryItem::RangeAfter(bytes(1)..),
        QueryItem::RangeAfterTo(bytes(1)..bytes(2)),
        QueryItem::RangeAfterToInclusive(bytes(1)..=bytes(2)),
        QueryItem::AggregateCountOnRange(Box::new(QueryItem::RangeFrom(bytes(1)..))),
        QueryItem::AggregateSumOnRange(Box::new(QueryItem::RangeFrom(bytes(1)..))),
        QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::RangeFrom(bytes(1)..))),
    ];
    let mut queries: Vec<PathQuery> = items
        .into_iter()
        .map(|item| PathQuery::new_single_query_item(path.clone(), item))
        .collect();

    let mut nested = Query::new_single_query_item_with_direction(QueryItem::RangeFull(..), false);
    nested.limit = Some(3);
    nested.offset = Some(4);
    let mut branching = Query::new_range_full();
    branching.set_subquery_path(vec![bytes(5), bytes(6)]);
    branching.set_subquery(nested.clone());
    branching.add_conditional_subquery(QueryItem::Key(bytes(7)), Some(vec![bytes(8)]), None);
    branching.add_conditional_subquery(QueryItem::RangeFrom(bytes(9)..), None, Some(nested));
    branching.add_parent_tree_on_subquery = true;
    queries.push(PathQuery::new(
        path.clone(),
        SizedQuery::new(branching.clone(), Some(10), None),
    ));
    queries.push(PathQuery::new(
        path.clone(),
        SizedQuery::new(branching, None, Some(11)),
    ));
    queries.push(PathQuery::new(
        path.clone(),
        SizedQuery::new(Query::new_range_full(), Some(12), Some(13)),
    ));
    for aggregate in [
        Query::new_aggregate_count_on_range,
        Query::new_aggregate_sum_on_range,
        Query::new_aggregate_count_and_sum_on_range,
    ] {
        let mut carrier = Query::new_single_key(bytes(1));
        carrier.set_subquery(aggregate(QueryItem::RangeTo(..bytes(2))));
        queries.push(PathQuery::new_unsized(path.clone(), carrier));
    }

    let axis_queries = [
        AxisQuery::top_k(IndexAxis::Count, 3, 4, true),
        AxisQuery::bottom_k(IndexAxis::Sum, 3, 4),
        AxisQuery::bounded(IndexAxis::Avg, -5, 6, 7, false),
        AxisQuery::rank_of_key(IndexAxis::Count, key.clone(), true),
        AxisQuery::aggregate_over_value_range(IndexAxis::Sum, -5, 6, AggregateFold::Population),
        AxisQuery::aggregate_over_value_range(IndexAxis::Sum, -5, 6, AggregateFold::Total),
        AxisQuery::top_k(IndexAxis::Count, 3, 4, true).with_projection(AxisProjection::Keys),
    ];
    for axis_query in axis_queries {
        queries.push(PathQuery::new_axis(path.clone(), axis_query));
    }
    queries.push(PathQuery::new_branched_axis(
        path.clone(),
        vec![bytes(1), bytes(2)],
        vec![bytes(3)],
        AxisQuery::top_k(IndexAxis::Count, 3, 0, false),
    ));
    for match_limit in [None, Some(9)] {
        queries.push(PathQuery::new_sum_budget(
            path.clone(),
            vec![QueryItem::RangeFull(..)],
            true,
            700,
            match_limit,
        ));
    }

    for query in queries {
        for proven in [false, true] {
            ops.push(Op::Read(Read::PathQuery {
                query: Box::new(query.clone()),
                proven,
            }));
        }
        ops.push(Op::Read(Read::Rows {
            query: Box::new(query),
            raw: true,
        }));
    }
    ops
}

const PINNED_NEW_KINDS: &str = "97278877b581d311033c13aa0ebdfd3e0f5c1141a7cf8a45da94b262dbcd5b49";

/// The canonical encoding of the kinds the second unit added: each one
/// differs from every other, and the bytes are pinned, so a change to the
/// encoding cannot pass as a change to a workload.
#[test]
fn the_new_kinds_have_a_pinned_encoding() {
    let ops = one_of_each_new_kind();
    let mut encodings = BTreeSet::new();
    for op in &ops {
        let mut bytes = Vec::new();
        op.encode(&mut bytes);
        assert!(encodings.insert(bytes), "two operations encode as {op:?}");
        assert_eq!(*op, op.clone());
    }
    let digest = StreamDigest::of(&ops);
    assert_eq!(digest.ops(), ops.len() as u64);
    assert_eq!(digest.hex(), PINNED_NEW_KINDS);

    let coverage = Coverage::of(&ops);
    let missing: Vec<Operation> = coverage
        .missing_operations()
        .into_iter()
        .filter(|operation| SECOND_UNIT_GROUPS.contains(&operation.group()))
        .collect();
    assert_eq!(missing, Vec::<Operation>::new());
    for op in &ops {
        let (query, through_rows) = match op {
            Op::Read(Read::PathQuery { query, .. }) => (query, false),
            Op::Read(Read::Rows { query, .. }) => (query, true),
            _ => continue,
        };
        if query.query.offset.is_none() || !query.query.query.has_subquery() {
            continue;
        }
        let operations = Coverage::of([op]).operations().clone();
        if through_rows {
            assert!(
                operations.contains(&Operation::QueryOffset),
                "the row readers take an offset next to a subquery"
            );
        } else {
            assert!(
                operations.is_empty(),
                "`run_path_query` refuses an offset next to a subquery"
            );
        }
    }
    let stored: Vec<Element> = ops
        .iter()
        .flat_map(|op| op.elements().into_iter().cloned())
        .collect();
    assert_eq!(
        stored,
        [Element::new_item(vec![2; 2]), Element::new_sum_item(5)]
    );
    assert!(ops
        .iter()
        .all(|op| op.follows_a_commit() == matches!(op, Op::Read(_) | Op::FlushPendingDrops)));
}

/// Reads are compared as they are encoded: the order of a query's
/// conditional branches counts, and so does having none against having an
/// empty set of them.
#[test]
fn reads_are_equal_when_they_encode_the_same() {
    use grovedb::{PathQuery, Query, QueryItem, SizedQuery};

    let read = |query: Query| {
        Op::Read(Read::PathQuery {
            query: Box::new(PathQuery::new(
                vec![b"tree".to_vec()],
                SizedQuery::new(query, None, None),
            )),
            proven: true,
        })
    };
    let branches = |first: u8, second: u8| {
        let mut query = Query::new_range_full();
        for key in [first, second] {
            query.add_conditional_subquery(
                QueryItem::Key(vec![key]),
                None,
                Some(Query::new_single_key(vec![key])),
            );
        }
        query
    };
    assert_eq!(read(branches(1, 2)), read(branches(1, 2)));
    assert_ne!(read(branches(1, 2)), read(branches(2, 1)));
    assert_eq!(branches(1, 2), branches(2, 1), "GroveDB ignores the order");

    let mut no_branches = Query::new_range_full();
    no_branches.conditional_subquery_branches = Some(Default::default());
    assert_ne!(read(no_branches), read(Query::new_range_full()));
}

/// The moves workload moves keys alone, swaps two keys and chains two
/// moves through one key, and goes on doing so for more rounds than its
/// default.
#[test]
fn the_moves_workload_swaps_and_chains() {
    let moves_of = |op: &Op| -> Vec<(Vec<u8>, Vec<u8>)> {
        let Op::Batch(ops) = op else {
            return Vec::new();
        };
        ops.iter()
            .filter_map(|op| match op {
                BatchOp::Move { key, new_key, .. } => Some((key.clone(), new_key.clone())),
                _ => None,
            })
            .collect()
    };
    let (mut alone, mut swaps, mut chains, mut longer, mut shorter) = (0, 0, 0, 0, 0);
    for op in Workload::Moves(Moves::default()).stream(SEED) {
        let moves = moves_of(&op);
        let taken = |key: &Vec<u8>| moves.iter().any(|(from, _)| from == key);
        let freed = |key: &Vec<u8>| moves.iter().any(|(_, to)| to == key);
        match moves.as_slice() {
            [] => {}
            [_] => alone += 1,
            [(a, b), (c, d)] if a == d && b == c => swaps += 1,
            _ if moves.iter().any(|(from, to)| taken(to) && !freed(from)) => chains += 1,
            _ => {}
        }
        for (from, to) in &moves {
            longer += u32::from(to.len() > from.len());
            shorter += u32::from(to.len() < from.len());
        }
    }
    println!("alone {alone}, swaps {swaps}, chains {chains}, longer {longer}, shorter {shorter}");
    assert!(alone > 0 && swaps > 0 && chains > 0 && longer > 0 && shorter > 0);

    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        for seed in [SEED, 2, 3] {
            let (_dir, db) = open();
            let workload = Moves {
                rounds: 7,
                features,
            };
            run("moves", &db, workload.stream(seed), grove_version);
        }
    });
}

/// A query GroveDB's grammar refuses counts as none of the operations.
#[test]
fn a_malformed_query_is_no_operation() {
    use grovedb::{PathQuery, Query, QueryItem, SizedQuery};

    let mut two_aggregates = Query::new();
    two_aggregates.items = vec![
        QueryItem::AggregateCountOnRange(Box::new(QueryItem::RangeFrom(vec![1]..))),
        QueryItem::AggregateSumOnRange(Box::new(QueryItem::RangeFrom(vec![1]..))),
    ];
    let malformed = PathQuery::new(
        vec![b"tree".to_vec()],
        SizedQuery::new(two_aggregates, None, None),
    );
    assert!(malformed.classify().is_err());
    let op = Op::Read(Read::PathQuery {
        query: Box::new(malformed),
        proven: false,
    });
    assert!(Coverage::of([&op]).operations().is_empty());
}

/// One operation of every kind the third unit added, in an order that also
/// produces the operations that depend on what came before.
fn one_of_each_direct_kind() -> Vec<Op> {
    use grovedb::Query;
    use lignin_testkit::workload::{ChunkIds, IndexedTree};

    let path = vec![b"tree".to_vec()];
    let key = b"key".to_vec();
    let item = Element::new_item(vec![1; 2]);
    let query = || {
        Box::new(PathQuery::new_unsized(
            path.clone(),
            Query::new_range_full(),
        ))
    };
    let step = |step: TransactionOp| Op::Transaction(step);
    let write = |write: Write| Op::Write(write);

    let mut ops = vec![
        step(TransactionOp::SetSavepoint),
        step(TransactionOp::SetSavepoint),
        step(TransactionOp::RollbackToSavepoint),
        Op::Commit,
        step(TransactionOp::Rollback),
        step(TransactionOp::Abandon),
        step(TransactionOp::OpenSnapshot),
        Op::Commit,
        step(TransactionOp::SnapshotRead(Read::IsEmptyTree {
            path: path.clone(),
        })),
        step(TransactionOp::CloseSnapshot),
        step(TransactionOp::BlockRead(Read::IsEmptyTree {
            path: path.clone(),
        })),
    ];
    for return_existing in [false, true] {
        ops.push(write(Write::InsertIfNotExists {
            path: path.clone(),
            key: key.clone(),
            element: item.clone(),
            return_existing,
        }));
    }
    ops.push(write(Write::InsertIfChanged {
        path: path.clone(),
        key: key.clone(),
        element: item.clone(),
    }));
    ops.push(write(Write::DeleteIfEmptyTree {
        path: path.clone(),
        key: key.clone(),
    }));
    for (stop_path_height, built) in [(None, false), (Some(3), false), (Some(3), true)] {
        ops.push(write(Write::DeleteUpTree {
            path: path.clone(),
            key: key.clone(),
            stop_path_height,
            built,
        }));
    }
    ops.push(write(Write::DeleteBuilt {
        path: path.clone(),
        key: key.clone(),
    }));
    for tree in [IndexedTree::Count, IndexedTree::Sum, IndexedTree::CountSum] {
        ops.push(write(Write::IndexedInsert {
            path: path.clone(),
            key: key.clone(),
            element: Element::new_sum_item(4),
            tree,
        }));
        ops.push(write(Write::IndexedDelete {
            path: path.clone(),
            key: key.clone(),
            tree,
        }));
    }
    ops.push(write(Write::ReconcileIndexed { path: path.clone() }));
    ops.push(write(Write::CommitmentInsert {
        path: path.clone(),
        cmx: [5; 32],
        rho: [6; 32],
        cv_net: [7; 32],
        payload: vec![8; 3],
    }));

    let mut reads = vec![
        Read::HasRaw {
            path: path.clone(),
            key: key.clone(),
        },
        Read::IsEmptyTree { path: path.clone() },
    ];
    for fetch in [Fetch::Resolved, Fetch::Raw, Fetch::RawOptional] {
        for cache in [false, true] {
            reads.push(Read::Fetch {
                path: path.clone(),
                key: key.clone(),
                fetch,
                cache,
            });
        }
    }
    let (start, limit) = (9, 10);
    for read in [
        TreeRead::MmrValue(1),
        TreeRead::MmrRootHash,
        TreeRead::MmrLeafCount,
        TreeRead::BulkValue(1),
        TreeRead::BulkRange { start, limit },
        TreeRead::BulkChunk(1),
        TreeRead::BulkBuffer,
        TreeRead::BulkCount,
        TreeRead::BulkChunkCount,
        TreeRead::DenseValue(1),
        TreeRead::DenseRootHash,
        TreeRead::DenseCount,
        TreeRead::CommitmentValue(1),
        TreeRead::CommitmentRange { start, limit },
        TreeRead::CommitmentAnchor,
        TreeRead::CommitmentCount,
        TreeRead::PrivateDocumentValue(1),
        TreeRead::PrivateDocumentRange { start, limit },
        TreeRead::PrivateDocumentCount,
    ] {
        reads.push(Read::Tree {
            path: path.clone(),
            read,
        });
    }
    for flag in [false, true] {
        reads.push(Read::KeysOptional {
            query: query(),
            raw: flag,
        });
        reads.push(Read::ItemValues {
            query: query(),
            or_sums: flag,
        });
        for per_key in [false, true] {
            reads.push(Read::BigSum {
                query: query(),
                with_count: flag,
                per_key,
            });
        }
    }
    reads.push(Read::SumValues { query: query() });
    reads.push(Read::Many {
        queries: vec![*query(), *query()],
    });
    reads.push(Read::Many {
        queries: Vec::new(),
    });
    let merk = |ids: &[&[u8]]| ChunkIds::Merk(ids.iter().map(|id| id.to_vec()).collect());
    for prove in [
        Prove::PathQuery(query()),
        Prove::Many(vec![*query()]),
        Prove::BulkPositionRange {
            path: path.clone(),
            key: key.clone(),
            start,
            limit,
        },
        Prove::TrunkChunk {
            path: path.clone(),
            max_depth: 3,
            min_depth: None,
        },
        Prove::TrunkChunk {
            path: path.clone(),
            max_depth: 3,
            min_depth: Some(2),
        },
        Prove::BranchChunk {
            path: path.clone(),
            key: key.clone(),
            depth: 3,
        },
        Prove::Chunk {
            path: Vec::new(),
            ids: merk(&[]),
        },
        Prove::Chunk {
            path: Vec::new(),
            ids: merk(&[&[]]),
        },
        Prove::Chunk {
            path: path.clone(),
            ids: merk(&[]),
        },
        Prove::Chunk {
            path: path.clone(),
            ids: merk(&[&[1, 0], &[0]]),
        },
        Prove::Chunk {
            path: path.clone(),
            ids: ChunkIds::Entries {
                start: 11,
                state: 12,
                parameter: 13,
            },
        },
    ] {
        reads.push(Read::Prove(prove));
    }
    ops.extend(reads.into_iter().map(Op::Read));
    ops
}

const PINNED_DIRECT_KINDS: &str =
    "b86aca0a9bef3f126e7a4e60c55e6aeafd32d2e7133c7c7e2ad4bcbd0dcc411c";

/// The canonical encoding of the kinds the third unit added: each one
/// differs from every other, and the bytes are pinned. Together they are
/// every operation of the groups that unit added.
#[test]
fn the_direct_kinds_have_a_pinned_encoding() {
    let ops = one_of_each_direct_kind();
    let mut encodings = BTreeSet::new();
    for op in &ops {
        let mut bytes = Vec::new();
        op.encode(&mut bytes);
        // A commit and a savepoint are in the list twice, for what follows
        // them.
        let twice = matches!(
            op,
            Op::Commit | Op::Transaction(TransactionOp::SetSavepoint)
        );
        assert!(
            encodings.insert(bytes) || twice,
            "two operations encode as {op:?}"
        );
        assert_eq!(*op, op.clone());
        assert!(
            !(op.ends_transaction() && op.keeps_transaction()),
            "{op:?} both ends and keeps the transaction"
        );
    }
    let digest = StreamDigest::of(&ops);
    assert_eq!(digest.ops(), ops.len() as u64);
    assert_eq!(digest.hex(), PINNED_DIRECT_KINDS);

    let missing: Vec<Operation> = Coverage::of(&ops)
        .missing_operations()
        .into_iter()
        .filter(|operation| {
            operation.group() != FIRST_UNIT_GROUP
                && !SECOND_UNIT_GROUPS.contains(&operation.group())
                && operation.group() != FLAG_CALLBACKS_GROUP
                && operation.group() != TREE_DELETES_GROUP
        })
        .collect();
    assert_eq!(missing, Vec::<Operation>::new());

    let stored = ops.iter().flat_map(|op| op.elements()).count();
    assert_eq!(stored, 6, "the conditional and the indexed inserts store");
    let ends: Vec<&Op> = ops.iter().filter(|op| op.ends_transaction()).collect();
    assert_eq!(
        ends,
        [
            &Op::Commit,
            &Op::Transaction(TransactionOp::Rollback),
            &Op::Transaction(TransactionOp::Abandon),
            &Op::Commit
        ]
    );
    let writes = ops.iter().filter(|op| !op.keeps_transaction()).count();
    assert_eq!(
        writes,
        4 + 1 + 16,
        "ends, a savepoint rollback and the writes"
    );
}

/// A read through a snapshot returns what was committed when the snapshot
/// was opened, and the workload makes that differ from what is committed
/// when the read runs.
#[test]
fn block_execution_reads_older_state_through_its_snapshot() {
    on_every_grove_version(|grove_version| {
        let (_dir, db) = open();
        let applied = run(
            "block-execution",
            &db,
            BlockExecution::default().stream(SEED),
            grove_version,
        );
        assert!(
            applied.snapshot_reads_of_older_state >= 5,
            "only {} snapshot reads saw an older state",
            applied.snapshot_reads_of_older_state
        );
    });
}

/// The branch chunks of the last round hang below every key of one tree,
/// so those below the keys at the bottom of its trunk are verified against
/// the hashes the trunk gives.
#[test]
fn chunk_proofs_verify_branches_against_their_trunk() {
    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        for seed in [SEED, OTHER_SEED, 3] {
            let (_dir, db) = open();
            let workload = ChunkProofs {
                features,
                ..ChunkProofs::default()
            };
            let applied = run("chunk-proofs", &db, workload.stream(seed), grove_version);
            assert!(
                applied.branches_verified >= 2,
                "seed {seed}: {} branches verified",
                applied.branches_verified
            );
        }
    });
}

/// Every typed read of the append-only trees returns what the stream
/// appended: the model is the stream's own appends.
#[test]
fn typed_reads_return_what_was_appended() {
    use lignin_testkit::workload::AppendEntry;

    on_every_grove_version(|grove_version| {
        let features = Features::for_grove_version(grove_version);
        let workload = TypedReads {
            rounds: 8,
            features,
        };
        let (_dir, db) = open();
        let mut transactions = Transactions::new(&db);
        let mut appended: BTreeMap<Vec<Vec<u8>>, Vec<Vec<u8>>> = BTreeMap::new();
        let mut checked = 0;
        for op in workload.stream(SEED) {
            let outcome = transactions
                .apply(&op, grove_version)
                .unwrap()
                .unwrap_or_else(|error| panic!("{op:?} refused: {error:?}"));
            match &op {
                Op::Write(Write::Append { path, entry }) => {
                    let value = match entry {
                        AppendEntry::Mmr(value)
                        | AppendEntry::Bulk(value)
                        | AppendEntry::Dense(value)
                        | AppendEntry::PrivateDocument(value) => value.clone(),
                        AppendEntry::Commitment { .. } => panic!("commitments are typed here"),
                    };
                    appended.entry(path.clone()).or_default().push(value);
                }
                Op::Write(Write::CommitmentInsert {
                    path,
                    cmx,
                    rho,
                    cv_net,
                    payload,
                }) => {
                    let value = [&cmx[..], &rho[..], &cv_net[..], payload].concat();
                    appended.entry(path.clone()).or_default().push(value);
                }
                Op::Read(Read::Tree { path, read }) => {
                    let values = &appended[path];
                    let count = values.len() as u64;
                    let at = |position: u64| values.get(position as usize).cloned();
                    let page = |start: u64, limit: u16| {
                        let end = (start + u64::from(limit)).min(count);
                        let entries = (start..end).map(|position| {
                            (position, at(position).expect("a position below the count"))
                        });
                        Outcome::Page(grovedb::RangePage {
                            entries: entries.collect(),
                            total_count: count,
                        })
                    };
                    // A chunk of the bulk tree holds four values.
                    let chunks = count / 4;
                    let expected = match *read {
                        TreeRead::MmrValue(position)
                        | TreeRead::BulkValue(position)
                        | TreeRead::CommitmentValue(position)
                        | TreeRead::PrivateDocumentValue(position) => {
                            Some(Outcome::Value(at(position)))
                        }
                        TreeRead::DenseValue(position) => {
                            Some(Outcome::Value(at(u64::from(position))))
                        }
                        TreeRead::MmrLeafCount
                        | TreeRead::BulkCount
                        | TreeRead::DenseCount
                        | TreeRead::CommitmentCount
                        | TreeRead::PrivateDocumentCount => Some(Outcome::Count(count)),
                        TreeRead::BulkChunkCount => Some(Outcome::Count(chunks)),
                        TreeRead::BulkBuffer => {
                            Some(Outcome::Values(values[(chunks * 4) as usize..].to_vec()))
                        }
                        TreeRead::BulkRange { start, limit }
                        | TreeRead::CommitmentRange { start, limit }
                        | TreeRead::PrivateDocumentRange { start, limit } => {
                            Some(page(start, limit))
                        }
                        TreeRead::BulkChunk(index) => {
                            let Outcome::Value(chunk) = &outcome else {
                                panic!("{op:?} returned {outcome:?}");
                            };
                            assert_eq!(chunk.is_some(), index < chunks, "{op:?}");
                            None
                        }
                        TreeRead::MmrRootHash
                        | TreeRead::DenseRootHash
                        | TreeRead::CommitmentAnchor => {
                            assert!(matches!(outcome, Outcome::Hash(_)), "{op:?}");
                            None
                        }
                    };
                    if let Some(expected) = expected {
                        assert_eq!(
                            format!("{outcome:?}"),
                            format!("{expected:?}"),
                            "grove v{}: {op:?}",
                            grove_version.protocol_version
                        );
                        checked += 1;
                    }
                }
                _ => {}
            }
        }
        assert!(checked > 100, "only {checked} typed reads were checked");
    });
}

/// What no stream holds is refused with an error, not applied some other
/// way: a step on a transaction that is not there, a snapshot read outside
/// a snapshot, a tree named by no path, a payload that is no ciphertext, a
/// chunk of something that is no tree.
#[test]
fn apply_refuses_what_no_stream_holds() {
    use lignin_testkit::workload::{AppendEntry, ChunkIds};

    let grove_version = GroveVersion::latest();
    let (_dir, db) = open();
    let mut transactions = Transactions::new(&db);
    let mut applies = |op: Op| {
        let applied = transactions.apply(&op, grove_version).unwrap();
        applied.map_err(|error| error.to_string())
    };
    let tree = vec![b"tree".to_vec()];
    for op in [
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"tree".to_vec(),
            element: Element::empty_tree(),
        }),
        Op::Write(Write::Insert {
            path: tree.clone(),
            key: b"item".to_vec(),
            element: Element::new_item(vec![1]),
        }),
        Op::Commit,
        Op::Transaction(TransactionOp::CloseSnapshot),
    ] {
        applies(op).expect("the setup applies");
    }
    let is_empty = Read::IsEmptyTree { path: tree.clone() };
    let refused: Vec<Op> = vec![
        Op::Transaction(TransactionOp::SnapshotRead(is_empty.clone())),
        Op::Transaction(TransactionOp::RollbackToSavepoint),
        Op::Read(Read::Tree {
            path: Vec::new(),
            read: TreeRead::MmrLeafCount,
        }),
        Op::Write(Write::Append {
            path: Vec::new(),
            entry: AppendEntry::Mmr(vec![1]),
        }),
        Op::Write(Write::CommitmentInsert {
            path: Vec::new(),
            cmx: [0; 32],
            rho: [0; 32],
            cv_net: [0; 32],
            payload: vec![0; COMMITMENT_PAYLOAD_LEN as usize],
        }),
        Op::Write(Write::CommitmentInsert {
            path: tree.clone(),
            cmx: [0; 32],
            rho: [0; 32],
            cv_net: [0; 32],
            payload: vec![0; 3],
        }),
        Op::Read(Read::Prove(Prove::Chunk {
            path: vec![b"tree".to_vec(), b"item".to_vec()],
            ids: ChunkIds::Merk(Vec::new()),
        })),
        Op::Read(Read::Prove(Prove::Chunk {
            path: vec![b"nowhere".to_vec()],
            ids: ChunkIds::Merk(Vec::new()),
        })),
    ];
    for op in refused {
        let refusal = applies(op.clone()).expect_err("no stream holds this");
        println!("{op:?}: {refusal}");
    }

    // The free function is given the transaction by its caller, and
    // refuses the one it cannot use.
    let block = db.start_transaction();
    let snapshot = db.start_snapshot_read_transaction();
    for (op, transaction) in [
        (TransactionOp::SetSavepoint, None),
        (TransactionOp::Rollback, None),
        (TransactionOp::RollbackToSavepoint, None),
        (TransactionOp::SnapshotRead(is_empty.clone()), Some(&block)),
        (TransactionOp::SnapshotRead(is_empty.clone()), None),
        (TransactionOp::BlockRead(is_empty.clone()), None),
        (TransactionOp::BlockRead(is_empty.clone()), Some(&snapshot)),
        (TransactionOp::SetSavepoint, Some(&snapshot)),
        (TransactionOp::Rollback, Some(&snapshot)),
        (TransactionOp::RollbackToSavepoint, Some(&snapshot)),
    ] {
        let op = Op::Transaction(op);
        apply(&db, &op, transaction, grove_version)
            .unwrap()
            .expect_err("the transaction is not the one the step needs");
    }
    let read = Op::Transaction(TransactionOp::SnapshotRead(is_empty));
    let empty = apply(&db, &read, Some(&snapshot), grove_version)
        .unwrap()
        .expect("a snapshot read runs in a snapshot");
    assert!(matches!(empty, Outcome::Flag(false)), "{empty:?}");
}

/// A snapshot opened while another is open takes its place, a snapshot that
/// is not open closes without a word, and a commit that fails is returned
/// as the error it is, with the next block open.
#[test]
fn transactions_keep_the_block_and_the_snapshot() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = open();
    let mut transactions = Transactions::new(&db);
    let insert = |key: &[u8], value: u8| {
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: key.to_vec(),
            element: Element::new_item(vec![value]),
        })
    };
    let has = |key: &[u8]| Read::HasRaw {
        path: Vec::new(),
        key: key.to_vec(),
    };
    let through_snapshot = |read: Read| Op::Transaction(TransactionOp::SnapshotRead(read));
    let applies = |transactions: &mut Transactions, op: Op| {
        transactions
            .apply(&op, grove_version)
            .unwrap()
            .unwrap_or_else(|error| panic!("{op:?} refused: {error:?}"))
    };

    assert!(transactions.snapshot().is_none());
    applies(
        &mut transactions,
        Op::Transaction(TransactionOp::OpenSnapshot),
    );
    applies(&mut transactions, insert(b"first", 1));
    applies(&mut transactions, Op::Commit);
    let seen = applies(&mut transactions, through_snapshot(has(b"first")));
    assert!(matches!(seen, Outcome::Flag(false)), "{seen:?}");
    applies(
        &mut transactions,
        Op::Transaction(TransactionOp::OpenSnapshot),
    );
    let seen = applies(&mut transactions, through_snapshot(has(b"first")));
    assert!(matches!(seen, Outcome::Flag(true)), "{seen:?}");
    assert!(transactions.snapshot().is_some());
    applies(
        &mut transactions,
        Op::Transaction(TransactionOp::CloseSnapshot),
    );
    applies(
        &mut transactions,
        Op::Transaction(TransactionOp::CloseSnapshot),
    );
    assert!(transactions.snapshot().is_none());

    // The block and a write outside it store the same key: the block's
    // commit conflicts.
    applies(&mut transactions, insert(b"contested", 1));
    apply(&db, &insert(b"contested", 2), None, grove_version)
        .unwrap()
        .expect("a write outside the block commits by itself");
    let conflict = transactions
        .apply(&Op::Commit, grove_version)
        .unwrap()
        .expect_err("the block wrote a key that was committed under it");
    println!("commit refused: {conflict}");
    applies(&mut transactions, insert(b"after", 3));
    applies(&mut transactions, Op::Commit);
    let stored = db
        .get(&[] as &[&[u8]], b"contested", None, grove_version)
        .unwrap()
        .expect("the write outside the block is stored");
    assert_eq!(stored, Element::new_item(vec![2]));
    let seen = applies(&mut transactions, Op::Read(has(b"after")));
    assert!(matches!(seen, Outcome::Flag(true)), "{seen:?}");
}

/// The flags Drive writes with an element in `epoch`, for the owner whose
/// id is 32 bytes of `owner`.
fn written_flags(epoch: u16, owner: Option<u8>) -> Option<Vec<u8>> {
    let owner = owner.map(|owner| [owner; 32]);
    Some(StorageFlags::new_single_epoch(epoch, owner).to_element_flags())
}

/// An item of `len` bytes written in `epoch` for `owner`.
fn flagged(len: usize, epoch: u16, owner: u8) -> Element {
    Element::new_item_with_flags(vec![7; len], written_flags(epoch, Some(owner)))
}

/// One of each kind the fourth unit added, as a stream GroveDB accepts,
/// which holds every case of the callbacks at least once.
fn one_of_each_flags_kind() -> Vec<Op> {
    let tree = vec![b"tree".to_vec()];
    let nested = vec![b"tree".to_vec(), b"upper".to_vec(), b"lower".to_vec()];
    let put = |key: &[u8], element: Element| BatchOp::Replace {
        path: tree.clone(),
        key: key.to_vec(),
        element,
    };
    let new = |path: &[Vec<u8>], key: &[u8], element: Element| BatchOp::InsertOnly {
        path: path.to_vec(),
        key: key.to_vec(),
        element,
    };
    let delete = |key: &[u8]| BatchOp::Delete {
        path: tree.clone(),
        key: key.to_vec(),
    };
    let whole = |op: BatchOp| Op::FlagsBatch(vec![op]);
    let in_segments = |ops: Vec<BatchOp>, add_on: Vec<BatchOp>, pause_height: Option<u8>| {
        Op::FlagsPartialBatch(PartialBatch {
            ops,
            add_on,
            pause_height,
        })
    };
    let hand_over = |op: BatchOp, full_handover_owners: Vec<[u8; 32]>| {
        Op::FlagsHandoverBatch(HandoverBatch {
            ops: vec![op],
            full_handover_owners: full_handover_owners.into_iter().collect(),
        })
    };
    let (a, b, c) = (1, 2, 3);
    vec![
        Op::FlagsBatch(vec![
            new(&[], b"tree", Element::empty_tree()),
            new(&tree, b"whole", flagged(40, 0, a)),
            new(&tree, b"bare", flagged(40, 0, a)),
            new(&tree, b"lone", flagged(40, 0, a)),
            new(
                &tree,
                b"upper",
                Element::empty_tree_with_flags(written_flags(0, Some(a))),
            ),
            new(
                &nested[..2],
                b"lower",
                Element::empty_tree_with_flags(written_flags(0, Some(b))),
            ),
            new(&nested, b"leaf", flagged(40, 0, b)),
            new(&nested[..2], b"more", flagged(40, 0, a)),
            new(&tree, b"stays", flagged(40, 0, b)),
        ]),
        // Longer in two later epochs, the same length under another owner,
        // shorter than the latest epoch made it, and deleted with the epoch
        // that is left.
        whole(put(b"whole", flagged(140, 1, a))),
        whole(put(b"whole", flagged(240, 2, a))),
        whole(put(b"whole", flagged(240, 3, b))),
        whole(put(b"whole", flagged(90, 3, a))),
        whole(delete(b"whole")),
        whole(put(b"bare", Element::new_item(vec![7; 80]))),
        whole(put(b"lone", flagged(140, 2, a))),
        Op::Write(Write::SectionalDelete {
            path: tree.clone(),
            key: b"lone".to_vec(),
        }),
        Op::Write(Write::SectionalDeleteUpTree {
            path: nested.clone(),
            key: b"leaf".to_vec(),
            stop_path_height: Some(1),
        }),
        Op::Write(Write::SectionalDeleteUpTree {
            path: nested[..2].to_vec(),
            key: b"more".to_vec(),
            stop_path_height: Some(1),
        }),
        // The same in partial batches, where an update that keeps the
        // length stores the flags written: over two epochs, whose history
        // goes, and under another owner.
        in_segments(
            vec![new(&tree, b"partial", flagged(40, 3, a))],
            Vec::new(),
            None,
        ),
        in_segments(
            vec![put(b"partial", flagged(140, 4, a))],
            Vec::new(),
            Some(1),
        ),
        in_segments(
            vec![put(b"stays", flagged(60, 5, b))],
            vec![put(b"partial", flagged(240, 5, a))],
            None,
        ),
        in_segments(vec![put(b"partial", flagged(90, 6, a))], Vec::new(), None),
        in_segments(vec![put(b"partial", flagged(90, 7, a))], Vec::new(), None),
        in_segments(vec![put(b"partial", flagged(90, 8, b))], Vec::new(), None),
        in_segments(vec![put(b"partial", flagged(190, 9, b))], Vec::new(), None),
        in_segments(vec![delete(b"partial")], Vec::new(), None),
        // Without flags, what a delete frees is not divided.
        in_segments(
            vec![new(&tree, b"plain", Element::new_item(vec![7; 20]))],
            Vec::new(),
            None,
        ),
        in_segments(vec![delete(b"plain")], Vec::new(), None),
        // An update of a document, which hands the element to the owner
        // written: with its history, and whole when that owner pays for
        // all of it.
        hand_over(put(b"stays", flagged(60, 10, a)), Vec::new()),
        hand_over(put(b"stays", flagged(60, 11, c)), vec![[c; 32]]),
        Op::Commit,
    ]
}

const PINNED_FLAGS_KINDS: &str = "037a46b43814d8bd4f177aa817caf046e248f76b36fa592f418a6ea78e1709a5";

/// The canonical encoding of the kinds the fourth unit added: each one
/// differs from every other, and the bytes are pinned. Together they are
/// every operation of the group that unit added, in a stream GroveDB
/// accepts.
#[test]
fn the_flags_kinds_have_a_pinned_encoding() {
    let ops = one_of_each_flags_kind();
    let mut encodings = BTreeSet::new();
    for op in &ops {
        let mut bytes = Vec::new();
        op.encode(&mut bytes);
        assert!(encodings.insert(bytes), "two operations encode as {op:?}");
        assert!(!op.keeps_transaction() && !op.follows_a_commit(), "{op:?}");
    }
    let digest = StreamDigest::of(&ops);
    assert_eq!(digest.ops(), ops.len() as u64);
    assert_eq!(digest.hex(), PINNED_FLAGS_KINDS);

    let produced = of_group(Coverage::of(&ops).operations(), FLAG_CALLBACKS_GROUP);
    let group = of_group(Operation::ALL, FLAG_CALLBACKS_GROUP);
    assert_eq!(produced, group);
    assert_eq!(group.len(), 19);

    // The same batch under GroveDB's own callbacks is another operation.
    let empty = PartialBatch {
        ops: Vec::new(),
        add_on: Vec::new(),
        pause_height: None,
    };
    let handover = |full_handover_owners: Vec<[u8; 32]>| {
        Op::FlagsHandoverBatch(HandoverBatch {
            ops: Vec::new(),
            full_handover_owners: full_handover_owners.into_iter().collect(),
        })
    };
    for (flags, plain) in [
        (Op::FlagsBatch(Vec::new()), Op::Batch(Vec::new())),
        (
            Op::FlagsPartialBatch(empty.clone()),
            Op::PartialBatch(empty),
        ),
        (handover(Vec::new()), Op::FlagsBatch(Vec::new())),
        (handover(vec![[1; 32]]), handover(Vec::new())),
        (handover(vec![[1; 32]]), handover(vec![[2; 32]])),
    ] {
        assert_ne!(
            StreamDigest::of([&flags]).hex(),
            StreamDigest::of([&plain]).hex()
        );
    }

    let expected = flags_ops(&ops);
    assert_eq!(expected, ops.len() as u64 - 1, "all but the commit");
    let (_dir, db) = open();
    let applied = run("one of each", &db, ops.into_iter(), GroveVersion::latest());
    let flags = applied.flags;
    assert_eq!(
        (flags.ops, flags.freed_bytes_predicted),
        (expected, expected)
    );
    assert_eq!(flags.replaced, 1, "the full handover");
}

/// Those of `operations` that belong to `group`.
fn of_group<'a>(
    operations: impl IntoIterator<Item = &'a Operation>,
    group: &str,
) -> Vec<Operation> {
    operations
        .into_iter()
        .copied()
        .filter(|operation| operation.group() == group)
        .collect()
}

/// How many operations of a stream run Drive's callbacks.
fn flags_ops(ops: &[Op]) -> u64 {
    let runs_them = |op: &&Op| {
        matches!(
            op,
            Op::FlagsBatch(_)
                | Op::FlagsPartialBatch(_)
                | Op::FlagsHandoverBatch(_)
                | Op::Write(Write::SectionalDelete { .. } | Write::SectionalDeleteUpTree { .. })
        )
    };
    ops.iter().filter(runs_them).count() as u64
}

/// The two workloads of the storage-flag callbacks hold the cases they are
/// for, and every one of their operations stores the flags and frees the
/// bytes the model of [`run`] calls for.
#[test]
fn the_callbacks_keep_the_flags_and_split_what_is_freed() {
    let whole_batches = [
        Operation::FlagsBatch,
        Operation::FlagsHandoverBatch,
        Operation::FlagsHandover,
        Operation::FlagsFullHandover,
        Operation::FlagsGrowth,
        Operation::FlagsShrinkAcrossEpochs,
        Operation::FlagsSameSize,
        Operation::FlagsOwnerChange,
        Operation::FlagsDelete,
    ];
    let flag_operations =
        |ops: &[Op]| of_group(Coverage::of(ops).operations(), FLAG_CALLBACKS_GROUP);
    for seed in [SEED, OTHER_SEED, 2, 3] {
        let ops: Vec<Op> = StorageFees::default().stream(seed).collect();
        assert_eq!(
            flag_operations(&ops),
            whole_batches,
            "storage-fees, seed {seed}"
        );
    }
    on_every_grove_version(|grove_version| {
        let version = grove_version.protocol_version;
        let features = Features::for_grove_version(grove_version);
        let supported = |operations: &[Operation]| -> Vec<Operation> {
            operations
                .iter()
                .copied()
                .filter(|operation| features.supports(*operation))
                .collect()
        };
        let fees = StorageFees {
            features,
            ..StorageFees::default()
        };
        let epochs = FlagEpochs {
            features,
            ..FlagEpochs::default()
        };
        assert_eq!(
            flag_operations(&fees.stream(SEED).collect::<Vec<_>>()),
            supported(&whole_batches),
            "storage-fees on grove v{version}"
        );
        assert_eq!(
            flag_operations(&epochs.stream(SEED).collect::<Vec<_>>()),
            supported(&of_group(Operation::ALL, FLAG_CALLBACKS_GROUP)),
            "flag-epochs on grove v{version}"
        );

        let workloads = [Workload::StorageFees(fees), Workload::FlagEpochs(epochs)];
        for workload in workloads {
            for seed in [SEED, 2] {
                let ops: Vec<Op> = workload.stream(seed).collect();
                let expected = flags_ops(&ops);
                let (_dir, db) = open();
                let name = workload.name();
                let flags = run(name, &db, ops.into_iter(), grove_version).flags;
                println!("grove v{version} {name}, seed {seed}: {flags:?}");
                assert_eq!(
                    (flags.ops, flags.freed_bytes_predicted),
                    (expected, expected),
                    "{name}: every operation is checked whole"
                );
                assert!(flags.freed_across_epochs >= 8, "{name}: {flags:?}");
                assert!(flags.recorded_an_epoch >= 20, "{name}: {flags:?}");
                assert_eq!(
                    flags.replaced > 0,
                    features.full_handovers,
                    "{name}: {flags:?}"
                );
            }
        }
    });
}

/// A ladder of any length is a stream, whatever the seed, and a long one
/// applies on every grove version: its flags pass the length an element
/// spends one byte on, and the element the length a node spends two on.
#[test]
fn a_ladder_of_any_length_applies() {
    for rungs in 4..=40 {
        for seed in [SEED, OTHER_SEED, 2, 3] {
            for features in [
                Features::ALL,
                Features::for_grove_version(&GROVE_VERSIONS[0]),
            ] {
                let ladder = FlagEpochs { rungs, features };
                assert!(ladder.stream(seed).count() > rungs as usize);
            }
        }
    }

    on_every_grove_version(|grove_version| {
        let ladder = FlagEpochs {
            rungs: 140,
            features: Features::for_grove_version(grove_version),
        };
        let ops: Vec<Op> = ladder.stream(SEED).collect();
        let (mut epochs, mut len) = (0, 0);
        let mut histories = Histories::new();
        for op in &ops {
            histories.record(op);
            if let Some(history) = histories.get(&[b"epochs".to_vec()], b"ladder") {
                epochs = epochs.max(history.later.len());
                len = len.max(history.len);
            }
        }
        // The flags of that many epochs are over 250 bytes long, and the
        // element is more than a node counts in two bytes.
        assert!(epochs >= 140 && len > 1 << 14, "{epochs} epochs, {len}");

        let expected = flags_ops(&ops);
        let (_dir, db) = open();
        let flags = run("a long ladder", &db, ops.into_iter(), grove_version).flags;
        assert_eq!(
            (flags.ops, flags.freed_bytes_predicted),
            (expected, expected)
        );
    });
}

/// What Drive's callbacks refuse, which no stream holds, and what the grove
/// versions before the fourth refuse of what the later ones accept.
#[test]
fn the_callbacks_refuse_what_no_stream_holds() {
    let tree = vec![b"tree".to_vec()];
    let put = |element: Element| BatchOp::InsertOrReplace {
        path: tree.clone(),
        key: b"item".to_vec(),
        element,
    };
    let whole = |element: Element| Op::FlagsBatch(vec![put(element)]);
    let in_segments = |element: Element| {
        Op::FlagsPartialBatch(PartialBatch {
            ops: vec![put(element)],
            add_on: Vec::new(),
            pause_height: None,
        })
    };
    // What `last` does after `first`, on an item of an empty tree.
    let applies = |first: Vec<Op>, last: Op, grove_version: &GroveVersion| {
        let (_dir, db) = open();
        let mut transactions = Transactions::new(&db);
        let tree = Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"tree".to_vec(),
            element: Element::empty_tree(),
        });
        for op in [tree].into_iter().chain(first) {
            let applied = transactions.apply(&op, grove_version).unwrap();
            applied.unwrap_or_else(|error| panic!("{op:?} refused: {error:?}"));
        }
        let applied = transactions.apply(&last, grove_version).unwrap();
        applied.map(|_| ()).map_err(|error| error.to_string())
    };
    let (a, b) = (1, 2);
    let unreadable = Element::new_item_with_flags(vec![7; 40], Some(vec![255, 1, 2]));
    let no_flags = Element::new_item_with_flags(vec![7; 90], Some(Vec::new()));
    let delete = BatchOp::Delete {
        path: tree.clone(),
        key: b"item".to_vec(),
    };
    let delete_in_segments = Op::FlagsPartialBatch(PartialBatch {
        ops: vec![delete.clone()],
        add_on: Vec::new(),
        pause_height: None,
    });
    let stored_unreadable = || vec![Op::Batch(vec![put(unreadable.clone())])];
    let hand_over = |element: Element| {
        Op::FlagsHandoverBatch(HandoverBatch {
            ops: vec![put(element)],
            full_handover_owners: [[b; 32]].into(),
        })
    };
    let latest = GroveVersion::latest();
    let refused = [
        // Another owner, where a partial batch changes the length.
        (
            vec![whole(flagged(40, 0, a))],
            in_segments(flagged(90, 1, b)),
            "drive could not combine storage flags (new flags were bigger):",
        ),
        (
            vec![whole(flagged(40, 0, a)), whole(flagged(90, 1, a))],
            in_segments(flagged(60, 2, b)),
            "drive could not combine storage flags (new flags were smaller):",
        ),
        // An epoch before the element's first.
        (
            vec![whole(flagged(40, 5, a))],
            whole(flagged(90, 2, a)),
            "older base epoch: drive could not combine storage flags (new flags were bigger)",
        ),
        (
            vec![whole(flagged(40, 5, a))],
            in_segments(flagged(90, 2, a)),
            "drive could not combine storage flags (new flags were bigger):",
        ),
        // Flags Drive cannot read: written, and stored.
        (
            vec![whole(flagged(40, 0, a))],
            whole(unreadable.clone()),
            "serialization: drive did not understand updated item flag information",
        ),
        (
            vec![whole(flagged(40, 0, a))],
            in_segments(unreadable.clone()),
            "drive did not understand updated item flag information",
        ),
        (
            stored_unreadable(),
            whole(flagged(90, 1, a)),
            "serialization: drive did not understand flags of old item being updated",
        ),
        (
            stored_unreadable(),
            in_segments(flagged(90, 1, a)),
            "drive did not understand flags of old item being updated",
        ),
        (
            stored_unreadable(),
            Op::FlagsBatch(vec![delete.clone()]),
            "serialization: drive did not understand flags of item being updated",
        ),
        (
            stored_unreadable(),
            delete_in_segments,
            "drive did not understand flags of item being updated:",
        ),
        (
            stored_unreadable(),
            Op::Write(Write::SectionalDelete {
                path: tree.clone(),
                key: b"item".to_vec(),
            }),
            "serialization: drive did not understand flags of item being updated",
        ),
        // The same where an owner pays for all of an element.
        (
            vec![whole(flagged(40, 0, a))],
            hand_over(unreadable.clone()),
            "unknown storage flags serialization",
        ),
        (
            stored_unreadable(),
            hand_over(flagged(90, 1, a)),
            "unknown storage flags serialization",
        ),
        // Flags taken off an element that has them, in a partial batch.
        (
            vec![whole(flagged(40, 0, a))],
            in_segments(no_flags),
            "removing flags from an item with flags is not allowed",
        ),
    ];
    for (first, last, text) in refused {
        let refusal = applies(first, last.clone(), latest).expect_err("no stream holds this");
        println!("{last:?}: {refusal}");
        assert!(refusal.contains(text), "{last:?}: {refusal}");
    }

    // A handover to an owner who pays for the whole element replaces it,
    // which only the latest grove version takes from a callback.
    let hand_over = hand_over(flagged(40, 1, b));
    for grove_version in GROVE_VERSIONS {
        let accepts = Features::for_grove_version(grove_version).full_handovers;
        let applied = applies(
            vec![whole(flagged(40, 0, a))],
            hand_over.clone(),
            grove_version,
        );
        assert_eq!(
            applied.is_ok(),
            accepts,
            "grove v{}: {applied:?}",
            grove_version.protocol_version
        );
    }

    // An update that stores flags of another length without the callback
    // changing them: what `Features::flag_length_changes` is for.
    let changes_length = [
        (
            vec![whole(flagged(40, 0, a)), whole(flagged(90, 1, a))],
            whole(Element::new_item(vec![7; 90])),
        ),
        (
            vec![whole(flagged(40, 0, a)), whole(flagged(90, 1, a))],
            in_segments(flagged(90, 2, a)),
        ),
    ];
    for grove_version in GROVE_VERSIONS {
        let accepts = Features::for_grove_version(grove_version).flag_length_changes;
        for (first, last) in &changes_length {
            let applied = applies(first.clone(), last.clone(), grove_version);
            assert_eq!(
                applied.is_ok(),
                accepts,
                "grove v{}: {last:?}: {applied:?}",
                grove_version.protocol_version
            );
        }
    }
}

/// The tree a tree-deletes operation deletes whole: the state the tree is
/// in, the way it is deleted and the round, as its key names them.
fn tree_deleted(op: &Op) -> Option<(String, String, u32)> {
    let key = match op {
        Op::Write(Write::Delete { key, .. } | Write::SectionalDelete { key, .. }) => key,
        Op::Batch(ops) | Op::FlagsBatch(ops) => match ops.as_slice() {
            [BatchOp::DeleteTree { key, .. }] => key,
            _ => return None,
        },
        _ => return None,
    };
    let name = String::from_utf8(key.clone()).expect("a tree is named in text");
    let (state, rest) = name.split_once('-').expect("a state");
    let (way, round) = rest.rsplit_once('-').expect("a way and a round");
    Some((
        state.to_string(),
        way.to_string(),
        round.parse().expect("a round"),
    ))
}

/// What deleting the tree at `key` with what it holds frees, by the rule
/// the cost model states (D2): the tree's own element as its writer costs
/// it, and for each node under it the stored key and the record the
/// **committed** state holds under that key, each with its length. A key
/// the committed state has no record of counts as an empty record. The
/// nodes are the ones the transaction sees.
fn committed_charge(
    db: &GroveDb,
    path: &[Vec<u8>],
    key: &[u8],
    transaction: &Transaction,
    grove_version: &GroveVersion,
) -> u32 {
    let dir = tempfile::tempdir().expect("temp dir");
    let checkpoint = dir.path().join("committed");
    db.create_checkpoint(&checkpoint).expect("a checkpoint");
    let committed = RocksDbStorage::checkpoint_rocksdb_with_path(&checkpoint).expect("it opens");
    let reader = committed.start_transaction();

    let tree = stored(db, path, key, Some(transaction), grove_version).expect("the tree");
    let value = tree.serialize(grove_version).expect("an element");
    let mut charge = KV::node_key_byte_cost_size(key.len() as u32)
        + Element::specialized_costs_for_key_value(
            key,
            &value,
            NodeType::NormalNode,
            grove_version,
        )
        .expect("the size of a value");
    let child = |path: &[Vec<u8>], key: &[u8]| [path, &[key.to_vec()]].concat();
    let mut trees = vec![child(path, key)];
    while let Some(tree) = trees.pop() {
        let records = committed
            .get_transactional_storage_context(tree.as_slice().into(), None, &reader)
            .unwrap();
        let nodes = PathQuery::new_unsized(tree.clone(), grovedb::Query::new_range_full());
        let (nodes, _) = db
            .query_raw(
                &nodes,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                Some(transaction),
                grove_version,
            )
            .unwrap()
            .expect("the nodes of the tree");
        for (node, element) in nodes.to_key_elements() {
            let record = records.get(&node).unwrap().expect("a raw read");
            let record = record.map_or(0, |record| record.len() as u32);
            let stored_key = rules::stored_key_len(node.len());
            charge += stored_key
                + rules::varint_len(u64::from(stored_key))
                + record
                + rules::varint_len(u64::from(record));
            if element.is_any_tree() {
                trees.push(child(&tree, &node));
            }
        }
    }
    charge
}

/// A tree deleted with what it holds is charged for the records the
/// committed state holds under its keys, whatever the transaction that
/// deletes it wrote there first. Every delete of the workload frees exactly
/// what that rule gives, read from the committed records themselves:
///
/// - a tree the transaction filled is charged for no record: nothing is
///   loaded for what it held, and each of its keys frees the same few
///   bytes;
/// - a tree the transaction rewrote is charged what the tree it was
///   committed as costs: the same as its twin that was left untouched,
///   where the transaction replaced values and nothing else, although one
///   value is a hundred bytes longer by then and another twenty shorter.
#[test]
fn a_deleted_tree_is_charged_for_what_was_committed() {
    let workload = TreeDeletes::default();
    let ops: Vec<Op> = workload.stream(SEED).collect();
    let states = of_group(Coverage::of(&ops).operations(), TREE_DELETES_GROUP);
    assert_eq!(
        states,
        [
            Operation::TreeDeleteFilledInTransaction,
            Operation::TreeDeleteRewrittenInTransaction,
            Operation::TreeDeleteCommitted
        ]
    );
    on_every_grove_version(|grove_version| {
        let version = grove_version.protocol_version;
        let (_dir, db) = open();
        let mut transactions = Transactions::new(&db);
        let mut costs: BTreeMap<(String, u32), BTreeMap<String, (u32, u64)>> = BTreeMap::new();
        for op in workload.stream(SEED) {
            let deleted = tree_deleted(&op).map(|(state, way, round)| {
                let key = format!("{state}-{way}-{round}").into_bytes();
                let path = [b"tree-deletes".to_vec()];
                let charge =
                    committed_charge(&db, &path, &key, transactions.block(), grove_version);
                (state, way, round, charge)
            });
            let result = transactions.apply(&op, grove_version);
            result
                .value
                .unwrap_or_else(|error| panic!("{op:?} refused: {error:?}"));
            if let Some((state, way, round, charge)) = deleted {
                let freed = Freed::of(&result.cost.storage_cost.removed_bytes);
                let removed = freed.unsplit + freed.by_epoch.values().sum::<u32>();
                assert_eq!(
                    removed, charge,
                    "grove v{version}, {state}, {way}, round {round}: freed and the charge by \
                     the committed records"
                );
                let cost = (removed, result.cost.storage_loaded_bytes);
                costs.entry((way, round)).or_default().insert(state, cost);
            }
        }
        assert_eq!(costs.len(), 4 * workload.rounds as usize);
        for ((way, round), states) in costs {
            let cost = |state: &str| states[state];
            let (fresh, rewritten, untouched) =
                (cost("fresh"), cost("rewritten"), cost("untouched"));
            println!(
                "grove v{version}, {way}, round {round}: removed and loaded: fresh {fresh:?}, \
                 rewritten {rewritten:?}, untouched {untouched:?}"
            );
            let context = format!("grove v{version}, {way}, round {round}");
            // The two trees hold records of the same lengths. Charged by
            // what the transaction sees, they would cost the same but for
            // the four bytes their own keys differ by.
            assert!(
                fresh.0 * 2 < untouched.0 && fresh.1 < untouched.1,
                "{context}"
            );
            if round % 2 == 0 {
                assert_eq!(rewritten.0, untouched.0, "{context}");
            } else {
                // A key the transaction deleted is not walked, and one it
                // inserted is charged for no record.
                assert!(rewritten.0 < untouched.0, "{context}");
            }
        }
    });
}
