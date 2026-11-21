//! A batch as it stands when it is applied: the outcome of each key, and
//! the one write of a transaction that they are.

use std::{collections::BTreeMap, fmt};

use lignin_tree::node::NodeHandle;

use super::{
    node::StagedNode,
    staged::{Entry, Keys, Leave, Move, Staged, SubtreeState, Write, DATA, ORDER},
};
use crate::txn::{
    Applied, Batch, BatchError, Column, Entering, HostPut, Hosted, MerkWrite, RecordWrite, RootPut,
    Scope, Subtree, SubtreeWrite, TxnError, Unit,
};

/// The most Merks an element hosts: a pointer is named by one byte.
const MAX_HOSTED: usize = u8::MAX as usize;

/// What a batch holds for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written<'a> {
    /// A put of a flat record, with its value.
    Record(&'a [u8]),
    /// A put of a node.
    Node(&'a StagedNode),
    /// A delete of a flat record.
    Removed,
    /// A delete of a node.
    NodeRemoved,
}

/// The one outcome a batch holds for a key, with what its call attached.
///
/// It is shown as the operation of a storage batch that it stands for:
/// `Put`, `PutAux`, `PutRoot`, `PutMeta` or the `Delete` of the same
/// column, with the subtree, the key and the value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Outcome<'a, C> {
    /// The column and the subtree of the key.
    pub unit: Unit,
    /// The key.
    pub key: &'a [u8],
    /// The call that stands.
    pub write: Written<'a>,
    /// What that call attached.
    pub tag: &'a C,
}

impl<C> Outcome<'_, C> {
    /// Whether the outcome is a put, of a record or of a node.
    pub const fn is_put(&self) -> bool {
        matches!(self.write, Written::Record(_) | Written::Node(_))
    }
}

fn shown(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("[hex: {hex}, str: {}]", String::from_utf8_lossy(bytes))
}

impl<C> fmt::Debug for Outcome<'_, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let column = match self.unit.column {
            Column::Data => "",
            Column::Aux => "Aux",
            Column::Roots => "Root",
            Column::Meta => "Meta",
        };
        let call = match self.is_put() {
            true => "Put",
            false => "Delete",
        };
        let mut shows = f.debug_struct(&format!("{call}{column}"));
        shows
            .field("subtree", &self.unit.subtree)
            .field("key", &shown(self.key));
        match self.write {
            Written::Record(value) => shows.field("value", &shown(value)),
            Written::Node(node) => shows.field("node", node),
            Written::Removed | Written::NodeRemoved => &mut shows,
        };
        shows.finish()
    }
}

/// The element a subtree hangs on, as the batch was last told.
struct Hung {
    subtree: Subtree,
    key: Box<[u8]>,
    position: u8,
}

/// A batch that no call is made on any more: what it holds for each key,
/// and the write of a transaction that it is.
pub struct Resolved<C> {
    staged: Staged<C>,
    hung: BTreeMap<Subtree, Hung>,
    /// The subtrees that leave, and those that are entered in the order
    /// they were.
    leaves: Vec<(Subtree, Leave)>,
    enters: Vec<Subtree>,
}

/// A staged write as the outcome it is for its key.
fn written(write: &Write) -> Written<'_> {
    match write {
        Write::Record(value) => Written::Record(value),
        Write::Node { node, .. } => Written::Node(node),
        Write::Remove { node: false, .. } => Written::Removed,
        Write::Remove { node: true, .. } => Written::NodeRemoved,
    }
}

impl<C> fmt::Debug for Resolved<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("leaves", &self.leaves)
            .field("enters", &self.enters)
            .field("outcomes", &self.outcomes().collect::<Vec<_>>())
            .finish()
    }
}

impl<C> Resolved<C> {
    pub(super) fn new(staged: Staged<C>) -> Result<Self, BatchError> {
        let (staged, refused) = staged.finish();
        if let Some(refused) = refused {
            return Err(refused);
        }
        // Where a subtree hangs is what the later statement says. An
        // element that the batch deletes hosts nothing.
        let mut hung = BTreeMap::new();
        for (subtree, key, host) in staged.hosts_in_order() {
            if host.hosted.len() > MAX_HOSTED {
                return Err(BatchError::TooManyHosted { subtree });
            }
            for (position, hosted) in host.hosted.iter().enumerate() {
                let at = Hung {
                    subtree,
                    key: key.into(),
                    position: position as u8,
                };
                hung.insert(*hosted, at);
            }
        }
        // A subtree is moved to the element that the batch was told hosts
        // it afterwards.
        let unhosted = |moving: &&Move| !hung.contains_key(&moving.to);
        if let Some(moving) = staged.moves.iter().find(unhosted) {
            let subtree = moving.from;
            return Err(BatchError::MoveHost { subtree });
        }
        let left = |(subtree, state): (&Subtree, &SubtreeState)| Some((*subtree, state.leave?));
        let leaves = staged.subtrees.iter().filter_map(left).collect();
        // A subtree is entered after the one its host is in, where the
        // batch enters both, and otherwise in the order the enters were
        // made.
        let entered = |subtree: &Subtree| {
            let state = staged.subtrees.get(subtree);
            state.and_then(|state| state.enter.as_ref())
        };
        let depth = |subtree: &Subtree| {
            let hosts = std::iter::successors(hung.get(subtree), |at| hung.get(&at.subtree));
            let entered = hosts.take_while(|at| entered(&at.subtree).is_some());
            entered.take(staged.subtrees.len()).count()
        };
        let mut enters: Vec<_> = (staged.subtrees.iter())
            .filter_map(|(subtree, state)| {
                Some((depth(subtree), state.enter.as_ref()?.order, *subtree))
            })
            .collect();
        enters.sort_unstable();
        Ok(Self {
            enters: enters.into_iter().map(|(.., subtree)| subtree).collect(),
            staged,
            hung,
            leaves,
        })
    }

    /// The subtrees the batch enters, in the order they are entered: those
    /// it enters as new ([`StagedBatch::enter`](super::StagedBatch::enter))
    /// and those it enters if they are absent
    /// ([`StagedBatch::enter_if_absent`](super::StagedBatch::enter_if_absent)).
    /// Each is entered once the batch is applied, and the second kind may
    /// have been before: [`Self::with_batch`] shows which enter each is.
    pub fn enters(&self) -> &[Subtree] {
        &self.enters
    }

    /// Whether the batch moves a subtree ([`StagedBatch::move_subtree`](super::StagedBatch::move_subtree)).
    pub fn moves(&self) -> bool {
        !self.staged.moves.is_empty()
    }

    /// Whether the batch drops or clears a subtree.
    pub fn leaves(&self) -> bool {
        !self.leaves.is_empty()
    }

    /// What the batch holds for `key` in the data column of `subtree`, or
    /// `None` if it holds no outcome for it.
    pub fn held(&self, subtree: Subtree, key: &[u8]) -> Option<Written<'_>> {
        (self.staged.held(subtree, key)).map(|entry| written(&entry.write))
    }

    /// How many keys the batch holds an outcome for.
    pub fn len(&self) -> usize {
        self.staged.count()
    }

    /// Whether the batch writes nothing: its application adds nothing to
    /// the transaction (`transactions.md` GM0). A batch that moves a
    /// subtree is not empty, whatever else it holds: the transaction
    /// judges the move, also one whose host the batch does not put.
    pub fn is_empty(&self) -> bool {
        self.len() == 0 && self.staged.subtrees.is_empty() && self.staged.moves.is_empty()
    }

    /// The outcome of each key, in the order a storage batch is iterated
    /// and charged in (`cost-model.md` B1): the meta, the aux, the roots
    /// and the data column, and in each by subtree and key.
    pub fn outcomes(&self) -> impl Iterator<Item = Outcome<'_, C>> {
        (ORDER.iter().zip(&self.staged.columns)).flat_map(|(column, held)| {
            held.iter().flat_map(move |(subtree, keys)| {
                keys.iter().map(move |(key, entry)| Outcome {
                    unit: Unit::new(*column, *subtree),
                    key,
                    write: written(&entry.write),
                    tag: &entry.tag,
                })
            })
        })
    }

    /// What the call of each outcome attached, in the order of
    /// [`Self::outcomes`], to take or to change.
    pub fn tags_mut(&mut self) -> impl Iterator<Item = &mut C> {
        let keys = self
            .staged
            .columns
            .iter_mut()
            .flat_map(BTreeMap::values_mut);
        keys.flat_map(|keys| keys.values_mut().map(|entry| &mut entry.tag))
    }

    /// The subtrees whose data column holds an outcome for a node, with
    /// those outcomes: the Merks of the write.
    fn merks(&self) -> impl Iterator<Item = (Subtree, &Keys<C>)> {
        (self.staged.columns[DATA].iter())
            .filter(|(_, keys)| {
                let is_node = |entry: &Entry<C>| {
                    matches!(
                        entry.write,
                        Write::Node { .. } | Write::Remove { node: true, .. }
                    )
                };
                keys.values().any(is_node)
            })
            .map(|(subtree, keys)| (*subtree, keys))
    }

    /// Whether the batch puts the node `key` of `subtree`.
    fn puts(&self, subtree: Subtree, key: &[u8]) -> bool {
        let held = self.staged.held(subtree, key);
        held.is_some_and(|entry| matches!(entry.write, Write::Node { .. }))
    }

    /// Calls `run` with the batch as one write of a transaction: each key
    /// once, a host naming the Merks it hosts, and a Merk whose host the
    /// batch does not put anchored.
    pub fn with_batch<R>(&self, run: impl FnOnce(&Batch<'_>) -> R) -> R {
        let mut records = Vec::new();
        for outcome in self.outcomes() {
            let Outcome { unit, key, .. } = outcome;
            match outcome.write {
                Written::Record(value) => records.push(RecordWrite::Put { unit, key, value }),
                Written::Removed => records.push(RecordWrite::Delete { unit, key }),
                Written::Node(_) | Written::NodeRemoved => {}
            }
        }
        let mut nodes = Vec::new();
        for (subtree, keys) in self.merks() {
            let hosts = self.staged.hosts.get(&subtree);
            let (mut puts, mut removed) = (Vec::new(), Vec::new());
            for (key, entry) in keys {
                match &entry.write {
                    Write::Node { node, .. } => {
                        let hosted = hosts.and_then(|hosts| hosts.get(key));
                        puts.push(node.as_put(match hosted {
                            Some(host) => Hosted::Set(&host.hosted),
                            None => Hosted::Keep,
                        }));
                    }
                    Write::Remove { node: true, .. } => removed.push(&**key),
                    Write::Record(_) | Write::Remove { node: false, .. } => {}
                }
            }
            nodes.push((subtree, puts, removed));
        }
        let merks: Vec<_> = (nodes.iter())
            .map(|(subtree, nodes, removed)| MerkWrite {
                subtree: *subtree,
                nodes,
                removed,
                root: match self.staged.roots.get(subtree) {
                    Some(root) => RootPut::Set(root.key.as_deref()),
                    None => RootPut::Keep,
                },
                anchored: (self.hung.get(subtree))
                    .is_some_and(|hung| !self.puts(hung.subtree, &hung.key)),
            })
            .collect();
        let leaves = self.leaves.iter().map(|(subtree, leave)| match leave {
            Leave::Drop => SubtreeWrite::Drop { subtree: *subtree },
            Leave::Clear => SubtreeWrite::Clear { subtree: *subtree },
        });
        let enters = self.enters.iter().map(|subtree| {
            let enter = (self.staged.subtrees.get(subtree)).and_then(|state| state.enter.as_ref());
            let entering = Entering {
                subtree: *subtree,
                tree_type: enter.map_or(&[][..], |enter| &enter.tree_type),
                host: self.hung.get(subtree).map(|hung| HostPut {
                    subtree: hung.subtree,
                    key: &hung.key,
                    position: hung.position,
                }),
                if_absent: enter.is_some_and(|enter| enter.if_absent),
            };
            entering.write()
        });
        let moves = self.staged.moves.iter().filter_map(|moving| {
            let hung = self.hung.get(&moving.to)?;
            Some(SubtreeWrite::Move {
                from: moving.from,
                to: moving.to,
                host: HostPut {
                    subtree: hung.subtree,
                    key: &hung.key,
                    position: hung.position,
                },
                read: moving.read,
                below: &moving.below,
            })
        });
        let subtrees: Vec<_> = leaves.chain(moves).chain(enters).collect();
        run(&Batch {
            records: &records,
            merks: &merks,
            subtrees: &subtrees,
        })
    }

    /// Applies the batch to the transaction of `scope` as one operation:
    /// all of it or nothing, with one read of the latest version and one
    /// first-write version for every key it holds (RD4, WT5).
    pub fn apply(&self, scope: &Scope<'_, '_>) -> Result<Applied, TxnError> {
        self.with_batch(|batch| scope.apply(batch))
    }

    /// Where each node that the batch put is, from what its application
    /// reported: the subtree, the key and the handle (`tree-layout.md` P5).
    /// It gives nothing for a report with no nodes, which is the report of
    /// a transaction that serves its records by key.
    pub fn placed<'a>(
        &'a self,
        applied: &'a Applied,
    ) -> impl Iterator<Item = (Subtree, &'a [u8], NodeHandle)> + 'a {
        let nodes = self.merks().flat_map(|(subtree, keys)| {
            let puts = keys
                .iter()
                .filter(|(_, entry)| matches!(entry.write, Write::Node { .. }));
            puts.map(move |(key, _)| (subtree, &**key))
        });
        // A node that the transaction keeps as an unlinked record has no
        // cell, and so no handle.
        let mut unlinked = applied.unlinked.iter().copied().peekable();
        let cells = (nodes.enumerate())
            .filter(move |(place, _)| unlinked.next_if_eq(place).is_none())
            .map(|(_, node)| node);
        (cells.zip(&applied.nodes)).map(|((subtree, key), handle)| (subtree, key, *handle))
    }
}
