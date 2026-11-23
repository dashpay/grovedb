//! The open differences of the two engines that are ruled and named, and
//! what the fuzzer does where a case comes to one (GROVE-10).
//!
//! RocksDB leaves records behind where GroveDB changes one element and
//! does not clear what hung under it, and reads them again later. LigninDB
//! drops a subtree with its element. Four such histories are ruled as
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
//!   after each operation both engines answered
//!   ([`pending_drop_paths`](super::state::pending_drop_paths)).
//!
//! The fourth has no place, and is of an answer alone:
//!
//! - [`Family::IndexedOverIndexed`]: on grove versions 1 to 3, which leave
//!   the records of the old tree where they are, an operation puts an
//!   indexed tree at a key where the reference read an indexed tree
//!   before the operation, and the candidate answers `NotSupported`
//!   ([`Known::indexed_under`], [`Known::family_of_refusal`]). Both are
//!   required. The case ends there, counted. Nothing was written, so no
//!   stored record is passed over for it.
//!
//! A case keeps the paths it made so ([`Known`]), from the operations both
//! engines took, and nothing else is of a family:
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
//!   each subquery. A difference at any other operation ends the run, as
//!   it always did: so does a read of the subtree of a dropped tree once
//!   the record of its drop is reclaimed. Whether a record is pending is
//!   asked for every read alike, also for one that reads an older version
//!   through a snapshot, or the committed state with no transaction.
//!
//! The counts are in the [`Tally`](super::pair::Tally) of a run
//! ([`Counts`]): the cases that made a place of a family, or for the
//! fourth came to such a put whatever was answered, those with a stored
//! difference passed over and those ended at an answer, and for the last
//! two the seed of one such case.

use grovedb_version::version::GroveVersion;

use grovedb::{Element, SubqueryBranch};

use super::state::Keyspace;
use super::state::Record;
use crate::golden::step::Outcome;
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
}

/// How many families there are.
const FAMILIES: usize = Family::ALL.len();

/// The last grove version on which GroveDB leaves the records of an
/// indexed tree where they are when an element is put over it.
const LAST_VERSION_THAT_LEAVES_THE_OLD_ROWS: u32 = 3;

impl Family {
    /// Every family.
    pub const ALL: [Family; 4] = [
        Family::WrittenOver,
        Family::BelowFlatDrop,
        Family::DroppedTree,
        Family::IndexedOverIndexed,
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
        }
    }

    fn index(self) -> usize {
        match self {
            Family::WrittenOver => 0,
            Family::BelowFlatDrop => 1,
            Family::DroppedTree => 2,
            Family::IndexedOverIndexed => 3,
        }
    }
}

/// What a run met of the families, by [`Family::ALL`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// The cases that made a place of the family: an element put over a
    /// tree that held elements, or a flat drop of a tree, whatever it
    /// held; and for the family of an indexed tree over an indexed tree
    /// the cases that came to such a put, whatever the engines answered.
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

/// The places of the families that one case has made.
#[derive(Clone, Debug, Default)]
pub struct Known {
    over: Vec<Path>,
    flat_drops: Vec<Path>,
    /// The trees among `flat_drops` whose drop has a pending record, as
    /// [`Known::pending`] was last told.
    pending: Vec<Path>,
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

/// The places of [`puts`], each with the element that is put there.
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
    for batch_op in op.batch_ops() {
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
    puts
}

/// The trees `op` drops flat.
fn flat_drops(op: &Op) -> Vec<Path> {
    let mut drops = Vec::new();
    if let Op::Write(Write::DropFlat { path, key }) = op {
        drops.push(joined(path, key));
    }
    for batch_op in op.batch_ops() {
        if let BatchOp::DropFlat { path, key, .. } = batch_op {
            drops.push(joined(path, key));
        }
    }
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

/// Whether `query`, asked of a subtree, reads the subtree at `below`, a
/// path down from its own, and if it does, whether it goes on into a
/// subtree below that one.
///
/// A query goes into the subtree under a key that its items select where
/// the branch of that key, the first conditional one that selects the key
/// or else the default one, has a subquery path or a subquery. It reads
/// the subtrees along the subquery path for the next key of the path, and
/// with a subquery the subtree at the end of the path, of which the
/// subquery is then asked. Which keys hold a tree, and where a limit ends
/// the query, is not looked at.
fn reads(query: &grovedb::Query, below: &[Vec<u8>]) -> Option<bool> {
    let Some((key, below)) = below.split_first() else {
        let conditional = query.conditional_subquery_branches.iter().flatten();
        let mut branches = conditional.map(|(_, branch)| branch);
        return Some(goes_down(&query.default_subquery_branch) || branches.any(goes_down));
    };
    if !query.items.iter().any(|item| item.contains(key)) {
        return None;
    }
    let mut conditional = query.conditional_subquery_branches.iter().flatten();
    let branch = conditional
        .find(|(item, _)| item.contains(key))
        .map_or(&query.default_subquery_branch, |(_, branch)| branch);
    let along = branch.subquery_path.as_deref().unwrap_or_default();
    if below.len() < along.len() {
        let further = below.len() + 1 < along.len() || branch.subquery.is_some();
        return along.starts_with(below).then_some(further);
    }
    let subquery = branch.subquery.as_ref()?;
    let below = below.strip_prefix(along)?;
    reads(subquery, below)
}

/// The places `op` names: the subtree it reads or writes in, and with a
/// key the place under that key.
fn places(op: &Op) -> Vec<Path> {
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
    for batch_op in op.batch_ops() {
        places.push(batch_op.path().clone());
        match batch_op {
            BatchOp::InsertOrReplace { path, key, .. }
            | BatchOp::InsertOnly { path, key, .. }
            | BatchOp::InsertIfNotExists { path, key, .. }
            | BatchOp::Replace { path, key, .. }
            | BatchOp::Patch { path, key, .. }
            | BatchOp::RefreshReference { path, key, .. }
            | BatchOp::Delete { path, key, .. }
            | BatchOp::DeleteTree { path, key, .. }
            | BatchOp::DropFlat { path, key, .. } => places.push(joined(path, key)),
            BatchOp::Move {
                path, key, new_key, ..
            } => {
                places.push(joined(path, key));
                places.push(joined(path, new_key));
            }
            BatchOp::Append { .. } => {}
        }
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

    /// The case came to an operation that puts an indexed tree over the
    /// indexed trees at `over` ([`Known::indexed_under`]).
    pub fn came_to_indexed_over_indexed(&mut self, over: &[Path]) {
        if !over.is_empty() {
            self.counts.histories[Family::IndexedOverIndexed.index()] = 1;
        }
    }

    /// The family of a differing answer to an operation that puts an
    /// indexed tree over the indexed trees at `over`
    /// ([`Known::indexed_under`]), which the candidate answered with
    /// `candidate`: [`Family::IndexedOverIndexed`] where there is such a
    /// tree and the candidate's answer is the error `NotSupported`, and
    /// none otherwise.
    pub fn family_of_refusal(over: &[Path], candidate: &Outcome) -> Option<Family> {
        let refused = matches!(candidate, Outcome::Failed(error) if error == "NotSupported");
        (refused && !over.is_empty()).then_some(Family::IndexedOverIndexed)
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
            reads(&query.query.query, below)
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

    /// The family an answer to `op` is of: that of the first place the
    /// operation names that is of one, and else, for a query, that of a
    /// subtree below its own that it reads.
    pub fn family_of_answer(&self, op: &Op) -> Option<Family> {
        let named = places(op).iter().find_map(|place| self.family(place));
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
    use crate::{diff::pair::testing::scratch, golden::step::Value, workload::Transactions};

    /// An answer is of the family of an indexed tree over an indexed tree
    /// where the operation puts an indexed tree, unconditionally, at a
    /// key at which the reference reads an indexed tree, on a grove
    /// version up to the third, and the candidate's answer is
    /// `NotSupported`. Without any one of these it is of no family: at a
    /// key that holds a tree of another kind, an item or nothing, with
    /// another element put, with a conditional insert, on grove version
    /// 4 or 5, and with another answer of the candidate.
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
                Known::family_of_refusal(&over_indexed, &refused),
                family.filter(|_| on_a_kept_version)
            );

            // A key that holds another tree, an item or nothing; another
            // element put over the indexed tree; an insert that is made
            // only where the key holds nothing.
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
            ] {
                let over = under(&elsewhere);
                assert_eq!(over, Vec::<Path>::new(), "{elsewhere:?}");
                assert_eq!(Known::family_of_refusal(&over, &refused), None);
                let mut known = Known::default();
                known.came_to_indexed_over_indexed(&over);
                assert!(known.counts().is_empty());
            }
        }

        // Another answer of the candidate at such a put is of no family.
        let over = vec![path(&[b"t", b"indexed"])];
        assert_eq!(Known::family_of_refusal(&over, &refused), family);
        for other in [
            Outcome::Failed("StorageError::StorageError".to_string()),
            Outcome::Failed("InvalidBatchOperation".to_string()),
            Outcome::Failed("NotSupported::".to_string()),
            Outcome::Returned(Value::Nothing),
        ] {
            assert_eq!(Known::family_of_refusal(&over, &other), None, "{other:?}");
        }
        // The family has no place: no path is of it, and no stored record.
        let mut known = Known::default();
        known.came_to_indexed_over_indexed(&over);
        assert_eq!(known.counts().histories, [0, 0, 0, 1]);
        assert_eq!(known.family(&over[0]), None);
        let differs = [record(&[b"t", b"indexed"], 1)];
        let (was, is) = known.passed_over(7, &differs, &[]);
        assert_eq!((was.len(), is.len()), (1, 0));
        assert_eq!(known.counts().stored, [0; 4]);
        known.ended_at(Family::IndexedOverIndexed, 11);
        let counts = known.counts();
        assert_eq!(counts.answers, [0, 0, 0, 1]);
        assert_eq!(counts.answer_seed[3], Some(11));
        assert_eq!(
            counts.lines(),
            ["open difference, an indexed tree written over an indexed tree, grove versions 1 to \
              3: 1 cases made it, 0 with stored records that differ passed over, 1 ended at an \
              answer that differs (seed 0xb)"]
        );
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
        assert_eq!(known.family_of_answer(&get(&dropped)), None);
        known.pending(vec![foreign.clone(), dropped.clone()]);
        assert_eq!(known.family(&dropped), tree);
        assert_eq!(known.family_of_answer(&get(&dropped)), tree);
        assert_eq!(known.family_of_answer(&remake), tree);
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
            assert_eq!(known.family_of_answer(&get(&elsewhere)), None);
            let mut differs = vec![record(&[b"c"], 1), record(&[], 2)];
            differs[1].path = elsewhere.clone();
            let (was, is) = known.passed_over(7, &differs, &differs[..1]);
            assert_eq!((was.len(), is.len()), (2, 1), "{elsewhere:?}");
        }
        assert_eq!(known.counts().stored, [0; 4]);

        // A stored difference in the dropped tree's own subtree is passed
        // over and counted, but not in the record of its drop.
        let kept = [record(&[b"c"], 1), record(&[b"a", b"dropped"], 1)];
        let (was, is) = known.passed_over(7, &kept, &kept[..1]);
        assert_eq!((was.len(), is.len()), (1, 1));
        assert_eq!(known.counts().stored, [0, 0, 1, 0]);
        assert_eq!(known.counts().stored_seed, [None, None, Some(7), None]);
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
        assert_eq!(known.family_of_answer(&query(b"dropped", true)), tree);
        assert_eq!(known.family_of_answer(&query(b"dropped", false)), None);
        assert_eq!(known.family_of_answer(&query(b"beside", true)), None);

        // The record is reclaimed: the subtree is compared again.
        known.pending(vec![foreign]);
        assert_eq!(known.family(&dropped), None);
        assert_eq!(known.family_of_answer(&get(&dropped)), None);
        assert_eq!(known.family_of_answer(&remake), None);
        let (was, is) = known.passed_over(7, &kept, &kept[..1]);
        assert_eq!((was.len(), is.len()), (2, 1));
        assert_eq!(known.family(&below), Some(Family::BelowFlatDrop));
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
        assert_eq!(counts.stored, [1, 1, 0, 0]);
        assert_eq!(counts.stored_seed, [Some(7), Some(7), None, None]);
        assert_eq!((counts.histories, counts.answers), ([1, 1, 1, 0], [0; 4]));
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
            assert_eq!(known.counts().stored, [0; 4]);
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
        assert_eq!(known.family_of_answer(&empty(&[b"b", b"over"])), over);
        assert_eq!(known.family_of_answer(&get(&[b"b", b"over"], b"old")), over);
        assert_eq!(known.family_of_answer(&delete(&[b"b"], b"over")), over);
        assert_eq!(
            known.family_of_answer(&get(&[b"a", b"dropped", b"child"], b"k")),
            Some(Family::BelowFlatDrop)
        );
        assert_eq!(known.family_of_answer(&empty(&[b"b"])), None);
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
        assert_eq!(known.family_of_answer(&query(&[b"b"])), over);
        assert_eq!(known.family_of_answer(&query(&[])), None);
        assert_eq!(known.family_of_answer(&query(&[b"a"])), None);
        assert_eq!(known.family_of_answer(&query(&[b"c"])), None);
        assert_eq!(known.family_of_answer(&query(&[b"b", b"other"])), None);
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
        assert_eq!(known.family_of_answer(&alone(&[b"b"])), None);
        assert_eq!(known.family_of_answer(&alone(&[b"a", b"dropped"])), None);
        assert_eq!(known.family_of_answer(&alone(&[])), None);
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
            assert_eq!(known.family_of_answer(&asked), family, "{asked:?}");
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
        assert_eq!(known.family_of_answer(&branched(b"c", false)), over);
        assert_eq!(known.family_of_answer(&branched(b"b", false)), None);
        assert_eq!(known.family_of_answer(&branched(b"b", true)), None);
        assert_eq!(known.family_of_answer(&alone(&[b"b", b"over"])), over);
        assert_eq!(
            known.family_of_answer(&query(&[b"a", b"dropped"])),
            Some(Family::BelowFlatDrop)
        );
        assert_eq!(known.family_of_answer(&get(&[b"b"], b"other")), None);
        assert_eq!(known.family_of_answer(&delete(&[b"a"], b"dropped")), None);
        // A proof of a chunk names its subtree, and a move its new key.
        let trunk = |at: &[&[u8]]| {
            Op::Read(Read::Prove(Prove::TrunkChunk {
                path: path(at),
                max_depth: 2,
                min_depth: None,
            }))
        };
        assert_eq!(known.family_of_answer(&trunk(&[b"b", b"over"])), over);
        assert_eq!(known.family_of_answer(&trunk(&[b"b"])), None);
        let moved = |key: &[u8], new_key: &[u8]| {
            Op::Batch(vec![BatchOp::Move {
                path: path(&[b"b"]),
                key: key.to_vec(),
                new_key: new_key.to_vec(),
            }])
        };
        assert_eq!(known.family_of_answer(&moved(b"src", b"over")), over);
        assert_eq!(known.family_of_answer(&moved(b"src", b"other")), None);
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
        assert_eq!(known.family_of_answer(&Op::Commit), None);

        let mut counts = Counts::default();
        let mut one = self::known();
        one.ended_at(Family::WrittenOver, 9);
        counts.add(&one.counts());
        counts.add(&one.counts());
        assert_eq!(counts.answers, [2, 0, 0, 0]);
        assert_eq!(counts.answer_seed, [Some(9), None, None, None]);
        assert_eq!(counts.answer_differences(), 2);
        assert_eq!(counts.histories, [2, 2, 2, 0]);
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
