//! The open differences of the two engines that are ruled and named, and
//! what the fuzzer does where a case comes to one (GROVE-10).
//!
//! RocksDB leaves records behind where GroveDB changes one element and
//! does not clear what hung under it, and reads them again later. LigninDB
//! drops a subtree with its element. Two such histories are ruled as open
//! differences that are put to the maintainer of GroveDB, and are no
//! defect of an engine until that is answered. Each is a [`Family`], with a
//! rule that is a path:
//!
//! - [`Family::WrittenOver`]: an element was put over a tree that held
//!   elements. The family is the subtree of that tree, and every subtree
//!   below it.
//! - [`Family::BelowFlatDrop`]: a tree was dropped flat while it held a
//!   tree, against its caller's word. The family is every subtree below
//!   the dropped tree, and not the tree's own.
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
//!   A query names the subtree it is of and, since it can go down by its
//!   subqueries, every place of a family below that subtree. A difference
//!   at any other operation ends the run, as it always did.
//!
//! The counts are in the [`Tally`](super::pair::Tally) of a run
//! ([`Counts`]), with the seed of the first case of each kind.

use grovedb_version::version::GroveVersion;

use super::state::Record;
use crate::workload::{BatchOp, Db, Op, Path, Read, StreamEngine, TransactionOp, Tx, Write};

/// A named open difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// An element put over a tree that held elements.
    WrittenOver,
    /// What is below a tree that was dropped flat.
    BelowFlatDrop,
}

impl Family {
    /// Every family.
    pub const ALL: [Family; 2] = [Family::WrittenOver, Family::BelowFlatDrop];

    /// What a report calls the family.
    pub fn name(self) -> &'static str {
        match self {
            Family::WrittenOver => "an element written over a tree that held elements",
            Family::BelowFlatDrop => "below a tree that was dropped flat",
        }
    }

    fn index(self) -> usize {
        match self {
            Family::WrittenOver => 0,
            Family::BelowFlatDrop => 1,
        }
    }
}

/// What a run met of the families, by [`Family::ALL`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// The cases that made the history of the family: an element put over
    /// a tree that held elements, or a flat drop.
    pub histories: [u64; 2],
    /// The cases in which the engines stored different records in a
    /// subtree of the family, which were passed over.
    pub stored: [u64; 2],
    /// The cases that ended at an answer the engines gave differently at
    /// an operation that names a place of the family.
    pub answers: [u64; 2],
    /// The seed of the first case counted in `stored`.
    pub stored_seed: [Option<u64>; 2],
    /// The seed of the first case counted in `answers`.
    pub answer_seed: [Option<u64>; 2],
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
    stored: [bool; 2],
    counts: Counts,
}

fn joined(path: &Path, key: &[u8]) -> Path {
    let mut joined = path.clone();
    joined.push(key.to_vec());
    joined
}

/// The places at which `op` puts an element.
fn puts(op: &Op) -> Vec<Path> {
    let mut puts = Vec::new();
    if let Op::Write(
        Write::Insert { path, key, .. }
        | Write::InsertIfNotExists { path, key, .. }
        | Write::InsertIfChanged { path, key, .. }
        | Write::IndexedInsert { path, key, .. },
    ) = op
    {
        puts.push(joined(path, key));
    }
    for batch_op in op.batch_ops() {
        if let BatchOp::InsertOrReplace { path, key, .. }
        | BatchOp::InsertOnly { path, key, .. }
        | BatchOp::InsertIfNotExists { path, key, .. }
        | BatchOp::Replace { path, key, .. }
        | BatchOp::Patch { path, key, .. } = batch_op
        {
            puts.push(joined(path, key));
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
        // A proof is of the whole grove from the root, and an aux record
        // is under no path: neither names a place.
        Read::Prove(_) | Read::GetAux { .. } | Read::AuxByPrefix { .. } => {}
    }
}

/// The subtrees the queries of `read` are of.
fn query_places(read: &Read) -> Vec<Path> {
    match read {
        Read::Query(query) => vec![query.path.clone()],
        Read::PathQuery { query, .. }
        | Read::Rows { query, .. }
        | Read::KeysOptional { query, .. }
        | Read::ItemValues { query, .. }
        | Read::SumValues { query }
        | Read::BigSum { query, .. } => vec![query.path.clone()],
        Read::SumBudget { query, .. } => vec![query.path.clone()],
        Read::Many { queries } => queries.iter().map(|query| query.path.clone()).collect(),
        Read::Get { .. }
        | Read::HasRaw { .. }
        | Read::Fetch { .. }
        | Read::IsEmptyTree { .. }
        | Read::Tree { .. }
        | Read::Prove(_)
        | Read::GetAux { .. }
        | Read::AuxByPrefix { .. } => Vec::new(),
    }
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
            | BatchOp::Move { path, key, .. }
            | BatchOp::DropFlat { path, key, .. } => places.push(joined(path, key)),
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
        }
        self.flat_drops.extend(drops);
    }

    /// The family the subtree at `path` is of.
    pub fn family(&self, path: &[Vec<u8>]) -> Option<Family> {
        if self.over.iter().any(|over| path.starts_with(over)) {
            return Some(Family::WrittenOver);
        }
        let below = |drop: &Path| path.len() > drop.len() && path.starts_with(drop);
        self.flat_drops
            .iter()
            .any(below)
            .then_some(Family::BelowFlatDrop)
    }

    /// The family that has a place at `path` or below it.
    fn family_below(&self, path: &[Vec<u8>]) -> Option<Family> {
        if self.over.iter().any(|over| over.starts_with(path)) {
            return Some(Family::WrittenOver);
        }
        self.flat_drops
            .iter()
            .any(|drop| drop.starts_with(path))
            .then_some(Family::BelowFlatDrop)
    }

    /// The family an answer to `op` is of: that of the first place the
    /// operation names that is of one, and for a query, which goes down
    /// by its subqueries, also that of a place below its subtree.
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
            let queried = read.map(query_places).unwrap_or_default();
            queried.iter().find_map(|place| self.family_below(place))
        })
    }

    /// The records of `listed` that are of no family, and for each family
    /// those that are of it.
    fn split(&self, listed: &[Record]) -> (Vec<Record>, [Vec<Record>; 2]) {
        let mut plain = Vec::new();
        let mut of_family = [Vec::new(), Vec::new()];
        for record in listed {
            match self.family(&record.path) {
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
    use grovedb::Element;

    use super::*;
    use crate::diff::state::Keyspace;

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
        // The dropped tree's own subtree is the drain's, and is compared.
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
        assert_eq!(
            (counts.stored, counts.stored_seed),
            ([1, 1], [Some(7), Some(7)])
        );
        assert_eq!((counts.histories, counts.answers), ([1, 1], [0, 0]));

        // Elsewhere: the dropped tree's own subtree, and a subtree beside.
        for elsewhere in [
            record(&[b"a", b"dropped"], 2),
            record(&[b"a", b"beside"], 2),
        ] {
            let mut known = self::known();
            let reference = [same.clone(), elsewhere.clone()];
            let (was, is) = known.passed_over(7, &reference, &candidate);
            assert_eq!(was, vec![same.clone(), elsewhere]);
            assert_eq!(is, vec![same.clone()]);
            assert_eq!(known.counts().stored, [0, 0]);
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
        // A query of a subtree above goes down to the place by a subquery.
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
        assert_eq!(known.family_of_answer(&query(&[])), over);
        assert_eq!(
            known.family_of_answer(&query(&[b"a"])),
            Some(Family::BelowFlatDrop)
        );
        assert_eq!(known.family_of_answer(&query(&[b"c"])), None);
        assert_eq!(known.family_of_answer(&query(&[b"b", b"other"])), None);
        assert_eq!(known.family_of_answer(&get(&[b"b"], b"other")), None);
        assert_eq!(known.family_of_answer(&delete(&[b"a"], b"dropped")), None);
        assert_eq!(known.family_of_answer(&Op::Commit), None);

        let mut counts = Counts::default();
        let mut one = self::known();
        one.ended_at(Family::WrittenOver, 9);
        counts.add(&one.counts());
        counts.add(&one.counts());
        assert_eq!(
            (counts.answers, counts.answer_seed),
            ([2, 0], [Some(9), None])
        );
        assert_eq!(counts.histories, [2, 2]);
        let lines = counts.lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with(
            "open difference, an element written over a tree that held elements: 2 cases made \
             it, 0 with stored records that differ passed over, 2 ended at an answer that \
             differs (seed 0x9)"
        ));
        assert!(Counts::default().lines().is_empty());
    }
}
