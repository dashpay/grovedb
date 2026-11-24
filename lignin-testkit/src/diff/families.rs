//! The open differences of the two engines that are ruled and named, and
//! what the fuzzer does where a case comes to one (GROVE-10).
//!
//! RocksDB leaves records behind where GroveDB changes one element and
//! does not clear what hung under it, and reads them again later. LigninDB
//! drops a subtree with its element, and refuses a write that would put
//! such records to use. Such histories are ruled as stated differences,
//! and are no defect of an engine. Each is a [`Family`]. Four have a rule
//! that is a path:
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
//! - [`Family::BelowPlainDelete`]: a batch's plain delete removed a
//!   tree that held elements, which a plain delete does not look at, and
//!   RocksDB kept a record under the tree's path. The family is the
//!   subtree at that path and every subtree below it, also once a tree
//!   is made at the path again. It is the narrowest of the four: of an
//!   answer it has a read alone, and of a read its result, its error and
//!   its cost ([`Known::family_of_answer`]). The read is one of a
//!   subtree at such a place: a read of one key by the subtree it reads
//!   in, so that a read of the removed tree's own element in the tree
//!   above is none, and a query by its own subtree and those below that
//!   it reads. A read that returns a proof or the root of an append-only
//!   tree as its result is none either, and a difference in it ends
//!   the run. A read that is of the family ends no case: it is counted
//!   and passed over, for a read changes nothing, and the case goes on
//!   to the tree that is made again and the writes into it. A write
//!   that names such a place, the root hash after it, and a proof with
//!   its cost are compared as anywhere else, and a difference there
//!   ends the run, but for the three kinds of a write below.
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
//! Of the same place a refused write is counted apart, by the owner's
//! ruling D-386:
//!
//! - [`Family::RefusedBelowPlainDelete`]: a write of elements into a
//!   subtree at or below such a place which both engines refuse. RocksDB
//!   gets further before it refuses, because it finds the records it
//!   kept, so the cost of the refusal differs, and on grove versions 4
//!   and 5 the error. Such a step is counted and passed over where the
//!   engines differ in the cost or the error and in nothing else, each
//!   engine has the root hash it had before the write, one at least of
//!   the subtrees the write names is at or below such a place, and
//!   every other is the subtree a removed tree was in, with what the
//!   caller of a partial batch adds among them; or, for a batch that
//!   also writes elsewhere, LigninDB's refusal is `InvalidPath` in
//!   GroveDB's words for no element above a subtree of the batch whose
//!   element is a record that a plain delete left, which the reference
//!   holds and LigninDB does not ([`Known::under_a_left_record`]):
//!   RocksDB opens that subtree by the kept record and refuses further
//!   on. At the next walk
//!   every stored record outside the family's places is compared; what
//!   is in no hash and inside a place is not
//!   ([`Known::family_of_refused_write`]).
//! - [`Family::TakenBelowPlainDelete`]: a tree is made again at such a
//!   path and a key under which the old tree held a tree is not written
//!   again. GroveDB reads an element by its key under the prefix of its
//!   subtree, so RocksDB finds the record it kept, opens the subtree
//!   under that key and takes a write into it, though no element of
//!   the tree made again leads there; LigninDB has no such subtree and
//!   refuses with `InvalidPath`. The groves differ from there, so the
//!   case ends there, counted apart (the owner's ruling D-390), where
//!   all of this holds ([`Known::named_by_a_kept_record_alone`],
//!   [`Known::family_of_taken_write`]): the operation is a direct
//!   insert ([`Write::Insert`]), a batch ([`Op::Batch`]) or a batch with
//!   the flags callbacks ([`Op::FlagsBatch`]), which the runner applies
//!   in the block the engines are read in, and no other kind, also no
//!   handover batch; one of the subtrees it writes into is such that,
//!   going down its path, the first key at which LigninDB answers that
//!   it reads no element is in a subtree at or below such a place, the
//!   reference held a node record under that key below the tree right
//!   before the plain delete that removed it, the case has not
//!   forgotten the record since, the reference holds a node record
//!   under the key now and LigninDB holds none; the reference took the
//!   write; LigninDB's answer is the error `InvalidPath`; and the
//!   refusal is about that subtree, which for a direct insert is the
//!   one it writes into and for a batch is the one LigninDB names, in
//!   GroveDB's words for a batch at a path whose element it cannot
//!   read. That the record is one the delete left is so taken from
//!   what the case did, and not from what the engines hold
//!   ([`Known::forgets`]): the case forgets a record where an operation
//!   that the reference took, or refused with another root hash than
//!   it had, names its key (a direct insert and an operation of a
//!   batch name one key; a direct delete names its key and what is
//!   below it; a move both of its keys and what is below them; any
//!   other direct write every key at or below the subtree it writes),
//!   and it forgets every record at a rollback of the block and at a
//!   commit that the reference refused. What the reference's own check
//!   answers in its block right after the write is kept with the seed
//!   and the step, and the case is not held to it. A write that one
//!   engine takes and the other refuses and that is of no family ends
//!   the run.
//!
//! One kind of a write is counted apart, by the owner's rulings D-382
//! and D-386, of a cost alone:
//!
//! - [`Family::KeptRecordsCost`]: on grove versions 4 and 5 a batch
//!   looks, before it removes a tree that holds elements by an
//!   operation that is to check backward references, at every element
//!   of the tree for one that takes part in such a reference
//!   (`grovedb/src/batch/backward_references.rs`, "A removal whose
//!   contents nothing in this batch reads is scanned";
//!   `GroveDb::backward_reference_participants`). The look walks the
//!   records under the prefix of the tree and of every tree in it that
//!   keeps a Merk, so RocksDB reads there the records it kept of a tree
//!   that a plain delete removed before, and is charged a seek and the
//!   bytes of each, and, where such a record is the element of a tree
//!   that keeps a Merk, a seek in the subtree it opens under it and
//!   the hash of that subtree's path (D-394), which is one hash call
//!   for every 64 bytes begun of the path's message.
//!   A step is of this family, and is counted and passed over, where
//!   all of this holds ([`Known::kept_below_a_removed_tree`],
//!   [`Known::family_of_cost`]): the grove version is 4 or 5; the
//!   operation is a batch that lists a plain delete, or a put that is
//!   made whatever the key holds, at a key it names once, of a path at,
//!   above or below a place of [`Family::BelowPlainDelete`] or of
//!   [`Family::WrittenOver`], whichever kept the records; at or
//!   below that path the reference holds, before the batch, a node
//!   record that the candidate does not hold; the engines differ in the
//!   cost of the batch and in no other part; and the costs differ in
//!   the seeks, the bytes loaded and the hash calls alone, the
//!   reference's being no smaller in any, and its hash calls the larger
//!   by no more than GroveDB charges for the paths of the subtrees
//!   under the records the reference alone holds there that are the
//!   element of a tree that keeps a Merk (D-403): a bound on the sum,
//!   within which a count that is off by another cause passes.
//!   The batch is a whole batch of checked
//!   operations ([`Op::Batch`] that lists no flat drop) which both
//!   engines took, the candidate reads at the path, before it, a tree
//!   that keeps a Merk and whose element names a root, which is the
//!   tree GroveDB looks through, and every record that the reference
//!   holds more is in a subtree at or below such a place, on the walk
//!   GroveDB makes: under the tree's prefix and that of every tree in
//!   it that keeps a Merk, not under a tree of flat records. The
//!   outcome, the bytes added, replaced and removed and the root hash
//!   are compared, and the hash calls within that bound. A cost that
//!   differs otherwise in a write that both engines take ends the run,
//!   or, where the write names a place of one of the first three
//!   families, the case.
//!
//! Three have no place, and are of an answer alone:
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
//! - [`Family::RootKeyLeftBehind`]: on grove versions 1 to 3 the delete
//!   of a tree that holds elements, by `GroveDb::delete` at the root
//!   path, leaves the root-key record of the root Merk naming the old
//!   root where the delete changes that root. RocksDB takes the next
//!   write of a node of the root Merk, and LigninDB refuses it. A case
//!   ends there, counted apart, where all of this holds
//!   ([`Known::trees_under_a_root_delete`],
//!   [`Known::family_of_root_key_refusal`]): the case is of one of those
//!   grove versions; both engines took, with the same answer, such a
//!   delete of a tree in which the reference read an element before it;
//!   the operation is a write of elements, which the reference takes;
//!   and the candidate answers `NotSupported` in the words of that
//!   refusal for the root Merk, whole, with a record that does not name
//!   the root. A put that both engines take after the delete shows that
//!   the record names the root, and ends what the delete began: a
//!   refusal after it is of no family until another such delete.
//!   Whether the delete changed the root is not asked of the
//!   reference: the words are the candidate's. Every answer before the
//!   refusal is compared, and the candidate's store is checked when the
//!   case has ended.
//!
//! A case keeps the paths it made so ([`Known`]), from the operations both
//! engines took. Of a batch it counts the operations by a rule about its
//! lists and not by what GroveDB applied: those the caller of a partial
//! batch adds last only where the batch has an operation of its own and
//! the reference took it. A put or a flat drop at a key that the batch
//! names twice makes no place of a family; the operations still name
//! that key where an answer is compared. A plain delete makes a place
//! where the reference still holds a record under the tree's path once
//! the batch is applied, as its block sees it: a batch that deletes what
//! the tree holds before the tree leaves none. Nothing else is of a
//! family:
//!
//! - **What is stored.** The records of a subtree of a family are not
//!   compared. Where the two engines hold different records there, the
//!   case counts the difference for the family and goes on.
//! - **An answer.** A result, an error, a cost, a root hash and a proof
//!   are compared at every step, also of a family. Where they differ at an
//!   operation that names a place of a family, the case ends there, is
//!   counted for the family, and is no divergence of the run. They differ
//!   there because the read takes the records under a prefix as they are.
//!   Below a plain delete that holds for a read alone, where the engines
//!   differ in its result, its error or its cost and in nothing else,
//!   and the case does not end: the read is counted and passed over.
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
//! families of an indexed tree over an indexed tree those in which the
//! reference took such a put, for that of the root-key record those in
//! which both engines took such a delete, those with a stored
//! difference passed over and those ended at an answer, and for the last
//! two the seed of one such case.

use std::collections::BTreeSet;

use grovedb_costs::OperationCost;
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
use grovedb_path::SubtreePath;
use grovedb_storage::prefix::build_prefix;
use grovedb_version::version::GroveVersion;

use grovedb::{operations::proof::util::hex_to_ascii, Element, GroveDb, SubqueryBranch};

use super::pair::Part;
use super::state::Record;
use super::state::{
    a_node_is_under, a_record_is_under, a_secondary_holds_a_row, node_keys_below,
    node_records_below, Keyspace, NodeKey,
};
use crate::golden::step::{Outcome, Proof, Proved, Step, Value, Verification};
use crate::workload::{
    BatchOp, Db, Op, Path, Prove, Read, StreamEngine, TransactionOp, TreeRead, Tx, Write,
};

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
    /// What is at and below the path of a tree that a batch's plain
    /// delete removed while it held elements: the reads there.
    BelowPlainDelete,
    /// A write after a delete at the root path that left the root-key
    /// record of the root Merk behind, on grove versions 1 to 3, which
    /// the candidate refuses.
    RootKeyLeftBehind,
    /// The cost of a whole batch of checked operations that removes or
    /// replaces a tree at or below which RocksDB holds records it kept,
    /// below a plain delete or below an element written over a tree, on
    /// grove versions 4 and 5: the seeks, the bytes loaded, and the
    /// hash calls, up to what the paths of the subtrees cost that the
    /// batch opens under such records.
    KeptRecordsCost,
    /// A write of elements at or below the path of a tree that a batch's
    /// plain delete removed while it held elements, which both engines
    /// refuse: the cost and the error of the refusal.
    RefusedBelowPlainDelete,
    /// A write of elements into a subtree at or below such a path that
    /// only a record RocksDB kept leads to, which RocksDB takes and the
    /// candidate refuses.
    TakenBelowPlainDelete,
}

/// How many families there are.
const FAMILIES: usize = Family::ALL.len();

/// Whether one of the remarks `said` is the refusal of a batch for no
/// element above one of `subtrees` ([`refuses_for_no_element_above`]).
fn says_no_element_above_one(said: &[String], subtrees: &[Path]) -> bool {
    subtrees.iter().any(|subtree| {
        subtree
            .split_last()
            .is_some_and(|(key, above)| said.contains(&refuses_for_no_element_above(above, key)))
    })
}

/// What an engine says where a batch writes into the subtree under `key`
/// in the subtree at `above` and no element is read there, as a remark of
/// the runner has it: GroveDB's words where it opens the Merk of a
/// batch's path by the element above it (`grovedb/src/batch/mod.rs`,
/// "could not get key for parent of subtree for batch").
fn refuses_for_no_element_above(above: &[Vec<u8>], key: &[u8]) -> String {
    let path: Vec<String> = above.iter().map(|segment| hex_to_ascii(segment)).collect();
    format!(
        "failed: invalid path: could not get key for parent of subtree for batch at path [{}] \
         for key {}",
        path.join("/"),
        hex_to_ascii(key)
    )
}

/// The trees a batch removes or replaces over records that the
/// reference kept ([`Known::kept_below_a_removed_tree`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeptBelow {
    /// The trees.
    pub trees: Vec<Path>,
    /// The hash calls GroveDB charges for the paths of the subtrees
    /// under the node records that the reference alone holds at or
    /// below them and that are the element of a tree which keeps a
    /// Merk: for each such subtree what `build_prefix` charges for its
    /// path, one call for every 64 bytes begun of the path's message.
    pub path_hashes: u64,
}

/// The first grove version on which a batch looks through a tree that
/// it removes for elements that take part in backward references.
const FIRST_VERSION_THAT_LOOKS_THROUGH_A_REMOVED_TREE: u32 = 4;

/// The most node records at and below a tree that
/// [`Known::kept_below_a_removed_tree`] lists of an engine.
const MOST_LISTED_BELOW: usize = 4096;

/// The last grove version on which GroveDB leaves the records of an
/// indexed tree where they are when an element is put over it.
const LAST_VERSION_THAT_LEAVES_THE_OLD_ROWS: u32 = 3;

impl Family {
    /// Every family.
    pub const ALL: [Family; 11] = [
        Family::WrittenOver,
        Family::BelowFlatDrop,
        Family::DroppedTree,
        Family::IndexedOverIndexed,
        Family::DroppedTreeProof,
        Family::InsertOnlyOverIndexed,
        Family::BelowPlainDelete,
        Family::RootKeyLeftBehind,
        Family::KeptRecordsCost,
        Family::RefusedBelowPlainDelete,
        Family::TakenBelowPlainDelete,
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
            Family::BelowPlainDelete => {
                "below a tree that a batch's plain delete removed while it held elements"
            }
            Family::RootKeyLeftBehind => {
                "a write after a delete at the root path that left the root-key record behind, \
                 grove versions 1 to 3"
            }
            Family::KeptRecordsCost => {
                "the cost of a checked batch that removes or replaces a tree over records that \
                 RocksDB kept, grove versions 4 and 5"
            }
            Family::RefusedBelowPlainDelete => {
                "a write that both engines refuse at or below a tree that a batch's plain delete \
                 removed while it held elements"
            }
            Family::TakenBelowPlainDelete => {
                "a write below a tree that a batch's plain delete removed, into a subtree that \
                 only a record RocksDB kept leads to, which RocksDB takes and LigninDB refuses"
            }
        }
    }

    /// Whether a case ends at an answer of the family. Below a plain
    /// delete it does not: the answer is of a read, which changes
    /// nothing, or of a write that differs in what it was charged for
    /// reading and in nothing it wrote, and is passed over.
    pub fn ends_a_case(self) -> bool {
        !matches!(
            self,
            Family::BelowPlainDelete | Family::KeptRecordsCost | Family::RefusedBelowPlainDelete
        )
    }

    fn index(self) -> usize {
        match self {
            Family::WrittenOver => 0,
            Family::BelowFlatDrop => 1,
            Family::DroppedTree => 2,
            Family::IndexedOverIndexed => 3,
            Family::DroppedTreeProof => 4,
            Family::InsertOnlyOverIndexed => 5,
            Family::BelowPlainDelete => 6,
            Family::RootKeyLeftBehind => 7,
            Family::KeptRecordsCost => 8,
            Family::RefusedBelowPlainDelete => 9,
            Family::TakenBelowPlainDelete => 10,
        }
    }
}

/// What a run met of the families, by [`Family::ALL`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// The cases that made a place of the family: an element put over a
    /// tree that held elements, a flat drop of a tree, whatever it
    /// held, or a batch's plain delete of a tree that left a record
    /// under its path; for the two families of an indexed tree over an
    /// indexed tree the cases in which the reference took such a put,
    /// whatever the candidate answered; for the root-key record left
    /// behind the cases in which both engines took a delete at the root
    /// path of a tree that held elements, whether or not it changed the
    /// root; and for a dropped tree through a proof and for the cost of
    /// a batch that removes a tree made again below a plain delete,
    /// which are counted apart, the cases that came to such a step.
    pub histories: [u64; FAMILIES],
    /// The cases in which the engines stored different records in a
    /// subtree of the family, which were passed over.
    pub stored: [u64; FAMILIES],
    /// The cases that ended at an answer the engines gave differently at
    /// an operation that names a place of the family. For the family
    /// below a plain delete, the cases with a read that the engines
    /// answered differently there, which was passed over and ended no
    /// case, and for the cost of a batch that removes a tree made again
    /// there, the cases with such a cost, passed over likewise.
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

    /// The cases with an answer of a family, a case counted once for
    /// each family it has one of: those that ended there, and for a
    /// family at whose answers no case ends ([`Family::ends_a_case`])
    /// those that passed one over.
    pub fn answer_differences(&self) -> u64 {
        self.answers.iter().sum()
    }

    /// The cases with an answer of `family`.
    pub fn answers_of(&self, family: Family) -> u64 {
        self.answers[family.index()]
    }

    /// Whether something was passed over: stored records that differ, or
    /// an answer of a family at whose answers no case ends.
    pub fn passed_over(&self) -> bool {
        let passed = |family: &Family| !family.ends_a_case() && self.answers_of(*family) > 0;
        self.stored_differences() > 0 || Family::ALL.iter().any(passed)
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
                let answers = match family {
                    Family::BelowPlainDelete => "with a read that differs passed over",
                    Family::KeptRecordsCost => "with such a cost passed over",
                    Family::RefusedBelowPlainDelete => "with such a refusal passed over",
                    _ => "ended at an answer that differs",
                };
                format!(
                    "open difference, {}: {} cases made it, {} with stored records that differ \
                     passed over{}, {} {answers}{}",
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

/// The words of LigninDB's refusal of a write of a node of a Merk whose
/// root-key record does not name its root, after the keys it names
/// (`RootLeftBehind::refusal` of `merk/src/store/lignin/mod.rs`).
const REFUSAL_OF_A_ROOT_KEY_LEFT_BEHIND: &str =
    ": no node of a Merk is written where its root-key record was read and does not name its \
     root. On grove versions 1 to 3 the delete of a tree that holds elements leaves the record \
     so where it changes the root of the Merk the tree was in \
     (grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs, GroveDB #686 and #1065)";

/// Whether `remark` is, whole, the message the corpus runner records
/// where LigninDB refuses a write of a node of the root Merk because its
/// root-key record does not name its root: the record absent or naming
/// a key, and the root under another key, each in lowercase hex as the
/// refusal writes it.
fn refuses_for_the_root_key_left_behind(remark: &str) -> bool {
    let root_merk = hex::encode(build_prefix(SubtreePath::from(&[] as &[&[u8]])).unwrap());
    let opening =
        format!("failed: not supported: lignindb: the root-key record of Merk {root_merk} ");
    let a_key = |rest: &'_ str| -> Option<usize> {
        let hex = |byte: &u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        let digits = rest.bytes().take_while(hex).count();
        (digits % 2 == 0).then_some(digits)
    };
    let parsed = || {
        let rest = remark.strip_prefix(&opening)?;
        let (record, rest) = match rest.strip_prefix("is absent") {
            Some(rest) => (None, rest),
            None => {
                let rest = rest.strip_prefix("names the key ")?;
                let (key, rest) = rest.split_at(a_key(rest)?);
                (Some(key), rest)
            }
        };
        let rest = rest.strip_prefix(", and the root of that Merk is the node under the key ")?;
        let (root, rest) = rest.split_at(a_key(rest)?);
        Some(record != Some(root) && rest == REFUSAL_OF_A_ROOT_KEY_LEFT_BEHIND)
    };
    parsed().unwrap_or(false)
}

/// Whether `op` is a write of elements: a direct write other than of an
/// aux record, or a batch of any kind.
fn writes_elements(op: &Op) -> bool {
    match op {
        Op::Write(Write::PutAux { .. } | Write::DeleteAux { .. }) => false,
        Op::Write(_)
        | Op::Batch(_)
        | Op::PartialBatch(_)
        | Op::FlagsBatch(_)
        | Op::FlagsPartialBatch(_)
        | Op::FlagsHandoverBatch(_) => true,
        Op::Read(_) | Op::Commit | Op::FlushPendingDrops | Op::Transaction(_) => false,
    }
}

/// The places of the families that one case has made.
#[derive(Clone, Debug, Default)]
pub struct Known {
    over: Vec<Path>,
    /// The trees that a batch's plain delete removed while they held
    /// elements, a record of which the reference kept.
    plain_deletes: Vec<Path>,
    /// The node records the reference held at or below such a tree
    /// right before the plain delete that removed it, less every one at
    /// whose key the case has written since ([`Known::left_records`],
    /// [`Known::forgets`]): the records that the delete left with no
    /// element leading to them, by what the case did and not by what an
    /// engine holds now.
    left_records: BTreeSet<NodeKey>,
    /// Whether both engines took a delete at the root path of a tree
    /// that held elements, on a grove version that can leave the
    /// root-key record behind there, and no write since that both took
    /// and that changed the root hash.
    root_delete: bool,
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

/// The trees a batch `op` removes by a plain delete, as the families
/// count them for an operation the reference takes ([`counted`],
/// [`named_once`]).
fn plain_deletes(op: &Op) -> Vec<Path> {
    let mut deletes = Vec::new();
    for batch_op in counted(op, true) {
        if let BatchOp::Delete { path, key } = batch_op {
            deletes.push(joined(path, key));
        }
    }
    deletes.retain(|place| named_once(op, place));
    deletes
}

/// Whether `transaction` reads in `db` a tree at `place` that holds an
/// element.
fn holds_elements<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    place: &Path,
    grove_version: &GroveVersion,
) -> bool {
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
}

/// The subtrees `op` writes elements into: of a direct write the subtree
/// it names, and of a batch that of each operation the families count
/// for it, where `taken` says whether the reference took it
/// ([`counted`]).
fn written_subtrees(op: &Op, taken: bool) -> Vec<Path> {
    let mut subtrees = Vec::new();
    if let Op::Write(write) = op {
        match write {
            Write::Insert { path, .. }
            | Write::Delete { path, .. }
            | Write::ClearSubtree { path }
            | Write::Append { path, .. }
            | Write::DropFlat { path, .. }
            | Write::InsertIfNotExists { path, .. }
            | Write::InsertIfChanged { path, .. }
            | Write::DeleteIfEmptyTree { path, .. }
            | Write::DeleteUpTree { path, .. }
            | Write::DeleteBuilt { path, .. }
            | Write::IndexedInsert { path, .. }
            | Write::IndexedDelete { path, .. }
            | Write::ReconcileIndexed { path }
            | Write::CommitmentInsert { path, .. }
            | Write::SectionalDelete { path, .. }
            | Write::SectionalDeleteUpTree { path, .. }
            | Write::ConvertToBigSumTree { path, .. } => subtrees.push(path.clone()),
            Write::PutAux { .. } | Write::DeleteAux { .. } => {}
        }
    }
    subtrees.extend(counted(op, taken).map(|batch_op| batch_op.path().clone()));
    subtrees
}

/// Whether `transaction` reads in `db` a tree at `place` that keeps a
/// Merk and whose element names a root: the tree a batch of grove
/// version 4 or 5 looks through before it removes it.
fn names_a_root_in_a_merk<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    place: &Path,
    grove_version: &GroveVersion,
) -> bool {
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
    element.is_ok_and(|tree| {
        let root = tree.root_key_and_tree_type();
        !tree.uses_non_merk_data_storage() && root.is_some_and(|(root, _)| root.is_some())
    })
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
        let mut trees = puts(op);
        trees.retain(|place| holds_elements(db, transaction, place, grove_version));
        trees
    }

    /// The trees that hold elements, as `transaction` reads `db`, among
    /// those that a batch `op` removes by a plain delete: asked before
    /// the operation is applied.
    pub fn trees_under_a_plain_delete<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Vec<Path> {
        let mut trees = plain_deletes(op);
        trees.retain(|place| holds_elements(db, transaction, place, grove_version));
        trees
    }

    /// Those of `trees` under whose path `transaction` reads a record in
    /// `db` ([`a_record_is_under`]): asked of the reference once a batch
    /// that removed them by a plain delete
    /// ([`Known::trees_under_a_plain_delete`]) is applied, and handed to
    /// [`Known::took`]. Where the reference cannot be read, none.
    pub fn left_a_record<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        mut trees: Vec<Path>,
    ) -> Vec<Path> {
        trees.retain(|tree| a_record_is_under(db, transaction, tree).is_ok_and(|left| left));
        trees
    }

    /// The trees that `op`, a whole batch of checked operations
    /// ([`Op::Batch`] that lists no flat drop), removes by a plain
    /// delete or replaces by a put that is made whatever the key holds,
    /// on a grove version from the fourth on, where `candidate` reads a
    /// tree that keeps a Merk and whose element names a root, and at or
    /// below which `reference` holds node records that `candidate` does
    /// not hold, each as its block sees it ([`node_keys_below`]), every
    /// one of them in a subtree at or below a place where RocksDB kept
    /// records (of [`Family::BelowPlainDelete`] or
    /// [`Family::WrittenOver`]): asked before the operation is applied.
    /// A tree at or below which an engine holds
    /// more than [`MOST_LISTED_BELOW`] records, or cannot be read, is
    /// none. A batch of operations that declare no check looks through
    /// nothing before it removes a tree, and has none; a partial batch
    /// has none either, though its first segment looks as a whole batch
    /// does: its cost is of no family. With the trees, what the paths of the subtrees cost in
    /// hash calls that the look opens under the records which the
    /// reference alone holds at or below them: under each that is the
    /// element of a tree that keeps a Merk ([`KeptBelow::path_hashes`]).
    pub fn kept_below_a_removed_tree<'db, A: StreamEngine, B: StreamEngine>(
        &self,
        reference: (&'db Db<A>, &Tx<'db, A>),
        candidate: (&'db Db<B>, &Tx<'db, B>),
        op: &Op,
        grove_version: &GroveVersion,
    ) -> KeptBelow {
        // A flat drop is the one operation of such a batch that declares
        // no check: a batch that lists one is not a batch of checked
        // operations.
        let checked = |batch_op: &BatchOp| !matches!(batch_op, BatchOp::DropFlat { .. });
        let checked_and_whole = matches!(op, Op::Batch(_)) && op.batch_ops().all(checked);
        let no_place = self.plain_deletes.is_empty() && self.over.is_empty();
        if grove_version.protocol_version < FIRST_VERSION_THAT_LOOKS_THROUGH_A_REMOVED_TREE
            || !checked_and_whole
            || no_place
        {
            return KeptBelow::default();
        }
        let mut trees = plain_deletes(op);
        trees.extend(puts(op));
        trees.retain(|tree| {
            let (db, transaction) = candidate;
            names_a_root_in_a_merk(db, transaction, tree, grove_version)
        });
        // A record is one that a plain delete or an element written
        // over a tree left where its subtree is at or below the place
        // of one.
        let kept = |(subtree, _): &&NodeKey| {
            let mut places = self.plain_deletes.iter().chain(&self.over);
            places.any(|place| subtree.starts_with(place))
        };
        // The records the reference alone holds that are the element of
        // a tree that keeps a Merk, each once though two of the trees
        // hold it.
        let mut merks = BTreeSet::new();
        trees.retain(|tree| {
            let most = MOST_LISTED_BELOW;
            let was = node_records_below(reference.0, reference.1, tree, most, grove_version);
            let is = node_keys_below(candidate.0, candidate.1, tree, most, grove_version);
            match (was, is) {
                (Ok(Some(was)), Ok(Some(is))) => {
                    let more: Vec<&NodeKey> = was.keys().filter(|key| !is.contains(*key)).collect();
                    let counts = !more.is_empty() && more.iter().all(kept);
                    if counts {
                        merks.extend(more.into_iter().filter(|key| was[*key]).cloned());
                    }
                    counts
                }
                _ => false,
            }
        });
        let path_hashes = merks.iter().map(|(subtree, key)| {
            let opened = joined(subtree, key);
            let segments: Vec<&[u8]> = opened.iter().map(Vec::as_slice).collect();
            let prefix = build_prefix(SubtreePath::from(segments.as_slice()));
            u64::from(prefix.cost.hash_node_calls)
        });
        KeptBelow {
            path_hashes: path_hashes.sum(),
            trees,
        }
    }

    /// The family of a differing answer to a batch that removes the
    /// trees `kept` ([`Known::kept_below_a_removed_tree`]), which the
    /// engines answered with `reference` and `candidate` and differently
    /// in `parts`: [`Family::KeptRecordsCost`] where there is such a
    /// tree, both engines took the batch, they differ in the cost and in
    /// no other part, and the costs differ in the seeks, the bytes
    /// loaded and the hash calls alone, the reference's being no smaller
    /// in any, and its hash calls the larger by no more than the paths
    /// of the subtrees cost that the look opens under the records it
    /// alone holds ([`KeptBelow::path_hashes`]; D-394, D-403). None
    /// otherwise.
    pub fn family_of_cost(
        kept: &KeptBelow,
        reference: &Step,
        candidate: &Step,
        parts: &[Part],
    ) -> Option<Family> {
        let (was, is) = (&reference.cost, &candidate.cost);
        let read_alone = OperationCost {
            seek_count: was.seek_count,
            storage_loaded_bytes: was.storage_loaded_bytes,
            hash_node_calls: was.hash_node_calls,
            ..is.clone()
        } == *was;
        let more =
            was.seek_count >= is.seek_count && was.storage_loaded_bytes >= is.storage_loaded_bytes;
        let opened = was.hash_node_calls.checked_sub(is.hash_node_calls);
        let opened = opened.is_some_and(|hashes| u64::from(hashes) <= kept.path_hashes);
        let taken = matches!(reference.outcome, Outcome::Returned(_))
            && matches!(candidate.outcome, Outcome::Returned(_));
        (!kept.trees.is_empty() && taken && parts == [Part::Cost] && read_alone && more && opened)
            .then_some(Family::KeptRecordsCost)
    }

    /// The subtrees that `op`, a batch or a batch with the flags
    /// callbacks, writes elements into ([`written_subtrees`]) and whose
    /// own element is a record that a plain delete left: the record
    /// under the subtree's key in the subtree above is one the case
    /// remembers ([`Known::left_records`], [`Known::forgets`]), which
    /// `reference` holds and `candidate` does not. Asked before the
    /// operation is applied. The tree that the delete removed need not
    /// have been made again: GroveDB opens the Merk of a batch's path by
    /// the element right above it and by nothing higher.
    pub fn under_a_left_record<'db, A: StreamEngine, B: StreamEngine>(
        &self,
        reference: (&'db Db<A>, &Tx<'db, A>),
        candidate: (&'db Db<B>, &Tx<'db, B>),
        op: &Op,
    ) -> Vec<Path> {
        if self.left_records.is_empty() || !matches!(op, Op::Batch(_) | Op::FlagsBatch(_)) {
            return Vec::new();
        }
        let mut subtrees = written_subtrees(op, true);
        subtrees.sort();
        subtrees.dedup();
        subtrees.retain(|subtree| {
            let Some((key, above)) = subtree.split_last() else {
                return false;
            };
            self.left_records.contains(&(above.to_vec(), key.clone()))
                && a_node_is_under(reference.0, reference.1, above, key) == Ok(true)
                && a_node_is_under(candidate.0, candidate.1, above, key) == Ok(false)
        });
        subtrees
    }

    /// The family of a differing answer to `op` which the engines
    /// answered with `reference` and `candidate` and differently in
    /// `parts`: [`Family::RefusedBelowPlainDelete`] where `op` is a write
    /// of elements, both engines refused it, they differ in the cost or
    /// the error and in no other part, with `unchanged` the root hash of
    /// each engine after it is that engine's own from before it, and
    /// one of two holds.
    ///
    /// - One at least of the subtrees it writes into
    ///   ([`written_subtrees`]) is at or below a place of
    ///   [`Family::BelowPlainDelete`] and every other is the subtree such
    ///   a removed tree was in. The subtrees are those of every
    ///   operation the batch lists, with what the caller of a partial
    ///   batch adds: how far a refused batch came is not known.
    /// - `op` is a batch or a batch with the flags callbacks, and the
    ///   candidate's answer is `InvalidPath` with, among its remarks
    ///   `said`, GroveDB's words for no element above one of
    ///   `under_a_left_record` ([`Known::under_a_left_record`]): the
    ///   candidate refused at a subtree that the reference opens by a
    ///   record it kept, and the reference refused further on, at
    ///   whatever else the batch asks.
    ///
    /// None otherwise: a write that one engine takes is of no family
    /// here.
    #[allow(clippy::too_many_arguments)]
    pub fn family_of_refused_write(
        &self,
        op: &Op,
        reference: &Outcome,
        candidate: &Outcome,
        parts: &[Part],
        unchanged: bool,
        under_a_left_record: &[Path],
        said: &[String],
    ) -> Option<Family> {
        let refused = |outcome: &Outcome| matches!(outcome, Outcome::Failed(_));
        let of_the_refusal = |part: &Part| matches!(part, Part::Cost | Part::Error);
        let below = |path: &Path| self.plain_deletes.iter().any(|tree| path.starts_with(tree));
        let above = |path: &Path| {
            let was_in = |tree: &Path| tree.split_last().is_some_and(|(_, above)| above == path);
            self.plain_deletes.iter().any(was_in)
        };
        let subtrees = written_subtrees(op, true);
        let within =
            subtrees.iter().any(below) && subtrees.iter().all(|path| below(path) || above(path));
        let at_a_left_record = matches!(op, Op::Batch(_) | Op::FlagsBatch(_))
            && matches!(candidate, Outcome::Failed(error) if error == "InvalidPath")
            && says_no_element_above_one(said, under_a_left_record);
        (writes_elements(op)
            && refused(reference)
            && refused(candidate)
            && unchanged
            && !parts.is_empty()
            && parts.iter().all(of_the_refusal)
            && (within || at_a_left_record))
            .then_some(Family::RefusedBelowPlainDelete)
    }

    /// The subtrees that `op` writes elements into ([`written_subtrees`])
    /// and that only a record which a plain delete left names. `op` is a
    /// direct insert ([`Write::Insert`]), a batch ([`Op::Batch`]) or a
    /// batch with the flags callbacks ([`Op::FlagsBatch`]), which the
    /// runner applies in the block that `reference` and `candidate` are
    /// read in; a partial batch, a handover batch and every other
    /// operation have none. Asked before the operation is applied.
    ///
    /// A subtree is so named where, going down its path, the first key
    /// at which `candidate` reads no element (a read that answers that
    /// there is none: a read that fails says nothing) is in a subtree at
    /// or below a place of [`Family::BelowPlainDelete`], the node record
    /// under that key is one the reference held below a tree right
    /// before the plain delete that removed it and that the case has
    /// not forgotten since ([`Known::left_records`], [`Known::forgets`]),
    /// `reference` holds a node record under the key now, and
    /// `candidate` holds none. GroveDB reads an element by its key under
    /// the prefix of its subtree, so RocksDB finds such a record though
    /// no element of the tree made again leads to it.
    pub fn named_by_a_kept_record_alone<'db, A: StreamEngine, B: StreamEngine>(
        &self,
        reference: (&'db Db<A>, &Tx<'db, A>),
        candidate: (&'db Db<B>, &Tx<'db, B>),
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Vec<Path> {
        let in_the_block = matches!(
            op,
            Op::Write(Write::Insert { .. }) | Op::Batch(_) | Op::FlagsBatch(_)
        );
        if self.left_records.is_empty() {
            return Vec::new();
        }
        let mut subtrees = written_subtrees(op, true);
        subtrees.sort();
        subtrees.dedup();
        let named = |subtree: &Path| {
            for depth in 1..=subtree.len() {
                let (key, above) = subtree[..depth].split_last().expect("a key");
                let segments: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
                let (db, transaction) = candidate;
                let read = db.get_raw_optional(
                    segments.as_slice().into(),
                    key,
                    Some(transaction),
                    grove_version,
                );
                match read.value {
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        let below = |tree: &Path| above.starts_with(tree);
                        let left = (above.to_vec(), key.clone());
                        return self.plain_deletes.iter().any(below)
                            && self.left_records.contains(&left)
                            && a_node_is_under(reference.0, reference.1, above, key) == Ok(true)
                            && a_node_is_under(db, transaction, above, key) == Ok(false);
                    }
                    Err(_) => return false,
                }
            }
            false
        };
        match in_the_block {
            true => subtrees.into_iter().filter(named).collect(),
            false => Vec::new(),
        }
    }

    /// The family of a differing answer to `op`, a write of elements, of
    /// whose subtrees `named` are named by a record that a plain delete
    /// left and by nothing else ([`Known::named_by_a_kept_record_alone`]),
    /// which the engines answered with `reference` and `candidate`, the
    /// candidate with the remarks `said`:
    /// [`Family::TakenBelowPlainDelete`] where the reference took the
    /// write, the candidate's answer is the error `InvalidPath`, and the
    /// refusal is about such a subtree. For a direct insert that is the
    /// one subtree it writes into. For a batch, which may write into
    /// other subtrees too, the candidate must say so, in GroveDB's words
    /// for a batch at a path whose element it cannot read, with the path
    /// and the key of such a subtree ([`refuses_for_no_element_above`]):
    /// a refusal in other words, or for another path, is of no family
    /// here. None otherwise.
    pub fn family_of_taken_write(
        op: &Op,
        named: &[Path],
        reference: &Outcome,
        candidate: &Outcome,
        said: &[String],
    ) -> Option<Family> {
        let taken = matches!(reference, Outcome::Returned(_));
        let refused = matches!(candidate, Outcome::Failed(error) if error == "InvalidPath");
        let about_one = match op {
            Op::Write(Write::Insert { path, .. }) => named.contains(path),
            Op::Batch(_) | Op::FlagsBatch(_) => says_no_element_above_one(said, named),
            _ => false,
        };
        (taken && refused && about_one).then_some(Family::TakenBelowPlainDelete)
    }

    /// Whether `op` is a delete by `GroveDb::delete` at the root path, on
    /// a grove version up to the third, of a tree in which `transaction`
    /// reads an element in `db`: asked before the operation is applied,
    /// and handed to [`Known::took`] once both engines took it. Such a
    /// delete leaves the root-key record of the root Merk behind where
    /// it changes that Merk's root, which is not asked here.
    pub fn trees_under_a_root_delete<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> bool {
        if grove_version.protocol_version > LAST_VERSION_THAT_LEAVES_THE_OLD_ROWS {
            return false;
        }
        let Op::Write(Write::Delete { path, key } | Write::SectionalDelete { path, key }) = op
        else {
            return false;
        };
        path.is_empty() && holds_elements(db, transaction, &joined(path, key), grove_version)
    }

    /// The family of a differing answer to `op` which the reference gave
    /// as `reference` and the candidate as `candidate`, with the
    /// messages `said`: [`Family::RootKeyLeftBehind`] where the case has
    /// a delete at the root path of a tree that held elements
    /// ([`Known::trees_under_a_root_delete`]) which both engines took,
    /// with no write since that both took and that changed the root
    /// hash ([`Known::wrote`]), `op` is a
    /// write of elements, the reference took it, the candidate's answer
    /// is the error `NotSupported`, and one of its messages is, whole,
    /// the refusal of a write of a node of the root Merk whose root-key
    /// record does not name its root. None otherwise.
    pub fn family_of_root_key_refusal(
        &self,
        op: &Op,
        reference: &Outcome,
        candidate: &Outcome,
        said: &[String],
    ) -> Option<Family> {
        let taken = matches!(reference, Outcome::Returned(_));
        let refused = matches!(candidate, Outcome::Failed(error) if error == "NotSupported");
        let in_those_words = || {
            said.iter()
                .any(|remark| refuses_for_the_root_key_left_behind(remark))
        };
        (self.root_delete && writes_elements(op) && taken && refused && in_those_words())
            .then_some(Family::RootKeyLeftBehind)
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

    /// Both engines took `op` and answered it alike: its flat drops, the
    /// trees it put an element over ([`Known::trees_under`]) and the
    /// trees `plain_deletes` that it removed by a plain delete and left
    /// a record of ([`Known::left_a_record`]) are places of their
    /// families from here on, and with `root_delete` the case has a
    /// delete at the root path of a tree that held elements
    /// ([`Known::trees_under_a_root_delete`]).
    pub fn took(&mut self, op: &Op, over: Vec<Path>, plain_deletes: Vec<Path>, root_delete: bool) {
        if !over.is_empty() {
            self.counts.histories[Family::WrittenOver.index()] = 1;
        }
        self.over.extend(over);
        if !plain_deletes.is_empty() {
            self.counts.histories[Family::BelowPlainDelete.index()] = 1;
        }
        self.plain_deletes.extend(plain_deletes);
        if root_delete {
            self.counts.histories[Family::RootKeyLeftBehind.index()] = 1;
            self.root_delete = true;
        }
        let drops = flat_drops(op);
        if !drops.is_empty() {
            self.counts.histories[Family::BelowFlatDrop.index()] = 1;
            self.counts.histories[Family::DroppedTree.index()] = 1;
        }
        self.flat_drops.extend(drops);
    }

    /// The node records `db` holds at or below `trees` as `transaction`
    /// sees them: asked of the reference before an operation for the
    /// trees its plain deletes remove ([`Known::trees_under_a_plain_delete`]),
    /// and handed to [`Known::left_records`] once both engines took it.
    /// A tree with more than [`MOST_LISTED_BELOW`] records, or one that
    /// cannot be read, gives none.
    pub fn records_below<'db, E: StreamEngine>(
        db: &'db Db<E>,
        transaction: &Tx<'db, E>,
        trees: &[Path],
        grove_version: &GroveVersion,
    ) -> Vec<NodeKey> {
        let mut records = Vec::new();
        for tree in trees {
            let most = MOST_LISTED_BELOW;
            if let Ok(Some(below)) = node_keys_below(db, transaction, tree, most, grove_version) {
                records.extend(below);
            }
        }
        records
    }

    /// Both engines took an operation before which the reference held
    /// `records` below the trees its plain deletes removed
    /// ([`Known::records_below`]): those at or below a place of
    /// [`Family::BelowPlainDelete`] are records that a plain delete
    /// left. Called after [`Known::took`], which makes the places.
    pub fn left_records(&mut self, records: Vec<NodeKey>) {
        let places = &self.plain_deletes;
        let left = |(subtree, _): &NodeKey| places.iter().any(|tree| subtree.starts_with(tree));
        self.left_records.extend(records.into_iter().filter(left));
    }

    /// The case came to `op`, which the reference took (`taken`) or
    /// refused, with (`root_changed`) or without another root hash than
    /// it had: a record that a plain delete left is no longer one where
    /// the operation can have put an element under its key, so that a
    /// record written since is never taken for one that was left.
    ///
    /// - A rollback of the block, which can undo the delete itself,
    ///   forgets every record, whatever it was answered, and so does a
    ///   commit that the reference refused, after which the runner
    ///   begins a new block on the committed state.
    /// - A write that the reference refused and that left its root hash
    ///   as it was put no element that an element leads to, and forgets
    ///   nothing.
    /// - Any other direct insert forgets the record under the key it
    ///   names. A direct delete forgets the record under its key and
    ///   every record below that key. Any other direct write forgets
    ///   every record at or below the subtree it writes into.
    /// - An operation of a batch, also one that the caller of a partial
    ///   batch adds, forgets the record under the key it names; a move
    ///   forgets every record at or below either of its two keys, since
    ///   it puts a subtree with what it holds under the new one; and an
    ///   operation with no key forgets every record at or below its
    ///   path.
    ///
    /// Called before [`Known::left_records`].
    pub fn forgets(&mut self, op: &Op, taken: bool, root_changed: bool) {
        let at_or_below = |records: &mut BTreeSet<NodeKey>, path: &Path| {
            records.retain(|(subtree, key)| !joined(subtree, key).starts_with(path));
        };
        let records = &mut self.left_records;
        let wrote = taken || root_changed;
        match op {
            Op::Read(_) | Op::FlushPendingDrops => {}
            Op::Commit if taken => {}
            Op::Commit => records.clear(),
            Op::Transaction(
                TransactionOp::Rollback
                | TransactionOp::Abandon
                | TransactionOp::RollbackToSavepoint,
            ) => records.clear(),
            Op::Transaction(_) => {}
            _ if !wrote => {}
            Op::Write(Write::Insert { path, key, .. }) => {
                records.remove(&(path.clone(), key.clone()));
            }
            Op::Write(Write::Delete { path, key }) => at_or_below(records, &joined(path, key)),
            Op::Write(_) => {
                for path in written_subtrees(op, true) {
                    at_or_below(records, &path);
                }
            }
            Op::Batch(_)
            | Op::PartialBatch(_)
            | Op::FlagsBatch(_)
            | Op::FlagsPartialBatch(_)
            | Op::FlagsHandoverBatch(_) => {
                for batch_op in op.batch_ops() {
                    let keyed = keyed(batch_op);
                    if keyed.is_empty() {
                        at_or_below(records, batch_op.path());
                    }
                    let moved = matches!(batch_op, BatchOp::Move { .. });
                    for place in keyed {
                        match place.split_last() {
                            Some(_) if moved => at_or_below(records, &place),
                            Some((key, above)) => {
                                records.remove(&(above.to_vec(), key.clone()));
                            }
                            None => {}
                        }
                    }
                }
            }
        }
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
        if self.flat_drops.iter().any(below) {
            return Some(Family::BelowFlatDrop);
        }
        let removed = |tree: &Path| path.starts_with(tree);
        (self.plain_deletes.iter())
            .any(removed)
            .then_some(Family::BelowPlainDelete)
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
        if (self.flat_drops.iter()).any(|drop| read(drop) == Some(true)) {
            return Some(Family::BelowFlatDrop);
        }
        (self.plain_deletes.iter())
            .any(|tree| read(tree).is_some())
            .then_some(Family::BelowPlainDelete)
    }

    /// The family an answer to `op` that differs in `parts` is of, where
    /// `taken` says whether the reference took the operation: that of
    /// the first place the operation names that is of one ([`places`]),
    /// and else, for a query, that of a subtree below its own that it
    /// reads. Where that is [`Family::BelowPlainDelete`], the answer is
    /// of it only where the operation is a read and the engines differ
    /// in its result, its error or its cost and in no other part: else
    /// it is of no family.
    pub fn family_of_answer(&self, op: &Op, taken: bool, parts: &[Part]) -> Option<Family> {
        let family = self.family_named(op, taken)?;
        if family != Family::BelowPlainDelete {
            return Some(family);
        }
        let (Op::Read(read)
        | Op::Transaction(TransactionOp::SnapshotRead(read) | TransactionOp::BlockRead(read))) = op
        else {
            return None;
        };
        let of_the_read = |part: &Part| matches!(part, Part::Result | Part::Error | Part::Cost);
        (parts.iter().all(of_the_read) && self.reads_below_a_plain_delete(read)).then_some(family)
    }

    /// Whether `read` takes the records of a subtree at or below a plain
    /// delete as they are, and returns neither a proof nor the root of
    /// an append-only tree as its result: a read of one key in such a
    /// subtree, of whether such a tree is empty, of an append-only tree
    /// there, or a query of such a subtree or of one above that reads
    /// one below its own ([`reads`]). A read of a key in the tree above
    /// the removed tree, also of the removed tree's own key, reads no
    /// such subtree.
    fn reads_below_a_plain_delete(&self, read: &Read) -> bool {
        let below = |path: &Path| self.plain_deletes.iter().any(|tree| path.starts_with(tree));
        match read {
            Read::Prove(_)
            | Read::Tree {
                read: TreeRead::MmrRootHash | TreeRead::DenseRootHash | TreeRead::CommitmentAnchor,
                ..
            }
            | Read::GetAux { .. }
            | Read::AuxByPrefix { .. } => false,
            Read::Get { path, .. }
            | Read::HasRaw { path, .. }
            | Read::Fetch { path, .. }
            | Read::IsEmptyTree { path }
            | Read::Tree { path, .. } => below(path),
            Read::SumBudget { query, .. } => below(&query.path),
            Read::Query(_)
            | Read::PathQuery { .. }
            | Read::Rows { .. }
            | Read::KeysOptional { .. }
            | Read::ItemValues { .. }
            | Read::SumValues { .. }
            | Read::BigSum { .. }
            | Read::Many { .. } => queries(read).iter().any(|query| {
                let goes_into = |tree: &Path| {
                    let down = tree.strip_prefix(query.path.as_slice());
                    down.is_some_and(|down| reads(&query.query.query, down, false).is_some())
                };
                below(&query.path) || self.plain_deletes.iter().any(goes_into)
            }),
        }
    }

    /// The family of the first place `op` names that is of one, and else,
    /// for a query, that of a subtree below its own that it reads.
    fn family_named(&self, op: &Op, taken: bool) -> Option<Family> {
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

    /// The case with the seed `seed` has an answer of `family`, one at
    /// which no case ends ([`Family::ends_a_case`]), that the engines
    /// gave differently, which is passed over.
    pub fn passed_an_answer(&mut self, family: Family, seed: u64) {
        let at = family.index();
        if family != Family::BelowPlainDelete {
            self.counts.histories[at] = 1;
        }
        self.counts.answers[at] = 1;
        self.counts.answer_seed[at] = Some(seed);
    }

    /// Both engines took `op`, and with `root_changed` the root hash
    /// after it is another than before it. Where `op` is a write of
    /// elements, that ends what an earlier delete at the root path
    /// began: a write that changes the root hash writes a node of the
    /// root Merk, and LigninDB writes none while the root-key record
    /// does not name its root. Called before [`Known::took`], which
    /// begins it again where `op` is itself such a delete.
    pub fn wrote(&mut self, op: &Op, root_changed: bool) {
        if writes_elements(op) && root_changed {
            self.root_delete = false;
        }
    }

    /// The case with the seed `seed` ended at an answer of `family`.
    pub fn ended_at(&mut self, family: Family, seed: u64) {
        if matches!(
            family,
            Family::DroppedTreeProof | Family::TakenBelowPlainDelete
        ) {
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
            self.family_of_answer(op, true, &[Part::Result])
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
        assert_eq!(known.counts().histories, [0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(known.family(&over[0]), None);
        let differs = [record(&[b"t", b"indexed"], 1)];
        let (was, is) = known.passed_over(7, &differs, &[]);
        assert_eq!((was.len(), is.len()), (1, 0));
        assert_eq!(known.counts().stored, [0; 11]);
        known.ended_at(Family::IndexedOverIndexed, 11);
        let counts = known.counts();
        assert_eq!(counts.answers, [0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
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
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
        assert_eq!(known.family(&over[0]), None);
        known.ended_at(Family::InsertOnlyOverIndexed, 13);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
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
        assert_eq!(known.counts().stored, [0; 11]);

        // A stored difference in the dropped tree's own subtree is passed
        // over and counted, but not in the record of its drop.
        let kept = [record(&[b"c"], 1), record(&[b"a", b"dropped"], 1)];
        let (was, is) = known.passed_over(7, &kept, &kept[..1]);
        assert_eq!((was.len(), is.len()), (1, 1));
        assert_eq!(known.counts().stored, [0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
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
        known.took(&drop, Vec::new(), Vec::new(), false);
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
        assert_eq!(counts.answers, [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(counts.histories, [0, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
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
        known.took(&drop, Vec::new(), Vec::new(), false);
        known.took(&put, vec![path(&[b"b", b"over"])], Vec::new(), false);
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
            Vec::new(),
            false,
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
        assert_eq!(counts.stored, [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(counts.stored_seed[..4], [Some(7), Some(7), None, None]);
        assert_eq!(
            (counts.histories, counts.answers),
            ([1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0], [0; 11])
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
            assert_eq!(known.counts().stored, [0; 11]);
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
        assert_eq!(
            known.family_of_answer(&nothing_of_its_own, false, &[Part::Result]),
            None
        );
        let one_of_its_own = added(vec![BatchOp::InsertOnly {
            path: path(&[b"c"]),
            key: b"k".to_vec(),
            element: Element::new_item(vec![1]),
        }]);
        assert_eq!(puts(&one_of_its_own).len(), 2);
        assert_eq!(flat_drops(&one_of_its_own), vec![path(&[b"a", b"dropped"])]);
        assert_eq!(known.family_of_taken(&one_of_its_own), over);
        assert_eq!(
            known.family_of_answer(&one_of_its_own, false, &[Part::Result]),
            None
        );
        let own_over = added(vec![BatchOp::Delete {
            path: path(&[b"b"]),
            key: b"over".to_vec(),
        }]);
        assert_eq!(
            known.family_of_answer(&own_over, false, &[Part::Result]),
            over
        );
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
        assert_eq!(counts.answers, [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(counts.answer_seed[..4], [Some(9), None, None, None]);
        assert_eq!(counts.answer_differences(), 2);
        assert_eq!(counts.histories, [2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        let lines = counts.lines();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with(
            "open difference, an element written over a tree that held elements: 2 cases made \
             it, 0 with stored records that differ passed over, 2 ended at an answer that \
             differs (seed 0x9)"
        ));
        assert!(Counts::default().lines().is_empty());
    }

    /// Below a tree that a plain delete removed, an answer is of the
    /// family where its operation is a read, also one in the block or
    /// through a snapshot, and the engines differ in the result, the
    /// error or the cost of the read and in nothing else. A write there,
    /// and a read that differs in its proof or in a root hash, are of no
    /// family. Where the place is also one of an element written over a
    /// tree, that family has every operation.
    #[test]
    fn below_a_plain_delete_a_read_is_of_the_family_by_its_result_and_its_cost_alone() {
        let removed = path(&[b"p", b"t"]);
        let delete = Op::Batch(vec![BatchOp::Delete {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
        }]);
        assert_eq!(plain_deletes(&delete), vec![removed.clone()]);
        let mut known = Known::default();
        known.took(&delete, Vec::new(), vec![removed.clone()], false);
        let family = Some(Family::BelowPlainDelete);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
        let of_it: [&[&[u8]]; 2] = [&[b"p", b"t"], &[b"p", b"t", b"below"]];
        for of in of_it {
            assert_eq!(known.family(&path(of)), family, "{of:?}");
        }
        let of_none: [&[&[u8]]; 4] = [&[b"p"], &[b"p", b"u"], &[b"t"], &[]];
        for elsewhere in of_none {
            assert_eq!(known.family(&path(elsewhere)), None, "{elsewhere:?}");
        }

        let has = || Read::HasRaw {
            path: removed.clone(),
            key: vec![1],
        };
        let reads = [
            Op::Read(has()),
            Op::Transaction(TransactionOp::BlockRead(has())),
            Op::Transaction(TransactionOp::SnapshotRead(has())),
        ];
        let of_a_read: [&[Part]; 4] = [
            &[Part::Result],
            &[Part::Cost],
            &[Part::Result, Part::Cost],
            &[Part::Error, Part::Cost],
        ];
        let of_more: [&[Part]; 5] = [
            &[Part::RootHash],
            &[Part::Result, Part::Proof],
            &[Part::ProofCost],
            &[Part::Cost, Part::RootHash],
            &[Part::Panic],
        ];
        for read in &reads {
            for parts in of_a_read {
                assert_eq!(known.family_of_answer(read, true, parts), family);
                assert_eq!(known.family_of_answer(read, false, parts), family);
            }
            for parts in of_more {
                assert_eq!(known.family_of_answer(read, true, parts), None);
            }
        }
        let write = Op::Batch(vec![BatchOp::InsertOrReplace {
            path: removed.clone(),
            key: vec![1],
            element: Element::empty_sum_tree(),
        }]);
        let made_again = Op::Write(Write::Insert {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
            element: Element::empty_tree(),
        });
        for parts in of_a_read {
            assert_eq!(known.family_of_answer(&write, true, parts), None);
            assert_eq!(known.family_of_answer(&made_again, true, parts), None);
        }

        // A read of the removed tree's own key reads the tree above, and
        // is of no family; so is a read that returns a proof or the root
        // of an append-only tree as its result.
        let in_the_tree_above = [
            Read::HasRaw {
                path: path(&[b"p"]),
                key: b"t".to_vec(),
            },
            Read::Get {
                path: path(&[b"p"]),
                key: b"t".to_vec(),
            },
            Read::Fetch {
                path: path(&[b"p"]),
                key: b"t".to_vec(),
                fetch: crate::workload::Fetch::Raw,
                cache: true,
            },
        ];
        let everything =
            grovedb::PathQuery::new_unsized(removed.clone(), grovedb::Query::new_range_full());
        let as_its_result = [
            Read::Prove(Prove::PathQuery(Box::new(everything.clone()))),
            Read::Prove(Prove::Many(vec![everything])),
            Read::Tree {
                path: removed.clone(),
                read: TreeRead::MmrRootHash,
            },
            Read::Tree {
                path: removed.clone(),
                read: TreeRead::DenseRootHash,
            },
            Read::Tree {
                path: removed.clone(),
                read: TreeRead::CommitmentAnchor,
            },
        ];
        for read in in_the_tree_above.into_iter().chain(as_its_result) {
            for op in [
                Op::Read(read.clone()),
                Op::Transaction(TransactionOp::BlockRead(read.clone())),
                Op::Transaction(TransactionOp::SnapshotRead(read.clone())),
            ] {
                for parts in of_a_read {
                    assert_eq!(known.family_of_answer(&op, true, parts), None, "{op:?}");
                }
            }
        }
        let below_it = [
            Read::IsEmptyTree {
                path: removed.clone(),
            },
            Read::Get {
                path: path(&[b"p", b"t", b"below"]),
                key: vec![1],
            },
            Read::Tree {
                path: removed.clone(),
                read: TreeRead::MmrLeafCount,
            },
        ];
        for read in below_it {
            let op = Op::Read(read);
            assert_eq!(known.family_of_answer(&op, true, &[Part::Result]), family);
        }

        // A query of the tree above is of the family where it goes down
        // into the tree, and of none where it reads the tree above alone.
        let range = |below: bool| {
            Op::Read(Read::Query(crate::workload::Query {
                path: path(&[b"p"]),
                items: vec![crate::workload::QueryItem::RangeFull],
                subquery_path: None,
                subquery_all: below,
                limit: None,
                left_to_right: true,
                raw: true,
            }))
        };
        assert_eq!(
            known.family_of_answer(&range(true), true, &[Part::Result]),
            family
        );
        assert_eq!(
            known.family_of_answer(&range(false), true, &[Part::Result]),
            None
        );

        // A key that the batch names twice is no place.
        let twice = Op::Batch(vec![
            BatchOp::Delete {
                path: path(&[b"p"]),
                key: b"t".to_vec(),
            },
            BatchOp::InsertOrReplace {
                path: path(&[b"p"]),
                key: b"t".to_vec(),
                element: Element::empty_tree(),
            },
        ]);
        assert!(plain_deletes(&twice).is_empty());

        // An element written over a tree at the same path: that family,
        // for any operation.
        known.took(&made_again, vec![removed.clone()], Vec::new(), false);
        let over = Some(Family::WrittenOver);
        assert_eq!(known.family_of_answer(&write, true, &[Part::Cost]), over);
        assert_eq!(
            known.family_of_answer(&reads[0], true, &[Part::Proof]),
            over
        );
    }

    /// A plain delete of a batch makes a place where the reference read
    /// a tree with an element at the key before the batch and still
    /// holds a record under the tree's path after it: not for a tree
    /// with nothing in it, an item, a key that holds nothing, or a tree
    /// whose elements the same batch deletes.
    #[test]
    fn a_plain_delete_makes_a_place_where_the_reference_kept_a_record_of_a_tree() {
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        let delete = |at: &[&[u8]], key: &[u8]| BatchOp::Delete {
            path: path(at),
            key: key.to_vec(),
        };
        for grove_version in GROVE_VERSIONS.iter().take(4) {
            let dir = scratch();
            let db: GroveDb<RocksDbEngine> =
                GroveDb::open_with_engine(dir.path()).expect("a database");
            let mut transactions = Transactions::new(&db);
            let mut apply = |op: &Op| {
                transactions
                    .apply(op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{op:?}: {error}"));
            };
            for op in [
                insert(&[], b"p", Element::empty_tree()),
                insert(&[b"p"], b"filled", Element::empty_tree()),
                insert(&[b"p", b"filled"], &[1], Element::empty_sum_tree()),
                insert(&[b"p"], b"emptied", Element::empty_tree()),
                insert(&[b"p", b"emptied"], &[1], Element::empty_sum_tree()),
                insert(&[b"p"], b"empty", Element::empty_tree()),
                insert(&[b"p"], b"item", Element::new_item(vec![1])),
                Op::Commit,
            ] {
                apply(&op);
            }
            let batch = Op::Batch(vec![
                delete(&[b"p"], b"filled"),
                delete(&[b"p", b"emptied"], &[1]),
                delete(&[b"p"], b"emptied"),
                delete(&[b"p"], b"empty"),
                delete(&[b"p"], b"item"),
                delete(&[b"p"], b"absent"),
            ]);
            let number = grove_version.protocol_version;
            let before =
                Known::trees_under_a_plain_delete(&db, transactions.block(), &batch, grove_version);
            let held = vec![path(&[b"p", b"filled"]), path(&[b"p", b"emptied"])];
            assert_eq!(before, held, "grove version {number}");
            transactions
                .apply(&batch, grove_version)
                .unwrap()
                .unwrap_or_else(|error| panic!("the batch on {number}: {error}"));
            let left = Known::left_a_record(&db, transactions.block(), before);
            assert_eq!(
                left,
                vec![path(&[b"p", b"filled"])],
                "grove version {number}"
            );
        }
    }

    /// The delete that can leave the root-key record behind is one by
    /// `GroveDb::delete` at the root path, on a grove version up to the
    /// third, of a tree that holds an element.
    #[test]
    fn a_delete_at_the_root_path_of_a_tree_with_an_element_is_asked_on_versions_1_to_3() {
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        let delete = |at: &[&[u8]], key: &[u8]| {
            Op::Write(Write::Delete {
                path: path(at),
                key: key.to_vec(),
            })
        };
        for grove_version in GROVE_VERSIONS.iter() {
            let dir = scratch();
            let db: GroveDb<RocksDbEngine> =
                GroveDb::open_with_engine(dir.path()).expect("a database");
            let mut transactions = Transactions::new(&db);
            for op in [
                insert(&[], b"filled", Element::empty_tree()),
                insert(&[b"filled"], b"k", Element::new_item(vec![1])),
                insert(&[b"filled"], b"below", Element::empty_tree()),
                insert(&[b"filled", b"below"], b"k", Element::new_item(vec![1])),
                insert(&[], b"empty", Element::empty_tree()),
                insert(&[], b"item", Element::new_item(vec![1])),
                Op::Commit,
            ] {
                transactions
                    .apply(&op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{op:?}: {error}"));
            }
            let asked = |op: &Op| {
                Known::trees_under_a_root_delete(&db, transactions.block(), op, grove_version)
            };
            let kept = grove_version.protocol_version <= 3;
            assert_eq!(asked(&delete(&[], b"filled")), kept);
            let sectional = Op::Write(Write::SectionalDelete {
                path: Vec::new(),
                key: b"filled".to_vec(),
            });
            assert_eq!(asked(&sectional), kept);
            let in_a_batch = Op::Batch(vec![BatchOp::Delete {
                path: Vec::new(),
                key: b"filled".to_vec(),
            }]);
            for other in [
                delete(&[], b"empty"),
                delete(&[], b"item"),
                delete(&[], b"absent"),
                delete(&[b"filled"], b"below"),
                in_a_batch,
            ] {
                assert!(!asked(&other), "{other:?}");
            }
        }
    }

    /// An answer is of the family of the root-key record left behind
    /// where the case has such a delete, the reference took the
    /// operation, and the candidate refused it in the words of that
    /// refusal for the root Merk, whole.
    #[test]
    fn the_refusal_for_a_root_key_record_left_behind_is_matched_whole() {
        let root_merk = "0".repeat(64);
        let said = |merk: &str, record: &str, root: &str, rest: &str| {
            format!(
                "failed: not supported: lignindb: the root-key record of Merk {merk} {record}, \
                 and the root of that Merk is the node under the key {root}{rest}"
            )
        };
        let whole = REFUSAL_OF_A_ROOT_KEY_LEFT_BEHIND;
        for refusal in [
            said(&root_merk, "is absent", "62", whole),
            said(&root_merk, "names the key 61", "6263", whole),
            said(&root_merk, "names the key ", "62", whole),
        ] {
            assert!(refuses_for_the_root_key_left_behind(&refusal), "{refusal}");
        }
        let cut = &whole[..whole.len() - 1];
        for other in [
            // A record that names the root, and a key in uppercase.
            said(&root_merk, "names the key 62", "62", whole),
            said(&root_merk, "names the key ", "", whole),
            said(&root_merk, "names the key 6A", "62", whole),
            said(&root_merk, "is absent", "6B", whole),
            said(&"1".repeat(64), "is absent", "62", whole),
            said(&root_merk, "is absent", "62", cut),
            said(&root_merk, "is absent", "62", &format!("{whole} and more")),
            said(&root_merk, "is absent", "6", whole),
            said(&root_merk, "names the key 6", "62", whole),
            said(&root_merk, "names the key zz", "62", whole),
            said(&root_merk, "is gone", "62", whole),
            format!("first: {}", said(&root_merk, "is absent", "62", whole)),
            String::new(),
        ] {
            assert!(!refuses_for_the_root_key_left_behind(&other), "{other}");
        }

        let words = vec![said(&root_merk, "is absent", "62", whole)];
        let took = Outcome::Returned(Value::Nothing);
        let refused = Outcome::Failed("NotSupported".to_string());
        let family = Some(Family::RootKeyLeftBehind);
        let delete = Op::Write(Write::Delete {
            path: Vec::new(),
            key: b"t".to_vec(),
        });
        let put = Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"n".to_vec(),
            element: Element::empty_tree(),
        });
        let batch = Op::Batch(vec![BatchOp::Delete {
            path: path(&[b"a"]),
            key: b"k".to_vec(),
        }]);
        let mut known = Known::default();
        let of = |known: &Known, op: &Op, was: &Outcome, is: &Outcome, said: &[String]| {
            known.family_of_root_key_refusal(op, was, is, said)
        };
        assert_eq!(of(&known, &put, &took, &refused, &words), None);
        known.took(&delete, Vec::new(), Vec::new(), true);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(of(&known, &put, &took, &refused, &words), family);
        assert_eq!(of(&known, &batch, &took, &refused, &words), family);
        assert_eq!(of(&known, &delete, &took, &refused, &words), family);
        let corrupted = Outcome::Failed("CorruptedData".to_string());
        let other_words = vec!["failed: not supported: lignindb: something else".to_string()];
        assert_eq!(of(&known, &put, &refused, &refused, &words), None);
        assert_eq!(of(&known, &put, &took, &corrupted, &words), None);
        assert_eq!(of(&known, &put, &took, &took, &words), None);
        assert_eq!(of(&known, &put, &took, &refused, &other_words), None);
        assert_eq!(of(&known, &put, &took, &refused, &[]), None);
        // The refusal of an operation that writes no element is of no
        // family: a read, a step on the transaction, a commit, a write
        // of an aux record.
        let read = Read::Get {
            path: Vec::new(),
            key: b"a".to_vec(),
        };
        for other in [
            Op::Read(read.clone()),
            Op::Transaction(TransactionOp::BlockRead(read)),
            Op::Transaction(TransactionOp::SetSavepoint),
            Op::Commit,
            Op::Write(Write::PutAux {
                key: b"k".to_vec(),
                value: vec![1],
            }),
        ] {
            assert_eq!(
                of(&known, &other, &took, &refused, &words),
                None,
                "{other:?}"
            );
        }
        // A write that both took and that left the root hash as it was,
        // and a step that is no write of elements, leave it; a write
        // that both took and that changed the root hash ends it, a
        // conditional insert that wrote among them, though the case
        // stays counted; and another such delete begins it again.
        known.wrote(&batch, false);
        known.wrote(&Op::Transaction(TransactionOp::Rollback), true);
        known.wrote(&Op::Commit, true);
        assert_eq!(of(&known, &put, &took, &refused, &words), family);
        let conditional = Op::Write(Write::InsertIfNotExists {
            path: Vec::new(),
            key: b"vacant".to_vec(),
            element: Element::empty_tree(),
            return_existing: false,
        });
        known.wrote(&conditional, true);
        assert_eq!(of(&known, &put, &took, &refused, &words), None);
        known.took(&delete, Vec::new(), Vec::new(), true);
        assert_eq!(of(&known, &put, &took, &refused, &words), family);
        known.wrote(&put, true);
        assert_eq!(of(&known, &put, &took, &refused, &words), None);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
        known.took(&delete, Vec::new(), Vec::new(), true);
        assert_eq!(of(&known, &put, &took, &refused, &words), family);
        // It is of an answer alone: no path is of it.
        assert_eq!(known.family(&[]), None);
        known.ended_at(Family::RootKeyLeftBehind, 9);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
    }

    /// A cost is of the family of a plain delete of a tree made again
    /// where the batch removes such a tree, the engines differ in the
    /// cost alone, and the costs differ in the seeks, the bytes loaded
    /// and the hash calls alone, the reference's the larger, its hash
    /// calls by no more than the trees that keep a Merk among the
    /// records it alone holds. Without any one of these it is of no
    /// family.
    #[test]
    fn a_cost_is_of_the_family_by_the_seeks_the_bytes_loaded_and_the_subtrees_opened() {
        let step = |seeks: u32, loaded: u64, hashes: u32, removed: u32| {
            Step {
            cost: OperationCost {
                seek_count: seeks,
                storage_loaded_bytes: loaded,
                hash_node_calls: hashes,
                storage_cost: grovedb_costs::storage_cost::StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 75,
                    removed_bytes:
                        grovedb_costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval(
                            removed,
                        ),
                },
                ..OperationCost::default()
            },
            outcome: Outcome::Returned(Value::Nothing),
            root_hash: Some([1; 32]),
            proof: None,
        }
        };
        let kept = KeptBelow {
            trees: vec![path(&[b"p", b"t"])],
            path_hashes: 0,
        };
        let family = Some(Family::KeptRecordsCost);
        let cost = [Part::Cost];
        let is = step(10, 966, 8, 142);
        for was in [
            step(11, 1147, 8, 142),
            step(11, 966, 8, 142),
            step(10, 967, 8, 142),
        ] {
            assert_eq!(Known::family_of_cost(&kept, &was, &is, &cost), family);
        }
        let was = step(11, 1147, 8, 142);
        // No such tree, another part, another field of the cost, and a
        // candidate that was charged more.
        let none = KeptBelow::default();
        assert_eq!(Known::family_of_cost(&none, &was, &is, &cost), None);
        // A hash call for each subtree that the reference alone opens:
        // one more than those, and fewer than the candidate's, are of
        // no family.
        let two = KeptBelow {
            path_hashes: 2,
            ..kept.clone()
        };
        for hashes in [8, 9, 10] {
            let was = step(12, 1400, hashes, 142);
            assert_eq!(Known::family_of_cost(&two, &was, &is, &cost), family);
        }
        for hashes in [7, 11] {
            let was = step(12, 1400, hashes, 142);
            assert_eq!(Known::family_of_cost(&two, &was, &is, &cost), None);
        }
        let hashes_alone = step(10, 966, 9, 142);
        assert_eq!(
            Known::family_of_cost(&two, &hashes_alone, &is, &cost),
            family
        );
        let parts = [Part::Cost, Part::RootHash];
        assert_eq!(Known::family_of_cost(&kept, &was, &is, &parts), None);
        assert_eq!(
            Known::family_of_cost(&kept, &was, &is, &[Part::Result]),
            None
        );
        for other in [step(11, 1147, 9, 142), step(11, 1147, 8, 215)] {
            assert_eq!(Known::family_of_cost(&kept, &other, &is, &cost), None);
        }
        for less in [step(9, 1147, 8, 142), step(11, 900, 8, 142)] {
            assert_eq!(Known::family_of_cost(&kept, &less, &is, &cost), None);
        }
        // A batch that both engines refuse alike.
        let refused = |mut step: Step| {
            step.outcome = Outcome::Failed("InvalidBatchOperation".to_string());
            step
        };
        let (was_refused, is_refused) = (refused(was.clone()), refused(is.clone()));
        assert_eq!(
            Known::family_of_cost(&kept, &was_refused, &is_refused, &cost),
            None
        );
        let mut known = Known::default();
        known.passed_an_answer(Family::KeptRecordsCost, 5);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0]);
        known.passed_an_answer(Family::BelowPlainDelete, 5);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
    }

    /// A refusal is of the family of a write that both engines refuse
    /// below a plain delete where the operation writes elements into a
    /// subtree at or below such a place, both engines failed, and they
    /// differ in the cost or the error alone. Without any one of these
    /// it is of no family.
    #[test]
    fn a_write_that_both_engines_refuse_below_a_plain_delete_is_of_its_family() {
        let delete = Op::Batch(vec![BatchOp::Delete {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
        }]);
        let mut known = Known::default();
        known.took(&delete, Vec::new(), vec![path(&[b"p", b"t"])], false);
        let put = |at: &[&[u8]], key: &[u8]| BatchOp::InsertOrReplace {
            path: path(at),
            key: key.to_vec(),
            element: Element::new_item(vec![1]),
        };
        let below = Op::Batch(vec![put(&[b"p", b"t", &[1]], b"k")]);
        let invalid = Outcome::Failed("InvalidPath".to_string());
        let not_found = Outcome::Failed("PathKeyNotFound".to_string());
        let took = Outcome::Returned(Value::Nothing);
        let family = Some(Family::RefusedBelowPlainDelete);
        let of = |op: &Op, was: &Outcome, is: &Outcome, parts: &[Part]| {
            known.family_of_refused_write(op, was, is, parts, true, &[], &[])
        };
        assert_eq!(of(&below, &invalid, &invalid, &[Part::Cost]), family);
        let both = [Part::Cost, Part::Error];
        assert_eq!(of(&below, &not_found, &invalid, &both), family);
        assert_eq!(of(&below, &not_found, &invalid, &[Part::Error]), family);
        // Into the tree's own subtree, by a direct write, and by a
        // batch that also writes into the subtree the tree was in.
        let direct = Op::Write(Write::Insert {
            path: path(&[b"p", b"t"]),
            key: vec![1],
            element: Element::empty_sum_tree(),
        });
        let mixed = Op::Batch(vec![put(&[b"p"], b"x"), put(&[b"p", b"t"], &[1])]);
        let partial = Op::PartialBatch(PartialBatch {
            ops: vec![put(&[b"p", b"t"], &[1])],
            add_on: Vec::new(),
            pause_height: None,
        });
        for op in [&direct, &mixed, &partial] {
            assert_eq!(of(op, &invalid, &invalid, &[Part::Cost]), family, "{op:?}");
        }
        // One engine took it; the root hash differs too; nothing
        // differs; a write into the tree above, of the removed tree's
        // own key; a write elsewhere; a read; an operation that a
        // partial batch's caller adds.
        assert_eq!(of(&below, &took, &invalid, &[Part::Error]), None);
        assert_eq!(of(&below, &invalid, &took, &[Part::Error]), None);
        let with_the_root = [Part::Cost, Part::RootHash];
        assert_eq!(of(&below, &invalid, &invalid, &with_the_root), None);
        assert_eq!(of(&below, &invalid, &invalid, &[]), None);
        let above = Op::Batch(vec![put(&[b"p"], b"t")]);
        let elsewhere = Op::Batch(vec![put(&[b"q", b"t"], b"k")]);
        // A batch that also writes into another subtree: its refusal
        // may be of that one. And a root hash that changed on an engine.
        let beside = Op::Batch(vec![put(&[b"q"], b"k"), put(&[b"p", b"t"], &[1])]);
        assert_eq!(of(&beside, &invalid, &invalid, &[Part::Cost]), None);
        let changed = known.family_of_refused_write(
            &below,
            &invalid,
            &invalid,
            &[Part::Cost],
            false,
            &[],
            &[],
        );
        assert_eq!(changed, None);
        let read = Op::Read(Read::HasRaw {
            path: path(&[b"p", b"t"]),
            key: vec![1],
        });
        let added = Op::PartialBatch(PartialBatch {
            ops: vec![put(&[b"q"], b"k")],
            add_on: vec![put(&[b"p", b"t"], &[1])],
            pause_height: None,
        });
        // And a partial batch whose caller adds a write elsewhere: how
        // far a refused batch came is not known.
        let adds_elsewhere = Op::PartialBatch(PartialBatch {
            ops: vec![put(&[b"p", b"t"], &[1])],
            add_on: vec![put(&[b"q"], b"k")],
            pause_height: None,
        });
        for op in [&above, &elsewhere, &read, &added, &adds_elsewhere] {
            assert_eq!(of(op, &invalid, &invalid, &[Part::Cost]), None, "{op:?}");
        }
        // A batch that also writes elsewhere is of the family where
        // the candidate's refusal names, in GroveDB's words, a subtree
        // under a record that a plain delete left; not in other words,
        // for another subtree, with another error, as a partial batch,
        // with a root hash that changed, or where one engine took it.
        let left = vec![path(&[b"p", b"t", &[1]])];
        let words = "failed: invalid path: could not get key for parent of subtree for batch at \
                     path [p/t] for key 0x01";
        let said = vec![words.to_string()];
        let cost = [Part::Cost];
        let of_left = |op: &Op, was: &Outcome, is: &Outcome, left: &[Path], said: &[String]| {
            known.family_of_refused_write(op, was, is, &cost, true, left, said)
        };
        assert_eq!(of_left(&beside, &invalid, &invalid, &left, &said), family);
        assert_eq!(of_left(&beside, &not_found, &invalid, &left, &said), family);
        assert_eq!(of_left(&beside, &invalid, &invalid, &left, &[]), None);
        assert_eq!(of_left(&beside, &invalid, &invalid, &[], &said), None);
        let other_words = vec![words.replace("0x01", "0x02")];
        assert_eq!(
            of_left(&beside, &invalid, &invalid, &left, &other_words),
            None
        );
        assert_eq!(of_left(&beside, &invalid, &not_found, &left, &said), None);
        assert_eq!(of_left(&beside, &took, &invalid, &left, &said), None);
        let partial_beside = Op::PartialBatch(PartialBatch {
            ops: vec![put(&[b"q"], b"k"), put(&[b"p", b"t", &[1]], b"k")],
            add_on: Vec::new(),
            pause_height: None,
        });
        assert_eq!(
            of_left(&partial_beside, &invalid, &invalid, &left, &said),
            None
        );
        let changed =
            known.family_of_refused_write(&beside, &invalid, &invalid, &cost, false, &left, &said);
        assert_eq!(changed, None);
        // A case with no such place.
        let none = Known::default();
        let parts = [Part::Cost];
        assert_eq!(
            none.family_of_refused_write(&below, &invalid, &invalid, &parts, true, &[], &[]),
            None
        );
        known.passed_an_answer(Family::RefusedBelowPlainDelete, 3);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0]);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
        assert!(known.counts().passed_over());
    }

    /// A write is of the family of a write that RocksDB alone takes
    /// where the reference took it, the candidate refused it with
    /// `InvalidPath`, and the refusal is about a subtree that only a
    /// record a plain delete left names: of a direct insert the subtree
    /// it writes into, of a batch one that the candidate names in
    /// GroveDB's words for a path whose element it cannot read.
    #[test]
    fn a_write_that_the_reference_alone_takes_is_of_its_family_by_the_subtree_and_the_refusal() {
        let held = path(&[b"removed", b"t", &[1]]);
        let subtrees = vec![held.clone()];
        let took = Outcome::Returned(Value::Nothing);
        let invalid = Outcome::Failed("InvalidPath".to_string());
        let family = Some(Family::TakenBelowPlainDelete);
        let put = |at: &Path, key: &[u8]| BatchOp::InsertOrReplace {
            path: at.clone(),
            key: key.to_vec(),
            element: Element::new_item(vec![1]),
        };
        let elsewhere = path(&[b"q"]);
        let batch = Op::Batch(vec![put(&elsewhere, b"k"), put(&held, b"k")]);
        let flags = Op::FlagsBatch(vec![put(&held, b"k")]);
        let direct = Op::Write(Write::Insert {
            path: held.clone(),
            key: b"k".to_vec(),
            element: Element::new_item(vec![1]),
        });
        // GroveDB's words, with the path and the key as it prints them.
        let words = "failed: invalid path: could not get key for parent of subtree for batch at \
                     path [removed/t] for key 0x01";
        assert_eq!(refuses_for_no_element_above(&held[..2], &[1]), words);
        let said = vec!["a remark".to_string(), words.to_string()];
        let of = |op: &Op, named: &[Path], was: &Outcome, is: &Outcome, said: &[String]| {
            Known::family_of_taken_write(op, named, was, is, said)
        };
        for op in [&batch, &flags] {
            assert_eq!(of(op, &subtrees, &took, &invalid, &said), family, "{op:?}");
            // A refusal in other words, or about another path or key.
            assert_eq!(of(op, &subtrees, &took, &invalid, &[]), None, "{op:?}");
            let about_another = [
                words.replace("[removed/t]", "[q]"),
                words.replace("0x01", "0x02"),
                words.replace("for batch", "for a batch"),
                format!("{words} "),
            ];
            for other in about_another {
                assert_eq!(of(op, &subtrees, &took, &invalid, &[other]), None);
            }
        }
        // A direct insert writes into one subtree: its refusal is about
        // that one, in whatever words.
        assert_eq!(of(&direct, &subtrees, &took, &invalid, &[]), family);
        let into_another = Op::Write(Write::Insert {
            path: elsewhere.clone(),
            key: b"k".to_vec(),
            element: Element::new_item(vec![1]),
        });
        assert_eq!(of(&into_another, &subtrees, &took, &invalid, &said), None);
        // No such subtree, another error, both refused, the candidate
        // alone took it, both took it, and an operation of another
        // kind.
        assert_eq!(of(&batch, &[], &took, &invalid, &said), None);
        assert_eq!(of(&direct, &[], &took, &invalid, &said), None);
        let other = Outcome::Failed("PathKeyNotFound".to_string());
        assert_eq!(of(&batch, &subtrees, &took, &other, &said), None);
        assert_eq!(of(&batch, &subtrees, &invalid, &invalid, &said), None);
        assert_eq!(of(&batch, &subtrees, &invalid, &took, &said), None);
        assert_eq!(of(&batch, &subtrees, &took, &took, &said), None);
        let partial = Op::PartialBatch(PartialBatch {
            ops: vec![put(&held, b"k")],
            add_on: Vec::new(),
            pause_height: None,
        });
        assert_eq!(of(&partial, &subtrees, &took, &invalid, &said), None);
        assert!(Family::TakenBelowPlainDelete.ends_a_case());
        let mut known = Known::default();
        known.ended_at(Family::TakenBelowPlainDelete, 4);
        assert_eq!(known.counts().histories, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(known.counts().answers, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert!(!known.counts().passed_over());
    }

    /// The subtrees a write goes into that count for a write the
    /// reference alone takes: each at or below a place of a plain
    /// delete, under a key whose record the reference held below the
    /// tree before the delete, which the case has not forgotten since,
    /// which the reference holds and the candidate does not, in a
    /// direct insert or a batch. And what makes a case forget a record.
    #[test]
    fn a_subtree_counts_where_only_a_record_that_a_plain_delete_left_names_it() {
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        let put = |at: &[&[u8]], key: &[u8]| {
            Op::Batch(vec![BatchOp::InsertOrReplace {
                path: path(at),
                key: key.to_vec(),
                element: Element::new_item(vec![1]),
            }])
        };
        let delete = Op::Batch(vec![BatchOp::Delete {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
        }]);
        let delete_tree = Op::Batch(vec![BatchOp::DeleteTree {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
            tree_type: grovedb::TreeType::NormalTree,
        }]);
        for grove_version in GROVE_VERSIONS.iter() {
            let dirs = [scratch(), scratch()];
            let open = |dir: &tempfile::TempDir| -> GroveDb<RocksDbEngine> {
                GroveDb::open_with_engine(dir.path()).expect("a database")
            };
            let (reference, candidate) = (open(&dirs[0]), open(&dirs[1]));
            let mut on_reference = Transactions::new(&reference);
            let mut on_candidate = Transactions::new(&candidate);
            // `t` with a sum tree `held`, removed, by a plain delete on
            // the reference, which keeps `held`, and with what it holds
            // on the candidate; then made again with another key.
            let sum_tree = Element::empty_sum_tree;
            let ops = [
                (insert(&[], b"p", Element::empty_tree()), None),
                (insert(&[b"p"], b"t", Element::empty_tree()), None),
                (insert(&[b"p", b"t"], b"held", sum_tree()), None),
                (insert(&[b"p"], b"u", Element::empty_tree()), None),
                (Op::Commit, None),
                (delete.clone(), Some(delete_tree.clone())),
                (Op::Commit, None),
                (insert(&[b"p"], b"t", Element::empty_tree()), None),
                (insert(&[b"p", b"t"], b"other", sum_tree()), None),
                (Op::Commit, None),
            ];
            for (op, on_the_candidate) in ops {
                let other = on_the_candidate.unwrap_or_else(|| op.clone());
                for (transactions, op) in [(&mut on_reference, &op), (&mut on_candidate, &other)] {
                    transactions
                        .apply(op, grove_version)
                        .unwrap()
                        .unwrap_or_else(|error| panic!("{op:?}: {error}"));
                }
            }
            // A tree `fresh` that the reference alone holds in the tree
            // made again, as if the candidate had lost it: no record
            // that the plain delete left.
            let fresh = insert(&[b"p", b"t"], b"fresh", sum_tree());
            on_reference
                .apply(&fresh, grove_version)
                .unwrap()
                .unwrap_or_else(|error| panic!("{fresh:?}: {error}"));
            let held = (path(&[b"p", b"t"]), b"held".to_vec());
            let mut known = Known::default();
            known.took(&delete, Vec::new(), vec![path(&[b"p", b"t"])], false);
            // The places alone, with no record of the history: nothing.
            let asked_of = |known: &Known, op: &Op| {
                known.named_by_a_kept_record_alone(
                    (&reference, on_reference.block()),
                    (&candidate, on_candidate.block()),
                    op,
                    grove_version,
                )
            };
            assert!(asked_of(&known, &put(&[b"p", b"t", b"held"], b"k")).is_empty());
            // A record outside the places is none that a delete left.
            known.left_records(vec![held.clone(), (path(&[b"p", b"u"]), b"held".to_vec())]);
            assert_eq!(known.left_records.len(), 1);
            let asked = |op: &Op| asked_of(&known, op);
            assert!(asked(&put(&[b"p", b"t", b"fresh"], b"k")).is_empty());
            // Both read the new tree and its sum tree `other`: nothing.
            assert!(asked(&put(&[b"p", b"t"], b"k")).is_empty());
            assert!(asked(&put(&[b"p", b"t", b"other"], b"k")).is_empty());
            // The subtree under the key whose record the reference kept.
            let kept = path(&[b"p", b"t", b"held"]);
            assert_eq!(asked(&put(&[b"p", b"t", b"held"], b"k")), [kept]);
            // Under a key that holds no element and no kept record,
            // outside the place, a read, and a case with no place.
            assert!(asked(&put(&[b"p", b"t", b"absent"], b"k")).is_empty());
            assert!(asked(&put(&[b"p", b"u", b"held"], b"k")).is_empty());
            let read = Op::Read(Read::HasRaw {
                path: path(&[b"p", b"t", b"held"]),
                key: b"k".to_vec(),
            });
            assert!(asked(&read).is_empty());
            let none = Known::default().named_by_a_kept_record_alone(
                (&reference, on_reference.block()),
                (&candidate, on_candidate.block()),
                &put(&[b"p", b"t", b"held"], b"k"),
                grove_version,
            );
            assert!(none.is_empty());
            // A direct insert counts as a whole batch does; a batch
            // that also writes into a subtree both engines read, and a
            // partial batch, have none.
            let batch_op = |at: &[&[u8]], key: &[u8]| BatchOp::InsertOrReplace {
                path: path(at),
                key: key.to_vec(),
                element: Element::new_item(vec![1]),
            };
            let direct = insert(&[b"p", b"t", b"held"], b"k", Element::new_item(vec![1]));
            assert_eq!(asked(&direct), [path(&[b"p", b"t", b"held"])]);
            let twice = Op::FlagsBatch(vec![
                batch_op(&[b"p", b"t", b"held"], b"k"),
                batch_op(&[b"p", b"t", b"held"], b"l"),
            ]);
            assert_eq!(asked(&twice), [path(&[b"p", b"t", b"held"])]);
            // Of a batch that also writes into a subtree both engines
            // read, the one subtree: whether a refusal is about it is
            // asked of the refusal.
            let with_another = Op::Batch(vec![
                batch_op(&[b"p", b"u"], b"k"),
                batch_op(&[b"p", b"t", b"held"], b"k"),
            ]);
            assert_eq!(asked(&with_another), [path(&[b"p", b"t", b"held"])]);
            let handover = Op::FlagsHandoverBatch(crate::workload::HandoverBatch {
                ops: vec![batch_op(&[b"p", b"t", b"held"], b"k")],
                full_handover_owners: Default::default(),
            });
            assert!(asked(&handover).is_empty());
            let partial = Op::PartialBatch(PartialBatch {
                ops: vec![batch_op(&[b"p", b"t", b"held"], b"k")],
                add_on: Vec::new(),
                pause_height: None,
            });
            assert!(asked(&partial).is_empty());
            // An operation that names the key of the record and that
            // the reference took, or refused with another root hash
            // than it had, ends it; one that names another key, a
            // commit and a read do not.
            let below = put(&[b"p", b"t", b"held"], b"k");
            let rollback = Op::Transaction(TransactionOp::Rollback);
            let delete_it = Op::Write(Write::Delete {
                path: path(&[b"p", b"t"]),
                key: b"held".to_vec(),
            });
            let clears = Op::Write(Write::ClearSubtree {
                path: path(&[b"p", b"t"]),
            });
            let names_it = [
                insert(&[b"p", b"t"], b"held", sum_tree()),
                put(&[b"p", b"t"], b"held"),
                Op::PartialBatch(PartialBatch {
                    ops: vec![batch_op(&[b"p"], b"x")],
                    add_on: vec![batch_op(&[b"p", b"t"], b"held")],
                    pause_height: None,
                }),
                delete_it,
                clears,
                Op::Batch(vec![BatchOp::Move {
                    path: path(&[b"p"]),
                    key: b"u".to_vec(),
                    new_key: b"t".to_vec(),
                }]),
                Op::Batch(vec![BatchOp::Move {
                    path: path(&[b"p"]),
                    key: b"t".to_vec(),
                    new_key: b"w".to_vec(),
                }]),
                rollback.clone(),
            ];
            for op in &names_it {
                for (taken, root_changed) in [(true, true), (true, false), (false, true)] {
                    let mut forgot = known.clone();
                    forgot.forgets(op, taken, root_changed);
                    assert!(asked_of(&forgot, &below).is_empty(), "{op:?}");
                }
                // Refused by the reference, with the root hash it had:
                // nothing was put under the key. A rollback forgets
                // whatever it is answered.
                let mut refused = known.clone();
                refused.forgets(op, false, false);
                let kept = usize::from(*op != rollback);
                assert_eq!(asked_of(&refused, &below).len(), kept, "{op:?}");
            }
            // A commit that the reference refused: the runner goes on
            // from the committed state, in which the delete may not be.
            let mut lost_its_block = known.clone();
            lost_its_block.forgets(&Op::Commit, false, false);
            assert!(asked_of(&lost_its_block, &below).is_empty());
            let names_another = [
                insert(&[b"p", b"t"], b"other", sum_tree()),
                insert(&[b"p"], b"t", Element::empty_tree()),
                put(&[b"p", b"t"], b"k"),
                put(&[b"p", b"t", b"held"], b"k"),
                Op::Commit,
                read,
            ];
            for op in names_another {
                let mut still = known.clone();
                still.forgets(&op, true, true);
                assert_eq!(asked_of(&still, &below).len(), 1, "{op:?}");
            }
        }
    }

    /// The trees a batch removes by a plain delete, or replaces by a
    /// put, that count for the cost: on grove versions 4 and 5, at,
    /// above or below a place where records were kept, where the
    /// reference holds a node record at or below the path that the
    /// candidate does not hold, also one below a tree that the new tree
    /// holds under an old key.
    #[test]
    fn a_tree_made_again_counts_for_the_cost_where_the_reference_kept_a_record_below_it() {
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path(at),
                key: key.to_vec(),
                element,
            })
        };
        let delete = |key: &[u8]| {
            Op::Batch(vec![BatchOp::Delete {
                path: path(&[b"p"]),
                key: key.to_vec(),
            }])
        };
        let delete_tree = |key: &[u8]| {
            Op::Batch(vec![BatchOp::DeleteTree {
                path: path(&[b"p"]),
                key: key.to_vec(),
                tree_type: grovedb::TreeType::NormalTree,
            }])
        };
        for grove_version in GROVE_VERSIONS.iter() {
            // Two databases with the same elements: in `kept` the trees
            // were removed by a plain delete, in `clean` with what they
            // held.
            let dirs = [scratch(), scratch()];
            let open = |dir: &tempfile::TempDir| -> GroveDb<RocksDbEngine> {
                GroveDb::open_with_engine(dir.path()).expect("a database")
            };
            let (kept, clean) = (open(&dirs[0]), open(&dirs[1]));
            let mut on_kept = Transactions::new(&kept);
            let mut on_clean = Transactions::new(&clean);
            let mut both = |plain: &Op, whole: &Op| {
                for (transactions, op) in [(&mut on_kept, plain), (&mut on_clean, whole)] {
                    transactions
                        .apply(op, grove_version)
                        .unwrap()
                        .unwrap_or_else(|error| panic!("{op:?}: {error}"));
                }
            };
            let sum_tree = Element::empty_sum_tree;
            for op in [
                insert(&[], b"p", Element::empty_tree()),
                // `other`: an item under a key the new tree does not write.
                insert(&[b"p"], b"other", Element::empty_tree()),
                insert(&[b"p", b"other"], &[0], Element::new_item(vec![1])),
                // `same`: the key is written again.
                insert(&[b"p"], b"same", Element::empty_tree()),
                insert(&[b"p", b"same"], &[1], sum_tree()),
                // `deep`: the key is written again, and a record is below it.
                insert(&[b"p"], b"deep", Element::empty_tree()),
                insert(&[b"p", b"deep"], &[1], sum_tree()),
                insert(&[b"p", b"deep", &[1]], b"sum", Element::new_sum_item(4)),
                // `bare` is made again with nothing in it, and `item` as
                // an item.
                insert(&[b"p"], b"bare", Element::empty_tree()),
                insert(&[b"p", b"bare"], &[0], Element::new_item(vec![1])),
                insert(&[b"p"], b"item", Element::empty_tree()),
                insert(&[b"p", b"item"], &[0], Element::new_item(vec![1])),
                Op::Commit,
            ] {
                both(&op, &op);
            }
            let mut known = Known::default();
            for tree in [&b"other"[..], b"same", b"deep", b"bare", b"item"] {
                let place = path(&[b"p", tree]);
                both(&delete(tree), &delete_tree(tree));
                known.took(&delete(tree), Vec::new(), vec![place], false);
            }
            for op in [
                Op::Commit,
                insert(&[b"p"], b"other", Element::empty_tree()),
                insert(&[b"p", b"other"], &[1], sum_tree()),
                insert(&[b"p"], b"same", Element::empty_tree()),
                insert(&[b"p", b"same"], &[1], sum_tree()),
                insert(&[b"p"], b"deep", Element::empty_tree()),
                insert(&[b"p", b"deep"], &[1], sum_tree()),
                insert(&[b"p"], b"new", Element::empty_tree()),
                insert(&[b"p", b"new"], &[1], sum_tree()),
                insert(&[b"p"], b"bare", Element::empty_tree()),
                insert(&[b"p"], b"item", Element::new_item(vec![2])),
                Op::Commit,
            ] {
                both(&op, &op);
            }
            let asked_of = |op: &Op| {
                known.kept_below_a_removed_tree(
                    (&kept, on_kept.block()),
                    (&clean, on_clean.block()),
                    op,
                    grove_version,
                )
            };
            let asked = |op: &Op| asked_of(op).trees;
            let number = grove_version.protocol_version;
            // What the reference kept there are items: the look opens no
            // subtree more for them.
            assert_eq!(asked_of(&delete(b"other")).path_hashes, 0, "{number}");
            let expected = |tree: &[u8]| match number {
                4 | 5 => vec![path(&[b"p", tree])],
                _ => Vec::new(),
            };
            assert_eq!(asked(&delete(b"other")), expected(b"other"), "{number}");
            assert_eq!(asked(&delete(b"deep")), expected(b"deep"), "{number}");
            // Every key written again, a tree that is no such place, a
            // delete of a tree with what it holds, and a put.
            assert!(asked(&delete(b"same")).is_empty(), "{number}");
            assert!(asked(&delete(b"new")).is_empty(), "{number}");
            assert!(asked(&delete_tree(b"other")).is_empty(), "{number}");
            // A tree made again that holds nothing, an item at the path,
            // a batch of operations that declare no check, and a partial
            // batch: nothing is looked through.
            assert!(asked(&delete(b"bare")).is_empty(), "{number}");
            assert!(asked(&delete(b"item")).is_empty(), "{number}");
            let plain = || {
                vec![BatchOp::Delete {
                    path: path(&[b"p"]),
                    key: b"other".to_vec(),
                }]
            };
            assert!(asked(&Op::FlagsBatch(plain())).is_empty(), "{number}");
            let partial = Op::PartialBatch(PartialBatch {
                ops: plain(),
                add_on: Vec::new(),
                pause_height: None,
            });
            assert!(asked(&partial).is_empty(), "{number}");
            // A put that replaces such a tree is looked through as a
            // delete of it is, and so is a delete of the tree above;
            // a conditional insert is not.
            let put = Op::Batch(vec![BatchOp::InsertOrReplace {
                path: path(&[b"p"]),
                key: b"other".to_vec(),
                element: Element::new_item(vec![1]),
            }]);
            assert_eq!(asked(&put), expected(b"other"), "{number}");
            let above = Op::Batch(vec![BatchOp::Delete {
                path: Vec::new(),
                key: b"p".to_vec(),
            }]);
            let the_tree_above = match number {
                4 | 5 => vec![path(&[b"p"])],
                _ => Vec::new(),
            };
            assert_eq!(asked(&above), the_tree_above, "{number}");
            let conditional = Op::Batch(vec![BatchOp::InsertIfNotExists {
                path: path(&[b"p"]),
                key: b"other".to_vec(),
                element: Element::new_item(vec![1]),
            }]);
            assert!(asked(&conditional).is_empty(), "{number}");
            // A flat drop beside the delete: no batch of checked
            // operations.
            let beside_a_flat_drop = Op::Batch(vec![
                BatchOp::Delete {
                    path: path(&[b"p"]),
                    key: b"other".to_vec(),
                },
                BatchOp::DropFlat {
                    path: path(&[b"p"]),
                    key: b"new".to_vec(),
                    tree_type: grovedb::TreeType::NormalTree,
                },
            ]);
            assert!(asked(&beside_a_flat_drop).is_empty(), "{number}");
            // A case whose one place is `same`, every key of which was
            // written again: the records the reference kept under the
            // other trees are of no place of it, so the tree above them
            // does not count.
            let mut one_place = Known::default();
            let same = vec![path(&[b"p", b"same"])];
            one_place.took(&delete(b"same"), Vec::new(), same, false);
            let above_them = one_place.kept_below_a_removed_tree(
                (&kept, on_kept.block()),
                (&clean, on_clean.block()),
                &above,
                grove_version,
            );
            assert_eq!(above_them, KeptBelow::default(), "{number}");
            // The place of an element written over a tree counts as
            // that of a plain delete does, and a case with no place has
            // no such tree.
            let mut written_over = Known::default();
            let over = vec![path(&[b"p", b"other"])];
            written_over.took(&delete(b"other"), over, Vec::new(), false);
            let none = Known::default();
            let of = |known: &Known| {
                let kept = known.kept_below_a_removed_tree(
                    (&kept, on_kept.block()),
                    (&clean, on_clean.block()),
                    &delete(b"other"),
                    grove_version,
                );
                kept.trees
            };
            assert_eq!(of(&written_over), expected(b"other"), "{number}");
            assert!(of(&none).is_empty(), "{number}");
        }
    }
}
