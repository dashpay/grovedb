//! The subtree directory of one committed version, as a transaction or a
//! cursor reads through it.

use std::collections::HashMap;

use lignin_format::{CellAddress, CommitVersion};
use lignin_tree::directory::{Directory, Entry, MerkId};

use super::{
    error::TxnError,
    key::Subtree,
    path::WritePath,
    unlinked,
    write_set::{EntryId, WriteSet},
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
        self.open(path, guard, subtree)?;
        for above in above {
            self.open(path, guard, above)?;
        }
        let Some(entry) = &self.entries[subtree] else {
            return Ok(false);
        };
        Ok(above.iter().any(|above| {
            // A Merk with no node hosts none, and a clear of it leaves the
            // directory as it is.
            let above = self.entries[above].as_ref();
            let above = above.filter(|above| above.root().is_some());
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
    /// what the transaction reads under `subtree`: a move names the
    /// subtree, on either side, or took it out with the subtree it moved;
    /// the transaction entered it under a subtree of which that holds; or
    /// it is below a prefix that a move names, on either side, at this
    /// version, which has the subtrees where the commits left them.
    pub(crate) fn moved<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        // Up the hosts that the transaction's own enters state, to the
        // first subtree it did not enter.
        let mut at = *subtree;
        let mut seen = vec![at];
        loop {
            if set.moves(&at) {
                return Ok(true);
            }
            match set.host_of(&at).flatten() {
                Some(host) if !seen.contains(&host) => {
                    seen.push(host);
                    at = host;
                }
                _ => break,
            }
        }
        self.below_one_of(path, guard, &set.moved_names(), &at)
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
        self.below_one_of(path, guard, &set.moved_from(), subtree)
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
        self.seen_before(path, guard, set, subtree, None)
    }

    /// As [`Self::seen`], with only what the transaction dropped or
    /// cleared before the entry `before` asked of this version's
    /// directory, or all of it where none is given: for the committed
    /// side of a subtree that the transaction moved
    /// ([`WriteSet::committed_from`]).
    pub(crate) fn seen_before<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
        before: Option<EntryId>,
    ) -> Result<Option<Option<CellAddress>>, TxnError> {
        let entry = self.seen_entry_before(path, guard, set, subtree, before)?;
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
        self.seen_entry_before(path, guard, set, subtree, None)
    }

    fn seen_entry_before<'s>(
        &mut self,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        set: &WriteSet,
        subtree: &Subtree,
        before: Option<EntryId>,
    ) -> Result<Option<&Entry>, TxnError> {
        let left: Vec<Subtree> = set.leaving_before(before).collect();
        if !left.is_empty() && self.below(path, guard, subtree, &left)? {
            return Ok(None);
        }
        self.open(path, guard, subtree)?;
        Ok(self.entries[subtree].as_ref())
    }
}
