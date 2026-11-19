//! The private frames of a write transaction, and how a batch is applied
//! to them by key.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use lignin_format::{CellAddress, CommitVersion};
use lignin_tree::{
    fragment::{
        FragmentError, Fragments, HostedWrite, LinkWrite, NodeWrite, RootWrite, Target, ValueWrite,
        Write,
    },
    node::{Lease, NodeHandle},
};

use super::{
    error::{BatchError, TxnError},
    key::{Column, Subtree},
    pages::{find, NodePages, TreePages, TreeValues},
    path::{FlatWriter, WritePath},
    write::{Applied, Batch, Hosted, LinkPut, MerkWrite, RecordWrite, RootPut, ValuePut},
    write_set::WriteSet,
};

/// Why a batch was not applied to the frames.
#[derive(Debug)]
pub(crate) struct Refused {
    pub(crate) error: TxnError,
    /// Whether the frames may hold a part of the batch. They are then of no
    /// use, and are made again from the write set (WT3).
    pub(crate) dirty: bool,
}

impl Refused {
    /// A refusal that changed no page.
    fn clean(error: impl Into<TxnError>) -> Self {
        Self {
            error: error.into(),
            dirty: false,
        }
    }

    /// A failure after which the frames are in no state to go on from.
    fn dirty(error: impl Into<TxnError>) -> Self {
        Self {
            error: error.into(),
            dirty: true,
        }
    }

    fn directory(error: super::path::DirectoryError) -> Self {
        Self::dirty(TxnError::Directory(error))
    }
}

/// The key a record of `subtree` is stored under in the flat tree of its
/// column: the subtree's 32 bytes, then the record's key.
pub(crate) fn stored_key(out: &mut Vec<u8>, subtree: &Subtree, key: &[u8]) {
    out.clear();
    out.extend_from_slice(subtree.as_bytes());
    out.extend_from_slice(key);
}

/// The first stored key after every key of `subtree`, or `None` if its keys
/// go to the end of the keyspace.
pub(crate) fn subtree_end(subtree: &Subtree) -> Option<Vec<u8>> {
    let mut end = subtree.as_bytes().to_vec();
    while let Some(last) = end.pop() {
        if last < u8::MAX {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

/// The private frames of a write transaction (`transactions.md` WT3, WT4):
/// its copies of tree pages and of flat pages, over the committed pages of
/// its base version, which it keeps pinned (PN1).
///
/// They are derived from the write set: applying the write set's
/// operations, in order, to the committed state of the base gives them.
/// Nothing is kept here that the write set does not say, so they can be
/// given up and made again at any time: for a rebase, a rollback to a
/// mark, or after an operation that failed half way.
pub(crate) struct Frames<'s> {
    base: CommitVersion,
    pub(crate) tree: TreePages<'s>,
    pub(crate) values: TreeValues<'s>,
    pub(crate) flat: FlatFrames<'s>,
    fragments: Fragments,
    key: Vec<u8>,
    /// For each Merk of the batch that was applied last, whether it had no
    /// root before.
    empty: Vec<bool>,
}

/// The flat pages of the frames. When they are dropped, their copies are
/// given up.
pub(crate) struct FlatFrames<'s>(Option<FlatWriter<'s>>);

impl<'s> FlatFrames<'s> {
    /// Ends the flat pages and hands the writer to the commit.
    pub(crate) fn into_writer(mut self) -> FlatWriter<'s> {
        self.0
            .take()
            .expect("the writer is there until it is taken")
    }
}

impl<'s> std::ops::Deref for FlatFrames<'s> {
    type Target = FlatWriter<'s>;

    fn deref(&self) -> &FlatWriter<'s> {
        self.0
            .as_ref()
            .expect("the writer is there until it is taken")
    }
}

impl std::ops::DerefMut for FlatFrames<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
            .as_mut()
            .expect("the writer is there until it is taken")
    }
}

impl Drop for FlatFrames<'_> {
    fn drop(&mut self) {
        if let Some(writer) = &mut self.0 {
            // What the pool will not let go of now cannot be given up
            // later either: nothing is left that could.
            let _ = writer.roll_back();
        }
    }
}

/// The nodes that the keys of one batch name, as the tree layer names them.
struct Resolver<'a> {
    merks: &'a [MerkWrite<'a>],
    /// The root of each Merk before the batch.
    roots: Vec<Option<CellAddress>>,
    /// The nodes each Merk's change set writes, by key.
    written: Vec<HashMap<&'a [u8], usize>>,
    by_subtree: HashMap<Subtree, usize>,
}

impl Resolver<'_> {
    /// The node under `key` in the Merk `merk`: a node of the batch, or a
    /// cell that is there.
    fn node(
        &self,
        tree: &mut TreePages<'_>,
        merk: usize,
        key: &[u8],
    ) -> Result<Option<Target>, TxnError> {
        if let Some(node) = self.written[merk].get(key) {
            return Ok(Some(Target::Node(*node)));
        }
        Ok(find(tree, self.roots[merk], key)?.map(Target::At))
    }

    /// The root of the Merk `merk` after the batch.
    fn root(&self, tree: &mut TreePages<'_>, merk: usize) -> Result<Option<Target>, TxnError> {
        match self.merks[merk].root {
            RootPut::Keep => Ok(self.roots[merk].map(Target::At)),
            RootPut::Set(None) => Ok(None),
            RootPut::Set(Some(key)) => {
                let subtree = self.merks[merk].subtree;
                let root = self.node(tree, merk, key)?;
                Ok(Some(root.ok_or(BatchError::NoRoot { subtree })?))
            }
        }
    }

    fn link<'l>(
        &self,
        tree: &mut TreePages<'_>,
        merk: usize,
        link: &LinkPut<'l>,
    ) -> Result<LinkWrite<'l>, TxnError> {
        let subtree = self.merks[merk].subtree;
        let child = self.node(tree, merk, link.key)?;
        Ok(LinkWrite {
            child: child.ok_or(BatchError::NoChild { subtree })?,
            hash: link.hash,
            heights: link.heights,
            child_key_len: link.key.len() as u8,
            aggregate: link.aggregate,
        })
    }
}

/// One node of a batch with its names resolved.
struct Resolved<'a> {
    at: Option<CellAddress>,
    left: Option<LinkWrite<'a>>,
    right: Option<LinkWrite<'a>>,
    /// The position of the node's list of hosted roots.
    hosted: Option<usize>,
    value: ValueWrite<'a>,
}

impl<'s> Frames<'s> {
    /// Frames with no copy, over the committed pages of the version `at`,
    /// which the transaction keeps pinned for as long as it has them.
    pub(crate) fn new(path: &WritePath<'s>, at: CommitVersion) -> Self {
        let owner = WritePath::next_owner();
        let lease = Lease::take();
        let size = path.layout.page_size();
        Self {
            tree: TreePages::new(path.loader, path.pool, path.ids, owner, at, lease, size),
            values: TreeValues::new(path.values),
            flat: FlatFrames(Some(FlatWriter::new(
                path.committed(at),
                path.pool,
                owner,
                path.ids,
            ))),
            fragments: Fragments::new(path.layout.clone()),
            key: Vec::new(),
            empty: Vec::new(),
            base: at,
        }
    }

    /// Frames that show the write set `set` applied, in order, to the
    /// committed state of the version `at` (WT3): every operation is
    /// applied again through the tree layer, which finds every cell by its
    /// key (WT7, LT4).
    pub(crate) fn build(
        path: &WritePath<'s>,
        at: CommitVersion,
        set: &WriteSet,
    ) -> Result<Self, TxnError> {
        let mut frames = Self::new(path, at);
        for op in 0..set.ops() {
            let hosted = set.hosted_lists(op);
            let batch = set.batch(op, &hosted);
            let merks = batch.merks();
            let batch = Batch {
                records: batch.records(),
                merks: &merks,
            };
            frames
                .apply(path, &batch, Some(&set.made(op)))
                .map_err(|refused| refused.error)?;
        }
        Ok(frames)
    }

    /// The version the frames are built over: *B(T)*.
    pub(crate) fn base(&self) -> CommitVersion {
        self.base
    }

    /// For each Merk of the batch that was applied last, whether it had no
    /// root before.
    pub(crate) fn empty_before(&self) -> &[bool] {
        &self.empty
    }

    /// The lease the nodes of the frames are read under. It ends with the
    /// frames (PN8).
    pub(crate) fn lease(&self) -> Lease {
        self.tree.lease()
    }

    /// The root cell of the Merk `subtree`, as the transaction sees it.
    pub(crate) fn root(
        &self,
        path: &WritePath<'s>,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, TxnError> {
        (path.directory.root(&self.flat, subtree)).map_err(TxnError::Directory)
    }

    /// The address `handle` names, if the handle is still good in these
    /// frames and its cell has the key `key`.
    pub(crate) fn follow(&mut self, handle: NodeHandle, key: &[u8]) -> Option<CellAddress> {
        if handle.lease() != self.lease() {
            return None;
        }
        let followed = self.tree.page(handle.page(), |page| {
            let node = page.resolve(handle)?;
            (node.key() == key).then(|| node.address())
        });
        followed.ok().flatten()
    }

    /// Applies `batch` to the frames: the change sets of its Merks as one
    /// write of the tree layer, with each Merk's directory entry, and then
    /// its records.
    ///
    /// The batch has passed [`check`](super::check::check). `made` is given
    /// for a batch that is applied again (WT7): it says of each Merk
    /// whether the transaction made its first node with this batch. Such a
    /// Merk has to be empty still, and the batch is refused if it is not
    /// (GM3).
    pub(crate) fn apply(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        made: Option<&[bool]>,
    ) -> Result<Applied, Refused> {
        let directory = path.directory;
        for record in batch.records {
            // A range of Merk nodes is not deleted through a flat tree.
            if let RecordWrite::DeleteRange { unit, .. } = *record
                && unit.column == Column::Data
                && (directory.root(&self.flat, &unit.subtree))
                    .map_err(|error| Refused::clean(TxnError::Directory(error)))?
                    .is_some()
            {
                return Err(Refused::clean(BatchError::MerkRange { unit }));
            }
        }
        let applied = self.apply_merks(path, batch.merks, made)?;
        for record in batch.records {
            let tree = path.keyspaces.of(record.unit().column);
            let subtree = record.unit().subtree;
            let done = match *record {
                RecordWrite::Put { key, value, .. } => {
                    stored_key(&mut self.key, &subtree, key);
                    tree.put(&mut *self.flat, &self.key, value)
                }
                RecordWrite::Delete { key, .. } => {
                    stored_key(&mut self.key, &subtree, key);
                    tree.delete(&mut *self.flat, &self.key).map(|_| ())
                }
                RecordWrite::DeleteRange { start, end, .. } => {
                    stored_key(&mut self.key, &subtree, start);
                    let end = match end {
                        Some(end) => {
                            let mut stored = Vec::new();
                            stored_key(&mut stored, &subtree, end);
                            Some(stored)
                        }
                        None => subtree_end(&subtree),
                    };
                    tree.delete_range(&mut *self.flat, &self.key, end.as_deref())
                }
            };
            done.map_err(Refused::dirty)?;
        }
        Ok(applied)
    }

    fn apply_merks(
        &mut self,
        path: &WritePath<'s>,
        merks: &[MerkWrite<'_>],
        made: Option<&[bool]>,
    ) -> Result<Applied, Refused> {
        self.empty.clear();
        if merks.is_empty() {
            return Ok(Applied::default());
        }
        let directory = path.directory;
        let mut offsets = Vec::with_capacity(merks.len());
        let mut total = 0;
        for merk in merks {
            offsets.push(total);
            total += merk.nodes.len();
        }
        let mut roots = Vec::with_capacity(merks.len());
        for merk in merks {
            let root = directory.root(&self.flat, &merk.subtree);
            roots.push(root.map_err(|error| Refused::clean(TxnError::Directory(error)))?);
        }
        self.empty.extend(roots.iter().map(Option::is_none));
        let made = made.unwrap_or_default().iter();
        for ((merk, made), empty) in merks.iter().zip(made).zip(&self.empty) {
            // The transaction made the first node of this Merk, and it has
            // a root now that is not the transaction's.
            if *made && !*empty {
                let subtree = merk.subtree;
                return Err(Refused::clean(BatchError::RootTaken { subtree }));
            }
        }
        let written = (merks.iter().zip(&offsets))
            .map(|(merk, offset)| {
                let nodes = merk.nodes.iter().enumerate();
                nodes.map(|(node, put)| (put.key, offset + node)).collect()
            })
            .collect();
        let by_subtree = (merks.iter().enumerate())
            .map(|(merk, write)| (write.subtree, merk))
            .collect();
        let resolver = Resolver {
            merks,
            roots,
            written,
            by_subtree,
        };

        // What every name of the batch stands for is found before anything
        // is changed, so a batch that names what is not there is refused
        // with every page as it was.
        let mut held = Vec::new();
        let resolved = self.resolve(path, &resolver, total, &mut held);
        let (nodes, hosted, removed, root_writes, root_targets) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                self.values.forget(&held);
                return Err(Refused::clean(error));
            }
        };
        let node_writes: Vec<NodeWrite<'_>> = (merks.iter().flat_map(|merk| merk.nodes))
            .zip(&nodes)
            .map(|(put, node)| NodeWrite {
                at: node.at,
                key: put.key,
                feature: put.feature,
                value_hash: put.value_hash,
                kv_hash: put.kv_hash,
                left: node.left,
                right: node.right,
                hosted: match node.hosted {
                    None => HostedWrite::Keep,
                    Some(list) => HostedWrite::Set(&hosted[list]),
                },
                value: node.value,
            })
            .collect();
        let roots: Vec<RootWrite> = root_writes.iter().flatten().copied().collect();
        let write = Write {
            nodes: &node_writes,
            removed: &removed,
            dropped: &[],
            roots: &roots,
        };
        let report = match (self.fragments).apply(&mut self.tree, &mut self.values, &write) {
            Ok(report) => report,
            Err(error @ (FragmentError::Write(_) | FragmentError::NoRoom { .. })) => {
                self.values.forget(&held);
                return Err(Refused::clean(error));
            }
            Err(error) => return Err(Refused::dirty(error)),
        };
        self.values.take_failure().map_err(Refused::dirty)?;

        // The directory entry of each Merk whose root is another cell now,
        // and of each Merk whose root went along with its host.
        let carried: HashMap<CellAddress, CellAddress> = (report.carried.iter())
            .map(|carried| (carried.from, carried.to.address()))
            .collect();
        let mut settled = HashSet::new();
        let mut pointer = 0;
        for (merk, write) in merks.iter().enumerate() {
            let root = match root_writes[merk] {
                Some(_) => {
                    pointer += 1;
                    report.roots[pointer - 1]
                }
                None => match root_targets[merk] {
                    None | Some(Target::Kept) => None,
                    Some(Target::Node(node)) => Some(report.nodes[node].address()),
                    Some(Target::At(at)) => Some(*carried.get(&at).unwrap_or(&at)),
                },
            };
            if root != resolver.roots[merk] {
                directory
                    .set_root(&mut self.flat, &write.subtree, root)
                    .map_err(Refused::directory)?;
            }
            settled.extend(root);
        }
        for carried in &report.carried {
            let to = carried.to.address();
            if carried.hosted_by.is_some() && !settled.contains(&to) {
                directory
                    .moved(&mut self.flat, carried.from, to)
                    .map_err(Refused::directory)?;
            }
        }
        Ok(Applied {
            carried: (report.carried.iter())
                .map(|carried| (carried.from, carried.to))
                .collect(),
            nodes: report.nodes,
        })
    }

    /// Finds what every key of the batch names: the cell of each written
    /// and each deleted node, the child of each link, and the root of each
    /// Merk, by a hint that is still good or by the key (PN8).
    #[allow(clippy::type_complexity)]
    fn resolve<'a>(
        &mut self,
        path: &WritePath<'s>,
        resolver: &Resolver<'a>,
        total: usize,
        held: &mut Vec<lignin_format::value_log::PendingValue>,
    ) -> Result<
        (
            Vec<Resolved<'a>>,
            Vec<Vec<Option<Target>>>,
            Vec<CellAddress>,
            Vec<Option<RootWrite>>,
            Vec<Option<Target>>,
        ),
        TxnError,
    > {
        let merks = resolver.merks;
        let mut root_targets = Vec::with_capacity(merks.len());
        for merk in 0..merks.len() {
            root_targets.push(resolver.root(&mut self.tree, merk)?);
        }
        let mut nodes = Vec::with_capacity(total);
        let mut hosted: Vec<Vec<Option<Target>>> = Vec::new();
        let mut removed = Vec::new();
        let mut hosted_here = vec![false; merks.len()];
        for (merk, write) in merks.iter().enumerate() {
            let subtree = write.subtree;
            for put in write.nodes {
                let hinted = put.at.and_then(|handle| self.follow(handle, put.key));
                let at = match hinted {
                    Some(at) => Some(at),
                    None => find(&mut self.tree, resolver.roots[merk], put.key)?,
                };
                let link = |tree: &mut TreePages<'_>, link: &Option<LinkPut<'a>>| {
                    (link.as_ref())
                        .map(|link| resolver.link(tree, merk, link))
                        .transpose()
                };
                let left = link(&mut self.tree, &put.left)?;
                let right = link(&mut self.tree, &put.right)?;
                let hosts = match put.hosted {
                    Hosted::Keep => None,
                    Hosted::Set(subtrees) => {
                        let mut targets = Vec::with_capacity(subtrees.len());
                        for hosted in subtrees {
                            targets.push(match resolver.by_subtree.get(hosted) {
                                Some(&merk) => {
                                    hosted_here[merk] = true;
                                    root_targets[merk]
                                }
                                None => (path.directory.root(&self.flat, hosted))
                                    .map_err(TxnError::Directory)?
                                    .map(Target::At),
                            });
                        }
                        hosted.push(targets);
                        Some(hosted.len() - 1)
                    }
                };
                let value = match put.value {
                    ValuePut::Keep if at.is_none() => {
                        return Err(BatchError::NoValue { subtree }.into())
                    }
                    ValuePut::Keep => ValueWrite::Keep,
                    ValuePut::Inline(value) => ValueWrite::Inline(value),
                    // A value of no bytes is never out of line.
                    ValuePut::OutOfLine([]) => ValueWrite::Inline(&[]),
                    ValuePut::OutOfLine(value) => {
                        let pending = self.values.hold(Arc::from(value))?;
                        held.push(pending);
                        ValueWrite::Pending(pending)
                    }
                };
                nodes.push(Resolved {
                    at,
                    left,
                    right,
                    hosted: hosts,
                    value,
                });
            }
            for key in write.removed {
                // A delete of a key with no node is a write of the key and
                // changes no page (ST5).
                removed.extend(find(&mut self.tree, resolver.roots[merk], key)?);
            }
        }
        // The root of a Merk that no node of the batch hosts is named by a
        // pointer the directory alone holds.
        let root_writes = (0..merks.len())
            .map(|merk| {
                (!hosted_here[merk]).then(|| RootWrite {
                    was: resolver.roots[merk],
                    now: root_targets[merk],
                    anchored: merks[merk].anchored,
                })
            })
            .collect();
        Ok((nodes, hosted, removed, root_writes, root_targets))
    }
}
