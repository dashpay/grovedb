//! Which subtrees a batch moves in the subtree directory, and how each
//! move is stated to the transaction.
//!
//! A batch `Move` renames the key of an element inside its Merk. GroveDB
//! works it out from the element it read under the old key, stages the
//! put and the delete of that element, and asks the engine afterwards to
//! walk the subtree of the tree's path, and of each tree in it, from the
//! prefix of the old path to that of the new one
//! (`Engine::move_subtree`): for a Merk that its element names a root
//! for, and for a tree that keeps its data flat always (`start_tree_copy`
//! in `grovedb/src/batch/move_element.rs`). On LigninDB a subtree goes to
//! another prefix with its nodes where they are (`tree-layout.md` D5):
//! the walk tells the batch each pair of prefixes whose subtree is
//! entered ([`LigninSettled::moves`]), and where the batch is applied the
//! engine states the moves to the transaction
//! (`lignindb::txn::SubtreeWrite::Move`), from what the subtree directory
//! holds of each subtree then ([`moved`]).
//!
//! - **A pair whose host is in a subtree that the batch moves is below
//!   that move**, and is named in it: the subtree goes with the one above
//!   it, under the same key of its host.
//! - **Every other pair is a move of its own**, under the element that
//!   the batch puts in the Merk its host is in: the node whose stated
//!   element has, at the position the subtree had, the prefix the
//!   subtree goes to. The prefix of an element's own path is worked out
//!   from the path of that Merk, which the engine told the handle that
//!   wrote it ([`LigninSettled::path`]).
//! - **The node of that element names the Merks it hosts**
//!   ([`LigninSettled::host`]): the moved subtree among them where the
//!   directory has it as a tree whose data is a Merk, and none where it
//!   has it as a tree that keeps its data flat, whose host points at no
//!   Merk before the move and after it (`transactions.md`, MV3). A node
//!   that hosted a moved subtree, and that the batch puts again with an
//!   element that no moved subtree goes to, hosts nothing: an item, or a
//!   tree with no subtree, that another move brought to its key.
//! - **A subtree that makes room where a moved subtree goes is dropped**
//!   with the move, where the batch does not move it away. A subtree
//!   makes room where it is entered at the first position of its host,
//!   and is the Merk with no node of a tree whose data is one Merk, or
//!   the subtree of a tree that keeps its data flat with no record in
//!   its data keyspace ([`makes_room`]).
//!   Such is the Merk of a tree that was written into and emptied, which
//!   GroveDB asks no walk for when it moves the tree away. The look
//!   states the drop, the drop takes what is entered under the prefix
//!   when the batch is applied, and the transaction is asked for no date
//!   of it. A drop takes the records of the subtree in every keyspace:
//!   those that no node links, where a Merk with no node holds any, go
//!   with it. RocksDB's move neither reads nor deletes them: it leaves
//!   each under the prefix, unless the copy of the moved tree, which
//!   lands there, writes a record under its key (`transactions.md`,
//!   14.5, D12).
//! - **Whatever else the look finds entered where a moved subtree goes,
//!   and that the batch does not move away, is left as it is**: the
//!   engine states no drop of it, and the transaction refuses a batch
//!   that brings a subtree to a prefix where one is entered. The primary
//!   of an indexed tree that was emptied is such a subtree (GROVE-04-F1).
//!   Where a commit beside the transaction wrote under that prefix since
//!   the mover's read, the transaction's refusal is a conflict. The look
//!   for the trees that are written over ([`super::over`]) says nothing
//!   of a prefix that a pair names on either side.
//!
//! The move is dated at the read of the node that hosted the subtree
//! before: the store keeps where the handle of that Merk first read it
//! ([`LigninSettled::move_as`]), and the transaction refuses the batch as
//! a conflict where a commit beside it wrote the node since.
//!
//! **The look can be stale when the batch is applied.** A pair is said by
//! the walk of a subtree that is entered. Where the subtree has no entry
//! when the batch is applied, the batch is refused as a conflict, with
//! nothing of it applied: between the walk and the application of one
//! batch of GroveDB, only a commit beside the transaction takes it. A
//! subtree that is entered then is stated as the directory has it then:
//! under its host, at its position, as the kind of tree it is entered as.
//! What a commit beside the transaction changed of it is the
//! transaction's to judge where it dates the move, unless the look cannot
//! state the move at all, which is the first point below.
//!
//! # A walk that found no subtree
//!
//! A walk that ends on a subtree with no entry tells the batch so
//! ([`LigninSettled::moves_of_none`]). Where the element that the batch
//! puts under the new key says that its tree holds no data, the move is
//! the put and the delete of that element.
//!
//! Where the element that the batch puts under the new key says that the
//! tree holds data, the directory does not hold what that element says:
//! the batch would put an element that names data over a prefix with no
//! subtree. The engine refuses it as a conflict, with nothing of it
//! applied ([`found_none`]). A commit beside the transaction that writes
//! an item over the tree, between the mover's read of the element and
//! the walk, makes this. So does one writer that inserts an element which
//! counts entries that its tree does not hold, and moves it.
//!
//! An element says that its tree holds data where it names a root, for a
//! tree whose data is a Merk, and where it counts at least one entry, for
//! a tree that keeps its data flat. Of a secondary Merk of an indexed
//! tree the asked walk says it: GroveDB asks for it where the element
//! names a root for that Merk.
//!
//! # What this does not do
//!
//! - It moves no subtree that hangs under no element, none under an
//!   element whose Merk the batch wrote through a handle that was not
//!   opened by a path, and none whose new prefix no element that the
//!   batch puts in that Merk has at the position the directory has the
//!   subtree at: such a batch is refused with `Error::NotSupported`,
//!   also where a commit beside the transaction left the subtree so.
//!   GroveDB makes neither of the first two. The third is an indexed
//!   tree whose subtrees are entered at other positions than its element
//!   has them (`engine.md`, "An indexed tree does not take over subtrees
//!   that are entered elsewhere under its key").
//! - It says nothing of a tree that GroveDB asks no walk for and whose
//!   data is a Merk: one whose element names no root. One with no entry
//!   is moved as its element is. One whose Merk is entered with no node
//!   is stated no move, and what becomes of its entry is said elsewhere.
//!   Where the batch leaves no element under the key the tree had, the
//!   entry leaves with the node of the element ([`super::gone`]). Where
//!   the batch puts an element under that key, the entry is one of a
//!   tree that an element is written over, which that look drops or
//!   keeps by its own rules ([`super::over`]): it keeps the entry where
//!   the element that is put is a tree of the entry's own type that
//!   names no root, as where two such trees change places. Or the entry
//!   is one that is entered where a moved subtree goes, above. Where the
//!   tree is in a tree that the batch moves, the transaction takes the
//!   entry out with that move (`transactions.md`, MV2). The records
//!   that no node links are dropped where the entry is dropped, and stay
//!   under the prefix where it is kept. RocksDB's move neither reads nor
//!   deletes them: it leaves each under the prefix the tree had, unless
//!   the copy of a moved tree lands under that prefix and writes a
//!   record under its key (D12).

use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::lignin::{
        LigninEntry, LigninHost, LigninMove, LigninNodeStore, LigninOutcome, LigninSettled,
        LigninSubtree, LigninView,
    },
    TreeType,
};
use grovedb_storage::{error::CommitError, Error, FlatStore, RawIterator};

use super::{
    enter::{element_of, entered, hosted_by},
    error::not_supported,
    over::prefix_under,
};

/// What a group that moves subtrees states of them.
#[derive(Debug, Default)]
pub(super) struct Moved {
    /// The moves, each with the batch of the group that makes it.
    pub(super) moves: Vec<(usize, LigninMove)>,
    /// What the elements that host moved subtrees, or hosted them, host
    /// once the group is applied.
    pub(super) hosts: Vec<LigninHost>,
    /// The subtrees that are dropped where a moved one goes, each with
    /// the batch that moves it there.
    pub(super) drops: Vec<(usize, [u8; 32])>,
}

/// A pair that a batch was told of, with the entry of its subtree.
struct Pair {
    from: [u8; 32],
    to: [u8; 32],
    entry: LigninEntry,
}

/// Whether the subtree under `prefix`, whose entry is `entry`, makes room
/// for a moved subtree. It does where it is entered at the first position
/// of its host, and is the Merk with no node of a tree whose data is one
/// Merk, whatever records that no node links it holds, or the subtree of
/// a tree that keeps its data flat with no record in its data keyspace.
fn makes_room(view: LigninView<'_>, prefix: [u8; 32], entry: &LigninEntry) -> Result<bool, Error> {
    let first = entry.host.as_ref().is_some_and(|host| host.position == 0);
    let Ok(tree) = entry.tree_type() else {
        return Ok(false);
    };
    if !first || entry.root_key.is_some() || tree.is_indexed_primary() {
        return Ok(false);
    }
    if !tree.uses_non_merk_data_storage() {
        return Ok(true);
    }
    let subtree = LigninNodeStore::read_only(view, LigninSubtree::alone(prefix));
    let mut records = FlatStore::cursor(&subtree);
    records.seek_to_first().unwrap();
    if records.valid().unwrap() {
        return Ok(false);
    }
    // A cursor that stopped on an error shows no record either.
    records.status().map(|()| true)
}

/// What `parts` do to the subtrees they were told to move.
pub(super) fn moved(view: LigninView<'_>, parts: &[LigninSettled]) -> Result<Moved, Error> {
    let mut moved = Moved::default();
    for (at, part) in parts.iter().enumerate() {
        let mut pairs = Vec::with_capacity(part.moves().len());
        for (from, to) in part.moves() {
            // The walk said the pair of a subtree that was entered. One
            // that is not entered now was taken by a commit beside the
            // transaction, which wrote the node that hosted it.
            let Some(entry) = entered(view, *from)? else {
                return Err(Error::Commit(CommitError::Conflict));
            };
            let (from, to) = (*from, *to);
            pairs.push(Pair { from, to, entry });
        }
        let source = |prefix: &[u8; 32]| pairs.iter().find(|pair| pair.from == *prefix);
        // The pair of the subtree that the host of a pair's subtree is
        // in, where the batch moves that one too.
        let above = |pair: &Pair| {
            let host = pair.entry.host.as_ref()?;
            source(&host.subtree)
        };
        // The move that takes a pair along: the pair above it that is
        // below none.
        let top = |pair: &Pair| {
            let mut top = pair;
            for _ in 0..pairs.len() {
                match above(top) {
                    Some(next) => top = next,
                    None => break,
                }
            }
            top.from
        };
        let own = || pairs.iter().filter(|pair| above(pair).is_none());
        for pair in own() {
            let Some(host) = &pair.entry.host else {
                return Err(not_supported("the move of a subtree that no element hosts"));
            };
            let Some(path) = part.path(&host.subtree) else {
                return Err(not_supported(
                    "the move of a subtree under an element whose Merk was written through no \
                     handle by path",
                ));
            };
            // The element that hosts the subtree afterwards.
            let mut hosting = None;
            for (key, value) in part.stated(&host.subtree) {
                let hosted = hosted_by(value, prefix_under(path, key))?;
                if hosted.get(usize::from(host.position)) == Some(&pair.to) {
                    hosting = Some((key.to_vec(), hosted));
                    break;
                }
            }
            let Some((new_key, hosted)) = hosting else {
                return Err(not_supported(
                    "the move of a subtree to a prefix that no element of its batch has at the \
                     subtree's position",
                ));
            };
            // The node of a tree that keeps its data flat points at no
            // Merk, before the move and after it.
            let flat = |tree: TreeType| tree.uses_non_merk_data_storage();
            let hosted = match pair.entry.tree_type().is_ok_and(flat) {
                true => Vec::new(),
                false => hosted,
            };
            let below = pairs.iter().filter(|below| above(below).is_some());
            let below = below.filter(|below| top(below) == pair.from);
            let stated = LigninMove {
                from: pair.from,
                to: pair.to,
                above: host.subtree,
                old_key: host.key.clone(),
                new_key: new_key.clone(),
                position: host.position,
                below: below.map(|below| (below.from, below.to)).collect(),
            };
            moved.moves.push((at, stated));
            let named = LigninHost {
                subtree: host.subtree,
                key: new_key,
                hosted,
            };
            if !moved.hosts.contains(&named) {
                moved.hosts.push(named);
            }
            // What is entered where the subtree goes, and stays, is
            // dropped where it makes room, and nothing else is.
            if source(&pair.to).is_none() {
                let room = match entered(view, pair.to)? {
                    Some(there) => makes_room(view, pair.to, &there)?,
                    None => false,
                };
                if room {
                    moved.drops.push((at, pair.to));
                }
            }
        }
        // A node that hosted a moved subtree and is put again hosts what
        // a move brings to it, or nothing.
        for pair in own() {
            let Some(host) = &pair.entry.host else {
                continue;
            };
            let put = part.node(&host.subtree, &host.key);
            let named =
                |named: &LigninHost| (named.subtree, &named.key) == (host.subtree, &host.key);
            if matches!(put, Some(LigninOutcome::Put(_))) && !moved.hosts.iter().any(named) {
                moved.hosts.push(LigninHost {
                    subtree: host.subtree,
                    key: host.key.clone(),
                    hosted: Vec::new(),
                });
            }
        }
    }
    Ok(moved)
}

/// Refuses, as a conflict, a group with a batch that was told of a walk
/// which found no subtree ([`LigninSettled::moves_of_none`]) and that
/// puts, over the prefix the subtree was to go to, an element which says
/// that its tree holds data. The element is looked for among the nodes
/// that the batch puts with a stated value in the Merks whose path the
/// engine knows.
pub(super) fn found_none(parts: &[LigninSettled]) -> Result<(), Error> {
    for part in parts {
        for (_, to) in part.moves_of_none() {
            for subtree in part.written() {
                let Some(path) = part.path(&subtree.prefix) else {
                    continue;
                };
                for (key, value) in part.stated(&subtree.prefix) {
                    let hosted = hosted_by(value, prefix_under(path, key))?;
                    let at = hosted.iter().position(|hosted| hosted == to);
                    if at.is_some_and(|at| holds_data(value, at)) {
                        return Err(Error::Commit(CommitError::Conflict));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether the element with the bytes `value` says that the subtree at
/// `position` among those it hosts holds data.
fn holds_data(value: &[u8], position: usize) -> bool {
    let Some(element) = element_of(value) else {
        return false;
    };
    match (position, element.non_merk_entry_count()) {
        (0, Some(entries)) => entries > 0,
        (0, None) => {
            let tree = element.root_key_and_tree_type_owned();
            tree.is_some_and(|(root_key, _)| root_key.is_some())
        }
        // A secondary Merk, which the walk is asked for where the element
        // names a root for it.
        _ => true,
    }
}
