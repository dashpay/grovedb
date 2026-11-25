//! The subtree directory of one committed version, as a transaction or a
//! cursor reads through it.

use std::collections::{BTreeSet, HashMap};

use lignin_format::{CellAddress, CommitVersion};
use lignin_tree::directory::{Directory, Entry, MerkId};

use super::{
    below::Below, error::TxnError, key::Subtree, path::WritePath, unlinked, write_set::WriteSet,
};
use crate::snapshot::OperationGuard;

/// A handle on the subtree directory of one committed version, and the
/// subtrees that were opened through it (`lignin_tree::directory`).
///
/// **No record of a subtree is read without its entry.** In every column
/// the committed records under a prefix belong to the entry the directory
/// has under that prefix, so a subtree with no live entry has none, and
/// nothing is read for it: not a node, and not a flat record
/// (`transactions.md` CV9).
///
/// A handle is of one version: a copy of the one the engine keeps for it
/// ([`WritePath::directory_at`]), so this is kept with its version and
/// made again for another. An entry that was found stays good
/// for that version; what the transaction itself dropped or cleared since
/// is asked of its write set at every read ([`Self::seen`]).
///
/// Whether a subtree holds unlinked records at the version is kept with
/// its entry, once it was looked up ([`Self::has_unlinked`]).
#[derive(Debug)]
pub(crate) struct Opened {
    at: CommitVersion,
    directory: Directory,
    entries: HashMap<Subtree, Option<Entry>>,
    /// For the subtrees it was looked up for: whether the data column
    /// holds a record under the subtree at this version.
    unlinked: HashMap<Subtree, bool>,
    /// Which of the subtrees that the transaction has written this
    /// version has below a Merk, for a transaction that is served by key:
    /// made when it is first asked for.
    written: Option<Box<WrittenBelow>>,
    /// While a call is held (`held`): the subtrees whose entries it
    /// asked for, opened before or not.
    #[cfg(any(test, feature = "held_reads"))]
    asked: Option<Vec<Subtree>>,
}

/// What a transaction that is served by key keeps, with the directory of
/// one committed version, of the subtrees it has written: which of them
/// it took in as standing as the committed ones, and under which
/// Merks the version has those. A batch that drops or clears a subtree
/// finds by it which written subtrees to ask whether they are below
/// (`ByKey::cut` in `super::txn`).
///
/// **The rule.** When it has been brought up to date for a batch, every
/// subtree that the set has an entry under, that the batch does not drop
/// or clear, and that stands as the committed one after every operation
/// of the set, is one of [`Self::standing`], and [`Self::list`] has it
/// under the id of each Merk that its entry of the version has it below:
/// under none where the version has no entry in use under its prefix.
///
/// **What it holds beyond that changes no answer.** What was taken in is
/// kept as it stood then. A subtree that has come to stand otherwise
/// since, by a drop, by an enter or with a subtree above it, stays in it
/// until a batch meets it there, asks how it stands, and forgets it:
/// listed under nothing, none of the subtrees that were asked, and none
/// of [`Self::standing`]. A clear of a subtree leaves how it stands as
/// it was, and the subtree listed as it is. So a subtree that is found
/// here is asked again before anything is answered of it.
///
/// **A subtree is taken into it once.** The set numbers a subtree when it
/// comes to have an entry under it ([`WriteSet::written_after`]). The
/// first batch after that which brings this up to date, and does not
/// itself drop or clear the subtree, asks how it stands. Where that is as
/// the committed one, it is listed. Where it is not, it is left out, and
/// is not asked for this again. A batch that brings this up to date and
/// itself drops or clears a subtree that was not asked yet puts it off:
/// the list has it under nothing, marked for the next batch that brings
/// this up to date, which asks it or, taking it as well, puts it off.
///
/// A subtree that is left out is none that the rule names from there on.
/// What makes a subtree the transaction's own, or takes it, is an
/// operation of the set, and whether an enter of a subtree if it was
/// absent found the subtree is said by the operations up to the one that
/// made the enter, and by the version. So a subtree that does not stand
/// as the committed one does not come to stand so while every operation
/// stays in the set and the version is this one.
///
/// **It is begun again, empty,** where an operation was taken out of the
/// set since it was begun ([`WriteSet::taken_out`]). It is of one
/// version, as the handle that keeps it: the handle of another version
/// begins its own.
#[derive(Debug, Default)]
pub(crate) struct WrittenBelow {
    /// The written subtrees that were asked and not forgotten, each
    /// under the ids of the Merks it is below, and how many first writes
    /// of the set were taken. The subtrees to ask again are those that
    /// were put off.
    pub(crate) list: Below,
    /// The subtrees that the list took in as standing as the committed
    /// ones and has not forgotten since: those that it lists and has
    /// not put off.
    pub(crate) standing: BTreeSet<Subtree>,
    /// How many times operations had been taken out of the set when
    /// this was begun.
    pub(crate) taken_out: u64,
}

impl Opened {
    /// The directory of the version `at` in `slot`: the one that is there,
    /// or a copy of the handle the engine keeps for `at`. The caller pins
    /// `at`, or read it as the latest version under `guard`.
    pub(crate) fn at<'a, 's>(
        slot: &'a mut Option<Self>,
        path: &WritePath<'s>,
        _guard: &OperationGuard<'s>,
        at: CommitVersion,
    ) -> Result<&'a mut Self, TxnError> {
        if slot.as_ref().is_none_or(|opened| opened.at != at) {
            *slot = Some(Self {
                at,
                directory: path.directory_at(at)?,
                entries: HashMap::new(),
                unlinked: HashMap::new(),
                written: None,
                #[cfg(any(test, feature = "held_reads"))]
                asked: None,
            });
        }
        Ok(slot.as_mut().expect("it was loaded"))
    }

    /// Opens `subtree`, if it was not opened at this version before: one
    /// lookup in the directory.
    fn open<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<(), TxnError> {
        #[cfg(any(test, feature = "held_reads"))]
        if let Some(asked) = &mut self.asked {
            asked.push(*subtree);
        }
        if !self.entries.contains_key(subtree) {
            let pages = path.committed(guard, self.at);
            let entry = self.directory.open(&pages, subtree.as_bytes())?;
            self.entries.insert(*subtree, entry);
        }
        Ok(())
    }

    /// Whether `subtree` is entered at this version, and where the root of
    /// its Merk is: `None` for a subtree that is not entered,
    /// `Some(None)` for one with no node.
    pub(crate) fn root<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<Option<Option<CellAddress>>, TxnError> {
        self.open(path, guard, subtree)?;
        let entry = self.entries[subtree].as_ref();
        Ok(entry.map(|entry| entry.root().map(|root| root.address)))
    }

    /// The tree type that this version has `subtree` entered with, if the
    /// subtree is entered.
    pub(crate) fn tree_type<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<Option<&[u8]>, TxnError> {
        self.open(path, guard, subtree)?;
        Ok(self.entries[subtree].as_ref().map(Entry::tree_type))
    }

    /// The id of the Merk that this version has under `subtree`, if the
    /// subtree is entered (`lignin_tree::directory`, "Ids").
    pub(crate) fn id<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<Option<MerkId>, TxnError> {
        self.open(path, guard, subtree)?;
        Ok(self.entries[subtree].as_ref().map(Entry::id))
    }

    /// Whether the committed `subtree` is below one of `above`: a Merk
    /// with a node that holds, or hosts further up, the element of
    /// `subtree`.
    pub(crate) fn below<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
        above: &[Subtree],
    ) -> Result<bool, TxnError> {
        let above: Vec<_> = above.iter().map(|above| (*above, None)).collect();
        self.below_taken(path, guard, subtree, &above)
    }

    /// [`Self::below`], for subtrees that the transaction dropped or
    /// cleared, each with whether that took the subtrees below it
    /// (`WriteSet::taking`): a drop did, and a clear did where the Merk
    /// had a node, which the transaction's own writes say or do not.
    /// Where they do not, this version says, if it has the subtree: its
    /// Merk is taken for the one that was cleared.
    pub(crate) fn below_taken<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
        above: &[(Subtree, Option<bool>)],
    ) -> Result<bool, TxnError> {
        self.open(path, guard, subtree)?;
        for (above, _) in above {
            self.open(path, guard, above)?;
        }
        let Some(entry) = &self.entries[subtree] else {
            return Ok(false);
        };
        Ok(above.iter().any(|(above, rooted)| {
            // A Merk with no node hosts none, and a clear of it leaves the
            // directory as it is.
            let above = self.entries[above].as_ref();
            let above = above.filter(|above| rooted.unwrap_or(above.root().is_some()));
            above.is_some_and(|above| entry.lineage().any(|id| id == above.id()))
        }))
    }

    /// The version the handle is of.
    pub(crate) const fn version(&self) -> CommitVersion {
        self.at
    }

    /// Which written subtrees this version has below a Merk, as a
    /// transaction that is served by key keeps it ([`WrittenBelow`]).
    pub(crate) fn written_below(&mut self) -> &mut WrittenBelow {
        self.written.get_or_insert_with(Box::default)
    }

    /// Whether the data column holds a record under `subtree` at this
    /// version: for a subtree whose data are the nodes of a Merk, whether
    /// it holds unlinked records (`super::unlinked`). It is one lookup,
    /// made once for the version, and the caller has found the subtree
    /// entered: no record is read under a subtree without its entry.
    pub(crate) fn has_unlinked<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        if let Some(known) = self.unlinked.get(subtree) {
            return Ok(*known);
        }
        let pages = path.committed(guard, self.at);
        let has = unlinked::any(&path.keyspaces.data, &pages, subtree)?;
        self.unlinked.insert(*subtree, has);
        Ok(has)
    }

    /// What [`Self::has_unlinked`] answers for `subtree`, if this handle
    /// keeps the answer already.
    pub(crate) fn unlinked_known(&self, subtree: &Subtree) -> Option<bool> {
        self.unlinked.get(subtree).copied()
    }

    /// Keeps `has` as the answer of [`Self::has_unlinked`] for `subtree`:
    /// what was found for this version by whoever looked it up.
    pub(crate) fn know_unlinked(&mut self, subtree: &Subtree, has: bool) {
        self.unlinked.insert(*subtree, has);
    }

    /// Whether a move of the transaction whose write set is `set` decides
    /// what the transaction reads under `subtree`. That holds of a
    /// subtree where
    ///
    /// - a move names it, on either side, or took it out with the subtree
    ///   it moved;
    /// - this version, which has the subtrees where the commits left
    ///   them, has it below a prefix that a move names, on either side;
    /// - or the last enter of it that the transaction made states a host
    ///   in a subtree of which one of these three holds.
    pub(crate) fn moved<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        // Up the hosts that the transaction's own enters state, to the
        // first subtree it did not enter. Each is asked of this version
        // as well: a commit may have entered a subtree of the same prefix
        // elsewhere.
        let mut at = *subtree;
        let mut seen = vec![at];
        loop {
            if set.moves(&at) || self.below_a_name(path, guard, set, &at)? {
                return Ok(true);
            }
            match set.host_of(&at).flatten() {
                Some(host) if !seen.contains(&host) => {
                    seen.push(host);
                    at = host;
                }
                _ => return Ok(false),
            }
        }
    }

    /// Whether `subtree` is below a subtree that this version has under
    /// a prefix which a move of `set` names, on either side
    /// ([`WriteSet::moved_names`]). It is asked of the subtree's own
    /// hosts ([`Self::above_by_hosts`]), which costs a lookup for each
    /// Merk the subtree is below, however many prefixes the moves of
    /// the transaction name.
    fn below_a_name<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        let named = |above: &Subtree, _: &Entry| set.named_by_a_move(above);
        let by_hosts = self.above_by_hosts(path, guard, subtree, named)?;
        #[cfg(any(test, feature = "held_reads"))]
        self.held_to_every_name(path, guard, set, subtree, by_hosts)?;
        match by_hosts {
            Some(below) => Ok(below),
            // The directory does not have the Merks above the subtree
            // where its entry names them: every prefix is opened.
            None => self.below_one_of(path, guard, &set.moved_names(), subtree),
        }
    }

    /// Whether `subtree` is below a subtree that this version has under
    /// one of `prefixes`.
    fn below_one_of<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        prefixes: &[Subtree],
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        self.open(path, guard, subtree)?;
        for prefix in prefixes {
            self.open(path, guard, prefix)?;
        }
        let Some(entry) = &self.entries[subtree] else {
            return Ok(false);
        };
        Ok(prefixes.iter().any(|prefix| {
            let above = self.entries[prefix].as_ref();
            above.is_some_and(|above| entry.lineage().any(|id| id == above.id()))
        }))
    }

    /// Whether `subtree`, which this version has entered, leaves with a
    /// move of `set` when the writes of the set are laid out over this
    /// version: no move names it, and it is below a prefix that a move
    /// took a subtree from. Such a subtree has no node, or the move is
    /// not laid out (`transactions.md` MV2); it left with the move where
    /// the move was stated, or a commit beside the transaction entered
    /// it there since.
    ///
    /// It is asked of the subtree's own hosts ([`Self::above_by_hosts`]),
    /// which costs a lookup for each Merk the subtree is below, however
    /// many subtrees the transaction moved.
    pub(crate) fn leaves_with_a_move<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        if !set.has_moves() || set.moves(subtree) {
            // No host is asked: the function began so before as well,
            // and that answer is held to it too.
            #[cfg(any(test, feature = "held_reads"))]
            self.held_before_the_hosts(path, guard, set, subtree)?;
            return Ok(false);
        }
        let source = |above: &Subtree, _: &Entry| set.took_from(above);
        let by_hosts = self.above_by_hosts(path, guard, subtree, source)?;
        #[cfg(any(test, feature = "held_reads"))]
        self.held_to_every_source(path, guard, set, subtree, by_hosts)?;
        match by_hosts {
            Some(leaves) => Ok(leaves),
            // The directory does not have the Merks above the subtree
            // where its entry names them: every prefix is opened.
            None => self.below_one_of(path, guard, &set.moved_from(), subtree),
        }
    }

    /// Whether `subtree` is below a Merk of which `found` says yes. The
    /// Merks that the subtree is below at this version are asked from the
    /// nearest on, each with the prefix it is entered under and its
    /// entry, and the walk ends at the first of which `found` says yes:
    /// what is above that Merk is not read.
    ///
    /// The entry of a Merk in use names the element that hosts it by the
    /// prefix of the Merk that element is in, and has the ids of the Merks
    /// it is below, the last of them that Merk's; the entry under that
    /// prefix is of that Merk, and says the same of the Merk above it
    /// (`lignin_tree::directory`, "Ids", and what its check holds). So
    /// the Merks that `subtree` is below are found from its entry, one
    /// lookup each, each with the prefix it is entered under. An entry
    /// that this version has under some prefix is the entry of a Merk
    /// the subtree is below exactly where that prefix is among them,
    /// since no two entries have one id: so what is asked of every
    /// entry under a prefix of some list, and of whether the subtree is
    /// below its Merk, is asked here of the entries above the subtree,
    /// and of whether the list has their prefixes.
    ///
    /// `None` where the walk, before it ends, does not find, under the
    /// prefix that an entry names for its host's Merk, the Merk whose id
    /// the entry has for it: nothing is answered from such a directory
    /// here.
    ///
    /// The walk reads the entry of the subtree and the entries above it
    /// that it comes to, and no entry of a list. A read that fails is
    /// this call's error. A function that opens the prefixes of a list
    /// reads other entries: where a page of the directory cannot be
    /// read, the two can fail at different entries, and one can fail
    /// where the other answers. What they have in common is the answer
    /// where each reads what it opens.
    fn above_by_hosts<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
        found: impl Fn(&Subtree, &Entry) -> bool,
    ) -> Result<Option<bool>, TxnError> {
        self.open(path, guard, subtree)?;
        let Some(entry) = &self.entries[subtree] else {
            return Ok(Some(false));
        };
        // The Merks the subtree is below, the nearest first.
        let above: Vec<MerkId> = entry.lineage().rev().collect();
        let mut host = entry.host().map(|host| Subtree::new(*host.parent));
        for id in above {
            let Some(parent) = host else {
                return Ok(None);
            };
            self.open(path, guard, &parent)?;
            let Some(entry) = (self.entries[&parent].as_ref()).filter(|entry| entry.id() == id)
            else {
                return Ok(None);
            };
            if found(&parent, entry) {
                return Ok(Some(true));
            }
            host = entry.host().map(|host| Subtree::new(*host.parent));
        }
        // The outermost Merk has no host.
        Ok(host.is_none().then_some(false))
    }

    /// The nearest subtree after `from` in the order of the prefixes, or
    /// with `back` the nearest before it, whose Merk has a node at this
    /// version, looked for no further than `until`
    /// ([`Directory::rooted_beside`](lignin_tree::directory::Directory::rooted_beside)).
    pub(crate) fn rooted_beside<'s>(
        &self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        from: &Subtree,
        until: Option<&Subtree>,
        back: bool,
    ) -> Result<Option<Subtree>, TxnError> {
        let pages = path.committed(guard, self.at);
        let (from, until) = (from.as_bytes(), until.map(Subtree::as_bytes));
        let found = (self.directory).rooted_beside(&pages, from, until, back)?;
        Ok(found.map(Subtree::new))
    }

    /// The ids of the Merks that `subtree` is below at this version, from
    /// the outermost, if it is entered: what tells the subtree from one
    /// that is entered under its prefix after a Merk above it was dropped
    /// or cleared (`lignin_tree::directory`, "Ids").
    pub(crate) fn lineage<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<Option<Vec<MerkId>>, TxnError> {
        self.open(path, guard, subtree)?;
        let entry = self.entries[subtree].as_ref();
        Ok(entry.map(|entry| entry.lineage().collect()))
    }

    /// The subtree of the element that hosts `subtree` at this version,
    /// also where the entry is one that a drop left.
    pub(crate) fn host<'s>(
        &self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        subtree: &Subtree,
    ) -> Result<Option<Subtree>, TxnError> {
        let pages = path.committed(guard, self.at);
        let entry = self.directory.stored(&pages, subtree.as_bytes())?;
        Ok(entry.and_then(|entry| entry.host().map(|host| Subtree::new(*host.parent))))
    }

    /// As [`Self::root`], for the transaction whose write set is `set`:
    /// `None` also for a subtree below one that the transaction dropped or
    /// cleared (`transactions.md` CV9, RD3).
    ///
    /// The subtree itself is not looked for among those: the entry of a
    /// drop or a clear covers its keys in the write set.
    pub(crate) fn seen<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<Option<Option<CellAddress>>, TxnError> {
        let entry = self.seen_entry(path, guard, set, subtree)?;
        Ok(entry.map(|entry| entry.root().map(|root| root.address)))
    }

    /// As [`Self::seen`], with all the directory holds of `subtree`.
    pub(crate) fn seen_entry<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<Option<&Entry>, TxnError> {
        if set.has_leaves() && self.taken_with_one_above(path, guard, set, subtree)? {
            return Ok(None);
        }
        self.open(path, guard, subtree)?;
        Ok(self.entries[subtree].as_ref())
    }

    /// Whether the transaction whose write set is `set` took `subtree`
    /// with a subtree above it that it dropped or cleared:
    /// [`Self::below_taken`] for every drop and clear of the set
    /// ([`WriteSet::taking`]). It is asked of the subtree's own hosts
    /// ([`Self::above_by_hosts`]), which costs a lookup for each Merk the
    /// subtree is below, however many subtrees the transaction dropped
    /// and cleared.
    fn taken_with_one_above<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        // A Merk with no node hosts none, and a clear of it leaves the
        // directory as it is.
        let takes = |above: &Subtree, entry: &Entry| set.takes_below(above, entry.root().is_some());
        let by_hosts = self.above_by_hosts(path, guard, subtree, takes)?;
        #[cfg(any(test, feature = "held_reads"))]
        self.held_to_every_leave(path, guard, set, subtree, by_hosts)?;
        match by_hosts {
            Some(taken) => Ok(taken),
            // The directory does not have the Merks above the subtree
            // where its entry names them: every subtree that left is
            // opened.
            None => self.below_taken(path, guard, subtree, &set.taking()),
        }
    }
}

#[cfg(any(test, feature = "held_reads"))]
pub(crate) mod held {
    //! What [`Opened::leaves_with_a_move`] answers is held, at every
    //! call, to the answer it was made with before the subtree's own
    //! hosts were asked: every prefix that a move of the transaction
    //! took a subtree from is opened, and the entry under each is looked
    //! for among the Merks the subtree is below. In the same way, what
    //! [`Opened::seen_entry`] finds of the subtrees that the transaction
    //! dropped or cleared is held to [`Opened::below_taken`] for every
    //! one of them, and what [`Opened::moved`] finds of the prefixes
    //! that the moves name to every one of those opened. That is so in
    //! a test build of this crate, and in a build with the feature
    //! `held_reads`, by which the tests of another crate hold the
    //! answers over their own calls.
    //!
    //! What is compared is an answer. A read of the directory that
    //! fails ends the call with its error before the two answers of
    //! what the subtree is below are compared: what the write set
    //! counts may have been held to its lists by then. And the walk
    //! up the hosts reads other entries than a look through a list
    //! does (`Opened::above_by_hosts`).

    #[cfg(test)]
    use std::cell::Cell;

    use super::*;

    /// How often the two answers were compared on this thread.
    #[cfg(test)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct Compared {
        /// The subtree leaves with a move.
        pub(crate) leaves: u64,
        /// It does not, and is entered.
        pub(crate) stays: u64,
        /// It does not: this version has no such subtree.
        pub(crate) absent: u64,
        /// [`Opened::leaves_with_a_move`] answered before it asked a
        /// host: the set holds no move, or a move names the subtree.
        pub(crate) unasked: u64,
        /// A walk up the hosts gave no answer, whichever was asked.
        pub(crate) unanswered: u64,
        /// The most Merks that a subtree which was asked for is below.
        pub(crate) deepest: usize,
        /// The transaction took the subtree with one above it that it
        /// dropped or cleared.
        pub(crate) taken: u64,
        /// It took it, and no drop or clear above it says so of itself:
        /// the version has a node in a Merk above it that was cleared.
        pub(crate) taken_as_the_version_says: u64,
        /// It did not, and the subtree is entered.
        pub(crate) kept: u64,
        /// The times that was asked while the transaction held a clear
        /// of a Merk that its own writes say had no node, and a clear
        /// of one that they say nothing of.
        pub(crate) beside_a_clear_of_no_node: u64,
        pub(crate) beside_a_clear_unsaid: u64,
        /// The subtree is below a prefix that a move names.
        pub(crate) below_a_name: u64,
        /// It is not, and is entered.
        pub(crate) below_no_name: u64,
    }

    #[cfg(test)]
    thread_local! {
        static COMPARED: Cell<Compared> = const {
            Cell::new(Compared {
                leaves: 0,
                stays: 0,
                absent: 0,
                unasked: 0,
                unanswered: 0,
                deepest: 0,
                taken: 0,
                taken_as_the_version_says: 0,
                kept: 0,
                beside_a_clear_of_no_node: 0,
                beside_a_clear_unsaid: 0,
                below_a_name: 0,
                below_no_name: 0,
            })
        };
    }

    /// What was compared on this thread since this was last asked.
    #[cfg(test)]
    pub(crate) fn compared() -> Compared {
        COMPARED.replace(Compared::default())
    }

    impl Opened {
        /// Notes, from here on, every subtree whose entry is asked for,
        /// whether it was opened before or not, in place of what was
        /// noted.
        pub(crate) fn note_asked(&mut self) {
            self.asked = Some(Vec::new());
        }

        /// The subtrees that were noted, in the order they were asked
        /// for. None is noted from here on.
        pub(crate) fn asked(&mut self) -> Vec<Subtree> {
            self.asked.take().unwrap_or_default()
        }

        /// The answer as it was made: no for a set with no move and
        /// for a subtree that a move names, by every name of every
        /// move, and otherwise by opening every source.
        fn leaves_by_every_source<'s>(
            &mut self,
            path: &WritePath<'s>,
            guard: &OperationGuard<'s>,
            set: &WriteSet,
            subtree: &Subtree,
        ) -> Result<bool, TxnError> {
            if !set.has_moves() || set.moves_by_every_name(subtree) {
                return Ok(false);
            }
            self.below_one_of(path, guard, &set.moved_from(), subtree)
        }

        /// Holds the answer that [`Opened::leaves_with_a_move`] gives
        /// before it asks a host, which is no, to the answer as it
        /// was made.
        pub(super) fn held_before_the_hosts<'s>(
            &mut self,
            path: &WritePath<'s>,
            guard: &OperationGuard<'s>,
            set: &WriteSet,
            subtree: &Subtree,
        ) -> Result<(), TxnError> {
            let by_every_source = self.leaves_by_every_source(path, guard, set, subtree)?;
            assert!(
                !by_every_source,
                "whether {subtree:?} leaves with a move, where no host is asked"
            );
            #[cfg(test)]
            {
                let mut compared = COMPARED.get();
                compared.unasked += 1;
                COMPARED.set(compared);
            }
            Ok(())
        }

        /// Holds `by_hosts`, what the walk up the hosts of `subtree`
        /// answered, to the answer by every source, and what the write
        /// set says it took a subtree from to the list of its sources.
        pub(super) fn held_to_every_source<'s>(
            &mut self,
            path: &WritePath<'s>,
            guard: &OperationGuard<'s>,
            set: &WriteSet,
            subtree: &Subtree,
            by_hosts: Option<bool>,
        ) -> Result<(), TxnError> {
            let sources = set.moved_from();
            for source in &sources {
                assert!(set.took_from(source), "{source:?} is a source of a move");
            }
            let entry = self.entries[subtree].as_ref();
            let mut above = entry.and_then(|entry| {
                let host = entry.host()?;
                Some(Subtree::new(*host.parent))
            });
            // Each prefix once: in a directory whose entries name each
            // other as hosts in a ring, the look ends where it comes
            // to one again.
            let mut asked: Vec<Subtree> = Vec::new();
            while let Some(prefix) = above.filter(|prefix| !asked.contains(prefix)) {
                asked.push(prefix);
                assert_eq!(
                    set.took_from(&prefix),
                    sources.contains(&prefix),
                    "{prefix:?}"
                );
                self.open(path, guard, &prefix)?;
                let host = self.entries[&prefix].as_ref().and_then(Entry::host);
                above = host.map(|host| Subtree::new(*host.parent));
            }
            let by_every_source = self.leaves_by_every_source(path, guard, set, subtree)?;
            if let Some(by_hosts) = by_hosts {
                assert_eq!(
                    by_hosts, by_every_source,
                    "whether {subtree:?} leaves with a move"
                );
            }
            // In this crate's tests the walks that gave no answer are
            // counted, for the tests that hold them to none. Where the
            // tests of another crate hold the answers, there is nobody
            // to count for: a walk that gives no answer stops the run.
            #[cfg(not(test))]
            assert!(
                by_hosts.is_some(),
                "the hosts of {subtree:?} do not say whether it leaves with a move"
            );
            #[cfg(test)]
            {
                let entry = self.entries[subtree].as_ref();
                let mut compared = COMPARED.get();
                match (by_hosts, entry.is_some()) {
                    (None, _) => compared.unanswered += 1,
                    (Some(true), _) => compared.leaves += 1,
                    (Some(false), true) => compared.stays += 1,
                    (Some(false), false) => compared.absent += 1,
                }
                compared.deepest = compared.deepest.max(entry.map_or(0, Entry::depth));
                COMPARED.set(compared);
            }
            Ok(())
        }

        /// Holds `by_hosts`, what the walk up the hosts of `subtree`
        /// answered, to [`Opened::below_taken`] for every drop and
        /// clear of the set, and the write set's counts of what
        /// those take to its list of them.
        pub(super) fn held_to_every_leave<'s>(
            &mut self,
            path: &WritePath<'s>,
            guard: &OperationGuard<'s>,
            set: &WriteSet,
            subtree: &Subtree,
            by_hosts: Option<bool>,
        ) -> Result<(), TxnError> {
            set.hold_taken();
            let left = set.taking();
            let by_every_leave = self.below_taken(path, guard, subtree, &left)?;
            if let Some(by_hosts) = by_hosts {
                assert_eq!(
                    by_hosts, by_every_leave,
                    "whether {subtree:?} went with a subtree above it"
                );
            }
            #[cfg(not(test))]
            assert!(
                by_hosts.is_some(),
                "the hosts of {subtree:?} do not say whether it went with a subtree above it"
            );
            #[cfg(test)]
            {
                let entry = self.entries[subtree].as_ref();
                // What each drop and clear of a Merk above the subtree
                // says of what is below it, and whether this version
                // has a node in that Merk.
                let above = |(above, rooted): &(Subtree, Option<bool>)| {
                    let above = self.entries[above].as_ref()?;
                    let below = entry?.lineage().any(|id| id == above.id());
                    below.then_some((*rooted, above.root().is_some()))
                };
                let said: Vec<(Option<bool>, bool)> = left.iter().filter_map(above).collect();
                let says_so = said.iter().any(|(rooted, _)| *rooted == Some(true));
                let mut compared = COMPARED.get();
                match (by_hosts, entry.is_some()) {
                    (None, _) => compared.unanswered += 1,
                    (Some(true), _) => {
                        compared.taken += 1;
                        compared.taken_as_the_version_says += u64::from(!says_so);
                    }
                    (Some(false), true) => compared.kept += 1,
                    (Some(false), false) => {}
                }
                let listed = |said: Option<bool>| left.iter().any(|(_, rooted)| *rooted == said);
                compared.beside_a_clear_of_no_node += u64::from(listed(Some(false)));
                compared.beside_a_clear_unsaid += u64::from(listed(None));
                COMPARED.set(compared);
            }
            Ok(())
        }

        /// Holds `by_hosts`, what the walk up the hosts of `subtree`
        /// answered, to the answer by every prefix that a move of
        /// the set names, each opened, and the write set's counts
        /// of those names to its list of them.
        pub(super) fn held_to_every_name<'s>(
            &mut self,
            path: &WritePath<'s>,
            guard: &OperationGuard<'s>,
            set: &WriteSet,
            subtree: &Subtree,
            by_hosts: Option<bool>,
        ) -> Result<(), TxnError> {
            set.hold_named();
            let by_every_name = self.below_one_of(path, guard, &set.moved_names(), subtree)?;
            if let Some(by_hosts) = by_hosts {
                assert_eq!(
                    by_hosts, by_every_name,
                    "whether {subtree:?} is below a prefix that a move names"
                );
            }
            #[cfg(not(test))]
            assert!(
                by_hosts.is_some(),
                "the hosts of {subtree:?} do not say whether it is below a name of a move"
            );
            #[cfg(test)]
            {
                let entered = self.entries[subtree].is_some();
                let mut compared = COMPARED.get();
                match (by_hosts, entered) {
                    (None, _) => compared.unanswered += 1,
                    (Some(true), _) => compared.below_a_name += 1,
                    (Some(false), true) => compared.below_no_name += 1,
                    (Some(false), false) => {}
                }
                COMPARED.set(compared);
            }
            Ok(())
        }
    }
}
