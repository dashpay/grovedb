//! The flat records of subtrees: the columns they are in, what a drop and
//! an enter write to them, and how the reclaimer takes out those of a
//! subtree that was nested in a dropped one.

use std::collections::HashSet;

use lignin_format::{flat_page::FlatPage, CommitVersion, PageId};
use lignin_store::{cache::PageRef, values::ValueStoreError};

use super::{
    error::DropError,
    flat::FlatDrops,
    step::{bucket_root, damaged, flat_page, value_bytes, Busy, Reclaimer, Step, LOOKS},
};
use crate::{
    directory::{Directory, Host, MerkId, Prefix},
    flat::{Committed, FlatConfig, FlatPagesMut, FlatTree, StepLog},
    OperationGuard,
};

/// A flat keyspace that holds records of subtrees: an aux, a meta or a
/// roots column, or the column of the trees that keep their data flat.
///
/// A record of the subtree `prefix` is under the key `lead`, then the 32
/// bytes of the prefix, then the record's own key. So the records of one
/// subtree are one range of the keyspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column<'a> {
    /// The keyspace.
    pub tree: FlatTree,
    /// What stands before a subtree's prefix in the keys: nothing where the
    /// column has a keyspace of its own, and the bytes that tell it from
    /// the others where several share one.
    pub lead: &'a [u8],
}

impl Column<'_> {
    /// What the keys of the records of the subtree `prefix` begin with.
    pub fn range(&self, prefix: &Prefix) -> Vec<u8> {
        [self.lead, prefix].concat()
    }
}

/// The flat side of an operation that drops or enters subtrees.
///
/// This is all that [`unlink`](super::unlink) and [`enter`] need of their
/// caller for the flat records, and it is the same for both:
///
/// - `pages`: the flat pages of the write transaction, which the directory
///   is written through as well;
/// - `columns`: **every** flat keyspace that holds records of subtrees. A
///   column that is left out keeps the records of a dropped subtree, and
///   shows them to a subtree that is entered again under its prefix;
/// - `drops`: the keyspace of the pages that lazy range deletes left
///   ([`FlatDrops`]). The engine keeps its root page, which never changes,
///   as it keeps the directory's. Its pages are flat pages of the
///   transaction like any other, and become durable with the commit and
///   through nothing else;
/// - `at`: the version the transaction reads at, which is the latest
///   version when it commits ([`FlatDrops::unlink_range`]). A transaction
///   that makes its pages again over a newer base makes these calls again,
///   with that base.
///
/// A column's keys are `lead ++ prefix ++ key` ([`Column`]), with a lead
/// that leaves a prefix room in a key that a page stores as it is.
#[derive(Debug)]
pub struct Flats<'a, F: ?Sized> {
    /// The flat pages of the write transaction.
    pub pages: &'a mut F,
    /// Every flat keyspace that holds records of subtrees.
    pub columns: &'a [Column<'a>],
    /// The keyspace of the unlinked pages.
    pub drops: &'a FlatDrops,
    /// The version the transaction reads at.
    pub at: CommitVersion,
}

impl<F: FlatPagesMut + ?Sized> Flats<'_, F> {
    /// Takes every record of the subtree `prefix` out of every column,
    /// with the lazy range delete: the leaf at each end of the subtree's
    /// range is written, and the pages between are unlinked unread. A
    /// column that has no record of the subtree is read down one path and
    /// not written.
    pub(super) fn unlink_subtree(&mut self, prefix: &Prefix) -> Result<(), DropError> {
        for column in self.columns {
            let range = column.range(prefix);
            self.drops
                .unlink_prefix(self.pages, &column.tree, &range, self.at)?;
        }
        Ok(())
    }
}

/// Enters a subtree under `prefix` ([`Directory::add`]), so that it starts
/// with no flat record: a prefix under which a subtree below a dropped one
/// is still entered has that subtree's flat records, and they are unlinked
/// from every column in the same operation.
///
/// **A write transaction enters every subtree through this**, and drops
/// through [`unlink`](super::unlink). The two keep the rule that the flat
/// records under a prefix are those of the entry that the directory has
/// under it (see the [module documentation](super)).
///
/// A subtree that is refused ([`Directory::add`]) has changed no page.
/// After any other error the caller rolls the operation's scope back.
pub fn enter<F: FlatPagesMut + ?Sized>(
    directory: &Directory,
    flats: &mut Flats<'_, F>,
    prefix: &Prefix,
    tree_type: &[u8],
    host: Option<Host<'_>>,
) -> Result<MerkId, DropError> {
    let left = directory.stored(flats.pages, prefix)?;
    let left = left.is_some_and(|entry| !directory.is_live(&entry));
    let id = directory.add(flats.pages, prefix, tree_type, host)?;
    if left {
        flats.unlink_subtree(prefix)?;
    }
    Ok(id)
}

/// How far the reclaimer has come with the flat records of one subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Sweep {
    /// The subtree.
    pub id: MerkId,
    /// The column it is in, or the number of columns once all are done.
    pub column: usize,
    /// The stored key it goes on from in that column.
    pub from: Vec<u8>,
}

/// What a sweep over the flat records of one subtree may still do, and
/// what it took out.
#[derive(Debug, Default)]
pub(super) struct Swept {
    /// The page steps it may still make.
    pub left: usize,
    pub records: u64,
    pub leaves: u64,
    pub values: u64,
    pub value_bytes: u64,
    pub pending: u64,
    pub miscount: Option<ValueStoreError>,
}

/// Why a sweep ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended {
    /// No record of the range is left, at the version that was read.
    Done,
    /// The steps that were asked for are made.
    More,
    Blocked(Busy),
    /// A leaf is not as the version that was read has it: a commit came
    /// since.
    Stale,
}

impl<'s, L: StepLog> Reclaimer<'s, L> {
    /// Takes the flat records of the subtree `prefix`, which had the id
    /// `sweep.id`, out of `columns`, by at most `swept.left` page steps,
    /// and says whether it could not go on.
    ///
    /// **Only the records of that subtree go.** A subtree can be entered
    /// again under the prefix at any time, and its records are under the
    /// same keys. So a step reads the entry that `directory` stores under
    /// the prefix and the leaf at one version, goes on only if the entry
    /// has the id, and is made only if that version of the leaf is still
    /// the current one. A page version that was visible at a version holds
    /// no write of a later commit, so its records under the prefix are
    /// those of the entry of that version.
    ///
    /// A step takes entries out of one leaf and changes nothing else: the
    /// leaf keeps its place also with no entry left, and a bucket of long
    /// keys keeps its tree, with no key in it
    /// ([`flat`](crate::flat), choice 12).
    pub(super) fn sweep_subtree<G: OperationGuard + ?Sized>(
        &self,
        directory: &Directory,
        columns: &[Column<'_>],
        prefix: &Prefix,
        guard: &G,
        sweep: &mut Sweep,
        swept: &mut Swept,
    ) -> Result<Option<Busy>, DropError> {
        let mut looks = 0;
        while let Some(column) = columns.get(sweep.column) {
            let at = self.log.latest();
            let pages = Committed::new(self.loader, self.values, guard, at);
            let entry = directory.stored(&pages, prefix)?;
            if entry.is_none_or(|entry| entry.id() != sweep.id) {
                // Another subtree has the prefix, or none has: what was
                // under it was unlinked with the entry.
                sweep.column = columns.len();
                break;
            }
            let range = column.range(prefix);
            if sweep.from.is_empty() {
                sweep.from.clone_from(&range);
            }
            let tree = &column.tree;
            if range.len() > tree.config().key_limit() {
                return Err(DropError::Record("a lead that leaves a prefix no room"));
            }
            match self.sweep(tree, &range, at, &mut sweep.from, swept)? {
                Ended::Done => {
                    sweep.column += 1;
                    sweep.from.clear();
                    looks = 0;
                }
                Ended::More => return Ok(None),
                Ended::Blocked(busy) => return Ok(Some(busy)),
                Ended::Stale => {
                    looks += 1;
                    if looks >= LOOKS {
                        return Ok(Some(Busy::Contended));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Takes the entries of `tree` whose stored keys begin with `range` and
    /// are at or after `from` out of their leaves, as the version `at` has
    /// the tree, the lowest first. `from` moves on past what is done.
    fn sweep(
        &self,
        tree: &FlatTree,
        range: &[u8],
        at: CommitVersion,
        from: &mut Vec<u8>,
        swept: &mut Swept,
    ) -> Result<Ended, DropError> {
        let config = tree.config();
        // Every page that was looked at: the tree of a bucket begins at
        // none of them.
        let mut passed = HashSet::new();
        // The pages to look at, the lowest keys last, each with the level
        // of the branch that names it. A page is one level below its
        // branch, so the walk of a tree ends.
        let mut walk = vec![(tree.root(), None::<u8>)];
        while let Some((page, above)) = walk.pop() {
            passed.insert(page);
            let frame = self
                .at(page, at)?
                .ok_or(damaged(page, "a flat tree that names no page"))?;
            let leaf = match flat_page(&frame, page, above)? {
                FlatPage::Branch(branch) => {
                    let first = branch.child_index(from);
                    let last = branch.len();
                    let above = Some(branch.level());
                    for index in (first..=last).rev() {
                        // A child whose keys are all past the range ends
                        // the walk of this branch.
                        let past = index
                            .checked_sub(1)
                            .and_then(|slot| branch.separator(slot))
                            .is_some_and(|low| {
                                let low = low.to_vec();
                                low.as_slice() > range && !low.starts_with(range)
                            });
                        if past {
                            continue;
                        }
                        let child = branch.child(index);
                        let child = child.ok_or(damaged(page, "a branch that names no child"))?;
                        walk.push((child, above));
                    }
                    continue;
                }
                FlatPage::Leaf(leaf) => leaf,
            };
            let mut keys = Vec::new();
            let mut buckets = Vec::new();
            let mut ended = false;
            for (key, value) in leaf.iter() {
                let key = key.to_vec();
                if key.as_slice() < from.as_slice() {
                    continue;
                }
                if !key.starts_with(range) {
                    ended = true;
                    break;
                }
                if key.len() > config.key_limit() {
                    let root = bucket_root(page, value)?;
                    buckets.push((key, root));
                } else {
                    keys.push(key);
                }
            }
            if let Some(ended) = self.take(&frame, &config, &keys, swept)? {
                return Ok(ended);
            }
            // The trees of the buckets among them are emptied, each from
            // its start: a bucket holds the keys of one head, and few.
            for (key, root) in buckets {
                match self.empty(root, &config, at, swept, &mut passed)? {
                    Ended::Done => {}
                    other => return Ok(other),
                }
                from.clone_from(&key);
                from.push(0);
            }
            if let Some(last) = keys.last()
                && last.as_slice() >= from.as_slice()
            {
                from.clone_from(last);
                from.push(0);
            }
            if ended {
                break;
            }
        }
        Ok(Ended::Done)
    }

    /// Takes every entry out of the tree of a bucket of long keys whose
    /// root is `root`, and out of the trees of the buckets in it, as the
    /// version `at` has them. The trees stay, with no key in them.
    ///
    /// A long key is as many trees deep as it has eighths of a page, and no
    /// key is too long ([`flat`](crate::flat)), so the trees are not
    /// counted. What ends the walk is that no tree begins at a page that
    /// was `passed`: a bucket that names such a page is damage, and would
    /// make the entries of another tree those of the bucket.
    ///
    /// A call that could not go on begins again at the root: a leaf that
    /// was emptied has no entry to take out, and costs a read.
    fn empty(
        &self,
        root: PageId,
        config: &FlatConfig,
        at: CommitVersion,
        swept: &mut Swept,
        passed: &mut HashSet<PageId>,
    ) -> Result<Ended, DropError> {
        let mut trees = vec![root];
        while let Some(root) = trees.pop() {
            let mut walk = vec![(root, None::<u8>)];
            while let Some((page, above)) = walk.pop() {
                if !passed.insert(page) {
                    let what = "a bucket of long keys whose tree is a tree it is in";
                    return Err(damaged(page, what));
                }
                let frame = self
                    .at(page, at)?
                    .ok_or(damaged(page, "a flat tree that names no page"))?;
                let leaf = match flat_page(&frame, page, above)? {
                    FlatPage::Branch(branch) => {
                        let above = Some(branch.level());
                        for index in 0..=branch.len() {
                            let child = branch.child(index);
                            let what = "a branch that names no child";
                            walk.push((child.ok_or(damaged(page, what))?, above));
                        }
                        continue;
                    }
                    FlatPage::Leaf(leaf) => leaf,
                };
                let mut keys = Vec::new();
                for (key, value) in leaf.iter() {
                    if key.len() > config.key_limit() {
                        trees.push(bucket_root(page, value)?);
                    } else {
                        keys.push(key.to_vec());
                    }
                }
                if let Some(ended) = self.take(&frame, config, &keys, swept)? {
                    return Ok(ended);
                }
            }
        }
        Ok(Ended::Done)
    }

    /// One page step of a sweep: takes the entries under `keys` out of the
    /// leaf in `frame`, and counts them. Returns why the sweep ends here,
    /// if it does.
    fn take(
        &self,
        frame: &PageRef<'s>,
        config: &FlatConfig,
        keys: &[Vec<u8>],
        swept: &mut Swept,
    ) -> Result<Option<Ended>, DropError> {
        if keys.is_empty() {
            return Ok(None);
        }
        if swept.left == 0 {
            return Ok(Some(Ended::More));
        }
        match self.remove_entries(frame, config.capacity(), keys)? {
            None => Ok(Some(Ended::Stale)),
            Some(Step::Refused(busy)) => Ok(Some(Ended::Blocked(busy))),
            Some(Step::Made(removed)) => {
                swept.left -= 1;
                swept.records += keys.len() as u64;
                swept.leaves += 1;
                swept.values += removed.released.len() as u64;
                swept.value_bytes += value_bytes(&removed.released);
                swept.pending += removed.pending;
                if swept.miscount.is_none() {
                    swept.miscount = removed.miscount;
                }
                Ok(None)
            }
        }
    }
}
