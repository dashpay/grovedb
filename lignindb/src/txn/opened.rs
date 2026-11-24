//! The subtree directory of one committed version, as a transaction or a
//! cursor reads through it.

use std::collections::HashMap;

use lignin_format::{CellAddress, CommitVersion};
use lignin_tree::directory::{Directory, Entry, MerkId};

use super::{error::TxnError, key::Subtree, path::WritePath, unlinked, write_set::WriteSet};
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
        let names = set.moved_names();
        // Up the hosts that the transaction's own enters state, to the
        // first subtree it did not enter. Each is asked of this version
        // as well: a commit may have entered a subtree of the same prefix
        // elsewhere.
        let mut at = *subtree;
        let mut seen = vec![at];
        loop {
            if set.moves(&at) || self.below_one_of(path, guard, &names, &at)? {
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
    /// It is asked of the subtree's own hosts ([`Self::below_a_source`]),
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
            return Ok(false);
        }
        let by_hosts = self.below_a_source(path, guard, set, subtree)?;
        #[cfg(any(test, feature = "held_reads"))]
        self.held_to_every_source(path, guard, set, subtree, by_hosts)?;
        match by_hosts {
            Some(leaves) => Ok(leaves),
            // The directory does not have the Merks above the subtree
            // where its entry names them: every prefix is opened.
            None => self.below_one_of(path, guard, &set.moved_from(), subtree),
        }
    }

    /// Whether `subtree` is below a subtree that this version has under a
    /// prefix which a move of `set` took a subtree from
    /// ([`WriteSet::took_from`]), asked of the Merks the subtree is below.
    ///
    /// The entry of a Merk in use names the element that hosts it by the
    /// prefix of the Merk that element is in, and has the ids of the Merks
    /// it is below, the last of them that Merk's; the entry under that
    /// prefix is of that Merk, and says the same of the Merk above it
    /// (`lignin_tree::directory`, "Ids", and what its check holds). So
    /// the Merks that `subtree` is below are found from its entry, one
    /// lookup each, each with the prefix it is entered under: the subtree
    /// is below a subtree under one of the prefixes exactly where one of
    /// those prefixes is among them, since no two entries have one id.
    ///
    /// `None` where the walk does not find, under the prefix that an
    /// entry names for its host's Merk, the Merk whose id the entry has
    /// for it: nothing is answered from such a directory here.
    fn below_a_source<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
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
            if set.took_from(&parent) {
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
        let left = set.taking();
        if !left.is_empty() && self.below_taken(path, guard, subtree, &left)? {
            return Ok(None);
        }
        self.open(path, guard, subtree)?;
        Ok(self.entries[subtree].as_ref())
    }
}

#[cfg(any(test, feature = "held_reads"))]
pub(crate) mod held {
    //! What [`Opened::leaves_with_a_move`] answers is held, at every
    //! call, to the answer it was made with before the subtree's own
    //! hosts were asked: every prefix that a move of the transaction
    //! took a subtree from is opened, and the entry under each is looked
    //! for among the Merks the subtree is below. That is so in a test
    //! build of this crate, and in a build with the feature
    //! `held_reads`, by which the tests of another crate hold the
    //! answers over their own calls.

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
        /// The walk up the hosts gave no answer.
        pub(crate) unanswered: u64,
        /// The most Merks that a subtree which was asked for is below.
        pub(crate) deepest: usize,
    }

    #[cfg(test)]
    thread_local! {
        static COMPARED: Cell<Compared> = const {
            Cell::new(Compared { leaves: 0, stays: 0, absent: 0, unanswered: 0, deepest: 0 })
        };
    }

    /// What was compared on this thread since this was last asked.
    #[cfg(test)]
    pub(crate) fn compared() -> Compared {
        COMPARED.replace(Compared::default())
    }

    impl Opened {
        /// The answer as it was made: by opening every source.
        fn leaves_by_every_source<'s>(
            &mut self,
            path: &WritePath<'s>,
            guard: &OperationGuard<'s>,
            set: &WriteSet,
            subtree: &Subtree,
        ) -> Result<bool, TxnError> {
            self.below_one_of(path, guard, &set.moved_from(), subtree)
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
            while let Some(prefix) = above {
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
    }
}
