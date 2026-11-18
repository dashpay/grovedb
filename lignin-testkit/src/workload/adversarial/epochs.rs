//! Storage flags driven through every case of Drive's callbacks.

use grovedb::{
    element::reference_path::ReferencePathType, Element, ElementFlags, PathQuery, Query,
};

use super::DOMAIN_EPOCHS;
use crate::workload::{
    build::{self, batch, child, path, storage_flags},
    flags::{Histories, History, BYTES_AN_EPOCH_KEEPS, CLEAR_OF_AN_EPOCH},
    op::{BatchOp, Op, PartialBatch, Path, Write},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"epochs";

/// The length of an element as it is first stored.
const FIRST_LEN: usize = 64;

/// The most rungs a ladder of [`FlagEpochs`] has: every rung is an epoch,
/// and the sections after the ladders need epochs of their own.
pub const MAX_LADDER_RUNGS: u32 = 2_000;

/// Elements written across many epochs, through every case the callbacks
/// Drive passes with a batch tell apart.
///
/// [`Op::FlagsBatch`], [`Op::FlagsPartialBatch`] and the two deletes that
/// take the callback for freed bytes are applied to elements whose flags
/// are in every state the callbacks can leave them in:
///
/// - **Ladders.** One element is made longer in each of many epochs, twice
///   in some, by amounts on both sides of the 128 bytes from which the
///   flags take another byte to count them. It is then made shorter step by
///   step: within the latest epoch, which alone gets bytes back, and across
///   one, two and three epochs, which leave the flags. Another is deleted
///   with all its epochs.
/// - **Owners.** Updates whose flags name another owner than the stored
///   ones. Drive's callback stores the owner written when the update makes
///   the element longer or makes one of several epochs shorter, and keeps
///   the stored flags when it keeps the length or shortens an element of
///   one epoch.
/// - **No owner.** The same history on flags that name none: the freed
///   bytes are named by the identity of 32 zero bytes.
/// - **A reference**, which grows by one byte, and whose flags record it.
/// - **Flags that come and go**, on the grove versions that measure such an
///   update from the bytes it stores: an element without flags over one
///   with them, and the other way round.
/// - **Partial batches.** Growth in the first segment and in the second,
///   shrinking across epochs, a delete, and the update that keeps the
///   length, which a partial batch stores with the flags written: under the
///   same owner, under another, and, on the same grove versions, over flags
///   of several epochs, whose history goes.
/// - **Direct deletes** of an element of several epochs, of a tree with
///   what it holds, and up the tree through trees of two owners.
///
/// Each section ends a block, after which everything under the root is read
/// as stored and proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagEpochs {
    /// How many epochs make a ladder's element longer, at least four and at
    /// most [`MAX_LADDER_RUNGS`].
    pub rungs: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for FlagEpochs {
    fn default() -> Self {
        Self {
            rungs: 9,
            features: Features::ALL,
        }
    }
}

struct Script {
    rng: Rng,
    root: Path,
    owners: [[u8; 32]; 2],
    epoch: u16,
    histories: Histories,
    ops: Vec<Op>,
}

/// Whose flags an element is written with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    None,
    First,
    Second,
}

impl FlagEpochs {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            (4..=MAX_LADDER_RUNGS).contains(&self.rungs),
            "a ladder has 4 to {MAX_LADDER_RUNGS} rungs"
        );
        let mut rng = Rng::derive(seed, DOMAIN_EPOCHS, 0);
        let owners = [rng.id(), rng.id()];
        let mut script = Script {
            rng,
            root: path(&[ROOT]),
            owners,
            epoch: 0,
            histories: Histories::new(),
            ops: vec![build::insert(&[], ROOT, Element::empty_tree()), Op::Commit],
        };
        script.ladders(self.rungs);
        script.owners();
        script.no_owner();
        script.reference();
        script.flags_come_and_go(self.features.flag_length_changes);
        script.partial_batches(self.features.flag_length_changes);
        script.direct_deletes();
        Prepared::stream(script.ops)
    }
}

impl Script {
    fn flags(&self, owner: Owner) -> Option<ElementFlags> {
        let owner = match owner {
            Owner::None => None,
            Owner::First => Some(&self.owners[0]),
            Owner::Second => Some(&self.owners[1]),
        };
        storage_flags(self.epoch, owner)
    }

    fn item(&mut self, len: usize, owner: Owner) -> Element {
        let flags = self.flags(owner);
        build::item_of_len(&mut self.rng, len, flags)
    }

    fn history(&self, key: &[u8]) -> &History {
        self.histories
            .get(&self.root, key)
            .expect("the element is followed")
    }

    fn len(&self, key: &[u8]) -> usize {
        self.history(key).len
    }

    fn push(&mut self, op: Op) {
        self.histories.record(&op);
        self.ops.push(op);
    }

    fn batch(&mut self, ops: Vec<BatchOp>) {
        self.push(Op::FlagsBatch(ops));
    }

    fn partial(&mut self, ops: Vec<BatchOp>, add_on: Vec<BatchOp>) {
        self.push(Op::FlagsPartialBatch(PartialBatch {
            ops,
            add_on,
            pause_height: None,
        }));
    }

    /// An element stored at `key`, and the next epoch begun.
    fn store(&mut self, key: &[u8], owner: Owner) {
        let element = self.item(FIRST_LEN, owner);
        self.batch(vec![batch::insert_only(&self.root, key, element)]);
        self.epoch += 1;
    }

    /// The element at `key` written again, `len` bytes long, by a batch.
    fn rewrite(&mut self, key: &[u8], len: usize, owner: Owner) {
        let element = self.item(len, owner);
        self.batch(vec![batch::replace(&self.root, key, element)]);
    }

    /// The same by a partial batch, in its first segment.
    fn rewrite_in_segments(&mut self, key: &[u8], len: usize, owner: Owner) {
        let element = self.item(len, owner);
        self.partial(vec![batch::replace(&self.root, key, element)], Vec::new());
    }

    /// A length longer than the element's by 100 to 160 bytes: the bytes an
    /// epoch adds take one byte to count below 128 and two from there.
    fn longer(&mut self, key: &[u8]) -> usize {
        self.len(key) + self.rng.between(100, 160) as usize
    }

    /// A length that frees every byte the `epochs` latest epochs in the
    /// flags can give back, and bytes of the one before them or of the
    /// first epoch. Where that epoch holds too few bytes to keep some, it
    /// is freed as well.
    fn across(&mut self, key: &[u8], epochs: usize) -> usize {
        let history = self.history(key);
        let (len, held) = (history.len, history.later.len());
        let (low, high) = (epochs..=held)
            .map(|epochs| {
                let high = if epochs < held {
                    history.bytes_of_latest(epochs + 1) - CLEAR_OF_AN_EPOCH
                } else {
                    len - FIRST_LEN / 2
                };
                (history.bytes_of_latest(epochs) + CLEAR_OF_AN_EPOCH, high)
            })
            .find(|(low, high)| low <= high)
            .expect("the first epoch holds more than the clearance");
        len - self.rng.between(low as u64, high as u64) as usize
    }

    /// Whether the latest epoch in the flags holds enough bytes to give
    /// some back and keep some.
    fn has_room_within(&self, key: &[u8]) -> bool {
        self.history(key).bytes_of_latest(1)
            >= 2 * CLEAR_OF_AN_EPOCH + BYTES_AN_EPOCH_KEEPS as usize
    }

    /// A length that frees bytes of the latest epoch in the flags only.
    fn within(&mut self, key: &[u8]) -> usize {
        let high = self.history(key).bytes_of_latest(1)
            - BYTES_AN_EPOCH_KEEPS as usize
            - CLEAR_OF_AN_EPOCH;
        self.len(key) - self.rng.between(CLEAR_OF_AN_EPOCH as u64, high as u64) as usize
    }

    /// Makes the element at `key` longer in each of `epochs` epochs.
    fn climb(&mut self, key: &[u8], epochs: u32, owner: Owner, partial: bool) {
        for rung in 0..epochs {
            for _ in 0..=u32::from(rung % 3 == 2) {
                let len = self.longer(key);
                if partial {
                    self.rewrite_in_segments(key, len, owner);
                } else {
                    self.rewrite(key, len, owner);
                }
            }
            self.epoch += 1;
        }
    }

    /// The end of a block, and what the root holds read back.
    fn commit(&mut self) {
        self.ops.push(Op::Commit);
        let all = || PathQuery::new_unsized(self.root.clone(), Query::new_range_full());
        self.ops.push(build::rows(all(), true));
        self.ops.push(build::proven(all()));
    }

    fn ladders(&mut self, rungs: u32) {
        let [ladder, tower]: [&[u8]; 2] = [b"ladder", b"tower"];
        self.store(ladder, Owner::First);
        self.climb(ladder, rungs, Owner::First, false);
        self.commit();

        // Down again: within the latest epoch, then across one, two and
        // three epochs, until the flags hold the first epoch alone.
        let mut step = 0;
        while !self.history(ladder).later.is_empty() {
            let held = self.history(ladder).later.len();
            let len = if step % 2 == 0 && self.has_room_within(ladder) {
                self.within(ladder)
            } else {
                self.across(ladder, (1 + step / 2 % 3).min(held))
            };
            self.rewrite(ladder, len, Owner::First);
            self.epoch += 1;
            step += 1;
        }
        // Of one epoch again: shorter once more, longer, and deleted.
        let len = self.len(ladder) - CLEAR_OF_AN_EPOCH;
        self.rewrite(ladder, len, Owner::First);
        self.epoch += 1;
        let len = self.longer(ladder);
        self.rewrite(ladder, len, Owner::First);
        self.commit();

        self.store(tower, Owner::Second);
        self.climb(tower, 4, Owner::Second, false);
        self.batch(vec![
            batch::delete(&self.root, tower),
            batch::delete(&self.root, ladder),
        ]);
        self.commit();
    }

    fn owners(&mut self) {
        let [deed, young]: [&[u8]; 2] = [b"deed", b"young"];
        // Longer under another owner: the owner written is stored.
        self.store(deed, Owner::First);
        let len = self.longer(deed);
        self.rewrite(deed, len, Owner::Second);
        self.epoch += 1;
        let len = self.longer(deed);
        self.rewrite(deed, len, Owner::Second);
        self.epoch += 1;
        // Shorter, of several epochs: the owner written is stored again.
        let len = self.within(deed);
        self.rewrite(deed, len, Owner::First);
        self.epoch += 1;
        // The same length: the stored flags stay.
        let len = self.len(deed);
        self.rewrite(deed, len, Owner::Second);
        self.epoch += 1;
        let len = self.across(deed, 1);
        self.rewrite(deed, len, Owner::Second);
        self.epoch += 1;
        self.commit();

        // An element of one epoch: shorter or of the same length under
        // another owner, it keeps its flags.
        self.store(young, Owner::First);
        let len = self.len(young) - CLEAR_OF_AN_EPOCH;
        self.rewrite(young, len, Owner::Second);
        self.epoch += 1;
        self.rewrite(young, len, Owner::Second);
        self.epoch += 1;
        let len = self.longer(young);
        self.rewrite(young, len, Owner::Second);
        self.epoch += 1;
        self.commit();
    }

    fn no_owner(&mut self) {
        let common: &[u8] = b"common";
        self.store(common, Owner::None);
        self.climb(common, 3, Owner::None, false);
        let len = self.across(common, 1);
        self.rewrite(common, len, Owner::None);
        self.epoch += 1;
        let len = self.len(common);
        self.rewrite(common, len, Owner::None);
        self.commit();
        self.batch(vec![batch::delete(&self.root, common)]);
        self.commit();
    }

    fn reference(&mut self) {
        let [anchor, link]: [&[u8]; 2] = [b"anchor", b"link"];
        let target = ReferencePathType::AbsolutePathReference(child(&self.root, anchor));
        let reference = |script: &Script, max_hop| {
            Element::new_reference_with_max_hops_and_flags(
                target.clone(),
                max_hop,
                script.flags(Owner::First),
            )
        };
        let element = self.item(FIRST_LEN, Owner::First);
        self.batch(vec![
            batch::insert_only(&self.root, anchor, element),
            batch::insert_only(&self.root, link, reference(self, None)),
        ]);
        self.epoch += 1;
        // A hop limit is one byte more.
        self.batch(vec![batch::replace(
            &self.root,
            link,
            reference(self, Some(4)),
        )]);
        self.epoch += 1;
        self.commit();
        self.batch(vec![batch::delete(&self.root, link)]);
        self.commit();
    }

    fn flags_come_and_go(&mut self, flag_length_changes: bool) {
        let bare: &[u8] = b"bare";
        if flag_length_changes {
            self.store(bare, Owner::First);
            let len = self.longer(bare);
            self.rewrite(bare, len, Owner::First);
            self.epoch += 1;
            // Without flags, over flags of two epochs.
            let element = build::item_of_len(&mut self.rng, len + CLEAR_OF_AN_EPOCH, None);
            self.batch(vec![batch::replace(&self.root, bare, element)]);
        } else {
            let element = build::item_of_len(&mut self.rng, FIRST_LEN, None);
            self.batch(vec![batch::insert_only(&self.root, bare, element)]);
        }
        self.epoch += 1;
        // With flags, over none: they are stored as written.
        self.rewrite(bare, 2 * FIRST_LEN, Owner::Second);
        self.epoch += 1;
        let len = self.longer(bare);
        self.rewrite(bare, len, Owner::Second);
        self.epoch += 1;
        self.commit();
    }

    fn partial_batches(&mut self, flag_length_changes: bool) {
        let [grown, kept, past]: [&[u8]; 3] = [b"p-grown", b"p-kept", b"p-past"];
        let element = self.item(FIRST_LEN, Owner::First);
        self.partial(
            vec![batch::insert_only(&self.root, grown, element)],
            Vec::new(),
        );
        self.epoch += 1;
        self.climb(grown, 2, Owner::First, true);
        // Longer in the second segment, beside an insert in the first.
        let len = self.longer(grown);
        let (first, second) = (
            self.item(FIRST_LEN, Owner::Second),
            self.item(len, Owner::First),
        );
        self.partial(
            vec![batch::insert_only(&self.root, kept, first)],
            vec![batch::replace(&self.root, grown, second)],
        );
        self.epoch += 1;
        let len = self.across(grown, 2);
        self.rewrite_in_segments(grown, len, Owner::First);
        self.epoch += 1;
        self.commit();

        // The same length, of one epoch: the flags written are stored,
        // under the same owner and under another.
        let len = self.len(kept);
        self.rewrite_in_segments(kept, len, Owner::Second);
        self.epoch += 1;
        self.rewrite_in_segments(kept, len, Owner::First);
        self.epoch += 1;
        if flag_length_changes {
            // Over flags of several epochs, whose history goes.
            self.store(past, Owner::First);
            self.climb(past, 2, Owner::First, true);
            let len = self.len(past);
            self.rewrite_in_segments(past, len, Owner::First);
            self.epoch += 1;
        }
        self.partial(vec![batch::delete(&self.root, grown)], Vec::new());
        self.commit();
    }

    fn direct_deletes(&mut self) {
        let [lone, held, top, upper, lower, leaf]: [&[u8]; 6] =
            [b"d-lone", b"d-held", b"d-top", b"upper", b"lower", b"leaf"];
        self.store(lone, Owner::First);
        self.climb(lone, 3, Owner::First, false);

        // A tree of one owner holding elements of both.
        let tree = |script: &Script, owner| Element::empty_tree_with_flags(script.flags(owner));
        let in_held = child(&self.root, held);
        let in_top = child(&self.root, top);
        let in_upper = child(&in_top, upper);
        let in_lower = child(&in_upper, lower);
        let elements = [
            self.item(FIRST_LEN, Owner::First),
            self.item(FIRST_LEN, Owner::Second),
            self.item(FIRST_LEN, Owner::First),
            self.item(FIRST_LEN, Owner::Second),
        ];
        let [first, second, stays, third] = elements;
        self.batch(vec![
            batch::insert_only(&self.root, held, tree(self, Owner::Second)),
            batch::insert_only(&in_held, b"a", first),
            batch::insert_only(&in_held, b"b", second),
            batch::insert_only(&self.root, top, Element::empty_tree()),
            batch::insert_only(&in_top, b"stays", stays),
            batch::insert_only(&in_top, upper, tree(self, Owner::First)),
            batch::insert_only(&in_upper, lower, tree(self, Owner::Second)),
            batch::insert_only(&in_lower, leaf, third),
        ]);
        self.epoch += 1;
        for (path, key) in [(&in_held, &b"a"[..]), (&in_lower, leaf)] {
            for _ in 0..2 {
                let len = self.histories.get(path, key).expect("followed").len
                    + self.rng.between(100, 160) as usize;
                let element = self.item(len, Owner::Second);
                self.batch(vec![batch::replace(path, key, element)]);
                self.epoch += 1;
            }
        }
        self.commit();

        self.push(Op::Write(Write::SectionalDelete {
            path: self.root.clone(),
            key: lone.to_vec(),
        }));
        self.push(Op::Write(Write::SectionalDelete {
            path: self.root.clone(),
            key: held.to_vec(),
        }));
        // The leaf goes, then the two trees above it, each of another
        // owner, which it leaves empty. The tree above them holds more.
        self.push(Op::Write(Write::SectionalDeleteUpTree {
            path: in_lower,
            key: leaf.to_vec(),
            stop_path_height: Some(1),
        }));
        self.commit();
    }
}
