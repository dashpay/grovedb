//! Which subtrees a transaction wrote below a Merk: an index of its frames.
//!
//! A batch that drops a Merk takes the Merks below it, and one that clears
//! a Merk takes them where the Merk has a node (`transactions.md` CV9).
//! What the transaction wrote under a subtree that goes so goes with it,
//! and the write set is told which subtrees those are when the batch is
//! added (`WriteSet::add`, `cut`). They are the subtrees the set has an
//! entry under ([`WriteSet::written`](super::write_set::WriteSet::written))
//! whose entry in the directory of the frames, before the batch, has the
//! id of such a Merk among the ids of the Merks it is below
//! (`lignin_tree::directory`, "Ids"). The frames answer that
//! (`Frames::below`).
//!
//! Asked of every written subtree at each such batch, the answer can
//! cost the square of a transaction that drops or clears a call each.
//! [`Below`] says, for the id of a Merk, which written subtrees to ask.
//!
//! # What it holds
//!
//! - **The subtrees it has asked of the directory**, each with the ids it
//!   is listed under: those of the Merks that the subtree's entry had it
//!   below when it was last asked, and none where the directory had no
//!   entry in use under its prefix.
//! - **For the id of a Merk, the subtrees listed under it.**
//! - **The subtrees to ask again**, which are among those it has asked.
//! - **How many first writes of the set it has taken**
//!   ([`WriteSet::first_writes`](super::write_set::WriteSet::first_writes)).
//!
//! # The rule
//!
//! > When the index has been brought up to date, every subtree that the
//! > set has an entry under, and that the directory of the frames has an
//! > entry in use for, is listed under the id of each Merk which that
//! > entry has it below.
//!
//! So the subtrees listed under the id of a Merk that a batch takes
//! include every written subtree below that Merk. What the index lists
//! beyond them changes no answer: each subtree that is listed under such
//! an id, and that the set has an entry under, is asked of the directory,
//! as every written subtree was before, and one that the set has no entry
//! under any more is forgotten, unopened.
//!
//! # When it changes
//!
//! It is brought up to date in one place, where a batch that takes a
//! Merk with a node asks (`Frames::below`), from the directory as it is
//! at that call: the subtrees that the set has an entry under, and has
//! numbered since the index last took its count of first writes, are
//! asked, and so are the subtrees to ask again. A subtree that is asked
//! and that the set has an entry under is opened in the directory,
//! whatever it is below, and a read that fails there ends the call. It
//! is listed under what its entry says now, in place of what it was
//! listed under. One that the set has no entry under any more is
//! forgotten, unopened. A batch that is applied to the frames only
//! says which of the subtrees the index has asked are asked again
//! (`Frames::apply`). A transaction with no such batch keeps an empty
//! index, and no batch says anything to it.
//!
//! In the table, a subtree that "is asked again" is one that the index
//! has asked. Of a subtree that it has not asked a batch says nothing:
//! if the set has an entry under that one at the next call, its number
//! is after the count the index took, and it is asked there, as the
//! first row says.
//!
//! | What happens | What becomes of the index |
//! | --- | --- |
//! | The set comes to have an entry under a subtree, for the first time or again | The set gives the subtree a number after the count the index has taken. The next call asks it, if the set has an entry under it then, and from there on it is one of those the index has asked. A subtree that the set has no entry under any more when that call comes is not asked. |
//! | A batch enters a subtree: it makes the entry under its prefix | The subtree is asked again. |
//! | A batch moves a subtree | Every prefix that a Merk is taken from, is brought to or leaves under is asked again. |
//! | A batch clears a subtree whose Merk has a node | The Merk has another id from here on, and the entries that have the id it had above them are in use no more: every subtree listed under that id is asked again. The cleared subtree keeps its entry, and stays listed as it is. |
//! | A batch clears a subtree whose Merk has no node | Nothing: the Merk keeps its id, and the clear takes nothing of what is entered below it. |
//! | A batch drops a subtree | The subtree is asked again, and where its Merk has a node, so is every subtree listed under the id of that Merk. One that has no entry in use when it is asked is listed under nothing, and while it is listed so, no batch opens it as a subtree below a Merk. It is asked again, and listed by what its entry says then, as the rows of this table say (a batch makes an entry under its prefix, the set numbers it again), or where a read failed before the index had taken its number. |
//! | A rollback to a mark, or a scope that is laid out and is taken out, takes a write or a change of a pending mark with it | The frames go. They are made again from the write set, and the index with them: it starts empty, and the next call asks every subtree the set has an entry under. |
//! | A scope that is not laid out is taken out, and the set still holds a write | The frames stay, and the index with them. A subtree that the set has no entry under any more is forgotten where it is next met: asked again, or listed under the id of a Merk that a batch takes. One that the set comes to have an entry under again is asked as the first row says: an operation that stood after one of the scope's is added again behind what stays. |
//! | A scope that is not laid out is taken out with the last write of the set | The frames go, as at the end of the transaction. |
//! | The frames are laid out again: after a commit beside the transaction, or after a batch that failed half way | The same: the index starts empty with the frames. |
//! | The transaction is given up, or commits | The frames go, and the index with them. |
//!
//! A batch that is refused with the frames as they were has nothing
//! asked again. Where it was refused after `Frames::below` had asked for
//! it, the index stays as that call brought it up to date. A read of the
//! directory that fails while the index is brought up to date leaves it
//! as far as it came: what was not asked is asked at the next call.
//!
//! # What is read no more
//!
//! Before the frames had this index, every written subtree was opened at
//! each such batch to find what is below. Now one is opened for that in
//! two places: where the index asks it, for the first time or again, and
//! where it is listed under the id of a Merk that the batch takes. For a
//! written subtree in neither place that read is not made: its failure
//! ended the call before, and cannot now. A read that is made and fails
//! ends the call, as before: those two, and the reads of the subtrees
//! that the batch itself names. In a build that holds the answer to the
//! old one (`Frames::below`), the old one still opens every written
//! subtree, and a read of it that fails ends the call.

use std::collections::{BTreeSet, HashMap};

use lignin_tree::directory::MerkId;

use super::key::Subtree;

/// The index of the frames that says which written subtrees to ask
/// whether they are below a Merk. See the [module documentation](self).
#[derive(Debug, Default)]
pub(crate) struct Below {
    /// For the id of a Merk, the subtrees that are listed under it. No
    /// id is kept with no subtree.
    under: HashMap<MerkId, BTreeSet<Subtree>>,
    /// The subtrees that were asked, each with the ids it is listed
    /// under.
    listed: HashMap<Subtree, Box<[MerkId]>>,
    /// The subtrees to ask again: each is one of `listed`.
    again: BTreeSet<Subtree>,
    /// How many first writes of the set were taken.
    taken: u64,
}

impl Below {
    /// Lists `subtree` under `above`, the ids of the Merks that its entry
    /// has it below, in place of what it was listed under: under nothing
    /// for a subtree with no entry in use. It is one of the subtrees that
    /// were asked from here on, and a mark to ask it again is taken off.
    pub(super) fn list(&mut self, subtree: Subtree, above: impl Iterator<Item = MerkId>) {
        self.forget(&subtree);
        let above: Box<[MerkId]> = above.collect();
        for id in above.iter() {
            self.under.entry(*id).or_default().insert(subtree);
        }
        self.listed.insert(subtree, above);
    }

    /// Forgets `subtree`: it is listed under nothing, has no mark to be
    /// asked again, and is none of the subtrees that were asked.
    pub(super) fn forget(&mut self, subtree: &Subtree) {
        self.again.remove(subtree);
        let Some(above) = self.listed.remove(subtree) else {
            return;
        };
        for id in above.iter() {
            let Some(listed) = self.under.get_mut(id) else {
                continue;
            };
            listed.remove(subtree);
            if listed.is_empty() {
                self.under.remove(id);
            }
        }
    }

    /// Has `subtree` asked again, if it is one of the subtrees that were
    /// asked: a batch that was applied made or took the entry under its
    /// prefix. One that was not asked takes no mark: the index has not
    /// taken a number of it, and where the set has an entry under it at
    /// the next call that brings the index up to date, it is asked there.
    pub(super) fn ask_again(&mut self, subtree: &Subtree) {
        if self.listed.contains_key(subtree) {
            self.again.insert(*subtree);
        }
    }

    /// Has every subtree that is listed under `id` asked again: the Merk
    /// with that id left with a node, so the entry that each of them is
    /// listed by is in use no more. One that the same batch entered again
    /// has another.
    pub(super) fn left(&mut self, id: MerkId) {
        if let Some(listed) = self.under.get(&id) {
            self.again.extend(listed.iter().copied());
        }
    }

    /// The subtrees that are listed under `id`, in the order of their
    /// prefixes.
    pub(super) fn under(&self, id: MerkId) -> impl Iterator<Item = Subtree> + '_ {
        self.under.get(&id).into_iter().flatten().copied()
    }

    /// The subtrees to ask again, in the order of their prefixes.
    pub(super) fn again(&self) -> Vec<Subtree> {
        self.again.iter().copied().collect()
    }

    /// How many first writes of the set were taken.
    pub(super) fn taken(&self) -> u64 {
        self.taken
    }

    /// The first writes of the set are taken up to its count `count`.
    pub(super) fn take(&mut self, count: u64) {
        self.taken = count;
    }
}

/// What a test reads of the index.
#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The subtrees that were asked, each with the ids it is listed
    /// under, in the order of the prefixes.
    pub(crate) listed: Vec<(Subtree, Vec<MerkId>)>,
    /// The subtrees to ask again.
    pub(crate) again: Vec<Subtree>,
    /// How many first writes of the set were taken.
    pub(crate) taken: u64,
}

#[cfg(test)]
impl Below {
    /// What the index holds, and that its two sides say the same: a
    /// subtree is listed under an id exactly where it has the id.
    pub(crate) fn shown(&self) -> Shown {
        let mut listed: Vec<(Subtree, Vec<MerkId>)> = (self.listed.iter())
            .map(|(subtree, above)| (*subtree, above.to_vec()))
            .collect();
        listed.sort_unstable_by_key(|(subtree, _)| *subtree);
        let mut pairs: Vec<(MerkId, Subtree)> = (listed.iter())
            .flat_map(|(subtree, above)| above.iter().map(|id| (*id, *subtree)))
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        let mut under: Vec<(MerkId, Subtree)> = (self.under.iter())
            .flat_map(|(id, listed)| listed.iter().map(|subtree| (*id, *subtree)))
            .collect();
        under.sort_unstable();
        assert_eq!(
            under, pairs,
            "the lists of the ids, and the ids of the subtrees"
        );
        assert!(self.under.values().all(|listed| !listed.is_empty()));
        assert!(self
            .again
            .iter()
            .all(|subtree| self.listed.contains_key(subtree)));
        Shown {
            listed,
            again: self.again(),
            taken: self.taken,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn subtree(name: u8) -> Subtree {
        Subtree::new([name; 32])
    }

    fn id(raw: u64) -> MerkId {
        MerkId::new(raw).expect("an id")
    }

    /// SplitMix64.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) % bound as u64) as usize
        }
    }

    /// The index answers as a plain map from each asked subtree to its
    /// ids does, through any sequence of its calls: what is listed under
    /// an id, what is asked again, and what is forgotten.
    #[test]
    fn the_index_agrees_with_a_map_of_the_asked_subtrees() {
        let mut calls = [0u32; 5];
        let (mut listed_under_many, mut asked_again, mut emptied) = (0, 0, 0);
        for seed in 0..60 {
            let mut rng = Rng(seed);
            let mut index = Below::default();
            let mut model: BTreeMap<Subtree, Vec<MerkId>> = BTreeMap::new();
            let mut again: BTreeSet<Subtree> = BTreeSet::new();
            for _ in 0..300 {
                let named = subtree(rng.below(14) as u8);
                let call = rng.below(10);
                match call {
                    0..=3 => {
                        // Listed under up to four of nine ids, the same
                        // one twice among them at times.
                        let above: Vec<MerkId> = (0..rng.below(5))
                            .map(|_| id(1 + rng.below(9) as u64))
                            .collect();
                        index.list(named, above.iter().copied());
                        listed_under_many += u32::from(above.len() > 1);
                        model.insert(named, above);
                        again.remove(&named);
                        calls[0] += 1;
                    }
                    4 | 5 => {
                        index.ask_again(&named);
                        if model.contains_key(&named) {
                            again.insert(named);
                            asked_again += 1;
                        }
                        calls[1] += 1;
                    }
                    6 | 7 => {
                        let left = id(1 + rng.below(9) as u64);
                        index.left(left);
                        let under = model.iter().filter(|(_, above)| above.contains(&left));
                        again.extend(under.map(|(subtree, _)| *subtree));
                        calls[2] += 1;
                    }
                    8 => {
                        index.forget(&named);
                        emptied += u32::from(model.remove(&named).is_some());
                        again.remove(&named);
                        calls[3] += 1;
                    }
                    _ => {
                        let count = rng.below(1_000) as u64;
                        index.take(count);
                        assert_eq!(index.taken(), count);
                        calls[4] += 1;
                    }
                }
                let shown = index.shown();
                let expected: Vec<(Subtree, Vec<MerkId>)> = (model.iter())
                    .map(|(subtree, above)| (*subtree, above.clone()))
                    .collect();
                assert_eq!(shown.listed, expected, "seed {seed}");
                let expected: Vec<Subtree> = again.iter().copied().collect();
                assert_eq!(shown.again, expected, "seed {seed}");
                for raw in 1..=9 {
                    let under: Vec<Subtree> = index.under(id(raw)).collect();
                    let expected: Vec<Subtree> = (model.iter())
                        .filter(|(_, above)| above.contains(&id(raw)))
                        .map(|(subtree, _)| *subtree)
                        .collect();
                    assert_eq!(under, expected, "seed {seed}: under {raw}");
                }
            }
        }
        // Every call was made, and the cases that the model is there for
        // came up.
        assert!(calls.iter().all(|made| *made > 500), "{calls:?}");
        assert!(listed_under_many > 1_000 && asked_again > 1_000 && emptied > 500);
    }

    /// A subtree that was not asked is not asked again, whatever names
    /// it: the index of a transaction that never asked stays empty.
    #[test]
    fn nothing_is_kept_of_a_subtree_that_was_not_asked() {
        let mut index = Below::default();
        index.ask_again(&subtree(1));
        index.left(id(3));
        index.forget(&subtree(2));
        assert_eq!(index.shown(), Shown::default());
        // One that was asked and has no entry in use is kept, listed
        // under nothing, and is asked again where a batch names it.
        index.list(subtree(1), std::iter::empty());
        assert_eq!(index.under(id(3)).count(), 0);
        index.ask_again(&subtree(1));
        assert_eq!(index.shown().again, [subtree(1)]);
        index.list(subtree(1), [id(3), id(4)].into_iter());
        let shown = index.shown();
        assert_eq!(shown.listed, [(subtree(1), vec![id(3), id(4)])]);
        assert!(shown.again.is_empty());
        // The Merk it is below leaves: it is asked again, and stays
        // listed until it is.
        index.left(id(4));
        assert_eq!(index.shown().again, [subtree(1)]);
        assert_eq!(index.under(id(4)).collect::<Vec<_>>(), [subtree(1)]);
        index.list(subtree(1), std::iter::empty());
        assert_eq!(index.under(id(4)).count(), 0);
        assert_eq!(index.shown().listed, [(subtree(1), Vec::new())]);
    }
}
