//! The open differences of the two engines that are ruled and named, and
//! what the fuzzer does where a case comes to one (GROVE-10).
//!
//! RocksDB leaves records behind where GroveDB changes one element and
//! does not clear what hung under it, and reads them again later. LigninDB
//! drops a subtree with its element, and refuses a write that would put
//! such records to use. Four such histories are ruled as
//! open differences that are put to the maintainer of GroveDB, and are no
//! defect of an engine until that is answered. Each is a [`Family`]. Three
//! have a rule that is a path:
//!
//! - [`Family::WrittenOver`]: an element was put over a tree that held
//!   elements. The family is the subtree of that tree, and every subtree
//!   below it.
//! - [`Family::BelowFlatDrop`]: a tree was dropped flat. Where it held a
//!   tree, against its caller's word, the engines differ below it. The
//!   family is every subtree below a tree that was dropped flat, and not
//!   the tree's own.
//! - [`Family::DroppedTree`]: the subtree of a tree that was dropped flat,
//!   itself, while the record of its drop is pending: from the drop until
//!   a drain reclaims the record, which no drain does while a tree stands
//!   at the path again. RocksDB keeps the tree's records until then, and
//!   a tree that is made again at the path before that reads them. The
//!   pending records are read from the reference, as its block sees them,
//!   after each operation both engines answered alike, from the case's
//!   first flat drop on
//!   ([`pending_drop_paths`](super::state::pending_drop_paths)): an
//!   answer is of the family by the records as they were before its
//!   operation.
//!
//! Of the family of a dropped tree one more kind of a step is counted
//! apart, by the owner's ruling D-348, of an answer alone:
//!
//! - [`Family::DroppedTreeProof`]: a proof binds a tree that is no Merk to
//!   the tree's own records, and RocksDB reads there the records it kept
//!   of the tree that was dropped, so its proof of a query that returns a
//!   tree made again at a dropped path is refused by its own verifier. A
//!   case ends there, counted, only where all of this holds
//!   ([`Known::through_a_proof`]): the operation is a raw query of
//!   committed state; under a key that its items select, at any level, is
//!   a tree the case dropped flat whose record is pending and at whose
//!   path the reference reads a tree again; the engines differ in the
//!   proof or its cost and in nothing else, so they returned the same
//!   rows at the same cost; and the candidate's proof verifies to the
//!   root hash of its committed state and gives the rows the candidate
//!   returned.
//!
//! In a debug build GroveDB's own assertion fails there on RocksDB and the
//! reference panics, which ends a run as a divergence. A debug run on
//! grove versions 4 and 5 generates cases that keep the contract of a
//! flat drop's caller instead
//! ([`Shape::keeps_the_drop_contract`](super::case::Shape)).
//!
//! The fourth has no place, and is of an answer alone:
//!
//! - [`Family::IndexedOverIndexed`]: on grove versions 1 to 3, which leave
//!   the records of the old tree where they are, an operation puts an
//!   indexed tree at a key where the reference read an indexed tree
//!   before the operation, the reference takes the operation, and the
//!   candidate answers `NotSupported` ([`Known::indexed_under`],
//!   [`Known::family_of_refusal`]). All three are required. The case
//!   ends there, counted, before what is stored is compared again: no
//!   stored record is passed over for it.
//! - [`Family::InsertOnlyOverIndexed`]: on grove versions 4 and 5 GroveDB
//!   removes the records of an indexed tree that an operation which can
//!   replace is put over, and not those under an insert that its caller
//!   knows not to replace (`InsertOnly`): it reads no old element for
//!   that. Where a batch lists such an insert of an indexed tree at a key
//!   where the reference read an indexed tree before, the reference
//!   takes the batch, and the candidate answers `NotSupported` in the
//!   words of that refusal for that key
//!   ([`Known::indexed_under_an_insert_only`],
//!   [`Known::family_of_insert_only_refusal`]), the case ends there,
//!   counted apart (the owner's ruling D-354). Any other operation that
//!   the candidate so refuses on those versions is a divergence.
//!
//! A case keeps the paths it made so ([`Known`]), from the operations both
//! engines took. Of a batch it counts the operations by a rule about its
//! lists and not by what GroveDB applied: those the caller of a partial
//! batch adds last only where the batch has an operation of its own and
//! the reference took it. A put or a flat drop at a key that the batch
//! names twice makes no place of a family; the operations still name
//! that key where an answer is compared. Nothing else is of a family:
//!
//! - **What is stored.** The records of a subtree of a family are not
//!   compared. Where the two engines hold different records there, the
//!   case counts the difference for the family and goes on.
//! - **An answer.** A result, an error, a cost, a root hash and a proof
//!   are compared at every step, also of a family. Where they differ at an
//!   operation that names a place of a family, the case ends there, is
//!   counted for the family, and is no divergence of the run. They differ
//!   there because the read takes the records under a prefix as they are.
//!   A query names the subtree it is of and every subtree below it that
//!   it reads: one under a key its items select, where the branch of that
//!   key has a subquery path or a subquery to go down by, and so on down
//!   each subquery. That is asked of the query as it is written, and not
//!   of the grove: whether such a key holds a tree, and whether a limit
//!   ends the query before it comes there, is not looked at, so a query
//!   can name a subtree that it did not come to in this grove. A
//!   difference at any other operation ends the run, as
//!   it always did: so does a read of the subtree of a dropped tree once
//!   the record of its drop is reclaimed. Whether a record is pending is
//!   asked for every read alike, also for one that reads an older version
//!   through a snapshot, or the committed state with no transaction.
//!
//! The counts are in the [`Tally`](super::pair::Tally) of a run
//! ([`Counts`]): the cases that made a place of a family, or for the
//! fourth those in which the reference took such a put, those with a stored
//! difference passed over and those ended at an answer, and for the last
//! two the seed of one such case.

use grovedb_version::version::GroveVersion;

use grovedb::{Element, GroveDb, SubqueryBranch};

use super::state::Record;
use super::state::{a_secondary_holds_a_row, Keyspace};
use crate::golden::step::{Outcome, Proof, Proved, Step, Value, Verification};
use crate::workload::{BatchOp, Db, Op, Path, Prove, Read, StreamEngine, TransactionOp, Tx, Write};

/// A named open difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// An element put over a tree that held elements.
    WrittenOver,
    /// What is below a tree that was dropped flat.
    BelowFlatDrop,
    /// The subtree of a tree that was dropped flat, while the record of
    /// its drop is pending.
    DroppedTree,
    /// An indexed tree put over an indexed tree on grove versions 1 to 3,
    /// which the candidate refuses.
    IndexedOverIndexed,
    /// A tree made again at a dropped path whose record is pending, in a
    /// proof of a query that returns it: the engines differ in the proof
    /// alone.
    DroppedTreeProof,
    /// An indexed tree put over an indexed tree on grove versions 4 and
    /// 5 by an insert that its caller knows not to replace, which the
    /// candidate refuses.
    InsertOnlyOverIndexed,
}

/// How many families there are.
const FAMILIES: usize = Family::ALL.len();

/// The last grove version on which GroveDB leaves the records of an
/// indexed tree where they are when an element is put over it.
const LAST_VERSION_THAT_LEAVES_THE_OLD_ROWS: u32 = 3;

impl Family {
    /// Every family.
    pub const ALL: [Family; 6] = [
        Family::WrittenOver,
        Family::BelowFlatDrop,
        Family::DroppedTree,
        Family::IndexedOverIndexed,
        Family::DroppedTreeProof,
        Family::InsertOnlyOverIndexed,
    ];

    /// What a report calls the family.
    pub fn name(self) -> &'static str {
        match self {
            Family::WrittenOver => "an element written over a tree that held elements",
            Family::BelowFlatDrop => "below a tree that was dropped flat",
            Family::DroppedTree => "a tree that was dropped flat, until the drain of its drop",
            Family::IndexedOverIndexed => {
                "an indexed tree written over an indexed tree, grove versions 1 to 3"
            }
            Family::DroppedTreeProof => {
                "a tree that was dropped flat and made again before the drain, through a proof"
            }
            Family::InsertOnlyOverIndexed => {
                "an indexed tree written over an indexed tree by an insert known not to replace, \
                 grove versions 4 and 5"
            }
        }
    }

    fn index(self) -> usize {
        match self {
            Family::WrittenOver => 0,
            Family::BelowFlatDrop => 1,
            Family::DroppedTree => 2,
            Family::IndexedOverIndexed => 3,
            Family::DroppedTreeProof => 4,
            Family::InsertOnlyOverIndexed => 5,
        }
    }
}

/// What a run met of the families, by [`Family::ALL`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// The cases that made a place of the family: an element put over a
    /// tree that held elements, or a flat drop of a tree, whatever it
    /// held; for the two families of an indexed tree over an indexed tree
    /// the cases in which the reference took such a put, whatever the
    /// candidate answered; and for a dropped tree through a proof, which
    /// is counted apart, the cases that ended there.
    pub histories: [u64; FAMILIES],
    /// The cases in which the engines stored different records in a
    /// subtree of the family, which were passed over.
    pub stored: [u64; FAMILIES],
    /// The cases that ended at an answer the engines gave differently at
    /// an operation that names a place of the family.
    pub answers: [u64; FAMILIES],
    /// The seed of a case counted in `stored`: of the first whose counts
    /// were added.
    pub stored_seed: [Option<u64>; FAMILIES],
    /// The seed of a case counted in `answers`, likewise.
    pub answer_seed: [Option<u64>; FAMILIES],
}

impl Counts {
    /// Adds what another run met.
    pub fn add(&mut self, other: &Counts) {
        for at in 0..Family::ALL.len() {
            self.histories[at] += other.histories[at];
            self.stored[at] += other.stored[at];
            self.answers[at] += other.answers[at];
            self.stored_seed[at] = self.stored_seed[at].or(other.stored_seed[at]);
            self.answer_seed[at] = self.answer_seed[at].or(other.answer_seed[at]);
        }
    }

    /// Whether nothing was met.
    pub fn is_empty(&self) -> bool {
        *self == Counts::default()
    }

    /// The cases with a stored difference passed over, a case counted
    /// once for each family it has one of.
    pub fn stored_differences(&self) -> u64 {
        self.stored.iter().sum()
    }

    /// The cases that ended at an answer of a family.
    pub fn answer_differences(&self) -> u64 {
        self.answers.iter().sum()
    }

    /// The counts, a line for each family that has one.
    pub fn lines(&self) -> Vec<String> {
        let seed =
            |seed: Option<u64>| seed.map_or(String::new(), |seed| format!(" (seed {seed:#x})"));
        Family::ALL
            .iter()
            .filter(|family| {
                let at = family.index();
                self.histories[at] + self.stored[at] + self.answers[at] > 0
            })
            .map(|family| {
                let at = family.index();
                format!(
                    "open difference, {}: {} cases made it, {} with stored records that differ \
                     passed over{}, {} ended at an answer that differs{}",
                    family.name(),
                    self.histories[at],
                    self.stored[at],
                    seed(self.stored_seed[at]),
                    self.answers[at],
                    seed(self.answer_seed[at]),
                )
            })
            .collect()
    }
}

/// The message the corpus runner records where LigninDB refuses an empty
/// indexed tree put under `key` over an indexed tree a secondary of which
/// holds rows that the batch does not remove (D-292; `rows_left` of
/// `grovedb/src/engine/lignin/over.rs`).
fn refusal_of_rows_left(key: &[u8]) -> String {
    let key: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "failed: not supported: lignindb: an empty indexed tree written under the key {key} over \
         an indexed tree, a secondary of which holds rows that the batch does not remove"
    )
}

/// The places of the families that one case has made.
#[derive(Clone, Debug, Default)]
pub struct Known {
    over: Vec<Path>,
    flat_drops: Vec<Path>,
    /// The trees among `flat_drops` whose drop has a pending record, as
    /// [`Known::pending`] was last told.
    pending: Vec<Path>,
    /// The trees among `pending` at whose path the reference reads a tree
    /// again, as [`Known::made_again`] was last told.
    remade: Vec<Path>,
    stored: [bool; FAMILIES],
    counts: Counts,
}

fn joined(path: &Path, key: &[u8]) -> Path {
    let mut joined = path.clone();
    joined.push(key.to_vec());
    joined
}

/// The places at which `op` puts an element whatever is there. An insert
/// that is made only where the key holds no element, or only where the
/// element differs, can succeed and write nothing, so it is none: where
/// such an insert does write over a tree, the place is of no family, and a
/// difference there ends the run.
fn puts(op: &Op) -> Vec<Path> {
    let places = put_elements(op).into_iter().map(|(place, _)| place);
    places.collect()
}

/// The operations of the batch `op` that the families count, where
/// `taken` says whether the reference took the operation: every one a
/// batch lists, and of a partial batch its own, with those its caller
/// adds last only where the batch has an operation of its own and the
/// reference took it. This is a rule about the lists, and no trace of
/// what GroveDB applied: it does not ask for the added operations of a
/// batch with none of its own, and whether a batch that the reference
/// did not take came to them, or to each of its own, is not looked at.
fn counted(op: &Op, taken: bool) -> impl Iterator<Item = &BatchOp> {
    let own = match op {
        Op::PartialBatch(batch) | Op::FlagsPartialBatch(batch) => Some(batch.ops.len()),
        _ => None,
    };
    let reached = match own {
        Some(0) => 0,
        Some(own) if !taken => own,
        _ => usize::MAX,
    };
    op.batch_ops().take(reached)
}

/// The places with a key that `batch_op` names.
fn keyed(batch_op: &BatchOp) -> Vec<Path> {
    match batch_op {
        BatchOp::InsertOrReplace { path, key, .. }
        | BatchOp::InsertOnly { path, key, .. }
        | BatchOp::InsertIfNotExists { path, key, .. }
        | BatchOp::Replace { path, key, .. }
        | BatchOp::Patch { path, key, .. }
        | BatchOp::RefreshReference { path, key, .. }
        | BatchOp::Delete { path, key, .. }
        | BatchOp::DeleteTree { path, key, .. }
        | BatchOp::DropFlat { path, key, .. } => vec![joined(path, key)],
        BatchOp::Move {
            path, key, new_key, ..
        } => vec![joined(path, key), joined(path, new_key)],
        BatchOp::Append { .. } => Vec::new(),
    }
}

/// Whether the operations of the batch `op`, those its caller adds last
/// among them, name `place` once at most, each key an operation names
/// counted. Where the batch names a key twice, one operation can take the
/// place of another, and which of them the batch applies is not looked
/// at: a put or a flat drop there makes no place of a family.
fn named_once(op: &Op, place: &Path) -> bool {
    let named = op.batch_ops().flat_map(keyed);
    named.filter(|named| named == place).count() <= 1
}

/// The places of [`puts`], each with the element that is put there, as
/// the families count them for an operation the reference takes
/// ([`counted`], [`named_once`]).
fn put_elements(op: &Op) -> Vec<(Path, &Element)> {
    let mut puts = Vec::new();
    if let Op::Write(
        Write::Insert { path, key, element }
        | Write::IndexedInsert {
            path, key, element, ..
        },
    ) = op
    {
        puts.push((joined(path, key), element));
    }
    for batch_op in counted(op, true) {
        if let BatchOp::InsertOrReplace {
            path, key, element, ..
        }
        | BatchOp::InsertOnly {
            path, key, element, ..
        }
        | BatchOp::Replace {
            path, key, element, ..
        }
        | BatchOp::Patch {
            path, key, element, ..
        } = batch_op
        {
            puts.push((joined(path, key), element));
        }
    }
    puts.retain(|(place, _)| named_once(op, place));
    puts
}

/// The trees `op` drops flat, as the families count them for an operation
/// the reference takes ([`counted`], [`named_once`]).
fn flat_drops(op: &Op) -> Vec<Path> {
    let mut drops = Vec::new();
    if let Op::Write(Write::DropFlat { path, key }) = op {
        drops.push(joined(path, key));
    }
    for batch_op in counted(op, true) {
        if let BatchOp::DropFlat { path, key, .. } = batch_op {
            drops.push(joined(path, key));
        }
    }
    drops.retain(|place| named_once(op, place));
    drops
}

fn read_places(read: &Read, places: &mut Vec<Path>) {
    match read {
        Read::Get { path, key } | Read::HasRaw { path, key } | Read::Fetch { path, key, .. } => {
            places.push(path.clone());
            places.push(joined(path, key));
        }
        Read::IsEmptyTree { path } | Read::Tree { path, .. } => places.push(path.clone()),
        Read::Query(query) => places.push(query.path.clone()),
        Read::PathQuery { query, .. }
        | Read::Rows { query, .. }
        | Read::KeysOptional { query, .. }
        | Read::ItemValues { query, .. }
        | Read::SumValues { query }
        | Read::BigSum { query, .. } => places.push(query.path.clone()),
        Read::SumBudget { query, .. } => places.push(query.path.clone()),
        Read::Many { queries } => places.extend(queries.iter().map(|query| query.path.clone())),
        Read::Prove(
            Prove::BulkPositionRange { path, key, .. } | Prove::BranchChunk { path, key, .. },
        ) => {
            places.push(path.clone());
            places.push(joined(path, key));
        }
        Read::Prove(Prove::TrunkChunk { path, .. } | Prove::Chunk { path, .. }) => {
            places.push(path.clone());
        }
        Read::Prove(Prove::PathQuery(query)) => places.push(query.path.clone()),
        Read::Prove(Prove::Many(queries)) => {
            places.extend(queries.iter().map(|query| query.path.clone()));
        }
        // An aux record is under no path.
        Read::GetAux { .. } | Read::AuxByPrefix { .. } => {}
    }
}

/// The path queries of `read`.
fn queries(read: &Read) -> Vec<grovedb::PathQuery> {
    match read {
        Read::Query(query) => vec![query.to_path_query()],
        Read::PathQuery { query, .. }
        | Read::Rows { query, .. }
        | Read::KeysOptional { query, .. }
        | Read::ItemValues { query, .. }
        | Read::SumValues { query }
        | Read::BigSum { query, .. }
        | Read::Prove(Prove::PathQuery(query)) => vec![(**query).clone()],
        Read::Many { queries } | Read::Prove(Prove::Many(queries)) => queries.clone(),
        Read::SumBudget { .. }
        | Read::Get { .. }
        | Read::HasRaw { .. }
        | Read::Fetch { .. }
        | Read::IsEmptyTree { .. }
        | Read::Tree { .. }
        | Read::Prove(_)
        | Read::GetAux { .. }
        | Read::AuxByPrefix { .. } => Vec::new(),
    }
}

/// Whether `branch` leads into the subtree under a key it is taken for.
fn goes_down(branch: &SubqueryBranch) -> bool {
    let along = branch.subquery_path.as_ref();
    branch.subquery.is_some() || along.is_some_and(|along| !along.is_empty())
}

/// The branch of `query` that `key` takes: the first conditional one
/// whose item selects the key, and else the default one.
fn branch_of<'query>(query: &'query grovedb::Query, key: &[u8]) -> &'query SubqueryBranch {
    let mut conditional = query.conditional_subquery_branches.iter().flatten();
    conditional
        .find(|(item, _)| item.contains(key))
        .map_or(&query.default_subquery_branch, |(_, branch)| branch)
}

/// Whether some key that the items of `query` select takes a branch that
/// goes down.
///
/// A few keys decide it. The items of the query and of its conditional
/// branches are ranges of keys, so the keys that are selected and take
/// one branch are made of the bounds of those ranges and of the gaps
/// between two bounds that follow each other. The least key of a gap is
/// the bound before it with a zero byte added, and the least key of all
/// is the empty one: where a set so made holds any key, it holds one of
/// these.
fn goes_down_somewhere(query: &grovedb::Query) -> bool {
    let conditional = query.conditional_subquery_branches.iter().flatten();
    let ranges = query.items.iter().chain(conditional.map(|(item, _)| item));
    let mut keys = vec![Vec::new()];
    for range in ranges {
        let bounds = [range.lower_bound().0, range.upper_bound().0];
        for bound in bounds.into_iter().flatten() {
            keys.push(bound.to_vec());
            keys.push([bound, &[0]].concat());
        }
    }
    let selected = |key: &Vec<u8>| query.items.iter().any(|item| item.contains(key));
    (keys.iter().filter(|key| selected(key))).any(|key| goes_down(branch_of(query, key)))
}

/// Whether `query`, asked of a subtree, reads the subtree at `below`, a
/// path down from its own, and if it does, whether it goes on into a
/// subtree below that one.
///
/// A query goes into the subtree under a key that its items select where
/// the branch of that key ([`branch_of`]) has a subquery path or a
/// subquery. It reads the subtrees along the subquery path for the next
/// key of the path, and with a subquery the subtree at the end of the
/// path, of which the subquery is then asked. Of its own subtree it goes
/// on below where some key its items select takes a branch that goes down
/// ([`goes_down_somewhere`]). Which keys hold a tree, and where a limit
/// ends the query, is not looked at.
///
/// With `proved`, the subtree under a key that the items of the query, or
/// of a subquery it comes to, select is read as well, and nothing below
/// it: a proof binds a tree it returns that is no Merk to the tree's own
/// records.
fn reads(query: &grovedb::Query, below: &[Vec<u8>], proved: bool) -> Option<bool> {
    let Some((key, below)) = below.split_first() else {
        return Some(goes_down_somewhere(query));
    };
    if !query.items.iter().any(|item| item.contains(key)) {
        return None;
    }
    let bound = (proved && below.is_empty()).then_some(false);
    let branch = branch_of(query, key);
    let along = branch.subquery_path.as_deref().unwrap_or_default();
    if below.len() < along.len() {
        let further = below.len() + 1 < along.len() || branch.subquery.is_some();
        return along.starts_with(below).then_some(further).or(bound);
    }
    let down = || {
        let subquery = branch.subquery.as_ref()?;
        let below = below.strip_prefix(along)?;
        reads(subquery, below, proved)
    };
    down().or(bound)
}

/// The places `op` names: the subtree it reads or writes in, and with a
/// key the place under that key. Of a batch, those of the operations
/// the families count for it, where `taken` says whether the reference
/// took it ([`counted`]).
fn places(op: &Op, taken: bool) -> Vec<Path> {
    let mut places = Vec::new();
    match op {
        Op::Read(read)
        | Op::Transaction(TransactionOp::SnapshotRead(read) | TransactionOp::BlockRead(read)) => {
            read_places(read, &mut places);
        }
        Op::Write(
            Write::Insert { path, key, .. }
            | Write::Delete { path, key }
            | Write::DropFlat { path, key }
            | Write::InsertIfNotExists { path, key, .. }
            | Write::InsertIfChanged { path, key, .. }
            | Write::DeleteIfEmptyTree { path, key }
            | Write::DeleteUpTree { path, key, .. }
            | Write::DeleteBuilt { path, key }
            | Write::IndexedInsert { path, key, .. }
            | Write::IndexedDelete { path, key, .. }
            | Write::SectionalDelete { path, key }
            | Write::SectionalDeleteUpTree { path, key, .. }
            | Write::ConvertToBigSumTree { path, key, .. },
        ) => {
            places.push(path.clone());
            places.push(joined(path, key));
        }
        Op::Write(
            Write::ClearSubtree { path }
            | Write::Append { path, .. }
            | Write::ReconcileIndexed { path }
            | Write::CommitmentInsert { path, .. },
        ) => places.push(path.clone()),
        Op::Write(Write::PutAux { .. } | Write::DeleteAux { .. })
        | Op::Commit
        | Op::FlushPendingDrops
        | Op::Transaction(_) => {}
        Op::Batch(_)
        | Op::PartialBatch(_)
        | Op::FlagsBatch(_)
        | Op::FlagsPartialBatch(_)
        | Op::FlagsHandoverBatch(_) => {}
    }
    for batch_op in counted(op, taken) {
        places.push(batch_op.path().clone());
        places.extend(keyed(batch_op));
    }
    places
}

impl Known {
    /// The trees that hold elements, as `transaction` reads `db`, among
    /// the places at which `op` puts an element: asked before the
    /// operation is applied, and handed to [`Known::took`] once both
    /// engines took it.
    pub fn trees_under<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Vec<Path> {
        let holds_elements = |place: &Path| {
            let Some((key, above)) = place.split_last() else {
                return false;
            };
            let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
            let element = db
                .get_raw(
                    above.as_slice().into(),
                    key,
                    Some(transaction),
                    grove_version,
                )
                .value;
            if !element.is_ok_and(|element| element.is_any_tree()) {
                return false;
            }
            let place: Vec<&[u8]> = place.iter().map(Vec::as_slice).collect();
            db.is_empty_tree(place.as_slice(), Some(transaction), grove_version)
                .value
                .is_ok_and(|empty| !empty)
        };
        let mut trees = puts(op);
        trees.retain(holds_elements);
        trees
    }

    /// The places at which `op` puts an indexed tree where `transaction`
    /// reads an indexed tree in `db`, filled or not: asked before the
    /// operation is applied. On a grove version after the third, which
    /// removes the records of the old tree, there is none.
    pub fn indexed_under<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Vec<Path> {
        if grove_version.protocol_version > LAST_VERSION_THAT_LEAVES_THE_OLD_ROWS {
            return Vec::new();
        }
        let holds_an_indexed_tree = |place: &Path| {
            let Some((key, above)) = place.split_last() else {
                return false;
            };
            let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
            let element = db
                .get_raw(
                    above.as_slice().into(),
                    key,
                    Some(transaction),
                    grove_version,
                )
                .value;
            element.is_ok_and(|element| element.is_indexed_tree())
        };
        let indexed = put_elements(op)
            .into_iter()
            .filter(|(_, element)| element.is_indexed_tree());
        let mut places: Vec<Path> = indexed.map(|(place, _)| place).collect();
        places.retain(holds_an_indexed_tree);
        places
    }

    /// The places at which a batch `op` lists an insert that its caller
    /// knows not to replace (`InsertOnly`) of an indexed tree where
    /// `transaction` reads in `db` an indexed tree a secondary of which
    /// holds a row, and the batch names no key in that tree or below it,
    /// so that it removes no row of it (D-358): asked before the
    /// operation is applied. The candidate's refusal names a key by its
    /// bytes and no path, so a place counts only where no other operation
    /// of the batch names a key of those bytes, in whatever tree. On a
    /// grove version up to the third, where [`Known::indexed_under`] has
    /// every such put, there is none.
    pub fn indexed_under_an_insert_only<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Vec<Path> {
        if grove_version.protocol_version <= LAST_VERSION_THAT_LEAVES_THE_OLD_ROWS {
            return Vec::new();
        }
        let holds_an_indexed_tree = |place: &Path| {
            let Some((key, above)) = place.split_last() else {
                return false;
            };
            let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
            let element = db
                .get_raw(
                    above.as_slice().into(),
                    key,
                    Some(transaction),
                    grove_version,
                )
                .value;
            element.is_ok_and(|element| element.is_indexed_tree())
        };
        let mut places = Vec::new();
        for batch_op in counted(op, true) {
            if let BatchOp::InsertOnly { path, key, element } = batch_op
                && element.is_indexed_tree()
            {
                places.push(joined(path, key));
            }
        }
        let named_by_its_key_once = |place: &Path| {
            let named = op.batch_ops().flat_map(keyed);
            named.filter(|named| named.last() == place.last()).count() == 1
        };
        let untouched_below = |place: &Path| {
            let mut named = op.batch_ops().flat_map(keyed);
            !named.any(|named| named.len() > place.len() && named.starts_with(place))
        };
        let holds_a_row = |place: &Path| {
            let read = a_secondary_holds_a_row(db, transaction, place);
            read.is_ok_and(|holds| holds)
        };
        places.retain(|place| {
            named_by_its_key_once(place)
                && untouched_below(place)
                && holds_an_indexed_tree(place)
                && holds_a_row(place)
        });
        places
    }

    /// The reference took a batch that lists an insert known not to
    /// replace of an indexed tree over the indexed trees at `over`
    /// ([`Known::indexed_under_an_insert_only`]).
    pub fn came_to_insert_only_over_indexed(&mut self, over: &[Path]) {
        if !over.is_empty() {
            self.counts.histories[Family::InsertOnlyOverIndexed.index()] = 1;
        }
    }

    /// The family of a differing answer to a batch that lists an insert
    /// known not to replace of an indexed tree over the indexed trees at
    /// `over` ([`Known::indexed_under_an_insert_only`]), which the
    /// reference answered with `reference` and the candidate with
    /// `candidate` and the messages `said`:
    /// [`Family::InsertOnlyOverIndexed`] where the reference took the
    /// batch, the candidate's answer is the error `NotSupported`, and one
    /// of its messages is, word for word, the candidate's refusal of an
    /// empty indexed tree over an indexed tree whose rows the batch does
    /// not remove ([`refusal_of_rows_left`]) for the key of one of the
    /// trees. None otherwise.
    pub fn family_of_insert_only_refusal(
        over: &[Path],
        reference: &Outcome,
        candidate: &Outcome,
        said: &[String],
    ) -> Option<Family> {
        let taken = matches!(reference, Outcome::Returned(_));
        let refused = matches!(candidate, Outcome::Failed(error) if error == "NotSupported");
        let for_a_tree = |tree: &Path| {
            let words = tree.last().map(|key| refusal_of_rows_left(key));
            words.is_some_and(|words| said.contains(&words))
        };
        (taken && refused && over.iter().any(for_a_tree)).then_some(Family::InsertOnlyOverIndexed)
    }

    /// The reference took an operation that puts an indexed tree over the
    /// indexed trees at `over` ([`Known::indexed_under`]).
    pub fn came_to_indexed_over_indexed(&mut self, over: &[Path]) {
        if !over.is_empty() {
            self.counts.histories[Family::IndexedOverIndexed.index()] = 1;
        }
    }

    /// The family of a differing answer to an operation that puts an
    /// indexed tree over the indexed trees at `over`
    /// ([`Known::indexed_under`]), which the reference answered with
    /// `reference` and the candidate with `candidate`:
    /// [`Family::IndexedOverIndexed`] where there is such a tree, the
    /// reference took the operation and the candidate's answer is the
    /// error `NotSupported`, and none otherwise.
    pub fn family_of_refusal(
        over: &[Path],
        reference: &Outcome,
        candidate: &Outcome,
    ) -> Option<Family> {
        let taken = matches!(reference, Outcome::Returned(_));
        let refused = matches!(candidate, Outcome::Failed(error) if error == "NotSupported");
        (taken && refused && !over.is_empty()).then_some(Family::IndexedOverIndexed)
    }

    /// Both engines took `op` and answered it alike: its flat drops and
    /// the trees it put an element over ([`Known::trees_under`]) are
    /// places of their families from here on.
    pub fn took(&mut self, op: &Op, over: Vec<Path>) {
        if !over.is_empty() {
            self.counts.histories[Family::WrittenOver.index()] = 1;
        }
        self.over.extend(over);
        let drops = flat_drops(op);
        if !drops.is_empty() {
            self.counts.histories[Family::BelowFlatDrop.index()] = 1;
            self.counts.histories[Family::DroppedTree.index()] = 1;
        }
        self.flat_drops.extend(drops);
    }

    /// Whether the case has dropped a tree flat.
    pub fn dropped_flat(&self) -> bool {
        !self.flat_drops.is_empty()
    }

    /// Told the paths that the pending records of flat drops name now:
    /// those of them that the case dropped flat are the places of
    /// [`Family::DroppedTree`], until this is told again.
    pub fn pending(&mut self, paths: Vec<Path>) {
        self.pending = paths;
        self.pending.retain(|path| self.flat_drops.contains(path));
    }

    /// The paths [`Known::pending`] was last told and kept.
    pub fn pending_paths(&self) -> &[Path] {
        &self.pending
    }

    /// Those of `paths` at which `transaction` reads a tree in `db`.
    pub fn trees_at<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        paths: &[Path],
        grove_version: &GroveVersion,
    ) -> Vec<Path> {
        let holds_a_tree = |place: &&Path| {
            let Some((key, above)) = place.split_last() else {
                return false;
            };
            let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
            let element = db
                .get_raw(
                    above.as_slice().into(),
                    key,
                    Some(transaction),
                    grove_version,
                )
                .value;
            element.is_ok_and(|element| element.is_any_tree())
        };
        paths.iter().filter(holds_a_tree).cloned().collect()
    }

    /// Told the paths at which the reference reads a tree now
    /// ([`Known::trees_at`]): those of them that are places of
    /// [`Family::DroppedTree`] are trees made again before the drain of
    /// their drop, until this is told again.
    pub fn made_again(&mut self, paths: Vec<Path>) {
        self.remade = paths;
        self.remade.retain(|path| self.pending.contains(path));
    }

    /// The trees made again at a dropped path whose record is pending
    /// ([`Known::made_again`]) that a proof of `op` binds: those under a
    /// key that the items of the query select, or those of a subquery it
    /// comes to ([`reads`]), where `op` is a raw query of committed state.
    /// None for any other operation.
    pub fn trees_a_proof_binds(&self, op: &Op) -> Vec<Path> {
        let Op::Read(Read::Query(query)) = op else {
            return Vec::new();
        };
        if !query.raw {
            return Vec::new();
        }
        let query = query.to_path_query();
        let bound = |tree: &&Path| {
            let below = tree.strip_prefix(query.path.as_slice());
            below.is_some_and(|below| reads(&query.query.query, below, true).is_some())
        };
        self.remade.iter().filter(bound).cloned().collect()
    }

    /// Whether `op`, which the reference answered with `reference` and
    /// the candidate with `candidate`, is a step of
    /// [`Family::DroppedTreeProof`] (the owner's ruling D-348):
    ///
    /// - the operation is a raw query of committed state, and a proof of
    ///   it binds a tree made again at a dropped path whose record is
    ///   pending ([`Known::trees_a_proof_binds`]);
    /// - the reference's cost, its outcome with every row, and its root
    ///   hash are the candidate's: the two differ in the proof or its
    ///   cost alone, if they differ;
    /// - the candidate returned rows, and its proof verifies to the root
    ///   hash of its committed state and gives those rows, path, key and
    ///   bytes.
    ///
    /// The first and the last are [`Known::proof_holds`].
    pub fn through_a_proof(
        &self,
        op: &Op,
        reference: &Step,
        candidate: &Step,
        grove_version: &GroveVersion,
    ) -> bool {
        let proof_alone = reference.cost == candidate.cost
            && reference.outcome == candidate.outcome
            && reference.root_hash == candidate.root_hash;
        proof_alone && self.proof_holds(op, candidate, grove_version)
    }

    /// What a step of a dropped tree through a proof needs of the
    /// operation and of the candidate alone: the operation is a raw query
    /// of committed state and a proof of it binds a tree made again at a
    /// dropped path whose record is pending
    /// ([`Known::trees_a_proof_binds`]), and the candidate returned rows
    /// and its proof verifies to the root hash of its committed state and
    /// gives those rows, path, key and bytes.
    fn proof_holds(&self, op: &Op, candidate: &Step, grove_version: &GroveVersion) -> bool {
        let Op::Read(Read::Query(query)) = op else {
            return false;
        };
        if self.trees_a_proof_binds(op).is_empty() {
            return false;
        }
        let query = query.to_path_query();
        let Outcome::Returned(Value::Rows(returned)) = &candidate.outcome else {
            return false;
        };
        let Some(Proof {
            proved: Proved::Bytes(proof, Verification::Verified),
            ..
        }) = &candidate.proof
        else {
            return false;
        };
        let Ok((_, proved)) = GroveDb::verify_query_raw(proof, &query, grove_version) else {
            return false;
        };
        let mut rows = returned.iter().zip(&proved);
        returned.len() == proved.len()
            && rows.all(|(row, proved)| {
                row.path == proved.path && row.key == proved.key && row.element == proved.value
            })
    }

    /// The family the subtree at `path` is of.
    pub fn family(&self, path: &[Vec<u8>]) -> Option<Family> {
        if self.over.iter().any(|over| path.starts_with(over)) {
            return Some(Family::WrittenOver);
        }
        if self.pending.iter().any(|dropped| dropped == path) {
            return Some(Family::DroppedTree);
        }
        let below = |drop: &Path| path.len() > drop.len() && path.starts_with(drop);
        self.flat_drops
            .iter()
            .any(below)
            .then_some(Family::BelowFlatDrop)
    }

    /// The family of a subtree below that of `query` which the query
    /// reads ([`reads`]).
    fn family_below(&self, query: &grovedb::PathQuery) -> Option<Family> {
        let read = |place: &Path| {
            let below = place.strip_prefix(query.path.as_slice())?;
            reads(&query.query.query, below, false)
        };
        if self.over.iter().any(|over| read(over).is_some()) {
            return Some(Family::WrittenOver);
        }
        if self.pending.iter().any(|dropped| read(dropped).is_some()) {
            return Some(Family::DroppedTree);
        }
        // Below a dropped tree: the query reads the tree's own subtree
        // and goes on into one under it.
        (self.flat_drops.iter())
            .any(|drop| read(drop) == Some(true))
            .then_some(Family::BelowFlatDrop)
    }

    /// The family an answer to `op` is of, where `taken` says whether the
    /// reference took the operation: that of the first place the
    /// operation names that is of one ([`places`]), and else, for a
    /// query, that of a subtree below its own that it reads.
    pub fn family_of_answer(&self, op: &Op, taken: bool) -> Option<Family> {
        let named = places(op, taken)
            .iter()
            .find_map(|place| self.family(place));
        let read = match op {
            Op::Read(read)
            | Op::Transaction(TransactionOp::SnapshotRead(read) | TransactionOp::BlockRead(read)) => {
                Some(read)
            }
            _ => None,
        };
        named.or_else(|| {
            let queries = read.map(queries).unwrap_or_default();
            queries.iter().find_map(|query| self.family_below(query))
        })
    }

    /// The records of `listed` that are of no family, and for each family
    /// those that are of it.
    fn split(&self, listed: &[Record]) -> (Vec<Record>, [Vec<Record>; FAMILIES]) {
        let mut plain = Vec::new();
        let mut of_family: [Vec<Record>; FAMILIES] = Default::default();
        for record in listed {
            // The data of a subtree, its aux and its meta records are the
            // subtree's. The record of a pending drop carries the path of
            // the tree it frees as a label and is kept in a namespace of
            // its own, and the root key is the root tree's: they are of
            // no family, whatever path they name.
            let of_a_subtree = matches!(
                record.keyspace,
                Keyspace::Nodes | Keyspace::Flat | Keyspace::Aux | Keyspace::Meta
            );
            match self.family(&record.path).filter(|_| of_a_subtree) {
                Some(family) => of_family[family.index()].push(record.clone()),
                None => plain.push(record.clone()),
            }
        }
        (plain, of_family)
    }

    /// The records two walks listed that are of no family, to compare, with
    /// the difference in the records of each family counted for the case
    /// with the seed `seed`.
    pub fn passed_over(
        &mut self,
        seed: u64,
        reference: &[Record],
        candidate: &[Record],
    ) -> (Vec<Record>, Vec<Record>) {
        let (reference, was) = self.split(reference);
        let (candidate, is) = self.split(candidate);
        for family in Family::ALL {
            let at = family.index();
            if was[at] != is[at] && !self.stored[at] {
                self.stored[at] = true;
                self.counts.stored[at] = 1;
                self.counts.stored_seed[at] = Some(seed);
            }
        }
        (reference, candidate)
    }

    /// The case with the seed `seed` ended at an answer of `family`.
    pub fn ended_at(&mut self, family: Family, seed: u64) {
        if family == Family::DroppedTreeProof {
            self.counts.histories[family.index()] = 1;
        }
        self.counts.answers[family.index()] = 1;
        self.counts.answer_seed[family.index()] = Some(seed);
    }

    /// What the case met.
    pub fn counts(&self) -> Counts {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use grovedb::{GroveDb, RocksDbEngine};
    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::{
        diff::pair::testing::scratch,
        golden::{run::Runner, step::Row},
        workload::{IndexedTree, PartialBatch, Transactions},
    };

    impl Known {
        /// The family of an answer to an operation the reference took.
        fn family_of_taken(&self, op: &Op) -> Option<Family> {
            self.family_of_answer(op, true)
        }
    }

    /// An answer is of the family of an indexed tree over an indexed tree
    /// where the operation puts an indexed tree, unconditionally, at a
    /// key at which the reference reads an indexed tree, on a grove
    /// version up to the third, the reference took the operation and the
    /// candidate's answer is `NotSupported`. Without any one of these it
    /// is of no family: at a
    /// key that holds a tree of another kind, an item or nothing, with
    /// another element put, with a conditional insert, on grove version
    /// 4 or 5, with a put that a partial batch with no operation of its
    /// own would have added, and with another answer of the candidate.
    #[test]
    fn a_refusal_is_of_the_family_of_an_indexed_tree_over_an_indexed_tree_and_no_other() {
        let indexed = Element::empty_provable_count_indexed_tree;
        let put = |key: &[u8], element: Element| {
            Op::Batch(vec![BatchOp::InsertOrReplace {
                path: path(&[b"t"]),
                key: key.to_vec(),
                element,
            }])
        };
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        let took = Outcome::Returned(Value::Nothing);
        let refused = Outcome::Failed("NotSupported".to_string());
        let family = Some(Family::IndexedOverIndexed);
        for grove_version in GROVE_VERSIONS.iter() {
            let dir = scratch();
            let db: GroveDb<RocksDbEngine> =
                GroveDb::open_with_engine(dir.path()).expect("a database");
            let mut transactions = Transactions::new(&db);
            for op in [
                insert(&[], b"t", Element::empty_tree()),
                insert(&[b"t"], b"indexed", indexed()),
                insert(&[b"t"], b"tree", Element::empty_tree()),
                insert(&[b"t"], b"item", Element::new_item(vec![1])),
                Op::Commit,
            ] {
                transactions
                    .apply(&op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{op:?}: {error}"));
            }
            let under =
                |op: &Op| Known::indexed_under(&db, transactions.block(), op, grove_version);
            let over_indexed = under(&put(b"indexed", indexed()));
            let expected = match grove_version.protocol_version {
                1..=3 => vec![path(&[b"t", b"indexed"])],
                _ => Vec::new(),
            };
            assert_eq!(over_indexed, expected, "{}", grove_version.protocol_version);
            assert_eq!(under(&insert(&[b"t"], b"indexed", indexed())), expected);
            let on_a_kept_version = !expected.is_empty();
            assert_eq!(
                Known::family_of_refusal(&over_indexed, &took, &refused),
                family.filter(|_| on_a_kept_version)
            );

            // A key that holds another tree, an item or nothing; another
            // element put over the indexed tree; an insert that is made
            // only where the key holds nothing; a put that the caller of
            // a partial batch is never asked for.
            let conditional = Op::Batch(vec![BatchOp::InsertIfNotExists {
                path: path(&[b"t"]),
                key: b"indexed".to_vec(),
                element: indexed(),
            }]);
            for elsewhere in [
                put(b"tree", indexed()),
                put(b"item", indexed()),
                put(b"absent", indexed()),
                put(b"indexed", Element::empty_tree()),
                put(b"indexed", Element::new_item(vec![2])),
                conditional,
                Op::PartialBatch(PartialBatch {
                    ops: Vec::new(),
                    add_on: vec![BatchOp::InsertOrReplace {
                        path: path(&[b"t"]),
                        key: b"indexed".to_vec(),
                        element: indexed(),
                    }],
                    pause_height: None,
                }),
            ] {
                let over = under(&elsewhere);
                assert_eq!(over, Vec::<Path>::new(), "{elsewhere:?}");
                assert_eq!(Known::family_of_refusal(&over, &took, &refused), None);
                let mut known = Known::default();
                known.came_to_indexed_over_indexed(&over);
                assert!(known.counts().is_empty());
            }
        }

        // Another answer of the candidate at such a put is of no family.
        let over = vec![path(&[b"t", b"indexed"])];
        assert_eq!(Known::family_of_refusal(&over, &took, &refused), family);
        for other in [
            Outcome::Failed("StorageError::StorageError".to_string()),
            Outcome::Failed("InvalidBatchOperation".to_string()),
            Outcome::Failed("NotSupported::".to_string()),
            Outcome::Returned(Value::Nothing),
        ] {
            assert_eq!(
                Known::family_of_refusal(&over, &took, &other),
                None,
                "{other:?}"
            );
        }
        // Where the reference did not take the operation, a refusal of
        // the candidate is not of this family.
        for failed in ["InvalidInput", "NotSupported"] {
            let failed = Outcome::Failed(failed.to_string());
            assert_eq!(Known::family_of_refusal(&over, &failed, &refused), None);
        }
        // The family has no place: no path is of it, and no stored record.
        let mut known = Known::default();
        known.came_to_indexed_over_indexed(&over);
        assert_eq!(known.counts().histories, [0, 0, 0, 1, 0, 0]);
        assert_eq!(known.family(&over[0]), None);
        let differs = [record(&[b"t", b"indexed"], 1)];
        let (was, is) = known.passed_over(7, &differs, &[]);
        assert_eq!((was.len(), is.len()), (1, 0));
        assert_eq!(known.counts().stored, [0; 6]);
        known.ended_at(Family::IndexedOverIndexed, 11);
        let counts = known.counts();
        assert_eq!(counts.answers, [0, 0, 0, 1, 0, 0]);
        assert_eq!(counts.answer_seed[3], Some(11));
        assert_eq!(
            counts.lines(),
            ["open difference, an indexed tree written over an indexed tree, grove versions 1 to \
              3: 1 cases made it, 0 with stored records that differ passed over, 1 ended at an \
              answer that differs (seed 0xb)"]
        );
    }

    /// An answer is of the family of an insert known not to replace over
    /// an indexed tree where a batch lists such an insert of an indexed
    /// tree at a key at which the reference reads an indexed tree with a
    /// row in a secondary, on grove version 4 or 5, the reference took
    /// the batch, and the candidate's answer is `NotSupported` in the
    /// words of that refusal for that key. Without any one of these it is
    /// of no such family.
    #[test]
    fn a_refusal_of_an_insert_known_not_to_replace_is_of_its_family_on_versions_4_and_5_alone() {
        let indexed = Element::empty_provable_count_indexed_tree;
        let insert_only = |key: &[u8], element: Element| {
            Op::Batch(vec![BatchOp::InsertOnly {
                path: path(&[b"t"]),
                key: key.to_vec(),
                element,
            }])
        };
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        let took = Outcome::Returned(Value::Nothing);
        let refused = Outcome::Failed("NotSupported".to_string());
        let said = vec![refusal_of_rows_left(b"indexed")];
        assert_eq!(
            said[0],
            "failed: not supported: lignindb: an empty indexed tree written under the key \
             696e6465786564 over an indexed tree, a secondary of which holds rows that the batch \
             does not remove"
        );
        let family = Some(Family::InsertOnlyOverIndexed);
        for grove_version in GROVE_VERSIONS.iter() {
            let dir = scratch();
            let db: GroveDb<RocksDbEngine> =
                GroveDb::open_with_engine(dir.path()).expect("a database");
            let mut transactions = Transactions::new(&db);
            for op in [
                insert(&[], b"t", Element::empty_tree()),
                insert(&[b"t"], b"indexed", indexed()),
                Op::Write(Write::IndexedInsert {
                    path: path(&[b"t", b"indexed"]),
                    key: b"entry".to_vec(),
                    element: Element::empty_count_tree(),
                    tree: IndexedTree::Count,
                }),
                insert(&[b"t"], b"bare", indexed()),
                insert(&[b"t"], b"tree", Element::empty_tree()),
                Op::Commit,
            ] {
                transactions
                    .apply(&op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{op:?}: {error}"));
            }
            let block = transactions.block();
            let rows = |at: &[&[u8]]| a_secondary_holds_a_row(&db, block, &path(at));
            assert_eq!(rows(&[b"t", b"indexed"]), Ok(true));
            assert_eq!(rows(&[b"t", b"bare"]), Ok(false));
            let under = |op: &Op| {
                Known::indexed_under_an_insert_only(&db, transactions.block(), op, grove_version)
            };
            let over = under(&insert_only(b"indexed", indexed()));
            let expected = match grove_version.protocol_version {
                1..=3 => Vec::new(),
                _ => vec![path(&[b"t", b"indexed"])],
            };
            assert_eq!(over, expected, "{}", grove_version.protocol_version);
            let on_a_later_version = !expected.is_empty();
            assert_eq!(
                Known::family_of_insert_only_refusal(&over, &took, &refused, &said),
                family.filter(|_| on_a_later_version)
            );
            // The two families of an indexed tree over an indexed tree
            // share no grove version.
            let by_any_put = Known::indexed_under(
                &db,
                transactions.block(),
                &insert_only(b"indexed", indexed()),
                grove_version,
            );
            assert_eq!(by_any_put.is_empty(), on_a_later_version);

            // Another operation at the key: one that can replace, a
            // conditional insert, a direct insert; another element; a
            // key that holds another tree, an indexed tree with no row,
            // or nothing; the key named twice; a key of the same bytes
            // named in another tree; a key named in the tree itself.
            let replacing = |op: BatchOp| Op::Batch(vec![op]);
            let at = || (path(&[b"t"]), b"indexed".to_vec());
            let twice = Op::Batch(vec![
                BatchOp::InsertOnly {
                    path: at().0,
                    key: at().1,
                    element: indexed(),
                },
                BatchOp::Delete {
                    path: at().0,
                    key: at().1,
                },
            ]);
            let in_another_tree = Op::Batch(vec![
                BatchOp::InsertOnly {
                    path: at().0,
                    key: at().1,
                    element: indexed(),
                },
                BatchOp::InsertOrReplace {
                    path: path(&[b"t", b"tree"]),
                    key: at().1,
                    element: indexed(),
                },
            ]);
            let with_a_delete_in_it = Op::Batch(vec![
                BatchOp::InsertOnly {
                    path: at().0,
                    key: at().1,
                    element: indexed(),
                },
                BatchOp::Delete {
                    path: path(&[b"t", b"indexed"]),
                    key: b"entry".to_vec(),
                },
            ]);
            for elsewhere in [
                replacing(BatchOp::InsertOrReplace {
                    path: at().0,
                    key: at().1,
                    element: indexed(),
                }),
                replacing(BatchOp::Replace {
                    path: at().0,
                    key: at().1,
                    element: indexed(),
                }),
                replacing(BatchOp::InsertIfNotExists {
                    path: at().0,
                    key: at().1,
                    element: indexed(),
                }),
                insert(&[b"t"], b"indexed", indexed()),
                insert_only(b"indexed", Element::empty_tree()),
                insert_only(b"indexed", Element::new_item(vec![1])),
                insert_only(b"tree", indexed()),
                insert_only(b"bare", indexed()),
                insert_only(b"absent", indexed()),
                twice,
                in_another_tree,
                with_a_delete_in_it,
            ] {
                let over = under(&elsewhere);
                assert_eq!(over, Vec::<Path>::new(), "{elsewhere:?}");
                let none = Known::family_of_insert_only_refusal(&over, &took, &refused, &said);
                assert_eq!(none, None);
                let mut known = Known::default();
                known.came_to_insert_only_over_indexed(&over);
                assert!(known.counts().is_empty());
            }
        }

        // Another answer: a reference that did not take the batch,
        // another error of the candidate, a candidate that took it,
        // other words, the words for another key, and the words inside
        // a longer message.
        let over = vec![path(&[b"t", b"indexed"])];
        let of = Known::family_of_insert_only_refusal;
        assert_eq!(of(&over, &took, &refused, &said), family);
        assert_eq!(of(&over, &refused, &refused, &said), None);
        let invalid = Outcome::Failed("InvalidBatchOperation".to_string());
        assert_eq!(of(&over, &invalid, &refused, &said), None);
        assert_eq!(of(&over, &took, &invalid, &said), None);
        assert_eq!(of(&over, &took, &took, &said), None);
        assert_eq!(of(&over, &took, &refused, &[]), None);
        let other_words = vec!["failed: not supported: lignindb: a write".to_string()];
        assert_eq!(of(&over, &took, &refused, &other_words), None);
        let other_key = vec![refusal_of_rows_left(b"other")];
        assert_eq!(of(&over, &took, &refused, &other_key), None);
        let longer = vec![format!("{}, and more", said[0])];
        assert_eq!(of(&over, &took, &refused, &longer), None);
        assert_eq!(of(&[], &took, &refused, &said), None);
        // The family has no place, and no stored record is of it.
        let mut known = Known::default();
        known.came_to_insert_only_over_indexed(&over);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 1]);
        assert_eq!(known.family(&over[0]), None);
        known.ended_at(Family::InsertOnlyOverIndexed, 13);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 1]);
        assert_eq!(known.counts().answer_seed[5], Some(13));
    }

    /// The subtree of a tree that was dropped flat is of its family while
    /// the record of its drop is pending, and of none before the case
    /// took the drop and after the record is reclaimed. No other place is
    /// of the family: not the tree above, not a tree beside, not the path
    /// of a pending record of a drop that is none of the case's, and what
    /// is below the dropped tree stays of the family it was of.
    #[test]
    fn a_dropped_tree_is_of_its_family_while_the_record_of_its_drop_is_pending() {
        let dropped = path(&[b"a", b"dropped"]);
        let foreign = path(&[b"not", b"of", b"the", b"case"]);
        let tree = Some(Family::DroppedTree);
        // A read that names the subtree at its path and no other place.
        let get = |at: &Path| Op::Read(Read::IsEmptyTree { path: at.clone() });
        let remake = Op::Write(Write::Insert {
            path: path(&[b"a"]),
            key: b"dropped".to_vec(),
            element: Element::empty_tree(),
        });

        let mut before = Known::default();
        before.pending(vec![dropped.clone()]);
        assert_eq!(before.family(&dropped), None);
        assert!(!before.dropped_flat());

        let mut known = known();
        assert!(known.dropped_flat());
        assert_eq!(known.family(&dropped), None);
        assert_eq!(known.family_of_taken(&get(&dropped)), None);
        known.pending(vec![foreign.clone(), dropped.clone()]);
        assert_eq!(known.family(&dropped), tree);
        assert_eq!(known.family_of_taken(&get(&dropped)), tree);
        assert_eq!(known.family_of_taken(&remake), tree);
        let below = path(&[b"a", b"dropped", b"child"]);
        assert_eq!(known.family(&below), Some(Family::BelowFlatDrop));
        for elsewhere in [
            path(&[]),
            path(&[b"a"]),
            path(&[b"a", b"beside"]),
            path(&[b"a", b"droppe"]),
            path(&[b"dropped"]),
            foreign.clone(),
        ] {
            assert_eq!(known.family(&elsewhere), None, "{elsewhere:?}");
            assert_eq!(known.family_of_taken(&get(&elsewhere)), None);
            let mut differs = vec![record(&[b"c"], 1), record(&[], 2)];
            differs[1].path = elsewhere.clone();
            let (was, is) = known.passed_over(7, &differs, &differs[..1]);
            assert_eq!((was.len(), is.len()), (2, 1), "{elsewhere:?}");
        }
        assert_eq!(known.counts().stored, [0; 6]);

        // A stored difference in the dropped tree's own subtree is passed
        // over and counted, but not in the record of its drop.
        let kept = [record(&[b"c"], 1), record(&[b"a", b"dropped"], 1)];
        let (was, is) = known.passed_over(7, &kept, &kept[..1]);
        assert_eq!((was.len(), is.len()), (1, 1));
        assert_eq!(known.counts().stored, [0, 0, 1, 0, 0, 0]);
        assert_eq!(known.counts().stored_seed[..4], [None, None, Some(7), None]);
        let mut drop_record = record(&[b"a", b"dropped"], 1);
        drop_record.keyspace = Keyspace::DropRecord;
        let (was, is) = known.passed_over(7, &[drop_record], &[]);
        assert_eq!((was.len(), is.len()), (1, 0));

        // A query above reads the dropped tree where it selects its key
        // and goes down into it.
        let query = |key: &[u8], all: bool| {
            Op::Read(Read::Query(crate::workload::Query {
                path: path(&[b"a"]),
                items: vec![crate::workload::QueryItem::Key(key.to_vec())],
                subquery_path: None,
                subquery_all: all,
                limit: None,
                left_to_right: true,
                raw: false,
            }))
        };
        assert_eq!(known.family_of_taken(&query(b"dropped", true)), tree);
        assert_eq!(known.family_of_taken(&query(b"dropped", false)), None);
        assert_eq!(known.family_of_taken(&query(b"beside", true)), None);

        // The record is reclaimed: the subtree is compared again.
        known.pending(vec![foreign]);
        assert_eq!(known.family(&dropped), None);
        assert_eq!(known.family_of_taken(&get(&dropped)), None);
        assert_eq!(known.family_of_taken(&remake), None);
        let (was, is) = known.passed_over(7, &kept, &kept[..1]);
        assert_eq!((was.len(), is.len()), (2, 1));
        assert_eq!(known.family(&below), Some(Family::BelowFlatDrop));
    }

    /// A step is of a dropped tree through a proof where the operation
    /// is a raw query of committed state, a tree made again at a dropped
    /// path whose record is pending is under a key its items select, and
    /// the candidate's proof verifies and gives the rows it returned.
    /// Without any one of these it is not.
    #[test]
    fn a_step_is_of_a_dropped_tree_through_a_proof_by_the_query_and_the_candidates_proof() {
        let grove_version = GROVE_VERSIONS.last().expect("a grove version");
        let dir = scratch();
        let db: GroveDb<RocksDbEngine> = GroveDb::open_with_engine(dir.path()).expect("a database");
        let mut runner = Runner::new(&db, grove_version).expect("a runner");
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        for op in [
            insert(&[], b"t", Element::empty_tree()),
            insert(&[b"t"], b"again", Element::empty_tree()),
            insert(&[b"t"], b"item", Element::new_item(vec![1])),
            Op::Commit,
        ] {
            let taken = runner.step(&op).expect("a step");
            assert!(!taken.step.failed(), "{op:?}: {taken:?}");
        }
        let query = |at: &[&[u8]], items, raw| crate::workload::Query {
            path: path(at),
            items,
            subquery_path: None,
            subquery_all: false,
            limit: None,
            left_to_right: true,
            raw,
        };
        let everything = vec![crate::workload::QueryItem::RangeFull];
        let asked = Op::Read(Read::Query(query(&[b"t"], everything.clone(), true)));
        let answered = runner.step(&asked).expect("a step").step;
        let Outcome::Returned(Value::Rows(rows)) = &answered.outcome else {
            panic!("a query returns rows: {answered:?}");
        };
        assert_eq!(rows.len(), 2);
        let verified = matches!(
            &answered.proof,
            Some(Proof {
                proved: Proved::Bytes(_, Verification::Verified),
                ..
            })
        );
        assert!(verified, "{answered:?}");

        let again = path(&[b"t", b"again"]);
        let mut known = Known::default();
        let drop = Op::Write(Write::DropFlat {
            path: path(&[b"t"]),
            key: b"again".to_vec(),
        });
        known.took(&drop, Vec::new());
        // Dropped, and no record told: not pending, so not made again.
        known.made_again(vec![again.clone()]);
        assert!(!known.through_a_proof(&asked, &answered, &answered, grove_version));
        // Pending, and not told that a tree stands there again.
        known.pending(vec![again.clone()]);
        assert!(!known.through_a_proof(&asked, &answered, &answered, grove_version));
        // Pending and made again: what `trees_at` reads of the database.
        let block = runner.transactions().block();
        let standing = Known::trees_at(&db, block, known.pending_paths(), grove_version);
        assert_eq!(standing, vec![again.clone()]);
        let item = [path(&[b"t", b"item"]), path(&[b"t", b"absent"])];
        assert!(Known::trees_at(&db, block, &item, grove_version).is_empty());
        known.made_again(standing);
        assert!(known.through_a_proof(&asked, &answered, &answered, grove_version));
        assert!(known.proof_holds(&asked, &answered, grove_version));
        assert_eq!(known.trees_a_proof_binds(&asked), vec![again.clone()]);
        // It is of no path: no place is of the kind, and no stored record.
        assert_eq!(known.family(&again), Some(Family::DroppedTree));
        assert_eq!(known.family(&path(&[b"t"])), None);

        // Another operation: a query that is not raw, one whose items do
        // not select the key, one of another subtree, a read through the
        // block, and a read that is no query.
        let one = |key: &[u8]| vec![crate::workload::QueryItem::Key(key.to_vec())];
        let selects = Op::Read(Read::Query(query(&[b"t"], one(b"again"), true)));
        let selected = runner.step(&selects).expect("a step").step;
        assert!(known.through_a_proof(&selects, &selected, &selected, grove_version));
        for other in [
            Op::Read(Read::Query(query(&[b"t"], everything.clone(), false))),
            Op::Read(Read::Query(query(&[b"t"], one(b"item"), true))),
            Op::Read(Read::Query(query(&[], everything.clone(), true))),
            Op::Read(Read::IsEmptyTree {
                path: path(&[b"t"]),
            }),
        ] {
            let step = runner.step(&other).expect("a step").step;
            assert!(
                !known.through_a_proof(&other, &step, &step, grove_version),
                "{other:?}"
            );
            assert!(!known.proof_holds(&other, &step, grove_version));
            assert!(known.trees_a_proof_binds(&other).is_empty(), "{other:?}");
        }
        let Op::Read(read) = asked.clone() else {
            unreachable!()
        };
        let through_the_block = Op::Transaction(TransactionOp::BlockRead(read));
        assert!(!known.through_a_proof(&through_the_block, &answered, &answered, grove_version));

        // Another answer of the candidate: no rows, a row more or less or
        // another one than its proof gives, a proof that verifies to
        // another root, one the verifier refuses, one that is not the
        // query's, and none.
        let with = |change: &dyn Fn(&mut Step)| {
            let mut step = answered.clone();
            change(&mut step);
            known.through_a_proof(&asked, &step, &step, grove_version)
        };
        assert!(with(&|_| {}));
        assert!(!with(
            &|step| step.outcome = Outcome::Failed("InvalidQuery".to_string())
        ));
        assert!(!with(
            &|step| step.outcome = Outcome::Returned(Value::Nothing)
        ));
        let rows_of = |step: &mut Step, change: &dyn Fn(&mut Vec<Row>)| {
            if let Outcome::Returned(Value::Rows(rows)) = &mut step.outcome {
                change(rows);
            }
        };
        assert!(!with(&|step| rows_of(step, &|rows| drop_last(rows))));
        assert!(!with(
            &|step| rows_of(step, &|rows| rows.push(rows[0].clone()))
        ));
        assert!(!with(&|step| rows_of(step, &|rows| rows[1]
            .element
            .push(0))));
        assert!(!with(&|step| rows_of(step, &|rows| rows[1].key.push(0))));
        assert!(!with(&|step| rows_of(step, &|rows| rows[0]
            .path
            .push(vec![0]))));
        let proof_of = |step: &mut Step, change: &dyn Fn(Vec<u8>) -> Proved| {
            let Some(Proof {
                proved: Proved::Bytes(bytes, _),
                cost,
            }) = step.proof.take()
            else {
                panic!("a proof");
            };
            step.proof = Some(Proof {
                cost,
                proved: change(bytes),
            });
        };
        assert!(!with(&|step| proof_of(step, &|bytes| {
            Proved::Bytes(bytes, Verification::OtherRoot)
        })));
        assert!(!with(&|step| proof_of(step, &|bytes| {
            Proved::Bytes(bytes, Verification::Refused("InvalidProof".to_string()))
        })));
        assert!(!with(&|step| proof_of(step, &|_| {
            Proved::Failed("InvalidQuery".to_string())
        })));
        let Some(Proof {
            proved: Proved::Bytes(of_one_key, _),
            ..
        }) = &selected.proof
        else {
            panic!("a proof");
        };
        assert!(!with(&|step| proof_of(step, &|_| {
            Proved::Bytes(of_one_key.clone(), Verification::Verified)
        })));
        assert!(!with(&|step| step.proof = None));

        // Another answer of the reference: it may differ from the
        // candidate's in the proof, in what the verifier made of it and
        // in the cost of proving, and in nothing else. A row with another
        // byte, a row less, another cost of the query, an error or a root
        // hash beside the candidate's valid proof is not of the kind.
        let against = |change: &dyn Fn(&mut Step)| {
            let mut reference = answered.clone();
            change(&mut reference);
            assert_ne!(reference, answered);
            known.through_a_proof(&asked, &reference, &answered, grove_version)
        };
        assert!(against(&|step| proof_of(step, &|bytes| {
            Proved::Bytes(bytes, Verification::Refused("InvalidProof".to_string()))
        })));
        assert!(against(&|step| proof_of(step, &|mut bytes| {
            bytes.push(0);
            Proved::Bytes(bytes, Verification::Verified)
        })));
        assert!(against(&|step| {
            let proof = step.proof.as_mut().expect("a proof");
            proof.cost.seek_count += 1;
        }));
        assert!(!against(&|step| rows_of(step, &|rows| rows[1]
            .element
            .push(0))));
        assert!(!against(&|step| rows_of(step, &|rows| drop_last(rows))));
        assert!(!against(&|step| step.cost.seek_count += 1));
        assert!(!against(
            &|step| step.outcome = Outcome::Failed("InvalidQuery".to_string())
        ));
        assert!(!against(&|step| step.root_hash = Some([0; 32])));

        // The case that ends there is counted for the kind alone.
        known.ended_at(Family::DroppedTreeProof, 5);
        let counts = known.counts();
        assert_eq!(counts.answers, [0, 0, 0, 0, 1, 0]);
        assert_eq!(counts.histories, [0, 1, 1, 0, 1, 0]);
        assert_eq!(counts.answer_seed[4], Some(5));
    }

    fn drop_last(rows: &mut Vec<Row>) {
        rows.pop();
    }

    /// With a proof, a query reads the subtree under each key that its
    /// items select, or those of a subquery it comes to, and no other.
    #[test]
    fn a_proved_query_reads_the_subtree_under_each_key_its_items_select() {
        let key = |key: &[u8]| grovedb::QueryItem::Key(key.to_vec());
        let below = |segments: &[&[u8]]| path(segments);
        let mut plain = grovedb::Query::new();
        plain.insert_item(key(b"k"));
        assert_eq!(reads(&plain, &below(&[b"k"]), false), None);
        assert_eq!(reads(&plain, &below(&[b"k"]), true), Some(false));
        assert_eq!(reads(&plain, &below(&[b"other"]), true), None);
        assert_eq!(reads(&plain, &below(&[b"k", b"l"]), true), None);
        // Under a subquery: the keys its items select, one level down.
        let mut inner = grovedb::Query::new();
        inner.insert_item(key(b"l"));
        let mut outer = grovedb::Query::new();
        outer.insert_item(key(b"k"));
        outer.set_subquery(inner);
        assert_eq!(reads(&outer, &below(&[b"k"]), true), Some(false));
        assert_eq!(reads(&outer, &below(&[b"k", b"l"]), false), None);
        assert_eq!(reads(&outer, &below(&[b"k", b"l"]), true), Some(false));
        assert_eq!(reads(&outer, &below(&[b"k", b"m"]), true), None);
        assert_eq!(reads(&outer, &below(&[b"k", b"l", b"m"]), true), None);
        // Along a subquery path the next key of the path is no key the
        // items select: the tree at the end of the path is not bound by
        // this rule.
        let mut along = grovedb::Query::new();
        along.insert_item(key(b"k"));
        along.set_subquery_path(vec![b"a".to_vec()]);
        assert_eq!(reads(&along, &below(&[b"k"]), true), Some(false));
        assert_eq!(reads(&along, &below(&[b"k", b"a"]), true), None);
    }

    fn path(segments: &[&[u8]]) -> Path {
        segments.iter().map(|segment| segment.to_vec()).collect()
    }

    fn record(at: &[&[u8]], value: u8) -> Record {
        Record {
            path: path(at),
            secondary: None,
            keyspace: Keyspace::Nodes,
            key: b"k".to_vec(),
            value: vec![value],
            left_behind: false,
        }
    }

    fn known() -> Known {
        let mut known = Known::default();
        let drop = Op::Write(Write::DropFlat {
            path: path(&[b"a"]),
            key: b"dropped".to_vec(),
        });
        let put = Op::Batch(vec![BatchOp::InsertOnly {
            path: path(&[b"b"]),
            key: b"over".to_vec(),
            element: Element::empty_tree(),
        }]);
        known.took(&drop, Vec::new());
        known.took(&put, vec![path(&[b"b", b"over"])]);
        known
    }

    /// The rule of a family is its path: the subtree of a tree that was
    /// written over and what is below it, and what is below a tree that
    /// was dropped flat, and no other place.
    #[test]
    fn a_family_is_a_path_and_nothing_beside_it() {
        let known = known();
        let over = Some(Family::WrittenOver);
        let below = Some(Family::BelowFlatDrop);
        assert_eq!(known.family(&path(&[b"b", b"over"])), over);
        assert_eq!(known.family(&path(&[b"b", b"over", b"deeper"])), over);
        assert_eq!(known.family(&path(&[b"a", b"dropped", b"child"])), below);
        assert_eq!(
            known.family(&path(&[b"a", b"dropped", b"child", b"x"])),
            below
        );
        // The dropped tree's own subtree is of no family where no record
        // of its drop is pending.
        assert_eq!(known.family(&path(&[b"a", b"dropped"])), None);
        for elsewhere in [
            path(&[]),
            path(&[b"a"]),
            path(&[b"b"]),
            path(&[b"a", b"other"]),
            path(&[b"b", b"other", b"over"]),
            path(&[b"a", b"droppe"]),
            path(&[b"over"]),
        ] {
            assert_eq!(known.family(&elsewhere), None, "{elsewhere:?}");
        }
        // A put that was over no tree with elements names no place.
        let mut none = Known::default();
        none.took(
            &Op::Batch(vec![BatchOp::InsertOnly {
                path: path(&[b"b"]),
                key: b"over".to_vec(),
                element: Element::empty_tree(),
            }]),
            Vec::new(),
        );
        assert_eq!(none.family(&path(&[b"b", b"over"])), None);
        assert!(none.counts().is_empty());
    }

    /// A difference in what is stored is passed over in a subtree of a
    /// family and counted once for the case, and a difference anywhere
    /// else is left for the comparison.
    #[test]
    fn a_stored_difference_is_passed_over_in_a_family_and_nowhere_else() {
        let mut known = known();
        let same = record(&[b"c"], 1);
        let reference = [
            same.clone(),
            record(&[b"a", b"dropped", b"child"], 1),
            record(&[b"b", b"over"], 1),
        ];
        let candidate = [same.clone()];
        let (was, is) = known.passed_over(7, &reference, &candidate);
        assert_eq!((was, is), (vec![same.clone()], vec![same.clone()]));
        let (was, is) = known.passed_over(7, &reference, &candidate);
        assert_eq!(was, is);
        let counts = known.counts();
        assert_eq!(counts.stored, [1, 1, 0, 0, 0, 0]);
        assert_eq!(counts.stored_seed[..4], [Some(7), Some(7), None, None]);
        assert_eq!(
            (counts.histories, counts.answers),
            ([1, 1, 1, 0, 0, 0], [0; 6])
        );
        assert_eq!(counts.stored_differences(), 2);
        assert_eq!(counts.answer_differences(), 0);

        // Elsewhere: the dropped tree's own subtree, a subtree beside, and
        // the record of a pending drop that names a place of a family.
        let mut pending = record(&[b"b", b"over"], 2);
        pending.keyspace = Keyspace::PendingDrop;
        let mut queued = record(&[b"a", b"dropped", b"child"], 2);
        queued.keyspace = Keyspace::DropRecord;
        for elsewhere in [
            record(&[b"a", b"dropped"], 2),
            record(&[b"a", b"beside"], 2),
            pending,
            queued,
        ] {
            let mut known = self::known();
            let reference = [same.clone(), elsewhere.clone()];
            let (was, is) = known.passed_over(7, &reference, &candidate);
            assert_eq!(was, vec![same.clone(), elsewhere]);
            assert_eq!(is, vec![same.clone()]);
            assert_eq!(known.counts().stored, [0; 6]);
        }
    }

    /// An answer is of a family where its operation names a place of the
    /// family, and of none otherwise.
    #[test]
    fn an_answer_is_of_a_family_where_its_operation_names_a_place_of_it() {
        let known = known();
        let empty = |at: &[&[u8]]| Op::Read(Read::IsEmptyTree { path: path(at) });
        let get = |at: &[&[u8]], key: &[u8]| {
            Op::Read(Read::Get {
                path: path(at),
                key: key.to_vec(),
            })
        };
        let delete = |at: &[&[u8]], key: &[u8]| {
            Op::Write(Write::Delete {
                path: path(at),
                key: key.to_vec(),
            })
        };
        let over = Some(Family::WrittenOver);
        assert_eq!(known.family_of_taken(&empty(&[b"b", b"over"])), over);
        assert_eq!(known.family_of_taken(&get(&[b"b", b"over"], b"old")), over);
        assert_eq!(known.family_of_taken(&delete(&[b"b"], b"over")), over);
        assert_eq!(
            known.family_of_taken(&get(&[b"a", b"dropped", b"child"], b"k")),
            Some(Family::BelowFlatDrop)
        );
        assert_eq!(known.family_of_taken(&empty(&[b"b"])), None);
        // A query of the subtree above goes down to the place by its
        // subquery, which is of one subtree down.
        let query = |at: &[&[u8]]| {
            Op::Read(Read::Query(crate::workload::Query {
                path: path(at),
                items: vec![crate::workload::QueryItem::RangeFull],
                subquery_path: None,
                subquery_all: true,
                limit: None,
                left_to_right: true,
                raw: false,
            }))
        };
        assert_eq!(known.family_of_taken(&query(&[b"b"])), over);
        assert_eq!(known.family_of_taken(&query(&[])), None);
        assert_eq!(known.family_of_taken(&query(&[b"a"])), None);
        assert_eq!(known.family_of_taken(&query(&[b"c"])), None);
        assert_eq!(known.family_of_taken(&query(&[b"b", b"other"])), None);
        // With no subquery a query reads its own subtree alone: the one
        // above a place is of no family, and neither is the one of the
        // dropped tree's own subtree.
        let alone = |at: &[&[u8]]| {
            let Op::Read(Read::Query(mut query)) = query(at) else {
                unreachable!()
            };
            query.subquery_all = false;
            Op::Read(Read::Query(query))
        };
        assert_eq!(known.family_of_taken(&alone(&[b"b"])), None);
        assert_eq!(known.family_of_taken(&alone(&[b"a", b"dropped"])), None);
        assert_eq!(known.family_of_taken(&alone(&[])), None);
        // With one, a query reads the subtrees under the keys its items
        // select, and no other.
        let of_key = |at: &[&[u8]], key: &[u8], subquery_path: Option<&[&[u8]]>, all: bool| {
            let Op::Read(Read::Query(mut query)) = query(at) else {
                unreachable!()
            };
            query.items = vec![crate::workload::QueryItem::Key(key.to_vec())];
            query.subquery_path = subquery_path.map(path);
            query.subquery_all = all;
            Op::Read(Read::Query(query))
        };
        let below = Some(Family::BelowFlatDrop);
        let check = |at: &[&[u8]], key: &[u8], along: Option<&[&[u8]]>, all: bool, family| {
            let asked = of_key(at, key, along, all);
            assert_eq!(known.family_of_taken(&asked), family, "{asked:?}");
        };
        check(&[b"b"], b"over", None, true, over);
        check(&[b"b"], b"other", None, true, None);
        check(&[b"b"], b"over", None, false, None);
        // One subquery goes one subtree down, and no further.
        check(&[], b"b", None, true, None);
        check(&[], b"a", None, true, None);
        check(&[], b"c", None, true, None);
        // A subquery path is read along: without a subquery the last
        // subtree read is the one that holds the path's last key.
        check(&[], b"b", Some(&[b"over"]), false, None);
        check(&[], b"b", Some(&[b"over"]), true, over);
        check(&[], b"b", Some(&[b"over", b"k"]), false, over);
        check(&[], b"b", Some(&[b"other", b"k"]), true, None);
        check(&[], b"b", Some(&[b"other"]), true, None);
        check(&[], b"c", Some(&[b"over"]), true, None);
        // Below the dropped tree: the query reads the tree's own subtree
        // and goes on into one under it.
        check(&[b"a"], b"dropped", None, true, None);
        check(&[b"a"], b"other", Some(&[b"k"]), true, None);
        check(&[b"a"], b"dropped", Some(&[b"k"]), false, None);
        check(&[b"a"], b"dropped", Some(&[b"k"]), true, below);
        check(&[b"a"], b"dropped", Some(&[b"k", b"l"]), false, below);
        check(&[], b"a", Some(&[b"dropped"]), false, None);
        check(&[], b"a", Some(&[b"dropped"]), true, None);
        check(&[], b"a", Some(&[b"dropped", b"k"]), false, None);
        check(&[], b"a", Some(&[b"dropped", b"k"]), true, below);
        // The branch of a key is the first conditional one that selects
        // it, and else the default one.
        let branched = |key: &[u8], conditional_goes_down: bool| {
            let mut query = grovedb::Query::new();
            query.insert_item(grovedb::QueryItem::Key(b"b".to_vec()));
            query.set_subquery_path(vec![b"over".to_vec(), b"k".to_vec()]);
            let along = conditional_goes_down.then(|| vec![b"other".to_vec(), b"k".to_vec()]);
            query.add_conditional_subquery(grovedb::QueryItem::Key(key.to_vec()), along, None);
            let query = grovedb::PathQuery::new_unsized(Vec::new(), query);
            Op::Read(Read::Prove(Prove::PathQuery(Box::new(query))))
        };
        assert_eq!(known.family_of_taken(&branched(b"c", false)), over);
        assert_eq!(known.family_of_taken(&branched(b"b", false)), None);
        assert_eq!(known.family_of_taken(&branched(b"b", true)), None);
        // Of its own subtree a query goes on below only where a key its
        // items select takes a branch that goes down: not by a
        // conditional branch of keys the items do not select, and not by
        // a default branch where conditional ones that end there take
        // every selected key.
        let down = |items: Vec<grovedb::QueryItem>,
                    default_goes_down: bool,
                    conditional: grovedb::QueryItem,
                    conditional_goes_down: bool| {
            let mut query = grovedb::Query::new();
            for item in items {
                query.insert_item(item);
            }
            if default_goes_down {
                query.set_subquery(grovedb::Query::new_range_full());
            }
            let subquery = conditional_goes_down.then(grovedb::Query::new_range_full);
            query.add_conditional_subquery(conditional, None, subquery);
            let query = grovedb::PathQuery::new_unsized(path(&[b"a", b"dropped"]), query);
            known.family_of_taken(&Op::Read(Read::Prove(Prove::PathQuery(Box::new(query)))))
        };
        let key = |key: &[u8]| grovedb::QueryItem::Key(key.to_vec());
        let from = |key: &[u8]| grovedb::QueryItem::RangeFrom(key.to_vec()..);
        let after = |key: &[u8]| grovedb::QueryItem::RangeAfter(key.to_vec()..);
        let full = || grovedb::QueryItem::RangeFull(..);
        assert_eq!(down(vec![key(b"s")], false, key(b"x"), true), None);
        assert_eq!(down(vec![key(b"s")], false, from(b"t"), true), None);
        assert_eq!(down(vec![key(b"s")], false, key(b"s"), true), below);
        assert_eq!(down(vec![key(b"s")], false, from(b"r"), true), below);
        assert_eq!(down(vec![from(b"t")], false, key(b"s"), true), None);
        assert_eq!(down(vec![from(b"s")], false, key(b"s"), true), below);
        assert_eq!(down(vec![key(b"s")], true, key(b"s"), false), None);
        assert_eq!(down(vec![key(b"s")], true, full(), false), None);
        assert_eq!(down(vec![from(b"s")], true, from(b"s"), false), None);
        assert_eq!(down(vec![full()], true, from(b""), false), None);
        assert_eq!(
            down(vec![key(b"s"), key(b"u")], true, key(b"s"), false),
            below
        );
        // The keys between two bounds, the key right after a bound, and
        // the empty key are left to the default branch.
        assert_eq!(down(vec![from(b"s")], true, from(b"t"), false), below);
        assert_eq!(down(vec![from(b"s")], true, key(b"s"), false), below);
        assert_eq!(down(vec![full()], true, after(b""), false), below);
        assert_eq!(down(vec![after(b"")], true, after(b""), false), None);
        assert_eq!(known.family_of_taken(&alone(&[b"b", b"over"])), over);
        assert_eq!(
            known.family_of_taken(&query(&[b"a", b"dropped"])),
            Some(Family::BelowFlatDrop)
        );
        assert_eq!(known.family_of_taken(&get(&[b"b"], b"other")), None);
        assert_eq!(known.family_of_taken(&delete(&[b"a"], b"dropped")), None);
        // A proof of a chunk names its subtree, and a move its new key.
        let trunk = |at: &[&[u8]]| {
            Op::Read(Read::Prove(Prove::TrunkChunk {
                path: path(at),
                max_depth: 2,
                min_depth: None,
            }))
        };
        assert_eq!(known.family_of_taken(&trunk(&[b"b", b"over"])), over);
        assert_eq!(known.family_of_taken(&trunk(&[b"b"])), None);
        let moved = |key: &[u8], new_key: &[u8]| {
            Op::Batch(vec![BatchOp::Move {
                path: path(&[b"b"]),
                key: key.to_vec(),
                new_key: new_key.to_vec(),
            }])
        };
        assert_eq!(known.family_of_taken(&moved(b"src", b"over")), over);
        assert_eq!(known.family_of_taken(&moved(b"src", b"other")), None);
        // An insert that is made only where the key holds none names no
        // place it puts an element at.
        let conditional = Op::Write(Write::InsertIfNotExists {
            path: path(&[]),
            key: b"t".to_vec(),
            element: Element::empty_tree(),
            return_existing: false,
        });
        assert!(puts(&conditional).is_empty());
        let batched = Op::Batch(vec![BatchOp::InsertIfNotExists {
            path: path(&[]),
            key: b"t".to_vec(),
            element: Element::empty_tree(),
        }]);
        assert!(puts(&batched).is_empty());
        assert_eq!(known.family_of_taken(&Op::Commit), None);
        // A partial batch with no operation of its own is answered before
        // its caller is asked for the operations it adds last: they put
        // nothing, drop nothing and name no place. With an operation of
        // its own, a batch that an engine takes applies them; one that
        // the reference did not take may have failed before, and names
        // the places of its own operations alone.
        let added = |ops: Vec<BatchOp>| {
            let put = BatchOp::InsertOrReplace {
                path: path(&[b"b"]),
                key: b"over".to_vec(),
                element: Element::empty_tree(),
            };
            let drop = BatchOp::DropFlat {
                path: path(&[b"a"]),
                key: b"dropped".to_vec(),
                tree_type: grovedb::TreeType::NormalTree,
            };
            Op::PartialBatch(PartialBatch {
                ops,
                add_on: vec![put, drop],
                pause_height: None,
            })
        };
        let nothing_of_its_own = added(Vec::new());
        assert!(puts(&nothing_of_its_own).is_empty());
        assert!(flat_drops(&nothing_of_its_own).is_empty());
        assert_eq!(known.family_of_taken(&nothing_of_its_own), None);
        assert_eq!(known.family_of_answer(&nothing_of_its_own, false), None);
        let one_of_its_own = added(vec![BatchOp::InsertOnly {
            path: path(&[b"c"]),
            key: b"k".to_vec(),
            element: Element::new_item(vec![1]),
        }]);
        assert_eq!(puts(&one_of_its_own).len(), 2);
        assert_eq!(flat_drops(&one_of_its_own), vec![path(&[b"a", b"dropped"])]);
        assert_eq!(known.family_of_taken(&one_of_its_own), over);
        assert_eq!(known.family_of_answer(&one_of_its_own, false), None);
        let own_over = added(vec![BatchOp::Delete {
            path: path(&[b"b"]),
            key: b"over".to_vec(),
        }]);
        assert_eq!(known.family_of_answer(&own_over, false), over);
        // A put or a flat drop at a key that another operation of the
        // batch names as well is not counted: one can take the place of
        // the other. The operations still name the place.
        let twice = |second: BatchOp| {
            let put = BatchOp::InsertOrReplace {
                path: path(&[b"c"]),
                key: b"t".to_vec(),
                element: Element::new_item(vec![1]),
            };
            let drop = BatchOp::DropFlat {
                path: path(&[b"c"]),
                key: b"d".to_vec(),
                tree_type: grovedb::TreeType::NormalTree,
            };
            Op::PartialBatch(PartialBatch {
                ops: vec![put, drop],
                add_on: vec![second],
                pause_height: None,
            })
        };
        let elsewhere = twice(BatchOp::Delete {
            path: path(&[b"c"]),
            key: b"other".to_vec(),
        });
        assert_eq!(puts(&elsewhere), vec![path(&[b"c", b"t"])]);
        assert_eq!(flat_drops(&elsewhere), vec![path(&[b"c", b"d"])]);
        let over_the_put = twice(BatchOp::InsertIfNotExists {
            path: path(&[b"c"]),
            key: b"t".to_vec(),
            element: Element::new_item(vec![2]),
        });
        assert!(puts(&over_the_put).is_empty());
        assert_eq!(flat_drops(&over_the_put), vec![path(&[b"c", b"d"])]);
        assert!(places(&over_the_put, true).contains(&path(&[b"c", b"t"])));
        let over_the_drop = twice(BatchOp::Move {
            path: path(&[b"c"]),
            key: b"src".to_vec(),
            new_key: b"d".to_vec(),
        });
        assert_eq!(puts(&over_the_drop), vec![path(&[b"c", b"t"])]);
        assert!(flat_drops(&over_the_drop).is_empty());
        let both_puts = Op::Batch(vec![
            BatchOp::InsertOrReplace {
                path: path(&[b"c"]),
                key: b"t".to_vec(),
                element: Element::empty_tree(),
            },
            BatchOp::Replace {
                path: path(&[b"c"]),
                key: b"t".to_vec(),
                element: Element::new_item(vec![3]),
            },
        ]);
        assert!(puts(&both_puts).is_empty());

        let mut counts = Counts::default();
        let mut one = self::known();
        one.ended_at(Family::WrittenOver, 9);
        counts.add(&one.counts());
        counts.add(&one.counts());
        assert_eq!(counts.answers, [2, 0, 0, 0, 0, 0]);
        assert_eq!(counts.answer_seed[..4], [Some(9), None, None, None]);
        assert_eq!(counts.answer_differences(), 2);
        assert_eq!(counts.histories, [2, 2, 2, 0, 0, 0]);
        let lines = counts.lines();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with(
            "open difference, an element written over a tree that held elements: 2 cases made \
             it, 0 with stored records that differ passed over, 2 ended at an answer that \
             differs (seed 0x9)"
        ));
        assert!(Counts::default().lines().is_empty());
    }
}
