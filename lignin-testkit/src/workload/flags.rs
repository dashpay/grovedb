//! What the storage flags of an element hold after each write of a stream.
//!
//! Drive stores with every element the epoch it was created in, its owner
//! and, for each later epoch that made it longer, the bytes that epoch
//! added. The callbacks [`super::apply()`] passes with an
//! [`Op::FlagsBatch`], an [`Op::FlagsHandoverBatch`] and an
//! [`Op::FlagsPartialBatch`] keep that history as the element is replaced.
//! [`Histories`] follows it along a stream, from
//! the operations alone, so a generator can aim at a case, [`Coverage`] can
//! tell which cases a stream holds, and a test can compare the flags
//! GroveDB stored with the ones expected.
//!
//! The bytes are those GroveDB charges for: the bytes of the element as it
//! is serialized, flags included, and of the length written before them.
//! The flags are part of the element, so recording an epoch makes the
//! element longer by the entry, which the epoch is charged for as well.
//! GroveDB therefore asks the callback again until the flags it answers no
//! longer change the size, and [`History`] does the same.
//!
//! Only what a stream writes through these operations is followed, and only
//! items and references in trees that aggregate nothing: a tree element is
//! rewritten by GroveDB itself with the flags it has, and the other
//! elements are charged a fixed size. An element another operation writes
//! or deletes is followed no further, and neither is anything once a
//! transaction is rolled back, in whole or to a savepoint, or dropped.
//!
//! [`Coverage`]: super::Coverage

use std::collections::{BTreeMap, BTreeSet};

use grovedb::Element;
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_version::version::v1::GROVE_V1;

use super::op::{BatchOp, Op, Path, TransactionOp, Write};

/// The 32-byte id of an owner.
pub type Owner = [u8; 32];

/// The bytes of its own that an epoch keeps when an update frees the rest:
/// the least its entry in the flags takes.
pub const BYTES_AN_EPOCH_KEEPS: u32 = 3;

/// How far, in bytes, a generator keeps what an update frees from the
/// point where one more epoch would leave the flags, or one fewer. The
/// lengths GroveDB writes before a value and before its flags can move
/// that point by a few bytes.
pub const CLEAR_OF_AN_EPOCH: usize = 16;

/// The bytes GroveDB stores with every value in a tree that aggregates
/// nothing, before the length it writes: two hashes and the feature byte.
const NODE_OVERHEAD: usize = 65;

/// How often GroveDB asks the flags callback about one update before it
/// gives up, at most.
const MOST_ROUNDS: usize = 10;

/// Which callbacks an update runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callbacks<'a> {
    /// Those of an [`Op::FlagsBatch`].
    Batch,
    /// Those of an [`Op::FlagsPartialBatch`].
    PartialBatch,
    /// Those of an [`Op::FlagsHandoverBatch`], with the owners a handover
    /// to or from which is a full one.
    Handover(&'a BTreeSet<Owner>),
}

/// What the flags of one stored element hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    /// The owner the flags name.
    pub owner: Option<Owner>,
    /// The epoch the flags hold the element's first bytes in.
    pub base_epoch: u16,
    /// The bytes each later epoch added, as the flags record them.
    pub later: BTreeMap<u16, u32>,
    /// The length of the element as it is serialized, its flags aside.
    pub len: usize,
}

/// What an update did to the history of the element it replaced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// It made the element longer in a later epoch than its first.
    pub grew_in_later_epoch: bool,
    /// It freed bytes of more than one epoch.
    pub shrank_across_epochs: bool,
    /// It kept the length, in a later epoch than the element's first.
    pub kept_size: bool,
    /// Its flags name another owner than the stored ones.
    pub named_another_owner: bool,
    /// It kept the length in a partial batch over flags of several epochs,
    /// whose history the flags written replace.
    pub dropped_history: bool,
    /// It stored an element without flags over one with flags.
    pub removed_flags: bool,
    /// It was accounted as a replacement: a full handover between two
    /// owners.
    pub replaced: bool,
}

/// What a flags operation did with the elements [`Histories`] follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// What its updates did, taken together.
    pub update: Update,
    /// Whether it deleted an element whose flags held several epochs.
    pub deleted_several_epochs: bool,
}

/// The bytes a length takes as GroveDB's elements write one.
fn length_prefix(len: usize) -> usize {
    match len {
        0..=250 => 1,
        251..=0xFFFF => 3,
        _ => 5,
    }
}

/// The bytes a number takes in the flags and before a stored value: seven
/// bits to the byte.
fn varint_len(mut number: usize) -> usize {
    let mut len = 1;
    while number >= 0x80 {
        number >>= 7;
        len += 1;
    }
    len
}

/// The flags `element` is written with, as the history of an element
/// stored where nothing was. The outer `None` is an element that is not
/// followed, the inner one an element written without flags.
fn written(element: &Element) -> Option<Option<History>> {
    if !matches!(element, Element::Item(..) | Element::Reference(..)) {
        return None;
    }
    let Some(flags) = element.get_flags() else {
        return Some(None);
    };
    let Ok(Some(flags @ (StorageFlags::SingleEpoch(_) | StorageFlags::SingleEpochOwned(..)))) =
        StorageFlags::from_element_flags_ref(flags)
    else {
        return None;
    };
    let mut bare = element.clone();
    bare.set_flags(None);
    Some(Some(History {
        owner: flags.owner_id().copied(),
        base_epoch: *flags.base_epoch(),
        later: BTreeMap::new(),
        len: bare.serialize(&GROVE_V1).ok()?.len(),
    }))
}

impl History {
    /// The bytes of the flags.
    fn flags_len(&self) -> usize {
        let entries = self.later.values();
        let entries: usize = entries.map(|bytes| 2 + varint_len(*bytes as usize)).sum();
        3 + self.owner.map_or(0, |_| 32) + entries
    }

    /// The bytes GroveDB charges for an element of `len` bytes, flags
    /// aside, stored with these flags in a tree that aggregates nothing,
    /// less what any element at its key costs.
    fn charged(&self, len: usize) -> usize {
        let flags = self.flags_len();
        let value = len + length_prefix(flags) + flags;
        value + varint_len(value + NODE_OVERHEAD)
    }

    /// The bytes of the `epochs` latest epochs in the flags.
    pub fn bytes_of_latest(&self, epochs: usize) -> usize {
        let latest = self.later.values().rev().take(epochs);
        latest.map(|bytes| *bytes as usize).sum()
    }

    /// The least and the most bytes an update may take off the element to
    /// free bytes of the latest epoch in the flags alone,
    /// [`CLEAR_OF_AN_EPOCH`] away from freeing none and from freeing all
    /// the epoch can give back. `None` when the epoch holds too few.
    pub fn freeable_within_latest(&self) -> Option<(usize, usize)> {
        let high = self
            .bytes_of_latest(1)
            .checked_sub(BYTES_AN_EPOCH_KEEPS as usize + CLEAR_OF_AN_EPOCH)?;
        (high >= CLEAR_OF_AN_EPOCH).then_some((CLEAR_OF_AN_EPOCH, high))
    }

    /// The least and the most bytes an update may take off the element so
    /// that exactly the `epochs` latest epochs leave the flags, each
    /// [`CLEAR_OF_AN_EPOCH`] away from another count. The update frees
    /// what it takes off and the entries of the epochs that leave. Each of
    /// them gives back all but the bytes it keeps, and what is left comes
    /// off the epoch before them, or off the first epoch when none stays,
    /// in which case the element is left at least `shortest` bytes long.
    /// `None` when no such number of bytes exists.
    pub fn freeable_across(&self, epochs: usize, shortest: usize) -> Option<(usize, usize)> {
        if epochs == 0 || epochs > self.later.len() {
            return None;
        }
        let keeps = BYTES_AN_EPOCH_KEEPS as usize;
        let entries: usize = self
            .later
            .values()
            .rev()
            .take(epochs)
            .map(|bytes| 2 + varint_len(*bytes as usize))
            .sum();
        // What the leaving epochs give back, less what their entries free.
        let given_back = self.bytes_of_latest(epochs) - keeps * epochs;
        let low = given_back.checked_sub(entries)? + CLEAR_OF_AN_EPOCH;
        let high = if epochs < self.later.len() {
            let before = self.bytes_of_latest(epochs + 1) - self.bytes_of_latest(epochs);
            (given_back + before).checked_sub(entries + keeps + CLEAR_OF_AN_EPOCH)?
        } else {
            self.len.checked_sub(shortest)?
        };
        (low <= high).then_some((low, high))
    }

    /// What `freed` bytes of the element give back to each epoch: the
    /// latest first, each down to the bytes it keeps, and the rest to the
    /// first epoch.
    fn given_back(&self, freed: u32) -> BTreeMap<u16, u32> {
        let mut given_back = BTreeMap::new();
        let mut left = freed;
        for (epoch, bytes) in self.later.iter().rev() {
            if left == 0 {
                break;
            }
            let frees = if *bytes <= left + BYTES_AN_EPOCH_KEEPS {
                bytes.saturating_sub(BYTES_AN_EPOCH_KEEPS)
            } else {
                left
            };
            if frees > 0 {
                given_back.insert(*epoch, frees);
            }
            left -= frees;
        }
        if left > 0 {
            given_back.insert(self.base_epoch, left);
        }
        given_back
    }

    /// What Drive's callback answers when the update of these flags by
    /// `written` adds `added` bytes or frees `freed`. `None` is no change
    /// to the flags written.
    fn merged(
        &self,
        written: &History,
        added: u32,
        freed: u32,
        callbacks: Callbacks,
    ) -> Option<History> {
        let mut merged = self.merged_as_a_batch_does(written, added, freed, callbacks)?;
        // A handover stores the owner written, whatever the merge kept.
        if let (Callbacks::Handover(_), Some(owner)) = (callbacks, written.owner) {
            merged.owner = Some(owner);
        }
        Some(merged)
    }

    /// [`Self::merged`] before a handover has its say on the owner.
    fn merged_as_a_batch_does(
        &self,
        written: &History,
        added: u32,
        freed: u32,
        callbacks: Callbacks,
    ) -> Option<History> {
        let epoch = written.base_epoch;
        let mut merged = History {
            owner: written.owner.or(self.owner),
            len: written.len,
            ..self.clone()
        };
        if added > 0 {
            if epoch > self.base_epoch {
                *merged.later.entry(epoch).or_default() += added;
            }
        } else if freed > 0 {
            if epoch > self.base_epoch {
                if self.later.is_empty() {
                    merged.owner = self.owner;
                }
                for (epoch, bytes) in self.given_back(freed) {
                    if let Some(recorded) = merged.later.get_mut(&epoch) {
                        *recorded -= bytes;
                        if *recorded <= BYTES_AN_EPOCH_KEEPS {
                            merged.later.remove(&epoch);
                        }
                    }
                }
            }
        } else if callbacks == Callbacks::PartialBatch {
            return None;
        } else {
            merged.owner = self.owner;
        }
        Some(merged)
    }

    /// Replaces the element by `written`. `None` when the callbacks refuse
    /// the update or the history cannot be followed past it.
    fn update(&mut self, written: &History, callbacks: Callbacks) -> Option<Update> {
        let epoch = written.base_epoch;
        let latest = self.later.keys().next_back().copied();
        if epoch < self.base_epoch || latest.is_some_and(|latest| epoch < latest) {
            return None;
        }
        let another_owner = match (self.owner, written.owner) {
            (Some(stored), Some(written)) => stored != written,
            _ => false,
        };
        if let Callbacks::Handover(full) = callbacks {
            let named = |owner: Option<Owner>| owner.is_some_and(|owner| full.contains(&owner));
            if another_owner && (named(self.owner) || named(written.owner)) {
                // A full handover: the element written replaces the one
                // stored, flags and all.
                *self = written.clone();
                return Some(Update {
                    named_another_owner: true,
                    replaced: true,
                    ..Update::default()
                });
            }
        }
        let was = self.charged(self.len);
        // GroveDB measures the update first with the flags the element
        // had, and that says whether it is longer, shorter or neither.
        let first = self.charged(written.len);
        // A partial batch refuses another owner where the length changes,
        // but not where flags of one epoch are kept as they are.
        let keeps_stored_flags = first < was && epoch > self.base_epoch && self.later.is_empty();
        if callbacks == Callbacks::PartialBatch
            && another_owner
            && first != was
            && !keeps_stored_flags
        {
            return None;
        }
        let mut update = Update {
            named_another_owner: another_owner,
            grew_in_later_epoch: first > was && epoch > self.base_epoch,
            kept_size: first == was && epoch > self.base_epoch,
            ..Update::default()
        };
        let mut now = first;
        let mut stored = None;
        for _ in 0..MOST_ROUNDS {
            let added = now.saturating_sub(was) as u32;
            let freed = was.saturating_sub(now) as u32;
            let merged = self
                .merged(written, added, freed, callbacks)
                .filter(|merged| merged != written);
            let Some(merged) = merged else {
                // The flags written are stored, and measured as they are.
                update.dropped_history = !self.later.is_empty();
                now = written.charged(written.len);
                stored = Some(written.clone());
                break;
            };
            let measured = merged.charged(written.len);
            if measured == now {
                stored = Some(merged);
                break;
            }
            now = measured;
        }
        // An update whose size has not settled by then is not followed.
        let stored = stored?;
        let freed = was.saturating_sub(now) as u32;
        update.shrank_across_epochs = self.given_back(freed).len() > 1;
        *self = stored;
        Some(update)
    }
}

/// A key in a subtree.
type Place = (Path, Vec<u8>);

/// What the operations of a stream left at the places they wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Places {
    /// The history of each element whose flags are known.
    followed: BTreeMap<Place, History>,
    /// The places that may hold an element whose flags are not known.
    unfollowed: BTreeSet<Place>,
    /// The trees that may hold elements whose flags are not known.
    unfollowed_trees: BTreeSet<Path>,
}

/// The history of every element a stream wrote through its flags
/// operations, in the open transaction.
///
/// A place holds a history only while the flags GroveDB stores there are
/// known. An element written some other way than by a put under Drive's
/// callbacks is followed no further, and neither is one written over it,
/// until the place is deleted. A transaction that is taken back gives the
/// places back as they were.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Histories {
    now: Places,
    committed: Places,
    savepoints: Vec<Places>,
}

impl Histories {
    /// No element yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The history of the element at `key` in the subtree at `path`.
    pub fn get(&self, path: &[Vec<u8>], key: &[u8]) -> Option<&History> {
        self.now.followed.get(&(path.to_vec(), key.to_vec()))
    }

    /// Follows one operation: what an operation that runs Drive's
    /// callbacks does to the flags of the elements it writes, what any
    /// other operation ends the history of, and what a step on the
    /// transaction keeps or takes back.
    pub fn record(&mut self, op: &Op) -> Seen {
        let mut seen = Seen::default();
        let callbacks = match op {
            Op::FlagsBatch(_) => Callbacks::Batch,
            Op::FlagsPartialBatch(_) => Callbacks::PartialBatch,
            Op::FlagsHandoverBatch(batch) => Callbacks::Handover(&batch.full_handover_owners),
            Op::Write(write) => {
                self.now.write_directly(write, &mut seen);
                return seen;
            }
            Op::Batch(_) | Op::PartialBatch(_) => {
                for batch_op in op.batch_ops() {
                    self.now.batch_op(batch_op, None, &mut Seen::default());
                }
                return seen;
            }
            Op::Commit => {
                self.committed = self.now.clone();
                self.savepoints.clear();
                return seen;
            }
            Op::Transaction(TransactionOp::Rollback | TransactionOp::Abandon) => {
                self.now = self.committed.clone();
                self.savepoints.clear();
                return seen;
            }
            Op::Transaction(TransactionOp::SetSavepoint) => {
                self.savepoints.push(self.now.clone());
                return seen;
            }
            Op::Transaction(TransactionOp::RollbackToSavepoint) => {
                if let Some(saved) = self.savepoints.pop() {
                    self.now = saved;
                }
                return seen;
            }
            Op::Read(_) | Op::FlushPendingDrops | Op::Transaction(_) => return seen,
        };
        for batch_op in op.batch_ops() {
            self.now.batch_op(batch_op, Some(callbacks), &mut seen);
        }
        seen
    }
}

impl Places {
    /// Whether the place may hold an element whose flags are not known.
    fn is_unfollowed(&self, path: &[Vec<u8>], key: &[u8]) -> bool {
        self.unfollowed.contains(&(path.to_vec(), key.to_vec()))
            || self
                .unfollowed_trees
                .iter()
                .any(|tree| path.starts_with(tree))
    }

    /// What one operation of a batch does: under Drive's `callbacks`, or
    /// under GroveDB's own.
    fn batch_op(&mut self, op: &BatchOp, callbacks: Option<Callbacks>, seen: &mut Seen) {
        match op {
            BatchOp::InsertOrReplace { path, key, element }
            | BatchOp::InsertOnly { path, key, element }
            | BatchOp::Replace { path, key, element } => match callbacks {
                Some(callbacks) => self.write(path, key, element, callbacks, seen),
                None => self.unfollow(path, key, element.is_any_tree()),
            },
            // Whether the key is taken is not known unless its element is
            // followed, in which case it stays as it is.
            BatchOp::InsertIfNotExists { path, key, .. } => {
                if callbacks.is_none() || !self.followed.contains_key(&(path.clone(), key.clone()))
                {
                    self.unfollow(path, key, false);
                }
            }
            BatchOp::Delete { path, key }
            | BatchOp::DeleteTree { path, key, .. }
            | BatchOp::DropFlat { path, key, .. } => self.delete(path, key, seen),
            BatchOp::Patch { path, key, .. } => self.unfollow(path, key, false),
            BatchOp::Move { path, key, new_key } => {
                self.delete(path, key, &mut Seen::default());
                self.delete(path, new_key, &mut Seen::default());
                self.unfollowed.insert((path.clone(), new_key.clone()));
                self.unfollowed_trees.insert(child(path, new_key));
            }
            BatchOp::RefreshReference { path, key, .. } => {
                if callbacks.is_none() {
                    self.unfollow(path, key, false);
                }
            }
            BatchOp::Append { .. } => {}
        }
    }

    /// What a write outside a batch does. It runs GroveDB's own callbacks,
    /// but for the two deletes that take Drive's.
    fn write_directly(&mut self, write: &Write, seen: &mut Seen) {
        match write {
            Write::SectionalDelete { path, key }
            | Write::SectionalDeleteUpTree { path, key, .. } => self.delete(path, key, seen),
            Write::Delete { path, key }
            | Write::DropFlat { path, key }
            | Write::DeleteUpTree { path, key, .. }
            | Write::DeleteBuilt { path, key }
            | Write::IndexedDelete { path, key, .. } => {
                self.delete(path, key, &mut Seen::default());
            }
            Write::Insert { path, key, .. }
            | Write::InsertIfNotExists { path, key, .. }
            | Write::InsertIfChanged { path, key, .. }
            | Write::IndexedInsert { path, key, .. } => self.unfollow(path, key, false),
            // The tree stays, with what it holds, unless it is empty.
            Write::DeleteIfEmptyTree { path, key } => self.unfollow(path, key, false),
            Write::ClearSubtree { path } => {
                self.forget_under(path);
            }
            Write::Append { .. }
            | Write::PutAux { .. }
            | Write::DeleteAux { .. }
            | Write::ReconcileIndexed { .. }
            | Write::CommitmentInsert { .. } => {}
        }
    }

    /// A put under Drive's callbacks.
    fn write(
        &mut self,
        path: &Path,
        key: &[u8],
        element: &Element,
        callbacks: Callbacks,
        seen: &mut Seen,
    ) {
        let place = (path.clone(), key.to_vec());
        let Some(flags) = written(element) else {
            self.unfollow(path, key, element.is_any_tree());
            return;
        };
        let Some(flags) = flags else {
            // GroveDB does not ask the callbacks about an element written
            // without flags: it is stored as it is, and the next flags
            // written over it are stored as they are.
            seen.update.removed_flags |= self.followed.remove(&place).is_some();
            self.unfollowed.remove(&place);
            return;
        };
        if self.is_unfollowed(path, key) {
            return;
        }
        let Some(history) = self.followed.get_mut(&place) else {
            self.followed.insert(place, flags);
            return;
        };
        match history.update(&flags, callbacks) {
            Some(update) => {
                let seen = &mut seen.update;
                seen.grew_in_later_epoch |= update.grew_in_later_epoch;
                seen.shrank_across_epochs |= update.shrank_across_epochs;
                seen.kept_size |= update.kept_size;
                seen.named_another_owner |= update.named_another_owner;
                seen.dropped_history |= update.dropped_history;
                seen.replaced |= update.replaced;
            }
            None => self.unfollow(path, key, false),
        }
    }

    /// The place holds an element whose flags are not known from here on.
    /// A tree written where something is known below is not known to hold
    /// it any more, or anything else.
    fn unfollow(&mut self, path: &[Vec<u8>], key: &[u8], tree: bool) {
        let place = (path.to_vec(), key.to_vec());
        self.followed.remove(&place);
        self.unfollowed.insert(place);
        let below = child(path, key);
        if tree && self.forget_under(&below) {
            self.unfollowed_trees.insert(below);
        }
    }

    /// Nothing is stored at the key, or under it where it held a tree.
    fn delete(&mut self, path: &[Vec<u8>], key: &[u8], seen: &mut Seen) {
        let place = (path.to_vec(), key.to_vec());
        if let Some(history) = self.followed.remove(&place) {
            seen.deleted_several_epochs |= !history.later.is_empty();
        }
        self.unfollowed.remove(&place);
        self.forget_under(&child(path, key));
    }

    /// Nothing is stored under `tree`. Returns whether something was known
    /// to be.
    fn forget_under(&mut self, tree: &[Vec<u8>]) -> bool {
        let before = (
            self.followed.len(),
            self.unfollowed.len(),
            self.unfollowed_trees.len(),
        );
        self.followed.retain(|(path, _), _| !path.starts_with(tree));
        self.unfollowed.retain(|(path, _)| !path.starts_with(tree));
        self.unfollowed_trees.retain(|path| !path.starts_with(tree));
        let after = (
            self.followed.len(),
            self.unfollowed.len(),
            self.unfollowed_trees.len(),
        );
        before != after
    }
}

/// The path of the tree at `key` in the subtree at `path`.
fn child(path: &[Vec<u8>], key: &[u8]) -> Path {
    let mut tree = path.to_vec();
    tree.push(key.to_vec());
    tree
}

#[cfg(test)]
mod tests {
    use grovedb::TreeType;

    use super::*;
    use crate::workload::op::PartialBatch;

    const TREE: &[u8] = b"tree";
    const KEY: &[u8] = b"key";

    fn tree() -> Path {
        vec![TREE.to_vec()]
    }

    fn flags(epoch: u16, owner: Option<u8>) -> Option<Vec<u8>> {
        let owner = owner.map(|owner| [owner; 32]);
        Some(StorageFlags::new_single_epoch(epoch, owner).to_element_flags())
    }

    /// An item whose element is `len` bytes long without its flags.
    fn item(len: usize, epoch: u16, owner: Option<u8>) -> Element {
        Element::new_item_with_flags(vec![7; len - 3], flags(epoch, owner))
    }

    fn put(element: Element) -> BatchOp {
        BatchOp::InsertOrReplace {
            path: tree(),
            key: KEY.to_vec(),
            element,
        }
    }

    fn whole(element: Element) -> Op {
        Op::FlagsBatch(vec![put(element)])
    }

    fn in_segments(element: Element) -> Op {
        Op::FlagsPartialBatch(PartialBatch {
            ops: Vec::new(),
            add_on: vec![put(element)],
            pause_height: None,
        })
    }

    /// The history of the key after `ops`, and what the last of them did.
    fn after(ops: &[Op]) -> (Option<History>, Seen) {
        let mut histories = Histories::new();
        let mut seen = Seen::default();
        for op in ops {
            seen = histories.record(op);
        }
        (histories.get(&tree(), KEY).cloned(), seen)
    }

    fn later(history: &Option<History>) -> Vec<(u16, u32)> {
        let history = history.as_ref().expect("the element is followed");
        history
            .later
            .iter()
            .map(|(epoch, bytes)| (*epoch, *bytes))
            .collect()
    }

    #[test]
    fn an_epoch_is_charged_for_its_own_entry() {
        // A hundred bytes more, and the three bytes that record them.
        let grown = [whole(item(103, 0, Some(1))), whole(item(203, 2, Some(1)))];
        let (history, seen) = after(&grown);
        assert_eq!(later(&history), [(2, 103)]);
        assert!(seen.update.grew_in_later_epoch);
        assert_eq!(
            history.as_ref().map(|history| history.bytes_of_latest(1)),
            Some(103)
        );

        // From 128 bytes on an epoch takes another byte to count them.
        let stored = whole(item(103, 0, None));
        let (history, _) = after(&[stored.clone(), whole(item(227, 1, None))]);
        assert_eq!(later(&history), [(1, 124 + 3)]);
        let (history, _) = after(&[stored, whole(item(228, 1, None))]);
        assert_eq!(later(&history), [(1, 125 + 4)]);
        // So does the length GroveDB writes before a value, which counts
        // the 65 bytes it stores with every value.
        let stored = whole(item(20, 0, None));
        let (history, _) = after(&[stored.clone(), whole(item(40, 1, None))]);
        assert_eq!(later(&history), [(1, 20 + 3)]);
        let (history, _) = after(&[stored, whole(item(70, 1, None))]);
        assert_eq!(later(&history), [(1, 50 + 3 + 1)]);
    }

    #[test]
    fn freed_bytes_go_back_to_the_latest_epochs_first() {
        let mut ops = vec![
            whole(item(63, 0, Some(1))),
            whole(item(123, 2, Some(1))),
            whole(item(183, 5, Some(1))),
        ];
        assert_eq!(later(&after(&ops).0), [(2, 63), (5, 63)]);
        // Within the latest epoch.
        ops.push(whole(item(153, 6, Some(1))));
        let (history, seen) = after(&ops);
        assert_eq!(later(&history), [(2, 63), (5, 33)]);
        assert!(!seen.update.shrank_across_epochs);
        // Across it: of its 33 bytes it gives back 30 and leaves the flags,
        // which frees the three of its entry. The epoch before it gives
        // back those and the 20 that are left of the 50.
        ops.push(whole(item(103, 7, Some(1))));
        let (history, seen) = after(&ops);
        assert_eq!(later(&history), [(2, 63 - 23)]);
        assert!(seen.update.shrank_across_epochs);
        // Down to the first epoch, which gets what the last one keeps.
        ops.push(whole(item(43, 8, Some(1))));
        let (history, seen) = after(&ops);
        assert_eq!(later(&history), []);
        assert!(seen.update.shrank_across_epochs);
        ops.push(whole(item(40, 9, Some(1))));
        assert!(!after(&ops).1.update.shrank_across_epochs);
    }

    #[test]
    fn the_owner_follows_drives_callbacks() {
        let owner = |ops: &[Op]| after(ops).0.and_then(|history| history.owner);
        let (a, b) = (Some([1; 32]), Some([2; 32]));
        let stored = whole(item(103, 0, Some(1)));
        // Longer: the owner written.
        let longer = [stored.clone(), whole(item(203, 1, Some(2)))];
        assert_eq!(owner(&longer), b);
        assert!(after(&longer).1.update.named_another_owner);
        // Shorter or of the same length, of one epoch: the owner stored.
        assert_eq!(owner(&[stored.clone(), whole(item(53, 1, Some(2)))]), a);
        let kept = [stored.clone(), whole(item(103, 1, Some(2)))];
        assert_eq!(owner(&kept), a);
        assert!(after(&kept).1.update.kept_size);
        // Shorter, of several epochs: the owner written.
        let shorter = [
            longer[0].clone(),
            longer[1].clone(),
            whole(item(153, 2, Some(1))),
        ];
        assert_eq!(owner(&shorter), a);
        // Flags without an owner keep the owner stored, and take the one
        // written, which is then stored as written.
        assert_eq!(owner(&[stored.clone(), whole(item(203, 1, None))]), a);
        let unowned = [whole(item(103, 0, None)), whole(item(203, 0, Some(2)))];
        assert_eq!(owner(&unowned), b);
        assert!(!after(&unowned).1.update.named_another_owner);
    }

    #[test]
    fn a_partial_batch_stores_the_flags_written_with_the_same_length() {
        let grown = [
            whole(item(103, 0, Some(1))),
            in_segments(item(203, 2, Some(1))),
        ];
        assert_eq!(later(&after(&grown).0), [(2, 103)]);
        let mut ops = grown.to_vec();
        ops.push(in_segments(item(203, 4, Some(2))));
        let (history, seen) = after(&ops);
        let history = history.expect("followed");
        assert_eq!((history.owner, history.base_epoch), (Some([2; 32]), 4));
        assert!(history.later.is_empty());
        let update = seen.update;
        assert!(update.kept_size && update.dropped_history && update.named_another_owner);
        // Under another owner it keeps flags of one epoch where the
        // element gets shorter, and refuses any other change of length.
        ops.push(in_segments(item(153, 5, Some(1))));
        let (history, seen) = after(&ops);
        let history = history.expect("followed");
        assert_eq!((history.owner, history.base_epoch), (Some([2; 32]), 4));
        assert!(seen.update.named_another_owner);
        ops.push(in_segments(item(303, 6, Some(1))));
        assert_eq!(after(&ops).0, None);
        let grown = [
            whole(item(103, 0, Some(1))),
            in_segments(item(203, 2, Some(1))),
            in_segments(item(153, 3, Some(2))),
        ];
        assert_eq!(after(&grown).0, None);
    }

    #[test]
    fn what_cannot_be_followed_is_forgotten() {
        let stored = whole(item(103, 3, Some(1)));
        let grown = [stored.clone(), whole(item(203, 5, Some(1)))];
        let forgotten = [
            // An epoch before the first, and one before the latest.
            vec![stored.clone(), whole(item(203, 2, Some(1)))],
            vec![
                grown[0].clone(),
                grown[1].clone(),
                whole(item(303, 4, Some(1))),
            ],
            // In the first epoch, with a later one in the flags.
            vec![
                grown[0].clone(),
                grown[1].clone(),
                whole(item(153, 3, Some(1))),
            ],
            // Flags of several epochs, flags Drive cannot read, an element
            // charged a fixed size and a tree.
            vec![
                stored.clone(),
                whole(Element::new_item_with_flags(
                    vec![7; 40],
                    Some(StorageFlags::MultiEpoch(3, [(4, 9)].into()).to_element_flags()),
                )),
            ],
            vec![
                stored.clone(),
                whole(Element::new_item_with_flags(vec![7; 40], Some(vec![9, 9]))),
            ],
            vec![
                stored.clone(),
                whole(Element::new_sum_item_with_flags(4, flags(4, Some(1)))),
            ],
            vec![
                stored.clone(),
                whole(Element::empty_tree_with_flags(flags(4, Some(1)))),
            ],
            // Written some other way than a put.
            vec![
                stored.clone(),
                Op::FlagsBatch(vec![BatchOp::Patch {
                    path: tree(),
                    key: KEY.to_vec(),
                    element: item(113, 4, Some(1)),
                    change_in_bytes: 10,
                }]),
            ],
            vec![
                stored.clone(),
                Op::FlagsBatch(vec![BatchOp::Move {
                    path: tree(),
                    key: KEY.to_vec(),
                    new_key: b"other".to_vec(),
                }]),
            ],
        ];
        for ops in forgotten {
            assert_eq!(after(&ops).0, None, "{:?}", ops.last());
        }

        // An element written without flags is stored as it is.
        let (history, seen) = after(&[stored.clone(), whole(Element::new_item(vec![7; 9]))]);
        assert_eq!(history, None);
        assert!(seen.update.removed_flags);
        let unflagged = [whole(Element::new_item(vec![7; 9])), stored.clone()];
        assert!(!after(&unflagged).1.update.removed_flags);
        assert_eq!(later(&after(&unflagged).0), []);
    }

    #[test]
    fn deletes_and_other_operations() {
        let stored = whole(item(103, 0, Some(1)));
        let grown = [stored.clone(), whole(item(203, 1, Some(1)))];
        let delete = Op::FlagsBatch(vec![BatchOp::Delete {
            path: tree(),
            key: KEY.to_vec(),
        }]);
        let directly = Op::Write(Write::SectionalDelete {
            path: tree(),
            key: KEY.to_vec(),
        });
        let up_the_tree = Op::Write(Write::SectionalDeleteUpTree {
            path: tree(),
            key: KEY.to_vec(),
            stop_path_height: None,
        });
        for delete in [delete, directly, up_the_tree] {
            let (history, seen) = after(&[grown[0].clone(), grown[1].clone(), delete.clone()]);
            assert_eq!(history, None);
            assert!(seen.deleted_several_epochs, "{delete:?}");
            assert!(!after(&[stored.clone(), delete]).1.deleted_several_epochs);
        }
        // A deleted tree takes what it holds with it.
        for tree in [
            BatchOp::DeleteTree {
                path: Vec::new(),
                key: TREE.to_vec(),
                tree_type: TreeType::NormalTree,
            },
            BatchOp::DropFlat {
                path: Vec::new(),
                key: TREE.to_vec(),
                tree_type: TreeType::NormalTree,
            },
        ] {
            assert_eq!(after(&[stored.clone(), Op::FlagsBatch(vec![tree])]).0, None);
        }

        // An insert that finds the key taken, an append, a read and a
        // commit change nothing. An insert at a key that is not followed
        // may find it taken, so it starts no history.
        let taken = BatchOp::InsertIfNotExists {
            path: tree(),
            key: KEY.to_vec(),
            element: item(203, 1, Some(2)),
        };
        let kept = after(std::slice::from_ref(&stored)).0;
        assert_eq!(after(&[Op::FlagsBatch(vec![taken.clone()])]).0, None);
        let others = [
            Op::FlagsBatch(vec![taken]),
            Op::FlagsBatch(vec![BatchOp::Append {
                path: tree(),
                entry: crate::workload::op::AppendEntry::Mmr(vec![1]),
            }]),
            Op::Read(crate::workload::Read::IsEmptyTree { path: tree() }),
            Op::Transaction(TransactionOp::SetSavepoint),
            Op::Commit,
        ];
        for other in others {
            assert_eq!(after(&[stored.clone(), other]).0, kept);
        }

        // What an operation under GroveDB's own callbacks writes or
        // deletes is followed no further, and neither is what a
        // transaction wrote once it is taken back.
        let key = || KEY.to_vec();
        let enders = [
            Op::Batch(vec![put(item(203, 1, Some(2)))]),
            Op::PartialBatch(PartialBatch {
                ops: vec![put(item(203, 1, Some(2)))],
                add_on: Vec::new(),
                pause_height: None,
            }),
            Op::Write(Write::Insert {
                path: tree(),
                key: key(),
                element: item(203, 1, Some(2)),
            }),
            Op::Write(Write::Delete {
                path: tree(),
                key: key(),
            }),
            Op::Write(Write::Delete {
                path: Vec::new(),
                key: TREE.to_vec(),
            }),
            Op::Write(Write::ClearSubtree { path: tree() }),
            Op::Write(Write::ClearSubtree { path: Vec::new() }),
            Op::Transaction(TransactionOp::Rollback),
            Op::Transaction(TransactionOp::Abandon),
        ];
        for ender in enders {
            assert_eq!(after(&[stored.clone(), ender.clone()]).0, None, "{ender:?}");
        }
        let elsewhere = [
            Op::Write(Write::PutAux {
                key: key(),
                value: vec![1],
            }),
            Op::Transaction(TransactionOp::RollbackToSavepoint),
            Op::Batch(vec![BatchOp::Append {
                path: tree(),
                entry: crate::workload::op::AppendEntry::Mmr(vec![1]),
            }]),
        ];
        for other in elsewhere {
            assert_eq!(after(&[stored.clone(), other]).0, kept);
        }
    }

    /// An element GroveDB stores with flags the model does not know is
    /// followed again only once the place has been deleted or holds an
    /// element without flags: until then GroveDB merges what is written
    /// with flags the model cannot name.
    #[test]
    fn a_place_is_followed_again_once_it_is_known_to_be_empty() {
        let delete = Op::FlagsBatch(vec![BatchOp::Delete {
            path: tree(),
            key: KEY.to_vec(),
        }]);
        let grown = whole(item(143, 2, Some(1)));
        let written_some_other_way = [
            Op::Batch(vec![put(item(43, 0, Some(1)))]),
            Op::Write(Write::Insert {
                path: tree(),
                key: KEY.to_vec(),
                element: item(43, 0, Some(1)),
            }),
            // Flags of several epochs, which a put does not start with.
            whole(Element::new_item_with_flags(
                vec![7; 40],
                Some(StorageFlags::MultiEpoch(0, [(1, 9)].into()).to_element_flags()),
            )),
            Op::FlagsBatch(vec![BatchOp::InsertIfNotExists {
                path: tree(),
                key: KEY.to_vec(),
                element: item(43, 0, Some(1)),
            }]),
            Op::FlagsBatch(vec![BatchOp::Patch {
                path: tree(),
                key: KEY.to_vec(),
                element: item(43, 0, Some(1)),
                change_in_bytes: 0,
            }]),
            Op::Batch(vec![BatchOp::RefreshReference {
                path: tree(),
                key: KEY.to_vec(),
                reference: grovedb::reference_path::ReferencePathType::SiblingReference(
                    b"other".to_vec(),
                ),
                max_hop: None,
                flags: None,
                non_counted: false,
                trusted: true,
            }]),
            Op::Write(Write::DeleteIfEmptyTree {
                path: tree(),
                key: KEY.to_vec(),
            }),
            // Moved here, and into a tree that was moved here.
            Op::FlagsBatch(vec![BatchOp::Move {
                path: tree(),
                key: b"other".to_vec(),
                new_key: KEY.to_vec(),
            }]),
            Op::Batch(vec![BatchOp::Move {
                path: Vec::new(),
                key: b"other".to_vec(),
                new_key: TREE.to_vec(),
            }]),
        ];
        for first in written_some_other_way {
            let ops = [first.clone(), grown.clone()];
            assert_eq!(after(&ops).0, None, "{first:?}");
            let ops = [first.clone(), grown.clone(), grown.clone()];
            assert_eq!(after(&ops).0, None, "{first:?}");
        }
        // Deleted, the place is empty, and without flags the element is
        // stored as it is written.
        let first = Op::Batch(vec![put(item(43, 0, Some(1)))]);
        let ops = [first.clone(), delete, grown.clone()];
        assert_eq!(later(&after(&ops).0), []);
        let ops = [first, whole(Element::new_item(vec![7; 9])), grown.clone()];
        assert_eq!(later(&after(&ops).0), []);

        // A tree written over one that holds something may hold anything.
        let over = Op::FlagsBatch(vec![BatchOp::InsertOrReplace {
            path: Vec::new(),
            key: TREE.to_vec(),
            element: Element::empty_tree(),
        }]);
        assert_eq!(after(&[grown.clone(), over.clone(), grown.clone()]).0, None);
        assert_eq!(later(&after(&[over, grown.clone()]).0), []);
        // A tree that is deleted or cleared holds nothing.
        let delete_tree = Op::Write(Write::Delete {
            path: Vec::new(),
            key: TREE.to_vec(),
        });
        let clear = Op::Write(Write::ClearSubtree { path: tree() });
        for emptied in [delete_tree, clear] {
            let ops = [
                Op::Batch(vec![put(item(43, 0, Some(1)))]),
                emptied,
                grown.clone(),
            ];
            assert_eq!(later(&after(&ops).0), []);
        }
    }

    /// A transaction that is taken back, wholly or to a savepoint, leaves
    /// the histories as they were then.
    #[test]
    fn a_transaction_taken_back_gives_the_histories_back() {
        let stored = whole(item(43, 0, Some(1)));
        let grown = whole(item(143, 2, Some(1)));
        let longer = whole(item(243, 4, Some(1)));
        let step = Op::Transaction;
        for back in [TransactionOp::Rollback, TransactionOp::Abandon] {
            // What was committed stays, and what was not is gone.
            let ops = [
                stored.clone(),
                Op::Commit,
                grown.clone(),
                step(back.clone()),
            ];
            assert_eq!(later(&after(&ops).0), [], "{back:?}");
            let ops = [stored.clone(), step(back.clone()), grown.clone()];
            assert_eq!(later(&after(&ops).0), [], "{back:?}");
            let ops = [
                stored.clone(),
                Op::Commit,
                step(back.clone()),
                grown.clone(),
            ];
            assert_eq!(later(&after(&ops).0), [(2, 103)], "{back:?}");
            // A savepoint does not outlive its transaction.
            let ops = [
                stored.clone(),
                step(TransactionOp::SetSavepoint),
                grown.clone(),
                step(back),
                step(TransactionOp::RollbackToSavepoint),
            ];
            assert_eq!(after(&ops).0, None);
        }
        // Back to the latest savepoint, which is used up, then to the one
        // before it.
        let ops = [
            stored.clone(),
            step(TransactionOp::SetSavepoint),
            grown.clone(),
            step(TransactionOp::SetSavepoint),
            longer,
            step(TransactionOp::RollbackToSavepoint),
        ];
        assert_eq!(later(&after(&ops).0), [(2, 103)]);
        let mut ops = ops.to_vec();
        ops.push(step(TransactionOp::RollbackToSavepoint));
        assert_eq!(later(&after(&ops).0), []);
        // A commit keeps what the transaction wrote and ends its
        // savepoints.
        let ops = [
            stored,
            step(TransactionOp::SetSavepoint),
            grown,
            Op::Commit,
            step(TransactionOp::RollbackToSavepoint),
            step(TransactionOp::Rollback),
        ];
        assert_eq!(later(&after(&ops).0), [(2, 103)]);
    }

    #[test]
    fn a_handover_stores_the_owner_written() {
        let hand_over = |element: Element, sponsors: &[Owner]| {
            Op::FlagsHandoverBatch(crate::workload::op::HandoverBatch {
                ops: vec![put(element)],
                full_handover_owners: sponsors.iter().copied().collect(),
            })
        };
        let owner = |ops: &[Op]| after(ops).0.and_then(|history| history.owner);
        let (a, b) = (Some([1; 32]), Some([2; 32]));
        let stored = whole(item(103, 0, Some(1)));
        // Where a batch keeps the stored owner: the same length, and
        // shorter with flags of one epoch.
        for len in [103, 53] {
            let ops = [stored.clone(), hand_over(item(len, 1, Some(2)), &[])];
            assert_eq!(owner(&ops), b, "{len}");
            let (history, seen) = after(&ops);
            assert_eq!(history.map(|history| history.base_epoch), Some(0));
            assert!(seen.update.named_another_owner && !seen.update.replaced);
        }
        // Flags that name no owner leave the stored one.
        assert_eq!(
            owner(&[stored.clone(), hand_over(item(103, 1, None), &[])]),
            a
        );
        // The history is kept as a batch keeps it.
        let grown = [stored.clone(), hand_over(item(203, 2, Some(2)), &[[9; 32]])];
        assert_eq!(later(&after(&grown).0), [(2, 103)]);
        assert_eq!(owner(&grown), b);

        // A full handover stores the element as written: to a sponsor,
        // from one, and not between two owners that are none.
        for sponsor in [[1; 32], [2; 32]] {
            let mut ops = grown.to_vec();
            ops.push(hand_over(item(243, 4, Some(1)), &[sponsor]));
            let (history, seen) = after(&ops);
            let history = history.expect("followed");
            assert_eq!((history.owner, history.base_epoch), (a, 4));
            assert!(history.later.is_empty());
            assert!(seen.update.replaced && seen.update.named_another_owner);
        }
        let mut ops = grown.to_vec();
        ops.push(hand_over(item(243, 4, Some(1)), &[[9; 32]]));
        assert_eq!(later(&after(&ops).0), [(2, 103), (4, 43)]);
        assert!(!after(&ops).1.update.replaced);
        // The same owner on both sides is no handover.
        let mut ops = grown.to_vec();
        ops.push(hand_over(item(243, 4, Some(2)), &[[2; 32]]));
        assert!(!after(&ops).1.update.replaced);
    }

    /// The bounds [`History::freeable_across`] gives are the ones the
    /// update itself keeps: at either end exactly that many epochs leave
    /// the flags, and the element stays followed.
    #[test]
    fn the_bounds_of_freeing_across_epochs_hold() {
        let mut rng = crate::workload::Rng::derive(7, 0xF1A6, 0);
        let mut checked = 0;
        for _ in 0..300 {
            let mut len = 3 + rng.between(40, 200) as usize;
            let mut ops = vec![whole(item(len, 0, Some(1)))];
            let epochs = rng.between(1, 6) as u16;
            for epoch in 1..=epochs {
                len += rng.between(1, 300) as usize;
                // An item of 254 or 255 bytes does not exist.
                len += usize::from(len == 254 || len == 255) * 2;
                ops.push(whole(item(len, epoch, Some(1))));
            }
            let history = after(&ops).0.expect("followed");
            assert_eq!(history.later.len(), usize::from(epochs));
            for leaving in 1..=history.later.len() {
                let Some((low, high)) = history.freeable_across(leaving, 43) else {
                    continue;
                };
                for freed in [low, high] {
                    let shorter = history.len - freed;
                    if shorter == 254 || shorter == 255 {
                        continue;
                    }
                    let mut ops = ops.clone();
                    ops.push(whole(item(shorter, epochs + 1, Some(1))));
                    let (after, seen) = after(&ops);
                    let after = after.expect("followed");
                    assert_eq!(
                        after.later.len(),
                        history.later.len() - leaving,
                        "{history:?} less {freed}"
                    );
                    assert!(seen.update.shrank_across_epochs);
                    checked += 1;
                }
            }
            assert_eq!(history.freeable_across(0, 43), None);
            assert_eq!(history.freeable_across(history.later.len() + 1, 43), None);
        }
        assert!(checked > 500, "{checked}");
    }

    #[test]
    fn lengths_take_the_bytes_grovedb_writes() {
        assert_eq!(
            [0, 250, 251, 0xFFFF, 0x1_0000].map(length_prefix),
            [1, 1, 3, 3, 5]
        );
        assert_eq!(
            [0, 127, 128, 16_383, 16_384].map(varint_len),
            [1, 1, 2, 2, 3]
        );
    }
}
