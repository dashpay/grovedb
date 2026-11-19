//! The subtree directory of one committed version, as a transaction or a
//! cursor reads through it.

use std::collections::HashMap;

use lignin_format::{CellAddress, CommitVersion};
use lignin_tree::directory::{Directory, Entry};

use super::{error::TxnError, key::Subtree, path::WritePath, write_set::WriteSet};
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
/// A handle is of the pages it was loaded from, so this is kept with its
/// version and made again for another. An entry that was found stays good
/// for that version; what the transaction itself dropped or cleared since
/// is asked of its write set at every read ([`Self::seen`]).
#[derive(Debug)]
pub(crate) struct Opened {
    at: CommitVersion,
    directory: Directory,
    entries: HashMap<Subtree, Option<Entry>>,
}

impl Opened {
    /// The directory of the version `at` in `slot`: the one that is there,
    /// or one that is loaded now, under `guard`.
    pub(crate) fn at<'a, 's>(
        slot: &'a mut Option<Self>,
        path: &WritePath<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
    ) -> Result<&'a mut Self, TxnError> {
        if slot.as_ref().is_none_or(|opened| opened.at != at) {
            *slot = Some(Self {
                at,
                directory: path.load_directory(&path.committed(guard, at))?,
                entries: HashMap::new(),
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
        let left: Vec<Subtree> = set.leaving().collect();
        if !left.is_empty() && self.below(path, guard, subtree, &left)? {
            return Ok(None);
        }
        self.root(path, guard, subtree)
    }
}
