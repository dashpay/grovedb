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
//!   the primary with no node of an indexed tree whose secondaries have
//!   none either, which leave with it ([`makes_room`]). The subtree of a
//!   tree that keeps its data flat makes none: GroveDB asks the walk of
//!   such a tree whenever it moves it away, so one that is entered where
//!   a moved subtree goes was entered since that walk.
//!   Such is the Merk of a tree that was written into and emptied, which
//!   GroveDB asks no walk for when it moves the tree away. The look
//!   states the drop of the subtree under the node it found it under
//!   (`lignindb::txn::SubtreeWrite::DropUnder`), dated at the read of
//!   that node as the element of a tree by a handle of the batch that
//!   writes the node's key: the transaction refuses the batch as a
//!   conflict where a commit beside it wrote the node since and the
//!   transaction did not give the node its value itself, as it does
//!   for the move, and refuses it where the subtree hangs under another
//!   node by then. Where no such handle read the node as the element
//!   of a tree, there is no read to date the drop at, and the look
//!   refuses the group as a conflict: the tree was made since. A
//!   drop takes the records of the subtree in every keyspace:
//!   those that no node links, where a Merk with no node holds any, go
//!   with it. RocksDB's move neither reads nor deletes them: it leaves
//!   each under the prefix, unless the copy of the moved tree, which
//!   lands there, writes a record under its key (`transactions.md`,
//!   14.5, D12).
//! - **Whatever else the look finds entered where a moved subtree goes,
//!   and that the batch does not move away, is left as it is**: the
//!   engine states no drop of it, and the transaction refuses a batch
//!   that brings a subtree to a prefix where one is entered.
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

use std::collections::{HashMap, HashSet};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::lignin::{
        LigninEntry, LigninHost, LigninMove, LigninOutcome, LigninSettled, LigninView,
    },
    TreeType,
};
use grovedb_storage::{error::CommitError, prefix::secondary_prefix_for, Error};

use super::{
    enter::{element_of, entered, hosted_by},
    error::not_supported,
    look_step,
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
    /// the batch that moves it there and the node it was found under:
    /// the subtree that node is in, and its key.
    pub(super) drops: Vec<(usize, [u8; 32], [u8; 32], Vec<u8>)>,
}

/// A pair that a batch was told of, with the entry of its subtree.
struct Pair {
    from: [u8; 32],
    to: [u8; 32],
    entry: LigninEntry,
}

/// How the pairs of one batch stand to each other: which moves are of
/// their own, and which pairs each of them takes along.
struct Standing {
    /// The first pair of each prefix that the pairs name as left.
    first: HashMap<[u8; 32], usize>,
    /// For each pair, the pair of the subtree that the host of its
    /// subtree is in, where the batch moves that one too: the first pair
    /// of that prefix.
    above: Vec<Option<usize>>,
    /// The pairs that are below another, in the order of the pairs, by
    /// the pair that takes each along: the pair above it that is below
    /// none.
    below: HashMap<usize, Vec<([u8; 32], [u8; 32])>>,
}

impl Standing {
    /// Works it out in one pass over `pairs`: each pair is found by the
    /// prefix it leaves, and the walk from a pair to the one that takes
    /// it along ends where it comes to a pair whose walk was made.
    fn of(pairs: &[Pair]) -> Self {
        let mut first = HashMap::with_capacity(pairs.len());
        for (at, pair) in pairs.iter().enumerate() {
            look_step();
            first.entry(pair.from).or_insert(at);
        }
        let above: Vec<Option<usize>> = (pairs.iter())
            .map(|pair| {
                look_step();
                first.get(&pair.entry.host.as_ref()?.subtree).copied()
            })
            .collect();
        let mut tops = vec![None; pairs.len()];
        let mut trail = Vec::new();
        let mut below: HashMap<usize, Vec<_>> = HashMap::new();
        for (at, pair) in pairs.iter().enumerate() {
            if above[at].is_some() {
                let top = top(at, &above, &mut tops, &mut trail);
                below.entry(top).or_default().push((pair.from, pair.to));
            }
        }
        Self {
            first,
            above,
            below,
        }
    }

    /// Whether the pair at `at` is a move of its own: below no pair.
    fn own(&self, at: usize) -> bool {
        self.above[at].is_none()
    }

    /// Whether a pair names `prefix` as left.
    fn leaves(&self, prefix: &[u8; 32]) -> bool {
        look_step();
        self.first.contains_key(prefix)
    }

    /// The pairs that the move of the subtree under `from` takes along,
    /// in the order of the pairs.
    fn below(&self, from: &[u8; 32]) -> Vec<([u8; 32], [u8; 32])> {
        look_step();
        let first = self.first.get(from);
        let below = first.and_then(|first| self.below.get(first));
        below.cloned().unwrap_or_default()
    }
}

/// The pair that takes the pair at `start` along: where the walk from it
/// to the pair above, and from that one on, comes to a pair that is below
/// none. `above` has the pair above each pair, and `tops` what the walks
/// that ended found, which this one adds to with the pairs it passed
/// (`trail` is its room to note them).
///
/// A walk makes at most as many steps as there are pairs. Pairs that are
/// above each other in a ring, which no directory holds, end none: the
/// walk gives the pair it has come to by then, and notes nothing.
fn top(
    start: usize,
    above: &[Option<usize>],
    tops: &mut [Option<usize>],
    trail: &mut Vec<usize>,
) -> usize {
    trail.clear();
    let mut at = start;
    let top = loop {
        look_step();
        if let Some(top) = tops[at] {
            break top;
        }
        match above[at] {
            Some(next) if trail.len() < above.len() => {
                trail.push(at);
                at = next;
            }
            Some(_) => return at,
            None => break at,
        }
    };
    for passed in trail.drain(..) {
        tops[passed] = Some(top);
    }
    top
}

/// The nodes that a batch puts in one Merk with a stated value, and what
/// a look has read of them so far.
#[derive(Default)]
struct Scan<'a> {
    /// The nodes, each by its key with its value, in the order of
    /// [`LigninSettled::stated`].
    nodes: Vec<(&'a [u8], &'a [u8])>,
    /// What the element of each node that was read hosts, by position.
    hosted: Vec<Vec<[u8; 32]>>,
    /// The first node that was read whose element hosts a prefix at a
    /// position.
    first: HashMap<(usize, [u8; 32]), usize>,
}

impl Scan<'_> {
    /// The first of the nodes whose element hosts `prefix` at `position`,
    /// where `path` is the path of their Merk.
    ///
    /// The nodes are read in their order, each once, and no further than
    /// the one that is asked for: what a node after it holds is not read
    /// for this, and fails nothing.
    fn hosting(
        &mut self,
        path: &[Vec<u8>],
        position: usize,
        prefix: &[u8; 32],
    ) -> Result<Option<usize>, Error> {
        look_step();
        if let Some(found) = self.first.get(&(position, *prefix)) {
            return Ok(Some(*found));
        }
        while let Some((key, value)) = self.nodes.get(self.hosted.len()) {
            look_step();
            let at = self.hosted.len();
            let hosted = hosted_by(value, prefix_under(path, key))?;
            for (place, hosted) in hosted.iter().enumerate() {
                self.first.entry((place, *hosted)).or_insert(at);
            }
            let found = hosted.get(position) == Some(prefix);
            self.hosted.push(hosted);
            if found {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }
}

/// The nodes that `part` puts with a stated value, by their Merk: for
/// each Merk what [`LigninSettled::stated`] gives, from one pass over the
/// batch.
fn stated_by_merk(part: &LigninSettled) -> HashMap<[u8; 32], Scan<'_>> {
    let mut by_merk: HashMap<[u8; 32], Scan<'_>> = HashMap::new();
    for (subtree, key, value) in part.stated_nodes() {
        by_merk.entry(subtree).or_default().nodes.push((key, value));
    }
    by_merk
}

/// What leaves where the subtree under `prefix`, whose entry is `entry`,
/// makes room for a moved subtree: that subtree, and with the primary of
/// an indexed tree the secondaries that are entered under its element.
/// None, where it makes no room.
///
/// A subtree makes room where it is entered at the first position of its
/// host, and is the Merk with no node of a tree whose data is one Merk,
/// whatever records that no node links it holds; or the primary with no
/// node of an indexed tree whose secondaries have none either. The
/// subtree of a tree that keeps its data flat makes none.
fn makes_room(
    view: LigninView<'_>,
    prefix: [u8; 32],
    entry: &LigninEntry,
) -> Result<Vec<[u8; 32]>, Error> {
    let first = entry.host.as_ref().filter(|host| host.position == 0);
    let (Some(host), Ok(tree)) = (first, entry.tree_type()) else {
        return Ok(Vec::new());
    };
    if entry.root_key.is_some() {
        return Ok(Vec::new());
    }
    if tree.is_indexed_primary() {
        // A secondary is stored under the tag of its axis, and the big
        // sum axis under that of the sum axis: these are all.
        let mut leaving = vec![prefix];
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            let secondary = secondary_prefix_for(&prefix, axis.tag()).value;
            let under = |at: &LigninEntry| {
                let at = at.host.as_ref();
                at.is_some_and(|at| (at.subtree, &at.key) == (host.subtree, &host.key))
            };
            match entered(view, secondary)?.filter(under) {
                Some(entry) if entry.root_key.is_some() => return Ok(Vec::new()),
                Some(_) => leaving.push(secondary),
                None => {}
            }
        }
        return Ok(leaving);
    }
    Ok(match tree.uses_non_merk_data_storage() {
        true => Vec::new(),
        false => vec![prefix],
    })
}

/// What `parts` do to the subtrees they were told to move.
///
/// The look makes one pass over the pairs of each batch and one over the
/// nodes that the batch states: a pair is found by the prefix it leaves,
/// the pairs that a move takes along are gathered once
/// ([`Standing`]), the element that hosts a moved subtree afterwards is
/// found among the nodes of its Merk as far as they were read
/// ([`Scan`]), and an element that is named is found by its subtree and
/// its key. The work grows with the pairs and the stated nodes of the
/// batch, and with nothing else.
pub(super) fn moved(view: LigninView<'_>, parts: &[LigninSettled]) -> Result<Moved, Error> {
    let mut moved = Moved::default();
    // Where the elements stand in `moved.hosts`, each by its subtree and
    // its key.
    let mut named: HashMap<([u8; 32], Vec<u8>), Vec<usize>> = HashMap::new();
    for (at, part) in parts.iter().enumerate() {
        let mut pairs = Vec::with_capacity(part.moves().len());
        for (from, to) in part.moves() {
            look_step();
            // The walk said the pair of a subtree that was entered. One
            // that is not entered now was taken by a commit beside the
            // transaction, which wrote the node that hosted it.
            let Some(entry) = entered(view, *from)? else {
                return Err(Error::Commit(CommitError::Conflict));
            };
            let (from, to) = (*from, *to);
            pairs.push(Pair { from, to, entry });
        }
        if pairs.is_empty() {
            continue;
        }
        // The pair of the subtree that the host of a pair's subtree is
        // in, where the batch moves that one too, is above the pair. A
        // pair that is below none is a move of its own, which takes
        // along every pair that the walk from pair to pair above ends on
        // it for.
        let standing = Standing::of(&pairs);
        let own = || (pairs.iter().enumerate()).filter(|(index, _)| standing.own(*index));
        let mut scans = stated_by_merk(part);
        for (_, pair) in own() {
            let Some(host) = &pair.entry.host else {
                return Err(not_supported("the move of a subtree that no element hosts"));
            };
            let Some(path) = part.path(&host.subtree) else {
                return Err(not_supported(
                    "the move of a subtree under an element whose Merk was written through no \
                     handle by path",
                ));
            };
            // The element that hosts the subtree afterwards: the first
            // node that the batch states in the Merk whose element has
            // the prefix the subtree goes to at the subtree's position.
            let scan = scans.entry(host.subtree).or_default();
            let Some(hosting) = scan.hosting(path, usize::from(host.position), &pair.to)? else {
                return Err(not_supported(
                    "the move of a subtree to a prefix that no element of its batch has at the \
                     subtree's position",
                ));
            };
            let new_key = scan.nodes[hosting].0.to_vec();
            // The node of a tree that keeps its data flat points at no
            // Merk, before the move and after it.
            let flat = |tree: TreeType| tree.uses_non_merk_data_storage();
            let hosted = match pair.entry.tree_type().is_ok_and(flat) {
                true => Vec::new(),
                false => scan.hosted[hosting].clone(),
            };
            let stated = LigninMove {
                from: pair.from,
                to: pair.to,
                above: host.subtree,
                old_key: host.key.clone(),
                new_key: new_key.clone(),
                position: host.position,
                below: standing.below(&pair.from),
            };
            moved.moves.push((at, stated));
            let host_named = LigninHost {
                subtree: host.subtree,
                key: new_key,
                hosted,
            };
            // An element is named once with what it hosts.
            look_step();
            let known = (named.entry((host_named.subtree, host_named.key.clone()))).or_default();
            if !known.iter().any(|index| moved.hosts[*index] == host_named) {
                known.push(moved.hosts.len());
                moved.hosts.push(host_named);
            }
            // What is entered where the subtree goes, and stays, is
            // dropped where it makes room, and nothing else is.
            if !standing.leaves(&pair.to)
                && let Some(there) = entered(view, pair.to)?
            {
                let leaving = makes_room(view, pair.to, &there)?;
                if let (false, Some(under)) = (leaving.is_empty(), &there.host) {
                    // The drops are dated at the mover's read of the node
                    // as the element of a tree. A node that its handle
                    // read as no tree hosted nothing then: a commit
                    // beside the transaction made the tree since.
                    if !part.read_as_tree(&under.subtree, &under.key) {
                        return Err(Error::Commit(CommitError::Conflict));
                    }
                    // What leaves is entered under that one node.
                    for left in leaving {
                        (moved.drops).push((at, left, under.subtree, under.key.clone()));
                    }
                }
            }
        }
        // A node that hosted a moved subtree and is put again hosts what
        // a move brings to it, or nothing.
        for (_, pair) in own() {
            let Some(host) = &pair.entry.host else {
                continue;
            };
            look_step();
            let put = part.node(&host.subtree, &host.key);
            if !matches!(put, Some(LigninOutcome::Put(_))) {
                continue;
            }
            let known = named.entry((host.subtree, host.key.clone())).or_default();
            if known.is_empty() {
                known.push(moved.hosts.len());
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
/// engine knows: each node is read once, whatever the number of walks
/// that found no subtree.
pub(super) fn found_none(parts: &[LigninSettled]) -> Result<(), Error> {
    for part in parts {
        let Some((_, first)) = part.moves_of_none().first() else {
            continue;
        };
        let none: HashSet<[u8; 32]> = (part.moves_of_none().iter()).map(|(_, to)| *to).collect();
        let stated = stated_by_merk(part);
        // A node that cannot be read fails the look where the look comes
        // to it before an element over the prefix of the first walk: an
        // element over the prefix of a later walk refuses the group once
        // every node is read.
        let mut refused = false;
        for subtree in part.written() {
            let Some(path) = part.path(&subtree.prefix) else {
                continue;
            };
            let Some(scan) = stated.get(&subtree.prefix) else {
                continue;
            };
            for (key, value) in &scan.nodes {
                look_step();
                let hosted = hosted_by(value, prefix_under(path, key))?;
                for (at, to) in hosted.iter().enumerate() {
                    // A prefix is looked for at the first position it has.
                    let named = none.contains(to) && !hosted[..at].contains(to);
                    if named && holds_data(value, at) {
                        if to == first {
                            return Err(Error::Commit(CommitError::Conflict));
                        }
                        refused = true;
                    }
                }
            }
        }
        if refused {
            return Err(Error::Commit(CommitError::Conflict));
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

#[cfg(test)]
mod tests {
    use grovedb_merk::store::lignin::LigninHosted;
    use rand::{rngs::StdRng, RngExt, SeedableRng};

    use super::*;
    use crate::engine::lignin::look_steps_taken;

    type Prefix = [u8; 32];

    fn prefix(number: usize) -> Prefix {
        let mut prefix = [0; 32];
        prefix[..8].copy_from_slice(&(number as u64).to_be_bytes());
        prefix
    }

    /// A pair of the subtree `from`, which is hosted in the subtree
    /// `host` where one is named.
    fn pair(from: usize, to: usize, host: Option<usize>) -> Pair {
        let host = host.map(|host| LigninHosted {
            subtree: prefix(host),
            key: Vec::new(),
            position: 0,
        });
        Pair {
            from: prefix(from),
            to: prefix(to),
            entry: LigninEntry {
                root_key: None,
                tree_type: Vec::new(),
                host,
            },
        }
    }

    /// Whether each pair is a move of its own, and what it takes along
    /// if it is, by the words of the module: every pair is looked for
    /// among all of them, and the pair that takes one along is walked to
    /// anew for each.
    fn by_looking_through_all(pairs: &[Pair]) -> Vec<(bool, Vec<(Prefix, Prefix)>)> {
        let source = |prefix: &Prefix| pairs.iter().find(|pair| pair.from == *prefix);
        let above = |pair: &Pair| source(&pair.entry.host.as_ref()?.subtree);
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
        let stands = |pair: &Pair| {
            if above(pair).is_some() {
                return (false, Vec::new());
            }
            let below = pairs.iter().filter(|below| above(below).is_some());
            let below = below.filter(|below| top(below) == pair.from);
            (true, below.map(|below| (below.from, below.to)).collect())
        };
        pairs.iter().map(stands).collect()
    }

    /// Whether a pair of `pairs` is below pairs with no end: the pairs
    /// above it come back to one of them.
    fn has_a_ring(pairs: &[Pair]) -> bool {
        let source = |prefix: &Prefix| pairs.iter().find(|pair| pair.from == *prefix);
        let above = |pair: &Pair| source(&pair.entry.host.as_ref()?.subtree);
        pairs.iter().any(|pair| {
            let mut at = pair;
            for _ in 0..pairs.len() {
                match above(at) {
                    Some(next) => at = next,
                    None => return false,
                }
            }
            true
        })
    }

    fn by_one_pass(pairs: &[Pair]) -> Vec<(bool, Vec<(Prefix, Prefix)>)> {
        let standing = Standing::of(pairs);
        let stands = |(at, pair): (usize, &Pair)| match standing.own(at) {
            true => (true, standing.below(&pair.from)),
            false => (false, Vec::new()),
        };
        pairs.iter().enumerate().map(stands).collect()
    }

    /// Pairs of subtrees that hang in each other at random, as a
    /// directory holds them and as none does: a subtree is told of more
    /// than once, with one prefix to go to or with several; subtrees are
    /// hosted in each other in a ring, or in themselves; and two pairs of
    /// one subtree come with two entries of it, as two reads of the
    /// directory with a commit between them give.
    #[test]
    fn the_pairs_stand_to_each_other_as_looking_through_all_of_them_says() {
        let mut rng = StdRng::seed_from_u64(0x6d6f_7665);
        let (mut own, mut taken, mut deepest, mut twice, mut rings) = (0, 0, 0, 0, 0);
        let mut two_entries = 0;
        for round in 0..4_000 {
            let subtrees = rng.random_range(1..40);
            // What the directory holds of each subtree: the subtree its
            // host is in, where it has one. In one round of four a
            // subtree hangs in any other, in itself or in one that no
            // pair is of. In the others it hangs in one with a lower
            // number, so there is no ring.
            let host_of =
                |rng: &mut StdRng, subtree: usize| match (rng.random_range(0..4), round % 4) {
                    (0, _) => None,
                    (_, 0) => Some(rng.random_range(0..subtrees + 3)),
                    _ => (subtree > 0).then(|| rng.random_range(0..subtree)),
                };
            let hosts: Vec<Option<usize>> = (0..subtrees)
                .map(|subtree| host_of(&mut rng, subtree))
                .collect();
            let told = rng.random_range(0..2 * subtrees);
            let mut pairs = Vec::with_capacity(told);
            for _ in 0..told {
                let from = rng.random_range(0..subtrees);
                // In one round of four each pair has an entry of its
                // own.
                let host = match round % 4 {
                    3 => host_of(&mut rng, from),
                    _ => hosts[from],
                };
                pairs.push(pair(from, rng.random_range(100..140), host));
            }
            let differ = |one: &Pair, other: &Pair| {
                one.from == other.from && one.entry.host != other.entry.host
            };
            let mut each = pairs.iter();
            two_entries +=
                usize::from(each.any(|one| pairs.iter().any(|other| differ(one, other))));
            let stand = by_one_pass(&pairs);
            assert_eq!(stand, by_looking_through_all(&pairs), "round {round}");
            let standing = Standing::of(&pairs);
            for subtree in 0..subtrees + 3 {
                let left = pairs.iter().any(|pair| pair.from == prefix(subtree));
                assert_eq!(standing.leaves(&prefix(subtree)), left, "round {round}");
            }
            own += stand.iter().filter(|(own, _)| *own).count();
            taken += stand.iter().map(|(_, below)| below.len()).sum::<usize>();
            deepest = deepest.max(
                stand
                    .iter()
                    .map(|(_, below)| below.len())
                    .max()
                    .unwrap_or(0),
            );
            let mut left: Vec<Prefix> = pairs.iter().map(|pair| pair.from).collect();
            left.sort_unstable();
            twice += usize::from(left.windows(2).any(|two| two[0] == two[1]));
            rings += usize::from(has_a_ring(&pairs));
        }
        eprintln!(
            "{own} moves of their own, {taken} pairs taken along, {deepest} by one move at the \
             most; {twice} rounds told of a subtree twice, {rings} with a ring, {two_entries} \
             with two entries of one subtree"
        );
        assert!(own > 10_000 && taken > 10_000 && deepest >= 20);
        assert!(twice > 500 && rings > 100 && two_entries > 100);
    }

    /// Subtrees that are each in the one before, told deepest first and
    /// told deepest last: the pair that takes them along is walked to
    /// once for all, not once for each.
    #[test]
    fn a_chain_of_pairs_below_each_other_is_walked_once() {
        const DEEP: usize = 4_000;
        let deepest_last: Vec<Pair> = (0..DEEP)
            .map(|subtree| pair(subtree, DEEP + subtree, subtree.checked_sub(1)))
            .collect();
        let deepest_first: Vec<Pair> = (0..DEEP)
            .rev()
            .map(|subtree| pair(subtree, DEEP + subtree, subtree.checked_sub(1)))
            .collect();
        for pairs in [deepest_last, deepest_first] {
            look_steps_taken();
            let standing = Standing::of(&pairs);
            let steps = look_steps_taken();
            let top = pairs.iter().position(|pair| pair.from == prefix(0));
            let top = top.expect("the pair of the first subtree");
            assert!(standing.own(top));
            assert_eq!(standing.below(&prefix(0)).len(), DEEP - 1);
            assert_eq!(
                pairs
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| standing.own(*at))
                    .count(),
                1
            );
            // Each pair is come to when it is entered by its prefix, when
            // the pair above it is found, and on the walks: once as a
            // walk's start and at most once more as one that a walk
            // passes.
            eprintln!("{steps} steps for {DEEP} pairs");
            assert!(steps <= 5 * DEEP as u64, "{steps}");
        }
    }
}
