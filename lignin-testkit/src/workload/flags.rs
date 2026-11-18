//! What the storage flags of an element hold after each write of a stream.
//!
//! Drive stores with every element the epoch it was created in, its owner
//! and, for each later epoch that made it longer, the bytes that epoch
//! added. The callbacks [`super::apply()`] passes with an
//! [`Op::FlagsBatch`] and an [`Op::FlagsPartialBatch`] keep that history as
//! the element is replaced. [`Histories`] follows it along a stream, from
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
//! items and references: a tree element is rewritten by GroveDB itself with
//! the flags it has, and the other elements are charged a fixed size.
//!
//! [`Coverage`]: super::Coverage

use std::collections::BTreeMap;

use grovedb::Element;
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_version::version::v1::GROVE_V1;

use super::op::{BatchOp, Op, Path, Write};

/// The 32-byte id of an owner.
pub type Owner = [u8; 32];

/// The bytes of its own that an epoch keeps when an update frees the rest:
/// the least its entry in the flags takes.
pub const BYTES_AN_EPOCH_KEEPS: u32 = 3;

/// How far, in bytes, a generator keeps what an update frees from the
/// bytes the epochs in the flags hold. An epoch that gives back all it can
/// leaves the flags, and the bytes of its entry are freed with it, so a few
/// bytes more are freed than the update takes off the element. This many
/// cover three epochs leaving at once.
pub const CLEAR_OF_AN_EPOCH: usize = 32;

/// The bytes GroveDB stores with every value in a tree that aggregates
/// nothing, before the length it writes: two hashes and the feature byte.
const NODE_OVERHEAD: usize = 65;

/// How often GroveDB asks the flags callback about one update before it
/// gives up.
const MOST_ROUNDS: usize = 8;

/// Which pair of callbacks an update runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callbacks {
    /// Those of an [`Op::FlagsBatch`].
    Batch,
    /// Those of an [`Op::FlagsPartialBatch`].
    PartialBatch,
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
        let was = self.charged(self.len);
        // GroveDB measures the update first with the flags the element
        // had, and that says whether it is longer, shorter or neither.
        let first = self.charged(written.len);
        if first != was {
            let at_base = epoch == self.base_epoch;
            if (callbacks == Callbacks::PartialBatch && another_owner)
                || (first < was && at_base && !self.later.is_empty())
            {
                // Refused, or flags that keep epochs whose bytes went.
                return None;
            }
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
            let settled = measured == now;
            now = measured;
            if settled {
                stored = Some(merged);
                break;
            }
        }
        // An update whose size does not settle is refused.
        let stored = stored?;
        let freed = was.saturating_sub(now) as u32;
        update.shrank_across_epochs = self.given_back(freed).len() > 1;
        *self = stored;
        Some(update)
    }
}

/// The history of every element a stream wrote through its flags
/// operations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Histories {
    elements: BTreeMap<(Path, Vec<u8>), History>,
}

impl Histories {
    /// No element yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The history of the element at `key` in the subtree at `path`.
    pub fn get(&self, path: &[Vec<u8>], key: &[u8]) -> Option<&History> {
        self.elements.get(&(path.to_vec(), key.to_vec()))
    }

    /// Follows one operation. An operation that is not one of the four
    /// that run Drive's callbacks is not followed.
    pub fn record(&mut self, op: &Op) -> Seen {
        let mut seen = Seen::default();
        let callbacks = match op {
            Op::FlagsBatch(_) => Callbacks::Batch,
            Op::FlagsPartialBatch(_) => Callbacks::PartialBatch,
            Op::Write(
                Write::SectionalDelete { path, key }
                | Write::SectionalDeleteUpTree { path, key, .. },
            ) => {
                self.delete(path, key, &mut seen);
                return seen;
            }
            _ => return seen,
        };
        for batch_op in op.batch_ops() {
            match batch_op {
                BatchOp::InsertOrReplace { path, key, element }
                | BatchOp::InsertOnly { path, key, element }
                | BatchOp::Replace { path, key, element } => {
                    self.write(path, key, element, callbacks, &mut seen);
                }
                BatchOp::InsertIfNotExists { path, key, element } => {
                    if self.get(path, key).is_none() {
                        self.write(path, key, element, callbacks, &mut seen);
                    }
                }
                BatchOp::Delete { path, key }
                | BatchOp::DeleteTree { path, key, .. }
                | BatchOp::DropFlat { path, key, .. } => self.delete(path, key, &mut seen),
                BatchOp::Patch { path, key, .. } | BatchOp::Move { path, key, .. } => {
                    self.elements.remove(&(path.clone(), key.clone()));
                }
                BatchOp::RefreshReference { .. } | BatchOp::Append { .. } => {}
            }
        }
        seen
    }

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
            self.elements.remove(&place);
            return;
        };
        let Some(flags) = flags else {
            // GroveDB does not ask the callbacks about an element written
            // without flags: it is stored as it is.
            seen.update.removed_flags |= self.elements.remove(&place).is_some();
            return;
        };
        let Some(history) = self.elements.get_mut(&place) else {
            self.elements.insert(place, flags);
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
            }
            None => {
                self.elements.remove(&place);
            }
        }
    }

    /// Forgets the element at the key and, where the key held a tree,
    /// everything under it.
    fn delete(&mut self, path: &Path, key: &[u8], seen: &mut Seen) {
        if let Some(history) = self.elements.remove(&(path.clone(), key.to_vec())) {
            seen.deleted_several_epochs |= !history.later.is_empty();
        }
        let mut tree = path.clone();
        tree.push(key.to_vec());
        self.elements
            .retain(|(path, _), _| !path.starts_with(&tree));
    }
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
        // Under another owner it refuses any other length.
        ops.push(in_segments(item(303, 5, Some(1))));
        assert_eq!(after(&ops).0, None);
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
            // Shorter in the first epoch, with a later one in the flags.
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

        // An insert that finds the key taken, a refreshed reference, an
        // append and an operation under GroveDB's own callbacks change
        // nothing.
        let taken = BatchOp::InsertIfNotExists {
            path: tree(),
            key: KEY.to_vec(),
            element: item(203, 1, Some(2)),
        };
        let kept = after(std::slice::from_ref(&stored)).0;
        assert_eq!(
            after(&[stored.clone(), Op::FlagsBatch(vec![taken.clone()])]).0,
            kept
        );
        let free = after(&[Op::FlagsBatch(vec![taken])]).0.expect("stored");
        assert_eq!((free.owner, free.base_epoch), (Some([2; 32]), 1));
        let others = [
            Op::FlagsBatch(vec![BatchOp::Append {
                path: tree(),
                entry: crate::workload::op::AppendEntry::Mmr(vec![1]),
            }]),
            Op::Batch(vec![put(item(203, 1, Some(2)))]),
            Op::Commit,
        ];
        for other in others {
            assert_eq!(after(&[stored.clone(), other]).0, kept);
        }
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
