//! Random histories of write transactions that enter, write, clear, drop
//! and move subtrees beside one another, leave unlinked records in their
//! Merks and state subtrees anew, with rollbacks, cursors that are kept
//! from call to call and reads that fail, on a store on disk, against the
//! model (`transactions.md` T-MV7).
//!
//! Two transactions stay open, and others are made, written and committed
//! between their calls. A history is a number of steps: a step draws what
//! is done next, which is one call, several, or none where what it drew
//! cannot be made.
//!
//! A transaction is read whole and held against the model after each
//! batch that is applied through it, after a rollback, after a look at
//! the commits that failed, and after some of the commits beside it.
//! Where its writes are laid out, or no move of it decides what is under
//! a subtree, that is each record and each node the model has, by its key
//! and by a cursor, and, where no commit has met the transaction and its
//! writes are laid out, each subtree as the directory shows it. Where it
//! serves its records by key and a move decides what is under a subtree,
//! every read there is refused: each key that the model has there, and
//! one that no history puts a record under, by key, and the first seek of
//! a cursor over the records, and, for the data under a prefix of another
//! kind than `BARE`, of a cursor over the nodes of a Merk. That is read
//! in every column of every subtree that is entered, in the committed
//! state or for the transaction, and under every prefix that holds a
//! record or a write of the transaction or that a move of it covers.
//! Each refusal is held against its kind: a conflict, the refusal of a
//! layout that stopped, or the refusal of a cursor of another version.
//! The first and the last are also held against the subtree they name.
//!
//! A cursor that is kept is read at later calls, and each of its answers
//! is held against the records of the version it was opened on with the
//! writes of its transaction over them. A read that the history has fail
//! is taken for that only where the model has the call served: a refusal
//! for a move comes first, and the read can fail at a later call of the
//! cursor. Every commit, also one that is refused because the writes
//! cannot be laid out, is held against the model by its outcome, and the
//! committed state is read after it: the records and the Merks that the
//! model has, and, under every prefix the history has named, the
//! directory as a transaction that wrote nothing is shown it. The store
//! is checked when the history ends.
//!
//! A Merk whose tree has a node is written nodes that its stated root
//! does not reach, under keys of their own: the engine keeps each as an
//! unlinked record, the model as a node of its key, and a move takes
//! them with their Merk. A subtree is stated anew with the tree type and
//! the place it has, which changes nothing that is read. A batch that
//! moves a subtree and states one anew that its moves name is refused,
//! and so is one that states anew by key a subtree that a move decides.
//! A history makes the first before some of its batches that move a
//! subtree, and the second after some of its writes of records by key,
//! where a move decides the subtree of those records, and holds each
//! refusal against its kind.
//!
//! A history follows from its seed alone, and one that fails prints the
//! calls it made.
//!
//! A history has none of these: a batch that writes an unlinked record
//! into a Merk with no node, or a commit that leaves one there (a
//! transaction that a commit has met can read one: it removed the nodes
//! of a Merk, and the commit left a record in it); a subtree that is
//! stated anew with another tree type or at
//! another place; an enter of a subtree if it is absent; a
//! commit of a Merk with no node that has a subtree below it, which is no
//! state of a grove; a move to another Merk than the one that hosted the
//! subtree; a write that keeps the value of a node by the handle that a
//! read through a cursor gave; a scope that is open beside another, or
//! one that is rolled back; a call of another thread. No history asks
//! whether a transaction watches the commits.

use std::{ops::Bound, rc::Rc};

use super::{
    model::{
        self, handle_of, Committed, Hosts, NodeRec, OwnedBatch, OwnedMerk, OwnedSubtree, Records,
        Val,
    },
    store::{Db, Outcome, Rng, TREE_TYPE},
    subtrees::Items,
    *,
};

/// The kinds of subtree a history enters, which the first byte of a
/// prefix says: a Merk with no host; a Merk under a node that names it;
/// and a subtree with no node, under a node that names no Merk.
const TOP: u8 = 0xe0;
const POINTED: u8 = 0xe1;
const BARE: u8 = 0xe2;

/// The prefix of a subtree of `kind`. `serial` says under which key of
/// its host's Merk it hangs ([`host_key`]), and `renamed` tells the
/// prefixes that moves gave one subtree apart.
fn prefix(kind: u8, serial: u32, renamed: u32) -> Subtree {
    let mut bytes = [0; Subtree::LEN];
    bytes[0] = kind;
    bytes[1..5].copy_from_slice(&serial.to_be_bytes());
    bytes[5..9].copy_from_slice(&renamed.to_be_bytes());
    Subtree::new(bytes)
}

fn kind(subtree: &Subtree) -> u8 {
    subtree.as_bytes()[0]
}

/// A key that no history puts a record under, in any column.
const ABSENT: &[u8] = b"z";

/// The first byte of the keys that a history writes unlinked records
/// under. No node of a tree has such a key.
const UNLINKED: u8 = b'u';

/// Whether `key` is one that a history writes unlinked records under.
fn is_unlinked(key: &[u8]) -> bool {
    key.first() == Some(&UNLINKED)
}

/// The key of the node that hosts `subtree` in the Merk above it.
fn host_key(subtree: &Subtree) -> Vec<u8> {
    let mut key = vec![b'h'];
    key.extend_from_slice(&subtree.as_bytes()[1..5]);
    key
}

fn tag(subtree: &Subtree) -> String {
    let bytes = subtree.as_bytes();
    let number = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
    let letter = match bytes[0] {
        TOP => 'T',
        POINTED => 'P',
        _ => 'B',
    };
    format!("{letter}{}.{}", number(1), number(5))
}

/// The writes of flat records `records`, for the calls of a history.
fn told(records: &[Write]) -> String {
    let told = records.iter().map(|write| match write {
        Write::Put(unit, key, _) => format!("put {:?} {key:?}", unit.column),
        Write::Delete(unit, key) => format!("delete {:?} {key:?}", unit.column),
        Write::Range(unit, key, end) => format!("delete {:?} {key:?}..{end:?}", unit.column),
        other => format!("{other:?}"),
    });
    told.collect::<Vec<_>>().join(", ")
}

fn depth(hosts: &Hosts, subtree: Subtree) -> usize {
    let above = std::iter::successors(hosts.get(&subtree), |host| hosts.get(*host));
    above.take(16).count()
}

/// The kinds of refusal a history expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// A conflict: a commit has met the transaction.
    Conflict,
    /// The writes are laid out, over another version than the reader's.
    Beside,
    /// The refusal that stops the layout of the writes.
    Layout,
    /// A read that the history had fail.
    Read,
}

fn refusal(error: &TxnError) -> Refusal {
    match error {
        TxnError::Batch(_) if error.is_conflict() => Refusal::Conflict,
        TxnError::Batch(BatchError::MoveBeside { .. }) => Refusal::Beside,
        TxnError::Batch(_) => Refusal::Layout,
        TxnError::Damaged(_) => Refusal::Read,
        other => panic!("{other}: no refusal that a history expects"),
    }
}

/// Holds `error`, the refusal of a call where a move decides what is
/// under `subtree`, against the kind that the model expects. A conflict
/// there is the refusal of a move by key, and that one, as the refusal of
/// a cursor of another version, names the subtree.
fn refused_as(error: &TxnError, expected: Refusal, subtree: Subtree) {
    assert_eq!(refusal(error), expected, "{error}");
    match error {
        TxnError::Batch(
            BatchError::MovedByKey { subtree: named } | BatchError::MoveBeside { subtree: named },
        ) => assert_eq!(*named, subtree, "{error}"),
        _ => assert_ne!(expected, Refusal::Conflict, "{error}"),
    }
}

/// The two reads that a history has fail, by what each says.
const FAILED_PLACE: &str = "a change of prefix that a test refuses";
const FAILED_NODE: &str = "a read of a node that a test refuses";

/// What the histories drew, and what they compared.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    pub histories: u64,
    pub steps: u64,
    pub batches: u64,
    pub commits: u64,
    pub conflicts: u64,
    /// Commits of a transaction whose writes cannot be laid out, and that
    /// no commit has met: each is refused.
    pub refused_commits: u64,
    pub enters: u64,
    pub drops: u64,
    pub clears: u64,
    /// Clears of a Merk with no node, and those of them that left a
    /// subtree entered below it.
    pub clears_of_no_node: u64,
    pub clears_that_kept: u64,
    pub moves: u64,
    /// Moves to the prefix of a subtree that the batch of the move drops.
    pub moves_onto_a_drop: u64,
    pub swaps: u64,
    /// Moves of a subtree that the transaction had cleared, or had
    /// cleared a Merk above or below, and clears where it had moved one.
    pub clear_then_move: u64,
    pub move_then_clear: u64,
    /// Pairs that a move stated for a subtree which was not there, and
    /// subtrees that a commit entered under the second prefix of a pair.
    pub pairs_for_none: u64,
    pub targets_taken: u64,
    pub rollbacks: u64,
    pub rewinds: u64,
    /// Rollbacks to a mark after which a cursor made the first read.
    pub cursor_first_after_rewind: u64,
    /// Reads through cursors that were kept; those of a version that is
    /// not the latest; and those that were refused, by kind.
    pub cursor_reads: u64,
    pub cursor_reads_behind: u64,
    pub refused_beside: u64,
    pub refused_conflict: u64,
    pub refused_layout: u64,
    /// Those of the refused reads that were made while the history had
    /// the next read of the cursor fail.
    pub refused_before_a_failed_read: u64,
    /// Reads that two cursors of one version made one after the other.
    pub twin_reads: u64,
    /// Reads that failed where the history had them fail.
    pub failed_looks: u64,
    pub failed_places: u64,
    pub failed_nodes: u64,
    /// Whole reads of a transaction by key, and those of them of one that
    /// a commit had met, or whose layout was stopped, with a move.
    pub checks: u64,
    pub checks_by_key_with_a_move: u64,
    /// Those of the whole reads that were made between two batches of
    /// one step, or of a transaction that commits one batch at a time.
    pub checks_between_batches: u64,
    /// Reads of the directory, prefix by prefix, after a commit.
    pub directory_checks: u64,
    /// Nodes that were written out of the tree of their Merk, and
    /// unlinked records that were deleted by their keys.
    pub unlinked_puts: u64,
    pub unlinked_deletes: u64,
    /// Batches that removed the last node of a tree and deleted the
    /// unlinked records of its Merk with it.
    pub emptied_with_unlinked: u64,
    /// Moves and changes of place of a subtree that held an unlinked
    /// record, itself or in a subtree that went with it.
    pub moves_of_unlinked: u64,
    /// Reads of an unlinked record through a cursor that was kept.
    pub cursor_reads_of_unlinked: u64,
    /// Subtrees that were stated anew, and those of them that a move of
    /// the transaction had brought to the prefix that was named.
    pub stated: u64,
    pub stated_after_a_move: u64,
    /// Batches that were refused: a move with a subtree that it names
    /// stated anew, and a subtree that a move decides stated anew by key.
    pub stated_beside_a_move: u64,
    pub stated_by_key: u64,
    /// The histories that had, at least once: a clear and a move of one
    /// transaction that touch, in either order; a read through a cursor
    /// of a version that commits have left behind; a rollback to a mark
    /// after which a cursor made the first read; reads of two cursors of
    /// one version, one after the other; and a read that failed between
    /// two calls.
    pub with_clear_and_move: u64,
    pub with_cursor_behind: u64,
    pub with_cursor_first_after_rewind: u64,
    pub with_twin_reads: u64,
    pub with_failed_read: u64,
    /// And those that had a move of a subtree with an unlinked record.
    pub with_unlinked_moved: u64,
}

impl Tally {
    pub(crate) fn add(&mut self, other: &Self) {
        macro_rules! sum {
            ($($field:ident),*) => { $(self.$field += other.$field;)* };
        }
        sum!(
            histories,
            steps,
            batches,
            commits,
            conflicts,
            refused_commits,
            enters,
            drops,
            clears,
            clears_of_no_node,
            clears_that_kept,
            moves,
            moves_onto_a_drop,
            swaps,
            clear_then_move,
            move_then_clear,
            pairs_for_none,
            targets_taken,
            rollbacks,
            rewinds,
            cursor_first_after_rewind,
            cursor_reads,
            cursor_reads_behind,
            refused_beside,
            refused_conflict,
            refused_layout,
            refused_before_a_failed_read,
            twin_reads,
            failed_looks,
            failed_places,
            failed_nodes,
            checks,
            checks_by_key_with_a_move,
            checks_between_batches,
            directory_checks,
            unlinked_puts,
            unlinked_deletes,
            emptied_with_unlinked,
            moves_of_unlinked,
            cursor_reads_of_unlinked,
            stated,
            stated_after_a_move,
            stated_beside_a_move,
            stated_by_key,
            with_clear_and_move,
            with_cursor_behind,
            with_cursor_first_after_rewind,
            with_twin_reads,
            with_failed_read,
            with_unlinked_moved
        );
    }
}

/// The calls of one history, which are printed where it fails.
struct Calls {
    seed: u64,
    made: Vec<String>,
    /// What was being read when the history failed.
    reading: String,
}

impl Drop for Calls {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("the history of seed {}:", self.seed);
            for (at, call) in self.made.iter().enumerate() {
                eprintln!("{at:5} {call}");
            }
            eprintln!("      while reading {}", self.reading);
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum Reader<'t, 's> {
    Flat(Cursor<'t, 's>),
    Merk(MerkCursor<'t, 's>),
}

/// A cursor that a history keeps, with the committed state of the version
/// it was opened on.
struct Kept<'t, 's> {
    tx: usize,
    unit: Unit,
    reader: Reader<'t, 's>,
    pin: u64,
    records: Rc<Records>,
    hosts: Rc<Hosts>,
    /// The key it stands on, where the history knows it.
    at: Option<Vec<u8>>,
    /// Whether its next change of prefix fails, and its next read of a
    /// node of its own version.
    place: bool,
    node: bool,
    /// Cursors that were opened together have the same number.
    twin: u64,
}

#[derive(Debug, Clone)]
enum Step {
    Seek(Vec<u8>),
    First,
    Last,
    Next,
    Prev,
    Read,
}

/// The nodes that a batch writes out of the tree of a Merk, each under
/// its key, and the keys of the unlinked records it deletes.
type Unlinked = (Vec<(Vec<u8>, NodeRec)>, Vec<Vec<u8>>);

/// The Merks one batch changes, each with the nodes its tree had and the
/// items it is to have, and what else the batch states.
#[derive(Default)]
struct Draft {
    merks: BTreeMap<Subtree, (BTreeMap<Vec<u8>, Val>, Items)>,
    /// The keys of the unlinked records that each of those Merks held.
    held: BTreeMap<Subtree, Vec<Vec<u8>>>,
    /// What the batch does to the unlinked records of a Merk.
    unlinked: BTreeMap<Subtree, Unlinked>,
    hosted: BTreeMap<Subtree, BTreeMap<Vec<u8>, Vec<Subtree>>>,
    subtrees: Vec<OwnedSubtree>,
    records: Vec<Write>,
    /// The subtrees it enters, each with the one above it.
    entered: Vec<(Subtree, Subtree)>,
    /// The Merk it clears, and the prefix it moves a subtree to.
    cleared: Option<Subtree>,
    moved: Option<Subtree>,
    /// The Merk it removes every node of.
    emptied: Option<Subtree>,
    what: String,
}

struct Run<'s> {
    world: World<'s>,
    rng: Rng,
    serials: u32,
    renames: u32,
    stamps: u64,
    twins: u64,
    tops: Vec<Subtree>,
    /// Every prefix the history named.
    named: Vec<Subtree>,
    /// Subtrees with no node that no transaction has entered, and whose
    /// host node a write put: a later batch enters one and writes no node.
    reserved: Vec<Subtree>,
    /// Subtrees that were entered under the host node of another prefix:
    /// by the subtree and the Merk that node is in, its key. Two
    /// transactions can each have a subtree under one prefix, below
    /// different Merks.
    loose: BTreeMap<(Subtree, Subtree), Vec<u8>>,
    /// The committed state of the latest version, for the cursors that
    /// are opened on it.
    latest: Option<(u64, Rc<Records>, Rc<Hosts>)>,
    marks: Vec<Vec<(Mark, usize)>>,
    /// Of each transaction: whether it was rolled back to a mark and has
    /// made no read since.
    rewound: Vec<bool>,
    tally: Tally,
    calls: Calls,
}

impl<'s> Run<'s> {
    fn call(&mut self, call: String) {
        self.calls.made.push(call);
    }

    fn stamp(&mut self) -> Vec<u8> {
        self.stamps += 1;
        self.stamps.to_be_bytes().to_vec()
    }

    fn fresh(&mut self, kind: u8) -> Subtree {
        self.serials += 1;
        let subtree = prefix(kind, self.serials, 0);
        self.named.push(subtree);
        subtree
    }

    /// Another prefix for `subtree`, which hangs under the same key.
    fn renamed(&mut self, subtree: &Subtree) -> Subtree {
        self.renames += 1;
        let bytes = subtree.as_bytes();
        let serial = u32::from_be_bytes(bytes[1..5].try_into().expect("four bytes"));
        let renamed = prefix(bytes[0], serial, self.renames);
        self.named.push(renamed);
        renamed
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> Option<T> {
        (!from.is_empty()).then(|| from[self.rng.below(from.len())])
    }

    fn hosts(&self, tx: &Tx<'s>) -> Hosts {
        tx.model.borrow().hosts(&self.world.model.hosts)
    }

    fn view(&self, tx: &Tx<'s>, merk: Subtree) -> BTreeMap<Vec<u8>, Val> {
        let model = tx.model.borrow();
        model.view(&self.world.model.records, Unit::data(merk))
    }

    /// The nodes of the tree of `merk`: its records but for the unlinked
    /// ones.
    fn tree(&self, tx: &Tx<'s>, merk: Subtree) -> BTreeMap<Vec<u8>, Val> {
        let mut tree = self.view(tx, merk);
        tree.retain(|key, _| !is_unlinked(key));
        tree
    }

    /// The items of the tree of `merk`.
    fn items(&self, tx: &Tx<'s>, merk: Subtree) -> Items {
        Committed::items(&self.tree(tx, merk))
    }

    /// Whether `merk` holds an unlinked record.
    fn holds_unlinked(&self, tx: &Tx<'s>, merk: Subtree) -> bool {
        self.view(tx, merk).keys().any(|key| is_unlinked(key))
    }

    /// Every subtree that is entered for a transaction whose subtrees hang
    /// as `hosts` has them.
    fn live(&self, hosts: &Hosts) -> Vec<Subtree> {
        let hosted = hosts.keys().copied();
        self.tops.iter().copied().chain(hosted).collect()
    }

    fn merks(&self, hosts: &Hosts) -> Vec<Subtree> {
        let merks = self.live(hosts).into_iter();
        merks.filter(|subtree| kind(subtree) != BARE).collect()
    }

    /// The key of the node that hosts `subtree`, where the subtrees hang
    /// as `hosts` has them: the key its prefix says, or the key it was
    /// entered under where it hangs in the Merk it was entered in so.
    fn key_of(&self, hosts: &Hosts, subtree: &Subtree) -> Vec<u8> {
        let above = hosts.get(subtree);
        let loose = above.and_then(|above| self.loose.get(&(*subtree, *above)));
        loose.cloned().unwrap_or_else(|| host_key(subtree))
    }

    /// Whether the node that hosts `subtree` is there.
    fn hosted(&self, tx: &Tx<'s>, hosts: &Hosts, subtree: Subtree) -> bool {
        match hosts.get(&subtree) {
            Some(above) => (self.items(tx, *above)).contains_key(&self.key_of(hosts, &subtree)),
            None => true,
        }
    }

    /// Whether a subtree below `subtree` has no host node: one below a
    /// Merk whose nodes the transaction removed.
    fn unhosted_below(&self, tx: &Tx<'s>, hosts: &Hosts, subtree: Subtree) -> bool {
        let below = model::below(hosts, subtree);
        below.iter().any(|below| !self.hosted(tx, hosts, *below))
    }

    /// Whether the writes of `tx` cannot be laid out over the committed
    /// state: a move of it is not made (`TxnModel::stopped`), or it
    /// enters a subtree under a prefix that a commit has entered one
    /// under since, or under an element of a subtree that is entered
    /// there no more. An enter writes no key, so the commit that did
    /// either has not met it.
    fn stopped(&self, tx: &Tx<'s>) -> bool {
        let model = tx.model.borrow();
        let mut hosts = self.world.model.hosts.clone();
        let mut refused = false;
        for (write, ..) in &model.writes {
            if let Write::Enter(subtree, Some(above)) = write {
                refused |= hosts.contains_key(subtree);
                refused |= kind(above) != TOP && !hosts.contains_key(above);
            }
            // A subtree that is stated anew is entered where the
            // statement is laid out, and a statement writes no key
            // either.
            if let Write::Stated(subtree) = write {
                refused |= kind(subtree) != TOP && !hosts.contains_key(subtree);
            }
            model::hosted(&mut hosts, write);
        }
        refused || model.stopped()
    }

    fn open<'d>(&self, draft: &'d mut Draft, tx: &Tx<'s>, merk: Subtree) -> &'d mut Items {
        let held = &mut draft.held;
        let entry = draft.merks.entry(merk).or_insert_with(|| {
            let mut tree = self.view(tx, merk);
            let records = tree.keys().filter(|key| is_unlinked(key)).cloned();
            held.insert(merk, records.collect());
            tree.retain(|key, _| !is_unlinked(key));
            let items = Committed::items(&tree);
            (tree, items)
        });
        &mut entry.1
    }

    /// Has the batch put the node that hosts `subtree` with another
    /// value, and the node that hosts the Merk that one is in, up to the
    /// Merk with no host. The node of a Merk names it; the node of a
    /// subtree with no node names none.
    fn hosts_above(&mut self, draft: &mut Draft, tx: &Tx<'s>, hosts: &Hosts, subtree: Subtree) {
        let mut at = subtree;
        while let Some(above) = hosts.get(&at).copied() {
            let (key, stamp) = (self.key_of(hosts, &at), self.stamp());
            self.open(draft, tx, above).insert(key.clone(), stamp);
            if kind(&at) == POINTED {
                let named = draft.hosted.entry(above).or_default();
                named.insert(key, vec![at]);
            }
            at = above;
        }
    }

    fn batch(&mut self, draft: Draft, hosts: &Hosts) -> OwnedBatch {
        let Draft {
            merks,
            mut held,
            mut unlinked,
            mut hosted,
            subtrees,
            mut records,
            entered,
            ..
        } = draft;
        let mut hosts = hosts.clone();
        hosts.extend(entered);
        let mut emptied = 0;
        let merks = merks.into_iter().map(|(subtree, (tree, items))| {
            let mut merk = OwnedMerk::change(subtree, &tree, &items);
            merk.hosted = hosted.remove(&subtree).unwrap_or_default();
            // The unlinked records are deleted by their keys, and a node
            // that the batch writes beside the tree, under a root that is
            // kept, becomes one. A Merk whose tree has no node afterwards
            // holds none: the batch deletes those it held.
            let held = held.remove(&subtree).unwrap_or_default();
            let (put, deleted) = unlinked.remove(&subtree).unwrap_or_default();
            if items.is_empty() {
                emptied += u64::from(!held.is_empty());
                merk.removed.extend(held);
            } else {
                merk.removed.extend(deleted);
                let put = put.into_iter();
                merk.nodes.extend(put.map(|(key, node)| (key, node, true)));
            }
            merk
        });
        let mut merks: Vec<OwnedMerk> = merks
            .filter(|merk| {
                !merk.nodes.is_empty() || !merk.removed.is_empty() || merk.root.is_some()
            })
            .collect();
        self.tally.emptied_with_unlinked += emptied;
        // A Merk comes before the Merk that hosts it.
        merks.sort_by_key(|merk| std::cmp::Reverse(depth(&hosts, merk.subtree)));
        // Every change of a Merk changes the hash of its root, which its
        // writer states in the element that hosts the Merk: two writers
        // of one Merk meet there. A Merk with no host has no such
        // element, and a record stands in for it (GM1, GM3).
        for merk in merks.iter().filter(|merk| kind(&merk.subtree) == TOP) {
            let unit = Unit::new(Column::Roots, merk.subtree);
            records.push(put(unit, b"root", &self.stamp()));
        }
        OwnedBatch {
            merks,
            records,
            subtrees,
        }
    }

    fn plan_nodes(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let merk = self.pick(&self.merks(hosts))?;
        let mut draft = Draft::default();
        for _ in 0..1 + self.rng.below(3) {
            let key = vec![b'n', self.rng.below(6) as u8];
            let value = vec![self.rng.below(250) as u8; 1 + self.rng.below(30)];
            let removes = self.rng.chance(30);
            let items = self.open(&mut draft, tx, merk);
            match removes {
                true => drop(items.remove(&key)),
                false => drop(items.insert(key, value)),
            }
        }
        self.hosts_above(&mut draft, tx, hosts, merk);
        draft.what = format!("nodes of {}", tag(&merk));
        Some(draft)
    }

    /// A node as a batch writes it out of the tree of its Merk: one with
    /// no child, and a value of its own.
    fn unlinked_node(&mut self) -> NodeRec {
        let items = BTreeMap::from([(vec![UNLINKED], self.stamp())]);
        let (_, mut records) = model::records(&items);
        records
            .remove(&[UNLINKED][..])
            .expect("the node of the item")
    }

    /// Nodes are written that the tree of their Merk does not reach, under
    /// a root that is kept, and unlinked records are deleted by their
    /// keys: in a Merk whose tree has a node.
    fn plan_unlinked(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let rooted: Vec<Subtree> = (self.merks(hosts).into_iter())
            .filter(|merk| !self.items(tx, *merk).is_empty())
            .collect();
        let merk = self.pick(&rooted)?;
        let mut draft = Draft::default();
        self.open(&mut draft, tx, merk);
        let held = draft.held[&merk].clone();
        let (mut put, mut deleted, mut told) = (Vec::new(), Vec::new(), Vec::new());
        let first = self.rng.below(3) as u8;
        let keys = [first, (first + 1) % 3].into_iter();
        for at in keys.take(1 + self.rng.below(2)) {
            let key = vec![UNLINKED, at];
            if held.contains(&key) && self.rng.chance(35) {
                told.push(format!("delete {key:?}"));
                deleted.push(key);
            } else {
                told.push(format!("put {key:?}"));
                put.push((key, self.unlinked_node()));
            }
        }
        self.tally.unlinked_puts += put.len() as u64;
        self.tally.unlinked_deletes += deleted.len() as u64;
        draft.unlinked.insert(merk, (put, deleted));
        self.hosts_above(&mut draft, tx, hosts, merk);
        draft.what = format!("unlinked records of {}: {}", tag(&merk), told.join(", "));
        Some(draft)
    }

    /// A subtree is stated anew, with the tree type and the place it has:
    /// often one that a move of the transaction brought to its prefix.
    fn plan_convert(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let live = self.live(hosts);
        let brought: Vec<Subtree> = {
            let model = tx.model.borrow();
            let moves = model.writes.iter().filter_map(|(write, ..)| match write {
                Write::Move(moved) => Some(moved),
                _ => None,
            });
            let brought = moves.flat_map(|moved| moved.pairs.iter().map(|(_, to)| *to));
            brought.filter(|to| live.contains(to)).collect()
        };
        let subtree = match self.rng.chance(50) {
            true if !brought.is_empty() => self.pick(&brought)?,
            _ => self.pick(&live)?,
        };
        let mut draft = Draft::default();
        let stated = OwnedSubtree::Convert(subtree, TREE_TYPE.to_vec(), None);
        draft.subtrees.push(stated);
        draft.what = format!("{} is stated anew", tag(&subtree));
        self.tally.stated += 1;
        self.tally.stated_after_a_move += u64::from(brought.contains(&subtree));
        Some(draft)
    }

    fn flat(&mut self, subtree: Subtree) -> Vec<Write> {
        let column = [Column::Aux, Column::Meta, Column::Roots][self.rng.below(3)];
        let unit = Unit::new(column, subtree);
        let mut written = BTreeSet::new();
        let mut records = Vec::new();
        for _ in 0..1 + self.rng.below(3) {
            let at = self.rng.below(6) as u8;
            if !written.insert(at) {
                continue;
            }
            let key = [b'f', at];
            records.push(match self.rng.below(10) {
                0..=5 => put(unit, &key, &self.stamp()),
                6..=8 => delete(unit, &key),
                _ => {
                    let end = self.rng.chance(70).then(|| vec![b'f', at + 2]);
                    Write::Range(unit, key.to_vec(), end)
                }
            });
        }
        records
    }

    fn plan_flat(&mut self, hosts: &Hosts) -> Option<Draft> {
        let subtree = self.pick(&self.live(hosts))?;
        let mut draft = Draft::default();
        draft.records = self.flat(subtree);
        draft.what = format!("records of {}: {}", tag(&subtree), told(&draft.records));
        Some(draft)
    }

    fn plan_enter(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let merks = self.merks(hosts).into_iter();
        let above: Vec<Subtree> = merks.filter(|merk| depth(hosts, *merk) < 3).collect();
        let above = self.pick(&above)?;
        let pointed = self.rng.chance(60);
        let new = self.fresh(match pointed {
            true => POINTED,
            false => BARE,
        });
        let mut draft = Draft::default();
        let stamp = self.stamp();
        self.open(&mut draft, tx, above)
            .insert(host_key(&new), stamp);
        if pointed {
            let named = draft.hosted.entry(above).or_default();
            named.insert(host_key(&new), vec![new]);
        }
        self.hosts_above(&mut draft, tx, hosts, above);
        let host = Some((above, host_key(&new), 0));
        draft.subtrees.push(OwnedSubtree::Enter(new, host));
        draft.entered.push((new, above));
        if pointed && self.rng.chance(70) {
            for _ in 0..1 + self.rng.below(3) {
                let key = vec![b'n', self.rng.below(6) as u8];
                let value = vec![self.rng.below(250) as u8; 1 + self.rng.below(30)];
                self.open(&mut draft, tx, new).insert(key, value);
            }
        }
        if self.rng.chance(60) {
            draft.records = self.flat(new);
        }
        draft.what = format!("enter {} under {}", tag(&new), tag(&above));
        self.tally.enters += 1;
        Some(draft)
    }

    /// The subtrees that a batch can drop. A Merk with no node hosts no
    /// Merk: one that the transaction emptied, with subtrees still
    /// entered below it, is not dropped, and no subtree is dropped with
    /// one such below it.
    fn droppable(&self, tx: &Tx<'s>, hosts: &Hosts) -> Vec<Subtree> {
        let hosts_none = |subtree: &Subtree| !hosts.values().any(|above| above == subtree);
        let dropped = hosts.keys().copied();
        dropped
            .filter(|subtree| hosts_none(subtree) || !self.items(tx, *subtree).is_empty())
            .filter(|subtree| !self.unhosted_below(tx, hosts, *subtree))
            .collect()
    }

    fn plan_drop(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let dropped = self.droppable(tx, hosts);
        let subtree = self.pick(&dropped)?;
        let above = hosts[&subtree];
        let mut draft = Draft::default();
        let key = self.key_of(hosts, &subtree);
        self.open(&mut draft, tx, above).remove(&key);
        self.hosts_above(&mut draft, tx, hosts, above);
        draft.subtrees.push(OwnedSubtree::Drop(subtree));
        draft.what = format!("drop {}", tag(&subtree));
        self.tally.drops += 1;
        Some(draft)
    }

    /// Whether `one` is `other`, or one of the two is below the other.
    fn in_line(hosts: &Hosts, one: Subtree, other: Subtree) -> bool {
        one == other
            || model::below(hosts, one).contains(&other)
            || model::below(hosts, other).contains(&one)
    }

    fn plan_clear(&mut self, tx: &Tx<'s>, hosts: &Hosts, of: Option<Subtree>) -> Option<Draft> {
        let merks = self.merks(hosts);
        let below: Vec<Subtree> = (merks.iter().copied())
            .filter(|merk| kind(merk) == POINTED)
            .collect();
        // Mostly a Merk under another: a clear of one with no host, where
        // it has a node, takes all that is below it.
        let merk = match of {
            Some(merk) => merk,
            None if self.rng.chance(85) && !below.is_empty() => self.pick(&below)?,
            None => self.pick(&merks)?,
        };
        // A clear of a Merk with a node takes what is below it, as a drop
        // does.
        if !self.items(tx, merk).is_empty() && self.unhosted_below(tx, hosts, merk) {
            return None;
        }
        let mut draft = Draft::default();
        self.hosts_above(&mut draft, tx, hosts, merk);
        draft.subtrees.push(OwnedSubtree::Clear(merk));
        draft.cleared = Some(merk);
        draft.what = format!("clear {}", tag(&merk));
        self.tally.clears += 1;
        if self.items(tx, merk).is_empty() {
            self.tally.clears_of_no_node += 1;
            let keeps = hosts.values().any(|above| *above == merk);
            self.tally.clears_that_kept += u64::from(keeps);
            draft.what.push_str(", which has no node");
        }
        let model = tx.model.borrow();
        let moved = model.writes.iter().any(|(write, ..)| match write {
            Write::Move(moved) => {
                (moved.pairs.iter()).any(|(_, to)| Self::in_line(hosts, *to, merk))
            }
            _ => false,
        });
        self.tally.move_then_clear += u64::from(moved);
        Some(draft)
    }

    /// Every node of a Merk is removed, and the subtrees below it stay
    /// entered until a later batch puts their host nodes again. It is a
    /// Merk whose nodes host subtrees with no node, and mostly one that
    /// hosts some: an element whose subtree has nodes is removed with its
    /// subtree, by a drop.
    fn plan_empty(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let merks = self.merks(hosts).into_iter();
        let hosts_nodes = |merk: &Subtree| {
            let mut below = hosts.iter().filter(|(_, above)| *above == merk);
            below.any(|(subtree, _)| !self.items(tx, *subtree).is_empty())
        };
        let full: Vec<Subtree> = merks
            .filter(|merk| !self.items(tx, *merk).is_empty() && !hosts_nodes(merk))
            .collect();
        let hosting: Vec<Subtree> = (full.iter().copied())
            .filter(|merk| hosts.values().any(|above| above == merk))
            .collect();
        let merk = match hosting.is_empty() {
            true => self.pick(&full)?,
            false => self.pick(&hosting)?,
        };
        let mut draft = Draft::default();
        self.open(&mut draft, tx, merk).clear();
        self.hosts_above(&mut draft, tx, hosts, merk);
        draft.emptied = Some(merk);
        draft.what = format!("every node of {} is removed", tag(&merk));
        Some(draft)
    }

    /// The host node of every subtree that has none is put again.
    fn plan_hosts(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let hosted = hosts.keys().copied();
        let unhosted: Vec<Subtree> = hosted
            .filter(|subtree| !self.hosted(tx, hosts, *subtree))
            .collect();
        if unhosted.is_empty() {
            return None;
        }
        let mut draft = Draft::default();
        for subtree in unhosted {
            self.hosts_above(&mut draft, tx, hosts, subtree);
        }
        draft.what = "the host nodes that were removed are put again".to_string();
        Some(draft)
    }

    /// The pairs for the subtrees below `subtree`, of which one with no
    /// node may be left out, and for subtrees that are not there.
    fn pairs(
        &mut self,
        tx: &Tx<'s>,
        hosts: &Hosts,
        subtree: Subtree,
        to: Subtree,
    ) -> Vec<(Subtree, Subtree)> {
        let mut pairs = Vec::new();
        let mut left = false;
        let under = model::below(hosts, subtree);
        for below in &under {
            if kind(below) == BARE && !left && self.rng.chance(12) {
                left = true;
                continue;
            }
            pairs.push((*below, self.renamed(below)));
        }
        for absent in self.reserved.clone() {
            let Some(merk) = self.reserved_in(tx, hosts, absent) else {
                continue;
            };
            if (merk == subtree || under.contains(&merk)) && self.rng.chance(60) {
                pairs.push((absent, self.renamed(&absent)));
                self.tally.pairs_for_none += 1;
            }
        }
        // A subtree that hangs under the host node of another prefix
        // hangs under it after the move as well.
        for (old, new) in &pairs {
            let key = self.key_of(hosts, old);
            if key != host_key(old) {
                let above = hosts[old];
                let moved = pairs.iter().find(|(from, _)| *from == above);
                let above = match above == subtree {
                    true => to,
                    false => moved.map_or(above, |(_, to)| *to),
                };
                self.loose.insert((*new, above), key);
            }
        }
        pairs
    }

    /// The Merk that has the host node of `absent`, which is not entered.
    fn reserved_in(&self, tx: &Tx<'s>, hosts: &Hosts, absent: Subtree) -> Option<Subtree> {
        if hosts.contains_key(&absent) {
            return None;
        }
        let merks = self.merks(hosts).into_iter();
        let key = host_key(&absent);
        merks
            .into_iter()
            .find(|merk| self.items(tx, *merk).contains_key(&key))
    }

    fn moved_after_a_clear(&mut self, tx: &Tx<'s>, hosts: &Hosts, subtree: Subtree) {
        let model = tx.model.borrow();
        let cleared = model.writes.iter().any(|(write, ..)| match write {
            Write::Leave(left) => left.cleared && Self::in_line(hosts, left.subtree, subtree),
            _ => false,
        });
        self.tally.clear_then_move += u64::from(cleared);
    }

    /// Counts a move of `subtree`, with the subtrees below it that
    /// `below` pairs, where one of them holds an unlinked record.
    fn moved_with_unlinked(&mut self, tx: &Tx<'s>, subtree: Subtree, below: &[(Subtree, Subtree)]) {
        let mut taken = std::iter::once(subtree).chain(below.iter().map(|(old, _)| *old));
        let carried = taken.any(|merk| kind(&merk) != BARE && self.holds_unlinked(tx, merk));
        self.tally.moves_of_unlinked += u64::from(carried);
    }

    fn movable(&self, tx: &Tx<'s>, hosts: &Hosts) -> Vec<Subtree> {
        let hosted = hosts.keys().copied();
        hosted
            .filter(|subtree| self.key_of(hosts, subtree) == host_key(subtree))
            .filter(|subtree| self.hosted(tx, hosts, *subtree))
            .collect()
    }

    fn plan_move(&mut self, tx: &Tx<'s>, hosts: &Hosts, of: Option<Subtree>) -> Option<Draft> {
        let movable = self.movable(tx, hosts);
        let subtree = match of {
            Some(subtree) => Some(subtree).filter(|subtree| movable.contains(subtree))?,
            None => self.pick(&movable)?,
        };
        let above = hosts[&subtree];
        // A subtree of the same kind beside it, under the key its prefix
        // says, which the batch of the move can drop.
        let beside: Vec<Subtree> = (self.droppable(tx, hosts).into_iter())
            .filter(|other| *other != subtree && hosts[other] == above)
            .filter(|other| kind(other) == kind(&subtree))
            .filter(|other| self.key_of(hosts, other) == host_key(other))
            .collect();
        // Under the same key, under another, or to the prefix of a
        // subtree beside it, which the batch drops.
        let mut dropped = None;
        let to = match self.rng.chance(50) {
            true => self.renamed(&subtree),
            false if !beside.is_empty() && self.rng.chance(30) => {
                dropped = self.pick(&beside);
                dropped.expect("a subtree beside it")
            }
            false => {
                self.serials += 1;
                self.renames += 1;
                let to = prefix(kind(&subtree), self.serials, self.renames);
                self.named.push(to);
                to
            }
        };
        let read = (tx.txn.node(above, &host_key(&subtree), handle_of)).expect("the host is read");
        let mut draft = Draft::default();
        if let Some(dropped) = dropped {
            draft.subtrees.push(OwnedSubtree::Drop(dropped));
            self.tally.moves_onto_a_drop += 1;
        }
        let stamp = self.stamp();
        let items = self.open(&mut draft, tx, above);
        items.remove(&host_key(&subtree));
        items.insert(host_key(&to), stamp);
        if kind(&to) == POINTED {
            let named = draft.hosted.entry(above).or_default();
            named.insert(host_key(&to), vec![to]);
        }
        self.hosts_above(&mut draft, tx, hosts, above);
        let below = self.pairs(tx, hosts, subtree, to);
        self.moved_with_unlinked(tx, subtree, &below);
        draft.what = format!(
            "move {} to {}{} with {:?}",
            tag(&subtree),
            tag(&to),
            dropped.map_or("", |_| ", which the batch drops"),
            below
                .iter()
                .map(|(old, new)| (tag(old), tag(new)))
                .collect::<Vec<_>>()
        );
        draft.subtrees.push(OwnedSubtree::Move {
            from: subtree,
            to,
            host: (above, host_key(&to), 0),
            read,
            below,
        });
        self.moved_after_a_clear(tx, hosts, subtree);
        self.tally.moves += 1;
        draft.moved = Some(to);
        Some(draft)
    }

    fn plan_swap(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let movable = self.movable(tx, hosts);
        let mut both = Vec::new();
        for (at, one) in movable.iter().enumerate() {
            for other in &movable[at + 1..] {
                if hosts[one] == hosts[other] && kind(one) == kind(other) {
                    both.push((*one, *other));
                }
            }
        }
        let (one, other) = self.pick(&both)?;
        let above = hosts[&one];
        let mut draft = Draft::default();
        for (from, to) in [(one, other), (other, one)] {
            let read = (tx.txn.node(above, &host_key(&from), handle_of)).expect("the host is read");
            let stamp = self.stamp();
            self.open(&mut draft, tx, above)
                .insert(host_key(&to), stamp);
            if kind(&to) == POINTED {
                let named = draft.hosted.entry(above).or_default();
                named.insert(host_key(&to), vec![to]);
            }
            let below = self.pairs(tx, hosts, from, to);
            self.moved_with_unlinked(tx, from, &below);
            draft.subtrees.push(OwnedSubtree::Move {
                from,
                to,
                host: (above, host_key(&to), 0),
                read,
                below,
            });
            self.moved_after_a_clear(tx, hosts, from);
        }
        self.hosts_above(&mut draft, tx, hosts, above);
        draft.what = format!("{} and {} change places", tag(&one), tag(&other));
        self.tally.swaps += 1;
        Some(draft)
    }

    /// The host node of a subtree that a later batch enters is put.
    fn plan_reserve(&mut self, tx: &Tx<'s>, hosts: &Hosts) -> Option<Draft> {
        let merk = self.pick(&self.merks(hosts))?;
        let absent = self.fresh(BARE);
        let mut draft = Draft::default();
        let stamp = self.stamp();
        self.open(&mut draft, tx, merk)
            .insert(host_key(&absent), stamp);
        self.hosts_above(&mut draft, tx, hosts, merk);
        self.reserved.push(absent);
        draft.what = format!("the host node of {} in {}", tag(&absent), tag(&merk));
        Some(draft)
    }

    /// A subtree is entered under a host node that is there, and no node
    /// is written: the one the node was put for, or one under the second
    /// prefix of a pair that a move of another transaction states.
    fn plan_enter_alone(
        &mut self,
        tx: &Tx<'s>,
        hosts: &Hosts,
        others: &[&Tx<'s>],
    ) -> Option<Draft> {
        let there = self.reserved.clone().into_iter();
        let there: Vec<(Subtree, Subtree)> = there
            .filter_map(|absent| Some((absent, self.reserved_in(tx, hosts, absent)?)))
            .collect();
        let (absent, merk) = self.pick(&there)?;
        self.reserved.retain(|reserved| *reserved != absent);
        // A prefix that a move of another transaction brings a subtree
        // with no node to, under another Merk than this one: which of the
        // two a subtree under that prefix is, the Merk above it tells. So
        // no subtree was entered under that prefix in this Merk before.
        let mut targets = Vec::new();
        for other in others {
            let model = other.model.borrow();
            let theirs = model.hosts(&self.world.model.hosts);
            for (write, ..) in &model.writes {
                if let Write::Move(moved) = write {
                    let free = |to: &Subtree| {
                        let entered = hosts.contains_key(to);
                        kind(to) == BARE && !entered && !self.loose.contains_key(&(*to, merk))
                    };
                    let apart = |from: &Subtree, to: &Subtree| {
                        let above = theirs.get(to).or(theirs.get(from));
                        above.is_some_and(|above| *above != merk)
                    };
                    let stated = moved.stated.iter();
                    let stated = stated.filter(|(from, to)| free(to) && apart(from, to));
                    targets.extend(stated.map(|(_, to)| *to));
                }
            }
        }
        let taken = self.pick(&targets).filter(|_| self.rng.chance(80));
        let new = taken.unwrap_or(absent);
        let mut draft = Draft::default();
        let host = Some((merk, host_key(&absent), 0));
        draft.subtrees.push(OwnedSubtree::Enter(new, host));
        draft.entered.push((new, merk));
        if taken.is_some() {
            self.loose.insert((new, merk), host_key(&absent));
            self.tally.targets_taken += 1;
        }
        draft.what = format!("enter {} under {} and write no node", tag(&new), tag(&merk));
        Some(draft)
    }

    /// One write of a transaction whose writes are laid out: one batch
    /// that is applied, or up to three that belong together, each after
    /// the batch that is refused before some of them
    /// ([`Self::stated_beside_a_move`]). It says whether one was made.
    fn write(&mut self, tx: &Tx<'s>, others: &[&Tx<'s>], who: &str) -> bool {
        let hosts = self.hosts(tx);
        let few = self.live(&hosts).len() < 14;
        let draft = match self.rng.below(112) {
            100..=108 => self.plan_unlinked(tx, &hosts),
            109..=111 => self.plan_convert(tx, &hosts),
            0..=13 => self.plan_nodes(tx, &hosts),
            14..=23 => self.plan_flat(&hosts),
            24..=39 if few => self.plan_enter(tx, &hosts),
            24..=45 => self.plan_drop(tx, &hosts),
            46..=54 => self.plan_clear(tx, &hosts, None),
            55..=60 => self.plan_empty(tx, &hosts),
            61..=64 => self.plan_hosts(tx, &hosts),
            65..=78 => self.plan_move(tx, &hosts, None),
            79..=84 => self.plan_swap(tx, &hosts),
            85..=91 => self.plan_reserve(tx, &hosts),
            _ => self.plan_enter_alone(tx, &hosts, others),
        };
        let Some(draft) = draft else {
            return false;
        };
        let (cleared, moved, emptied) = (draft.cleared, draft.moved, draft.emptied);
        self.apply(tx, draft, &hosts, who);
        // A clear is often followed by a move of the Merk, a move by a
        // clear of what was moved, and a Merk that lost its nodes is
        // often cleared before its host nodes are put again.
        let hosts = self.hosts(tx);
        let next = match (cleared, moved, emptied) {
            (Some(merk), ..) if self.rng.chance(40) => self.plan_move(tx, &hosts, Some(merk)),
            (_, Some(to), _) if kind(&to) == POINTED && self.rng.chance(35) => {
                self.plan_clear(tx, &hosts, Some(to))
            }
            (.., Some(merk)) if self.rng.chance(70) => self.plan_clear(tx, &hosts, Some(merk)),
            _ => None,
        };
        if let Some(next) = next {
            self.check_between(tx, who);
            let moved = next.cleared.filter(|_| emptied.is_some());
            self.apply(tx, next, &hosts, who);
            // And the Merk that was cleared with no node is often moved
            // before its host nodes are put again, or after.
            let hosts = self.hosts(tx);
            let after = match (moved, self.rng.below(3)) {
                (Some(_), 0) => self.plan_hosts(tx, &hosts),
                (Some(merk), 1) => self.plan_move(tx, &hosts, Some(merk)),
                _ => None,
            };
            if let Some(after) = after {
                self.check_between(tx, who);
                self.apply(tx, after, &hosts, who);
            }
        }
        true
    }

    /// Before some of the batches that move a subtree: the same batch
    /// with one more statement, of a subtree that its moves name, anew.
    /// It is refused for that, and changes nothing.
    fn stated_beside_a_move(&mut self, tx: &Tx<'s>, batch: &OwnedBatch, who: &str) {
        let mut named = Vec::new();
        for write in &batch.subtrees {
            if let OwnedSubtree::Move {
                from, to, below, ..
            } = write
            {
                named.extend([*from, *to]);
                named.extend(below.iter().flat_map(|(old, new)| [*old, *new]));
            }
        }
        if named.is_empty() || !self.rng.chance(25) {
            return;
        }
        let subtree = self.pick(&named).expect("a prefix that a move names");
        let mut refused = batch.clone();
        let at = self.rng.below(refused.subtrees.len() + 1);
        let stated = OwnedSubtree::Convert(subtree, TREE_TYPE.to_vec(), None);
        refused.subtrees.insert(at, stated);
        self.call(format!(
            "{who}: the next batch, with {} stated anew",
            tag(&subtree)
        ));
        match self.world.apply(tx, &refused) {
            Err(TxnError::Batch(BatchError::ConvertMoved { subtree: named })) => {
                assert_eq!(named, subtree, "the subtree that the refusal names");
            }
            other => panic!("a subtree that a move names is stated anew: {other:?}"),
        }
        self.tally.stated_beside_a_move += 1;
    }

    fn apply(&mut self, tx: &Tx<'s>, draft: Draft, hosts: &Hosts, who: &str) {
        let what = draft.what.clone();
        let batch = self.batch(draft, hosts);
        self.stated_beside_a_move(tx, &batch, who);
        self.call(format!("{who}: {what}"));
        if let Err(error) = self.world.apply(tx, &batch) {
            panic!("{who}: {what}: {error}\n{batch:?}");
        }
        self.tally.batches += 1;
    }

    /// What a transaction that a commit has met, or whose layout is
    /// stopped, is refused, and what it is not.
    fn write_by_key(&mut self, tx: &Tx<'s>, who: &str) {
        let (moves, met) = {
            let model = tx.model.borrow();
            (model.moves(), model.conflicts())
        };
        let expected = match met {
            true => Refusal::Conflict,
            false => Refusal::Layout,
        };
        let hosts = self.hosts(tx);
        let Some(subtree) = self.pick(&self.live(&hosts)) else {
            return;
        };
        let records = self.flat(subtree);
        let decided = moves && (tx.model.borrow()).decided(&self.world.model.hosts, subtree);
        let batch = OwnedBatch {
            records: records.clone(),
            ..OwnedBatch::default()
        };
        self.call(format!(
            "{who}: records of {}, by key: {}",
            tag(&subtree),
            told(&records)
        ));
        match (decided, self.world.apply_as_read(tx, &batch)) {
            (false, Ok(_)) => self.tally.batches += 1,
            (true, Err(error)) => refused_as(&error, expected, subtree),
            (decided, applied) => panic!("a move decides: {decided}, and {applied:?}"),
        }
        if decided {
            // Nor is a subtree that a move decides stated anew by key.
            let stated = OwnedBatch {
                subtrees: vec![OwnedSubtree::Convert(subtree, TREE_TYPE.to_vec(), None)],
                ..OwnedBatch::default()
            };
            self.call(format!("{who}: {} is stated anew, by key", tag(&subtree)));
            let refused = self.world.apply_as_read(tx, &stated);
            let refused = refused.expect_err("a statement by key where a move decides");
            refused_as(&refused, expected, subtree);
            self.tally.stated_by_key += 1;
        }
        if moves {
            // No subtree is dropped or cleared by key beside a move.
            let cleared = OwnedBatch {
                subtrees: vec![OwnedSubtree::Clear(self.tops[0])],
                ..OwnedBatch::default()
            };
            self.call(format!("{who}: a clear, by key"));
            let refused = self.world.apply_as_read(tx, &cleared);
            let refused = refused.expect_err("a clear by key beside a move");
            refused_as(&refused, expected, self.tops[0]);
        }
    }

    /// Reads `tx` whole between two batches of one step, or before the
    /// commit of a transaction that is made for it.
    fn check_between(&mut self, tx: &Tx<'s>, who: &str) {
        self.call(format!("{who}: is read whole"));
        self.tally.checks_between_batches += 1;
        self.check(tx, None);
    }

    /// Reads `tx` whole, by key and by cursors that are opened for it,
    /// and holds every answer against the model. With `index`, it is the
    /// read that ends a step of the transaction of that number.
    fn check(&mut self, tx: &Tx<'s>, index: Option<usize>) {
        if let Some(index) = index {
            self.rewound[index] = false;
            self.call(format!("tx {index}: is read whole"));
        }
        self.tally.checks += 1;
        let (moves, met, stopped) = {
            let model = tx.model.borrow();
            (model.moves(), model.conflicts(), self.stopped(tx))
        };
        // The transaction takes the commits in at a call. Whether one
        // of them has met it is held against the model, and not how many
        // of its writes are met: where the transaction took a subtree
        // out and then dropped or cleared one that the committed state
        // has above it, a commit that writes under the first meets both
        // for the engine, which asks the committed directory (CF5), and
        // the first alone for the model.
        let mut out = Vec::new();
        let nowhere = Unit::new(Column::Aux, prefix(BARE, 0, 0));
        let taken = tx.txn.get(nowhere, b"f0", &mut out);
        assert!(!taken.expect("a read of a prefix that no subtree is under"));
        let overlapped = tx.txn.stats().overlapped;
        assert_eq!(overlapped > 0, met, "whether a commit has met it");
        if !(moves && (met || stopped)) {
            for unit in self.world.units(tx) {
                self.calls.reading = format!("{:?} of {}", unit.column, tag(&unit.subtree));
                self.world.check_unit(tx, unit);
            }
            if !met && !stopped {
                self.check_subtrees(tx, index.is_some());
            }
            return;
        }
        self.tally.checks_by_key_with_a_move += 1;
        let expected = match met {
            true => Refusal::Conflict,
            false => Refusal::Layout,
        };
        // Every unit that holds a record or a write, or that a move
        // covers, and every column of each subtree that is entered and
        // that a move decides, whether it holds a record or not.
        let mut units = self.world.units(tx);
        {
            let model = tx.model.borrow();
            let committed = &self.world.model.hosts;
            let entered = self.live(committed).into_iter();
            let entered = entered.chain(self.live(&model.hosts(committed)));
            let decided = entered.filter(|subtree| model.decided(committed, *subtree));
            units.extend(
                decided.flat_map(|subtree| Column::ALL.map(|column| Unit::new(column, subtree))),
            );
        }
        for unit in units {
            let decided = (tx.model.borrow()).decided(&self.world.model.hosts, unit.subtree);
            self.calls.reading = format!("{:?} of {}", unit.column, tag(&unit.subtree));
            if !decided {
                self.world.check_unit(tx, unit);
                continue;
            }
            self.refused(tx, unit, expected);
        }
    }

    /// Makes every read of `unit` through `tx`, which serves its records
    /// by key and has a move that decides what is under the unit's
    /// subtree, and holds each refusal against `expected`: each key that
    /// the model has there, and one that no history puts a record under,
    /// by key, and the first seek of a cursor over the records, and, for
    /// the data under a prefix of another kind than `BARE`, of one over
    /// the nodes of a Merk.
    fn refused(&self, tx: &Tx<'s>, unit: Unit, expected: Refusal) {
        let view = tx.model.borrow().view(&self.world.model.records, unit);
        let mut keys: Vec<Vec<u8>> = view.keys().cloned().collect();
        assert!(!view.contains_key(ABSENT));
        keys.push(ABSENT.to_vec());
        for key in &keys {
            let mut out = Vec::new();
            let by_key = match unit.column {
                Column::Data => tx.txn.node(unit.subtree, key, |view| view.is_some()),
                _ => tx.txn.get(unit, key, &mut out),
            };
            let refused = by_key.expect_err("a read where a move decides, by key");
            refused_as(&refused, expected, unit.subtree);
        }
        let refused = tx.txn.cursor(unit).seek_to_first();
        let refused = refused.expect_err("a seek where a move decides");
        refused_as(&refused, expected, unit.subtree);
        if unit.column == Column::Data && kind(&unit.subtree) != BARE {
            let opened = tx.txn.merk_cursor(unit.subtree);
            let refused = opened.and_then(|mut cursor| cursor.seek_to_first());
            let refused = refused.expect_err("a seek over nodes where a move decides");
            refused_as(&refused, expected, unit.subtree);
        }
    }

    /// Holds what the directory shows `tx` of each subtree against the
    /// model, and, with `sampled`, of some prefixes that it has none
    /// under.
    fn check_subtrees(&mut self, tx: &Tx<'s>, sampled: bool) {
        let hosts = self.hosts(tx);
        let mut asked = self.live(&hosts);
        if sampled {
            for _ in 0..6 {
                asked.extend(self.pick(&self.named.clone()));
            }
        }
        self.shown(tx, &hosts, &asked);
    }

    /// Holds what the directory shows a transaction that has written
    /// nothing against the committed model, for every prefix the history
    /// has named: the subtrees that are entered, each with its root, its
    /// tree type and its host, and no subtree under every other prefix.
    fn check_directory(&mut self) {
        let reader = self.world.begin();
        let hosts = self.hosts(&reader);
        self.shown(&reader, &hosts, &self.named);
        self.tally.directory_checks += 1;
    }

    /// Holds what the directory shows `tx` under each prefix of `asked`
    /// against the model, where the subtrees hang as `hosts` has them.
    fn shown(&self, tx: &Tx<'s>, hosts: &Hosts, asked: &[Subtree]) {
        type Shown = Option<(Option<Vec<u8>>, Vec<u8>, Option<(Subtree, Vec<u8>, u8)>)>;
        let live = self.live(hosts);
        for subtree in asked {
            let shown: Shown = (tx.txn)
                .subtree(*subtree, |view| {
                    view.map(|view| {
                        let host = view.host;
                        let host =
                            host.map(|host| (host.subtree, host.key.to_vec(), host.position));
                        let root = view.root_key.map(<[u8]>::to_vec);
                        (root, view.tree_type.to_vec(), host)
                    })
                })
                .expect("the directory is read");
            let expected: Shown = live.contains(subtree).then(|| {
                let root = model::records(&self.items(tx, *subtree)).0;
                let above = hosts.get(subtree);
                let host = above.map(|above| (*above, self.key_of(hosts, subtree), 0));
                (root, TREE_TYPE.to_vec(), host)
            });
            assert_eq!(shown, expected, "the subtree {}", tag(subtree));
        }
    }

    fn latest(&mut self) -> (u64, Rc<Records>, Rc<Hosts>) {
        let version = self.world.model.version;
        match &self.latest {
            Some(latest) if latest.0 == version => latest.clone(),
            _ => {
                let records = Rc::new(self.world.model.records.clone());
                let hosts = Rc::new(self.world.model.hosts.clone());
                self.latest = Some((version, records, hosts));
                self.latest.clone().expect("it was made")
            }
        }
    }

    /// Opens a cursor through one of `txs`, or two over the same records,
    /// and keeps them.
    fn open_cursors<'t>(&mut self, txs: &'t [Tx<'s>], index: usize, kept: &mut Vec<Kept<'t, 's>>) {
        let tx = &txs[index];
        let hosts = self.hosts(tx);
        let mut over = self.live(&hosts);
        // And sometimes a prefix that the transaction has no subtree under.
        over.extend(self.pick(&self.named.clone()));
        let Some(subtree) = self.pick(&over) else {
            return;
        };
        let entered = hosts.contains_key(&subtree) || kind(&subtree) == TOP;
        let nodes = entered && kind(&subtree) != BARE && self.rng.chance(50);
        let unit = match nodes {
            true => Unit::data(subtree),
            false => {
                let column = [Column::Aux, Column::Meta, Column::Roots][self.rng.below(3)];
                Unit::new(column, subtree)
            }
        };
        let (pin, records, hosts) = self.latest();
        self.twins += 1;
        let both = 1 + usize::from(self.rng.chance(35));
        for _ in 0..both {
            let reader = match nodes {
                true => Reader::Merk(tx.txn.merk_cursor(subtree).expect("a cursor")),
                false => Reader::Flat(tx.txn.cursor(unit)),
            };
            kept.push(Kept {
                tx: index,
                unit,
                reader,
                pin,
                records: Rc::clone(&records),
                hosts: Rc::clone(&hosts),
                at: None,
                place: false,
                node: false,
                twin: self.twins,
            });
        }
        self.call(format!(
            "tx {index}: {both} cursor(s) {} over {unit} ({}) at version {pin}",
            self.twins,
            tag(&subtree)
        ));
        while kept.len() > 8 {
            kept.swap_remove(self.rng.below(kept.len()));
        }
    }

    /// The records a cursor shows now, or the refusal of its calls: the
    /// writes of its transaction over the committed records of its
    /// version (RD6), and no answer where a move decides and the writes
    /// are not laid out over that version (MV7).
    fn expected(
        &self,
        tx: &Tx<'s>,
        kept: &Kept<'_, 's>,
    ) -> Result<BTreeMap<Vec<u8>, Val>, Refusal> {
        let model = tx.model.borrow();
        let latest = self.world.model.version;
        if model.moves() && model.decided(&kept.hosts, kept.unit.subtree) {
            if model.conflicts() {
                return Err(Refusal::Conflict);
            }
            if self.stopped(tx) {
                return Err(Refusal::Layout);
            }
            if kept.pin != latest {
                return Err(Refusal::Beside);
            }
        }
        if kept.pin == latest {
            return Ok(model.view(&kept.records, kept.unit));
        }
        let mut over = model.clone();
        over.lay(&kept.hosts, &kept.records);
        Ok(over.view(&kept.records, kept.unit))
    }

    fn step(&mut self, at: bool) -> Step {
        match self.rng.below(100) {
            0..=24 => Step::Seek(match self.rng.below(5) {
                0 | 1 => vec![b'f', self.rng.below(6) as u8],
                2 | 3 => vec![b'n', self.rng.below(6) as u8],
                _ => vec![UNLINKED, self.rng.below(3) as u8],
            }),
            25..=34 => Step::First,
            35..=39 => Step::Last,
            40..=59 if at => Step::Next,
            60..=69 if at => Step::Prev,
            70..=99 if at => Step::Read,
            _ => Step::First,
        }
    }

    /// One call of a cursor that is kept, held against the model.
    fn read_cursor(&mut self, txs: &[Tx<'s>], kept: &mut Kept<'_, 's>, step: &Step) {
        let tx = &txs[kept.tx];
        let step = match (step, &kept.at) {
            (Step::Next | Step::Prev | Step::Read, None) => &Step::First,
            (step, _) => step,
        };
        if self.rng.chance(12) && !kept.place {
            kept.place = true;
            match &mut kept.reader {
                Reader::Flat(cursor) => cursor.refuse_place(),
                Reader::Merk(cursor) => cursor.refuse_place(),
            }
        }
        if let (Reader::Merk(cursor), false, true) =
            (&mut kept.reader, kept.node, self.rng.chance(5))
        {
            kept.node = true;
            cursor.refuse_node();
        }
        let expected = self.expected(tx, kept);
        let latest = self.world.model.version;
        self.tally.cursor_reads += 1;
        self.tally.cursor_reads_behind += u64::from(kept.pin != latest);
        if std::mem::take(&mut self.rewound[kept.tx]) {
            self.tally.cursor_first_after_rewind += 1;
        }
        self.call(format!(
            "tx {}: cursor {} over {unit} at version {}: {step:?}, from {:?}",
            kept.tx,
            kept.twin,
            kept.pin,
            kept.at,
            unit = kept.unit
        ));
        // Where the cursor comes to stand, for a call that moves it.
        let stands = |view: &BTreeMap<Vec<u8>, Val>| -> Option<Vec<u8>> {
            let from = kept.at.clone().unwrap_or_default();
            let found = match step {
                Step::Seek(key) => view.range(key.clone()..).next(),
                Step::First => view.iter().next(),
                Step::Last => view.iter().next_back(),
                Step::Next => view.range((Bound::Excluded(from), Bound::Unbounded)).next(),
                Step::Prev => view.range(..from).next_back(),
                Step::Read => unreachable!("a read moves no cursor"),
            };
            found.map(|(key, _)| key.clone())
        };
        let answer: Result<Option<Vec<u8>>, TxnError> = match (step, &mut kept.reader) {
            (Step::Read, Reader::Flat(cursor)) => {
                let key = kept.at.clone().expect("a key");
                let mut out = Vec::new();
                cursor.value(&mut out).map(|found| {
                    let view = expected.as_ref().expect("no refusal");
                    let record = match view.get(&key) {
                        Some(Val::Flat(value)) => Some(value.clone()),
                        Some(Val::Node(_)) => panic!("a node among records"),
                        None => None,
                    };
                    assert_eq!(found.then_some(out), record, "the record under {key:?}");
                    Some(key)
                })
            }
            (Step::Read, Reader::Merk(cursor)) => {
                let key = kept.at.clone().expect("a key");
                let view = expected.as_ref().ok();
                let node = view.map(|view| match view.get(&key) {
                    Some(Val::Node(node)) => Some(node),
                    Some(Val::Flat(_)) => panic!("a record among nodes"),
                    None => None,
                });
                // A committed unlinked record is shown as one. One that
                // the transaction wrote is shown so too, or from the
                // write set where the transaction serves it by key.
                let own = tx.model.borrow().own(kept.unit, &key).is_some();
                let committed = kept.pin == latest && !own;
                let read = cursor.node(|shown| {
                    let node = node.expect("no refusal");
                    match (node, shown) {
                        (Some(node), Some(shown)) => {
                            let (record, cell) = (shown.is_unlinked(), shown.handle().is_some());
                            assert!(
                                !is_unlinked(&key) || !committed || record,
                                "{key:?} is shown as no unlinked record"
                            );
                            assert!(
                                is_unlinked(&key) || !record,
                                "{key:?} is no unlinked record"
                            );
                            assert!(!is_unlinked(&key) || !cell, "{key:?} is no node of a tree");
                            node.check(&key, &shown);
                        }
                        (None, None) => {}
                        (node, shown) => panic!("{node:?} is expected, {shown:?} is shown"),
                    }
                });
                let record = is_unlinked(&key) && node.flatten().is_some() && read.is_ok();
                self.tally.cursor_reads_of_unlinked += u64::from(record);
                read.map(|()| Some(key))
            }
            (step, reader) => {
                macro_rules! moved {
                    ($cursor:expr) => {{
                        let moved = match step {
                            Step::Seek(key) => $cursor.seek(key),
                            Step::First => $cursor.seek_to_first(),
                            Step::Last => $cursor.seek_to_last(),
                            Step::Next => $cursor.next(),
                            _ => $cursor.prev(),
                        };
                        moved.map(|()| $cursor.key().map(<[u8]>::to_vec))
                    }};
                }
                let moved = match reader {
                    Reader::Flat(cursor) => moved!(cursor),
                    Reader::Merk(cursor) => moved!(cursor),
                };
                moved.inspect(|shown| {
                    let view = expected.as_ref().expect("no refusal");
                    assert_eq!(*shown, stands(view), "where the cursor stands");
                })
            }
        };
        match (answer, expected) {
            (Ok(at), Ok(_)) => kept.at = at,
            (Err(error), expected) if refusal(&error) == Refusal::Read => {
                let TxnError::Damaged(what) = &error else {
                    unreachable!("a read that failed");
                };
                let (place, node) = (*what == FAILED_PLACE, *what == FAILED_NODE);
                assert!(place && kept.place || node && kept.node, "{error}");
                // A refusal for a move comes before the read that fails,
                // and only the read of a node of the cursor's own version
                // fails as such a read.
                if let Err(expected) = expected {
                    panic!("{expected:?} is expected, and the read fails: {error}");
                }
                let nodes = matches!((step, &kept.reader), (Step::Read, Reader::Merk(_)));
                assert!(place || nodes, "{error}");
                kept.place &= !place;
                kept.node &= !node;
                self.tally.failed_places += u64::from(place);
                self.tally.failed_nodes += u64::from(node);
                kept.at = None;
            }
            (Err(error), Err(expected)) => {
                refused_as(&error, expected, kept.unit.subtree);
                match expected {
                    Refusal::Beside => self.tally.refused_beside += 1,
                    Refusal::Conflict => self.tally.refused_conflict += 1,
                    _ => self.tally.refused_layout += 1,
                }
                // A read that the history has fail is made at a later
                // call, which the model has served.
                let fails = u64::from(kept.place || kept.node);
                self.tally.refused_before_a_failed_read += fails;
                kept.at = None;
            }
            (Ok(at), Err(expected)) => {
                panic!("{expected:?} is expected, and the call is served: {at:?}")
            }
            (Err(error), Ok(_)) => panic!("the call is refused: {error}"),
        }
    }

    /// A few calls of cursors that are kept. Cursors that were opened
    /// together are asked for the same call one after the other; one of
    /// them that stands on no key makes a first seek in the place of a
    /// step or a read.
    fn read_cursors(&mut self, txs: &[Tx<'s>], kept: &mut [Kept<'_, 's>], of: Option<usize>) {
        for _ in 0..1 + self.rng.below(3) {
            let asked: Vec<usize> = (0..kept.len())
                .filter(|at| of.is_none_or(|of| kept[*at].tx == of))
                .collect();
            let Some(first) = self.pick(&asked) else {
                return;
            };
            let step = self.step(kept[first].at.is_some());
            self.read_cursor(txs, &mut kept[first], &step);
            let twin = kept[first].twin;
            let other = (0..kept.len()).find(|at| *at != first && kept[*at].twin == twin);
            if let Some(other) = other.filter(|_| self.rng.chance(70)) {
                self.read_cursor(txs, &mut kept[other], &step);
                self.tally.twin_reads += 1;
            }
        }
    }

    /// Has the next look of a transaction at the commits fail, and makes
    /// the call that looks.
    fn fail_look(&mut self, tx: &Tx<'s>, index: usize) {
        let writes = (tx.model.borrow().writes.iter())
            .any(|(write, ..)| !matches!(write, Write::Enter(..) | Write::Stated(_)));
        if !writes || tx.txn.looked_at() == Some(self.world.latest()) {
            return;
        }
        self.call(format!("tx {index}: a look that fails"));
        tx.txn.refuse_look();
        let mut out = Vec::new();
        let unit = Unit::new(Column::Aux, self.tops[0]);
        let refused = tx.txn.get(unit, b"f0", &mut out);
        let refused = refused.expect_err("the look fails");
        assert_eq!(refusal(&refused), Refusal::Read, "{refused}");
        self.tally.failed_looks += 1;
    }

    fn commit(&mut self, txs: &[Tx<'s>], index: usize) {
        let tx = &txs[index];
        let (met, stopped) = (tx.model.borrow().conflicts(), self.stopped(tx));
        if stopped && !met {
            // The commit is refused with what stops the layout, and the
            // transaction is as it was: the read that ends the step says.
            self.call(format!("tx {index}: a commit that is refused"));
            let refused = self.world.db.commit(self.world.versions, &tx.txn);
            let refused = refused.expect_err("the writes cannot be laid out");
            assert_eq!(refusal(&refused), Refusal::Layout, "{refused}");
            self.tally.refused_commits += 1;
            self.check(tx, Some(index));
            self.check_committed();
            return;
        }
        if !met {
            // An element is there for every subtree that is committed.
            let hosts = self.hosts(tx);
            if let Some(draft) = self.plan_hosts(tx, &hosts) {
                let who = format!("tx {index}");
                self.apply(tx, draft, &hosts, &who);
                self.check_between(tx, &who);
            }
        }
        self.call(format!("tx {index}: commit"));
        let others: Vec<&Tx<'s>> = (txs.iter().enumerate())
            .filter(|(other, _)| *other != index)
            .map(|(_, other)| other)
            .collect();
        match self.world.commit_of(tx, &others) {
            Outcome::Committed(_) => self.tally.commits += 1,
            Outcome::Conflict => self.tally.conflicts += 1,
            Outcome::Empty => {}
        }
        self.marks[index].clear();
        self.check_committed();
    }

    /// Holds the committed state against the model: the records and the
    /// Merks, and the directory.
    fn check_committed(&mut self) {
        self.call("the committed state is read whole".to_string());
        self.world.check_committed_but(is_unlinked);
        // No history leaves an unlinked record in a Merk with no node.
        let records = &self.world.model.records;
        for ((unit, key), _) in records.iter().filter(|((_, key), _)| is_unlinked(key)) {
            let mut nodes = records.range((*unit, Vec::new())..(*unit, vec![UNLINKED]));
            assert!(nodes.next().is_some(), "{key:?} of {unit} is alone");
        }
        self.check_directory();
    }

    /// One write step, or two, of a transaction of their own, each of up
    /// to three batches, and its commit.
    fn commit_outside(&mut self, txs: &[Tx<'s>]) {
        let tx = self.world.begin();
        let others: Vec<&Tx<'s>> = txs.iter().collect();
        let mut wrote = false;
        for _ in 0..1 + self.rng.below(2) {
            if self.write(&tx, &others, "outside") {
                wrote = true;
                self.check_between(&tx, "outside");
            }
        }
        if !wrote {
            return;
        }
        let hosts = self.hosts(&tx);
        if let Some(draft) = self.plan_hosts(&tx, &hosts) {
            self.apply(&tx, draft, &hosts, "outside");
            self.check_between(&tx, "outside");
        }
        self.call("outside: commit".to_string());
        match self.world.commit(tx, &others) {
            Outcome::Committed(_) => self.tally.commits += 1,
            Outcome::Conflict => self.tally.conflicts += 1,
            Outcome::Empty => {}
        }
        self.check_committed();
    }
}

/// One history of `steps` steps, and what it drew.
pub(crate) fn run(seed: u64, steps: usize) -> Tally {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut run = Run {
        world,
        rng: Rng::new(seed),
        serials: 0,
        renames: 0,
        stamps: 0,
        twins: 0,
        tops: Vec::new(),
        named: Vec::new(),
        reserved: Vec::new(),
        loose: BTreeMap::new(),
        latest: None,
        marks: vec![Vec::new(), Vec::new()],
        rewound: vec![false, false],
        tally: Tally::default(),
        calls: Calls {
            seed,
            made: Vec::new(),
            reading: String::new(),
        },
    };
    {
        // Two Merks with no host, and a few subtrees below them.
        let made = run.world.begin();
        for _ in 0..2 {
            let top = run.fresh(TOP);
            let subtrees = vec![OwnedSubtree::Enter(top, None)];
            let entered = OwnedBatch {
                subtrees,
                ..OwnedBatch::default()
            };
            run.world
                .apply(&made, &entered)
                .expect("a Merk with no host");
            run.tops.push(top);
            run.check_between(&made, "the first commit");
        }
        for at in 0..9 {
            let hosts = run.hosts(&made);
            let draft = match at {
                0..=5 if run.rng.chance(65) => run.plan_enter(&made, &hosts),
                0..=5 => run.plan_nodes(&made, &hosts),
                _ => run.plan_reserve(&made, &hosts),
            };
            let draft = draft.expect("a Merk to write");
            run.apply(&made, draft, &hosts, "the first commit");
            run.check_between(&made, "the first commit");
        }
        for _ in 0..2 {
            let hosts = run.hosts(&made);
            if let Some(draft) = run.plan_unlinked(&made, &hosts) {
                run.apply(&made, draft, &hosts, "the first commit");
                run.check_between(&made, "the first commit");
            }
        }
        assert!(matches!(run.world.commit(made, &[]), Outcome::Committed(_)));
        run.check_committed();
        run.tally = Tally::default();

        let txs = [run.world.begin(), run.world.begin()];
        let mut kept: Vec<Kept<'_, '_>> = Vec::new();
        for _ in 0..steps {
            run.tally.steps += 1;
            let index = run.rng.below(txs.len());
            let tx = &txs[index];
            let who = format!("tx {index}");
            let others: Vec<&Tx<'_>> = (txs.iter().enumerate())
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| other)
                .collect();
            let (met, stopped) = (tx.model.borrow().conflicts(), run.stopped(tx));
            match run.rng.below(100) {
                0..=39 if met || stopped => {
                    run.write_by_key(tx, &who);
                    run.check(tx, Some(index));
                }
                0..=39 => {
                    if run.write(tx, &others, &who) {
                        if run.rng.chance(25) {
                            run.read_cursors(&txs, &mut kept, Some(index));
                        }
                        run.check(tx, Some(index));
                    }
                }
                40..=53 => {
                    run.commit_outside(&txs);
                    // A transaction learns of a commit at its next call:
                    // some make one now.
                    for (other, tx) in txs.iter().enumerate() {
                        if run.rng.chance(40) {
                            run.check(tx, Some(other));
                        }
                    }
                }
                54..=60 => {
                    run.commit(&txs, index);
                    for (other, tx) in txs.iter().enumerate() {
                        if other != index && run.rng.chance(40) {
                            run.check(tx, Some(other));
                        }
                    }
                }
                61..=63 => {
                    run.call(format!("{who}: rollback"));
                    tx.txn.rollback().expect("no scope is open");
                    tx.forget();
                    run.marks[index].clear();
                    run.tally.rollbacks += 1;
                    run.check(tx, Some(index));
                }
                64..=67 => {
                    let writes = tx.model.borrow().writes.len();
                    run.marks[index].push((tx.txn.mark(), writes));
                    run.call(format!("{who}: a mark after {writes} writes"));
                }
                68..=73 => {
                    if run.marks[index].is_empty() {
                        continue;
                    }
                    let to = run.rng.below(run.marks[index].len());
                    let (mark, writes) = run.marks[index][to];
                    run.marks[index].truncate(to);
                    run.call(format!("{who}: back to the mark after {writes} writes"));
                    assert!(tx.txn.rewind(mark), "a mark of the transaction");
                    tx.model.borrow_mut().writes.truncate(writes);
                    run.tally.rewinds += 1;
                    run.rewound[index] = true;
                    // The first read afterwards is a cursor's, or by key.
                    if run.rng.chance(70) {
                        run.open_cursors(&txs, index, &mut kept);
                        run.read_cursors(&txs, &mut kept, Some(index));
                    }
                    run.check(tx, Some(index));
                }
                74..=77 => {
                    run.fail_look(tx, index);
                    run.check(tx, Some(index));
                }
                78..=85 => run.open_cursors(&txs, index, &mut kept),
                _ => run.read_cursors(&txs, &mut kept, None),
            }
            if run.rng.chance(30) {
                run.read_cursors(&txs, &mut kept, None);
            }
        }
        for (index, tx) in txs.iter().enumerate() {
            run.check(tx, Some(index));
        }
        drop(kept);
        for tx in &txs {
            tx.txn.rollback().expect("no scope is open");
        }
    }
    run.check_committed();
    assert_eq!(db.private_ids(), 0, "no id is left with a transaction");
    let report = db.verify_still();
    let unsound = db.unsound(&report);
    assert!(unsound.is_empty(), "the store is unsound: {unsound:?}");
    let mut tally = run.tally;
    tally.histories = 1;
    let had = |count: u64| u64::from(count > 0);
    tally.with_clear_and_move = had(tally.clear_then_move + tally.move_then_clear);
    tally.with_cursor_behind = had(tally.cursor_reads_behind);
    tally.with_cursor_first_after_rewind = had(tally.cursor_first_after_rewind);
    tally.with_twin_reads = had(tally.twin_reads);
    tally.with_failed_read = had(tally.failed_looks + tally.failed_places + tally.failed_nodes);
    tally.with_unlinked_moved = had(tally.moves_of_unlinked);
    tally
}

/// The short form: every history of a few seeds agrees with the model,
/// and the histories draw what they are for. Under Miri it is two short
/// histories, and what they draw is not asked.
#[test]
fn histories_of_moves_clears_drops_rollbacks_and_cursors_agree_with_the_model() {
    let (seeds, steps) = match cfg!(miri) {
        true => (0..2, 30),
        false => (0..40, 160),
    };
    let mut tally = Tally::default();
    for seed in seeds {
        tally.add(&run(seed, steps));
    }
    println!("{tally:#?}");
    if cfg!(miri) {
        return;
    }
    assert!(tally.commits > 400 && tally.conflicts > 20, "{tally:?}");
    assert!(tally.refused_commits > 0, "{tally:?}");
    assert!(tally.moves > 150 && tally.swaps > 20, "{tally:?}");
    assert!(tally.moves_onto_a_drop > 5, "{tally:?}");
    assert!(
        tally.clear_then_move > 10 && tally.move_then_clear > 10,
        "{tally:?}"
    );
    assert!(tally.clears_that_kept > 0, "{tally:?}");
    assert!(tally.cursor_reads_behind > 300, "{tally:?}");
    assert!(
        tally.refused_beside > 10 && tally.refused_conflict > 0,
        "{tally:?}"
    );
    assert!(tally.cursor_first_after_rewind > 20, "{tally:?}");
    assert!(tally.twin_reads > 100, "{tally:?}");
    assert!(
        tally.failed_looks > 5 && tally.failed_places > 0,
        "{tally:?}"
    );
    assert!(tally.refused_before_a_failed_read > 20, "{tally:?}");
    assert!(
        tally.checks_between_batches > 500 && tally.directory_checks > 500,
        "{tally:?}"
    );
    assert!(
        tally.unlinked_puts > 100 && tally.unlinked_deletes > 5,
        "{tally:?}"
    );
    assert!(
        tally.moves_of_unlinked > 20 && tally.cursor_reads_of_unlinked > 5,
        "{tally:?}"
    );
    assert!(tally.emptied_with_unlinked > 0, "{tally:?}");
    assert!(
        tally.stated > 20 && tally.stated_after_a_move > 2,
        "{tally:?}"
    );
    assert!(
        tally.stated_beside_a_move > 20 && tally.stated_by_key > 0,
        "{tally:?}"
    );
}

/// The long form: the histories of the seeds `LIGNIN_HISTORIES` names
/// (`first..last`), each of `LIGNIN_HISTORY_STEPS` steps.
#[test]
#[ignore = "the long form of the histories: LIGNIN_HISTORIES=first..last"]
fn the_histories_of_the_seeds_that_are_named_agree_with_the_model() {
    let named = std::env::var("LIGNIN_HISTORIES").unwrap_or_else(|_| "1000..1100".to_string());
    let (first, last) = named.split_once("..").expect("first..last");
    let seeds: std::ops::Range<u64> = first.parse().expect("a seed")..last.parse().expect("a seed");
    let steps =
        std::env::var("LIGNIN_HISTORY_STEPS").map_or(300, |steps| steps.parse().expect("a count"));
    let mut tally = Tally::default();
    for seed in seeds {
        tally.add(&run(seed, steps));
    }
    println!("{tally:#?}");
}
