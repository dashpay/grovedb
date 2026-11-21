//! What a batch holds, and how each call changes it.

use std::collections::BTreeMap;

use lignin_tree::node::NodeHandle;

use super::node::StagedNode;
use crate::txn::{BatchError, Column, Differs, Hosted, NodePut, Subtree, Unit};

/// The columns in the order a batch is iterated in, and so charged in
/// (`cost-model.md` B1): the order `StorageBatch` iterates its maps in.
pub(super) const ORDER: [Column; 4] = [Column::Meta, Column::Aux, Column::Roots, Column::Data];

pub(super) const DATA: usize = 3;

const fn slot(column: Column) -> usize {
    match column {
        Column::Meta => 0,
        Column::Aux => 1,
        Column::Roots => 2,
        Column::Data => DATA,
    }
}

/// What a delete took the place of: the value that a put of the same
/// operation keeps if it keeps its value, as that put wins over the delete
/// in whichever order the two came.
pub(super) enum Under {
    /// Nothing the batch held, or a node that kept the transaction's value.
    Transaction,
    /// A node whose value the batch held, and the generation that stated
    /// the value.
    Value(Box<[u8]>, u64),
    /// The delete of an earlier operation, or a node that had no value to
    /// keep: the node has no value.
    Removed,
}

/// The one outcome a batch holds for a key.
pub(super) enum Write {
    /// A put of a flat record.
    Record(Box<[u8]>),
    /// A put of a node, the generation that stated the value it has, and
    /// whether it keeps the value of a node that an earlier operation
    /// removed, which has none.
    Node {
        node: Box<StagedNode>,
        stated: u64,
        lost: bool,
    },
    /// A delete, of a node or of a flat record.
    Remove { node: bool, under: Under },
}

pub(super) struct Entry<C> {
    pub(super) generation: u64,
    pub(super) write: Write,
    pub(super) tag: C,
}

pub(super) type Keys<C> = BTreeMap<Box<[u8]>, Entry<C>>;

/// How a subtree leaves: with its entry, or with what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Leave {
    Drop,
    Clear,
}

pub(super) struct Enter {
    pub(super) tree_type: Box<[u8]>,
    pub(super) order: u64,
    /// Whether the subtree is entered only where the transaction does not
    /// have it ([`SubtreeWrite::EnterIfAbsent`](crate::txn::SubtreeWrite)).
    pub(super) if_absent: bool,
}

/// The refusal of two enters of one subtree that state two tree types.
fn other_type(subtree: Subtree, entered: &Enter, stated: &Enter) -> BatchError {
    let differs = Box::new(Differs::TreeType {
        entered: entered.tree_type.clone(),
        stated: stated.tree_type.clone(),
    });
    BatchError::EnteredOtherwise { subtree, differs }
}

/// Which enter of a subtree stands.
enum Entered {
    No,
    Before,
    Now,
}

/// What the batch does to one subtree as a whole: to the subtree as the
/// transaction has it, and then the enter of a subtree that is new.
///
/// The statements of one operation are kept apart from what the operations
/// before it came to, and are taken together: the leave first and then the
/// enter, in whichever order they were made.
#[derive(Default)]
pub(super) struct SubtreeState {
    /// What the operations before `at` came to.
    pub(super) leave: Option<Leave>,
    pub(super) enter: Option<Enter>,
    /// The subtree this one hung in when the batch said it leaves: where
    /// the transaction has it, whatever the batch does to its host since.
    parent: Option<Subtree>,
    /// The generation of the two statements below.
    at: u64,
    leaving: Option<Leave>,
    entering: Option<Enter>,
}

impl SubtreeState {
    /// What the statements come to, or the error the transaction would
    /// give the operation that made the last of them.
    fn outcome(&self, subtree: Subtree) -> Result<(Option<Leave>, Entered), BatchError> {
        let before = (self.leave, self.enter.as_ref());
        Self::judge(subtree, before, (self.leaving, self.entering.as_ref()))
    }

    /// As [`Self::outcome`], for a subtree below one that leaves in the
    /// operation of the two statements. The subtree goes with the one
    /// above, and a leave of it by that operation is not handed to the
    /// transaction. That leave is held against a leave of an earlier
    /// operation, as anywhere, and not against an earlier enter if it is
    /// absent: whether the transaction has the subtree to leave need not
    /// be known. The operation's enter comes after the subtree went, and
    /// is of a new one: it is held against the leave of its own operation,
    /// as in a batch that holds nothing else, and against no enter of an
    /// operation before.
    fn outcome_below(&self, subtree: Subtree) -> Result<(), BatchError> {
        let before = (self.leave, self.enter.as_ref());
        match Self::judge(subtree, before, (self.leaving, None)) {
            Ok(_) | Err(BatchError::LeavesIfEntered { .. }) => {}
            Err(refusal) => return Err(refusal),
        }
        Self::judge(
            subtree,
            (None, None),
            (self.leaving, self.entering.as_ref()),
        )
        .map(drop)
    }

    /// What a leave and an enter of one operation come to after `before`,
    /// which is what the operations before it came to.
    fn judge(
        subtree: Subtree,
        (leave, enter): (Option<Leave>, Option<&Enter>),
        (leaving, entering): (Option<Leave>, Option<&Enter>),
    ) -> Result<(Option<Leave>, Entered), BatchError> {
        let (mut leave, mut own) = (leave, enter.is_some());
        match (leaving, enter, leave) {
            (None, ..) => {}
            // A subtree that an earlier operation entered if it was
            // absent, with no leave before it, is the batch's own or the
            // transaction's, and the batch cannot say which: whether
            // there is one to leave.
            (
                Some(_),
                Some(Enter {
                    if_absent: true, ..
                }),
                None,
            ) => {
                return Err(BatchError::LeavesIfEntered { subtree });
            }
            // After a clear such an enter found the transaction's subtree,
            // which the clear left entered. The leave is of that one, and
            // a drop of it leaves nothing that was entered.
            (
                Some(leaving),
                Some(Enter {
                    if_absent: true, ..
                }),
                Some(Leave::Clear),
            ) => (leave, own) = (Some(leaving), leaving == Leave::Clear),
            // A subtree that an earlier operation entered as one that is
            // new, or if it was absent after a drop, is the batch's own: a
            // drop takes that enter back, and the transaction has none to
            // leave.
            (Some(leaving), Some(_), _) => own = leaving == Leave::Clear,
            (Some(_), None, Some(Leave::Drop)) => {
                return Err(BatchError::NotEntered { subtree });
            }
            (Some(leaving), None, _) => leave = Some(leaving),
        }
        match (entering, own) {
            (None, true) => Ok((leave, Entered::Before)),
            (None, false) => Ok((leave, Entered::No)),
            // An enter of a subtree that may be entered finds the one the
            // batch entered, and is satisfied where the two state one tree
            // type. Their host is the one statement the batch has of it.
            (
                Some(
                    entering @ Enter {
                        if_absent: true, ..
                    },
                ),
                true,
            ) => match enter {
                Some(held) if held.tree_type != entering.tree_type => {
                    Err(other_type(subtree, held, entering))
                }
                _ => Ok((leave, Entered::Before)),
            },
            // A clear leaves the subtree entered, as an enter does.
            (Some(_), true) => Err(BatchError::Entered { subtree }),
            // The subtree that a clear leaves entered is the
            // transaction's, which judges an enter of a subtree that may
            // be entered against it.
            (
                Some(Enter {
                    if_absent: false, ..
                }),
                false,
            ) if leave == Some(Leave::Clear) => Err(BatchError::Entered { subtree }),
            (Some(_), false) => Ok((leave, Entered::Now)),
        }
    }

    /// Makes the statements of the operation a part of what the operations
    /// before came to.
    fn fold(&mut self, subtree: Subtree) -> Result<(), BatchError> {
        let (leave, entered) = self.outcome(subtree)?;
        let entering = self.entering.take();
        self.leaving = None;
        self.leave = leave;
        match entered {
            Entered::No => self.enter = None,
            Entered::Before => {}
            Entered::Now => self.enter = entering,
        }
        Ok(())
    }

    fn is_blank(&self) -> bool {
        self.leave.is_none()
            && self.enter.is_none()
            && self.leaving.is_none()
            && self.entering.is_none()
    }
}

pub(super) struct Root {
    generation: u64,
    pub(super) key: Option<Box<[u8]>>,
}

/// A move of a subtree, as it was stated.
pub(super) struct Move {
    generation: u64,
    pub(super) from: Subtree,
    pub(super) to: Subtree,
    pub(super) read: Option<NodeHandle>,
    pub(super) below: Vec<(Subtree, Subtree)>,
}

impl Move {
    fn sources(&self) -> impl Iterator<Item = Subtree> + '_ {
        std::iter::once(self.from).chain(self.below.iter().map(|(from, _)| *from))
    }

    fn targets(&self) -> impl Iterator<Item = Subtree> + '_ {
        std::iter::once(self.to).chain(self.below.iter().map(|(_, to)| *to))
    }
}

pub(super) struct Host {
    generation: u64,
    pub(super) order: u64,
    pub(super) hosted: Vec<Subtree>,
    /// What the statement of an earlier operation said that this one took
    /// the place of: those hung below the element before this operation.
    replaced: Vec<Subtree>,
}

pub(super) struct Staged<C> {
    /// The operation generation that calls are stamped with.
    generation: u64,
    /// Counts the statements whose order is kept: enters and hosts.
    order: u64,
    /// The outcome of each key, by column in [`ORDER`], subtree and key.
    pub(super) columns: [BTreeMap<Subtree, Keys<C>>; 4],
    /// The root each Merk was last said to have.
    pub(super) roots: BTreeMap<Subtree, Root>,
    /// The Merks each element was last said to host, by the element's
    /// subtree and key.
    pub(super) hosts: BTreeMap<Subtree, BTreeMap<Box<[u8]>, Host>>,
    pub(super) subtrees: BTreeMap<Subtree, SubtreeState>,
    /// The moves, in the order they were stated.
    pub(super) moves: Vec<Move>,
    /// The first call that no outcome can stand for.
    pub(super) refused: Option<BatchError>,
}

impl<C> Default for Staged<C> {
    fn default() -> Self {
        Self {
            generation: 0,
            order: 0,
            columns: Default::default(),
            roots: BTreeMap::new(),
            hosts: BTreeMap::new(),
            subtrees: BTreeMap::new(),
            moves: Vec::new(),
            refused: None,
        }
    }
}

impl<C> Staged<C> {
    /// How many keys have an outcome, in a batch that is settled.
    pub(super) fn count(&self) -> usize {
        let keys = self.columns.iter().flat_map(BTreeMap::values);
        keys.map(BTreeMap::len).sum()
    }

    /// How many keys have an outcome once the operation the batch is in is
    /// settled.
    pub(super) fn len(&self) -> usize {
        let (leaving, below) = self.leaving();
        let generation = self.generation;
        let stays = |column: Column, subtree: &Subtree, keys: &Keys<C>| {
            let left = |(left, leave): &(Subtree, Leave)| {
                left == subtree && (*leave == Leave::Drop || column == Column::Data)
            };
            match below.contains(subtree) || leaving.iter().any(left) {
                true => (keys.values().filter(|entry| entry.generation >= generation)).count(),
                false => keys.len(),
            }
        };
        let columns = ORDER.into_iter().zip(&self.columns);
        let keys = columns.flat_map(|(column, held)| held.iter().map(move |held| (column, held)));
        keys.map(|(column, (subtree, keys))| stays(column, subtree, keys))
            .sum()
    }

    pub(super) fn is_empty(&self) -> bool {
        let (_, below) = self.leaving();
        let generation = self.generation;
        let blank = |(subtree, state): (&Subtree, &SubtreeState)| match below.contains(subtree) {
            // What earlier operations said of a subtree below one that
            // leaves goes with it.
            true if state.at < generation => state.outcome(*subtree).is_ok(),
            true => state.outcome_below(*subtree).is_ok() && state.entering.is_none(),
            false => matches!(state.outcome(*subtree), Ok((None, Entered::No))),
        };
        let nothing = self.len() == 0 && self.moves.is_empty();
        self.refused.is_none() && nothing && self.subtrees.iter().all(blank)
    }

    pub(super) fn next_operation(&mut self) {
        self.settle();
        self.generation += 1;
    }

    fn refuse(&mut self, error: BatchError) {
        self.refused.get_or_insert(error);
    }

    fn next_order(&mut self) -> u64 {
        self.order += 1;
        self.order
    }

    fn insert(&mut self, unit: Unit, key: Box<[u8]>, write: Write, tag: C) {
        let generation = self.generation;
        let entry = Entry {
            generation,
            write,
            tag,
        };
        let keys = self.columns[slot(unit.column)].entry(unit.subtree);
        keys.or_default().insert(key, entry);
    }

    /// A put always stands: it takes the place of whatever the batch holds
    /// for the key.
    pub(super) fn put(&mut self, unit: Unit, key: &[u8], value: &[u8], tag: C) {
        self.insert(unit, key.into(), Write::Record(value.into()), tag);
    }

    pub(super) fn put_node(&mut self, subtree: Subtree, put: &NodePut<'_>, tag: C) {
        if let Hosted::Set(hosted) = put.hosted {
            self.host(subtree, put.key, hosted);
        }
        self.put_staged(subtree, StagedNode::of(put), tag);
    }

    fn put_staged(&mut self, subtree: Subtree, mut node: StagedNode, tag: C) {
        let generation = self.generation;
        let held = (self.columns[DATA].get_mut(&subtree)).and_then(|keys| keys.remove(&*node.key));
        // The value the node has in the batch before the put, with the
        // generation that stated it; whether an earlier operation removed
        // the node, so that the put makes a new one; and whether the node
        // has no value to keep.
        let (mut value, mut removed, mut lost) = (None, false, false);
        match held.map(|held| (held.generation, held.write)) {
            None | Some((_, Write::Record(_))) => {}
            Some((
                _,
                Write::Node {
                    node,
                    stated,
                    lost: was,
                },
            )) => {
                value = node.value.map(|value| (value, stated));
                lost = was;
            }
            // The put wins over a delete of its own operation, so the
            // node is what it was before that delete.
            Some((at, Write::Remove { under, .. })) if at == generation => match under {
                Under::Transaction => {}
                Under::Value(held, stated) => value = Some((held, stated)),
                Under::Removed => (removed, lost) = (true, true),
            },
            Some((_, Write::Remove { .. })) => (removed, lost) = (true, true),
        }
        let mut stated = generation;
        match (&node.value, value) {
            (Some(_), _) => lost = false,
            (None, Some((value, at))) => (node.value, stated, lost) = (Some(value), at, false),
            (None, None) => {}
        }
        // A new node hosts nothing of what was said of its key before this
        // operation.
        if removed
            && let Some(hosts) = self.hosts.get_mut(&subtree)
            && (hosts.get(&*node.key)).is_some_and(|host| host.generation < generation)
        {
            hosts.remove(&*node.key);
        }
        let (key, node) = (node.key.clone(), Box::new(node));
        let write = Write::Node { node, stated, lost };
        self.insert(Unit::data(subtree), key, write, tag);
    }

    /// A delete stands where the batch holds nothing for the key, or an
    /// outcome of an earlier operation, which it takes the place of. An
    /// outcome of the same operation stays.
    pub(super) fn delete(&mut self, unit: Unit, key: &[u8], node: bool, tag: C) {
        let generation = self.generation;
        let held = match self.columns[slot(unit.column)].get_mut(&unit.subtree) {
            Some(keys) if (keys.get(key)).is_some_and(|held| held.generation >= generation) => {
                return;
            }
            Some(keys) => keys.remove(key),
            None => None,
        };
        let under = match held.map(|held| held.write) {
            Some(Write::Node { node, stated, lost }) => match node.value {
                Some(value) => Under::Value(value, stated),
                None if lost => Under::Removed,
                None => Under::Transaction,
            },
            Some(Write::Remove { .. }) => Under::Removed,
            Some(Write::Record(_)) | None => Under::Transaction,
        };
        self.insert(unit, key.into(), Write::Remove { node, under }, tag);
    }

    /// The root of a Merk is held as a record is: a key is a put, and no
    /// root is a delete.
    pub(super) fn set_root(&mut self, subtree: Subtree, root: Option<&[u8]>) {
        let generation = self.generation;
        let stays = |held: &Root| root.is_none() && held.generation >= generation;
        if !self.roots.get(&subtree).is_some_and(stays) {
            let key = root.map(Into::into);
            self.roots.insert(subtree, Root { generation, key });
        }
    }

    /// The later statement of what an element hosts stands.
    pub(super) fn host(&mut self, subtree: Subtree, key: &[u8], hosted: &[Subtree]) {
        let generation = self.generation;
        let order = self.next_order();
        let removed = self.removed_before(subtree, key);
        let hosts = self.hosts.entry(subtree).or_default();
        let replaced = match hosts.remove(key) {
            // What was said before this operation of an element that an
            // earlier one deleted is said of no element.
            Some(held) if removed && held.generation < generation => Vec::new(),
            Some(held) if held.generation < generation => held.hosted,
            Some(held) => held.replaced,
            None => Vec::new(),
        };
        let host = Host {
            generation,
            order,
            hosted: hosted.to_vec(),
            replaced,
        };
        hosts.insert(key.into(), host);
    }

    /// Whether an earlier operation removed the node `key` of `subtree`,
    /// and none has put it since: the batch holds that delete, or a delete
    /// of this operation that took its place.
    fn removed_before(&self, subtree: Subtree, key: &[u8]) -> bool {
        self.held(subtree, key)
            .is_some_and(|held| match held.write {
                Write::Remove { .. } if held.generation < self.generation => true,
                Write::Remove { ref under, .. } => matches!(under, Under::Removed),
                _ => false,
            })
    }

    /// The outcome the batch holds for `key` in the data column of `subtree`.
    pub(super) fn held(&self, subtree: Subtree, key: &[u8]) -> Option<&Entry<C>> {
        self.columns[DATA].get(&subtree)?.get(key)
    }

    /// What was said of each element that the batch does not delete, in
    /// the order it was said.
    pub(super) fn hosts_in_order(&self) -> Vec<(Subtree, &[u8], &Host)> {
        let deleted = |subtree: Subtree, key: &[u8]| {
            let held = self.held(subtree, key);
            held.is_some_and(|held| matches!(held.write, Write::Remove { .. }))
        };
        let mut hosts: Vec<_> = (self.hosts.iter())
            .flat_map(|(subtree, hosts)| {
                (hosts.iter()).map(move |(key, host)| (*subtree, &**key, host))
            })
            .filter(|(subtree, key, _)| !deleted(*subtree, key))
            .collect();
        hosts.sort_by_key(|(.., host)| host.order);
        hosts
    }

    /// The state of `subtree`, with room for a statement of this
    /// operation.
    fn current(&mut self, subtree: Subtree) -> &mut SubtreeState {
        let state = self.subtrees.entry(subtree).or_default();
        if state.at < self.generation {
            if let Err(refusal) = state.fold(subtree) {
                self.refused.get_or_insert(refusal);
            }
            state.at = self.generation;
        }
        state
    }

    /// Two enters of one operation are refused, unless one of them is of
    /// a subtree that may be entered: that one is satisfied by the other,
    /// where the two state one tree type. The enter of a new subtree
    /// stands over it, and of two of its own kind the first.
    pub(super) fn enter(&mut self, subtree: Subtree, tree_type: &[u8], if_absent: bool) {
        let enter = Enter {
            tree_type: tree_type.into(),
            order: self.next_order(),
            if_absent,
        };
        let state = self.current(subtree);
        let (stands, refusal) = match state.entering.take() {
            None => (enter, None),
            Some(held) if held.if_absent || enter.if_absent => {
                let differs = held.tree_type != enter.tree_type;
                let refusal = differs.then(|| other_type(subtree, &held, &enter));
                match held.if_absent && !enter.if_absent {
                    true => (enter, refusal),
                    false => (held, refusal),
                }
            }
            Some(_) => (enter, Some(BatchError::SubtreeTwice { subtree })),
        };
        state.entering = Some(stands);
        if let Some(refusal) = refusal {
            self.refuse(refusal);
        }
    }

    /// What the batch holds beside its moves is judged when it is
    /// finished ([`Self::moved`]).
    pub(super) fn move_subtree(
        &mut self,
        from: Subtree,
        to: Subtree,
        read: Option<NodeHandle>,
        below: &[(Subtree, Subtree)],
    ) {
        let generation = self.generation;
        self.moves.push(Move {
            generation,
            from,
            to,
            read,
            below: below.to_vec(),
        });
    }

    /// The refusal of what the batch holds beside its moves, if it holds
    /// anything it does not hold beside them: a move of another operation
    /// than the first one's, which would not be made together with it
    /// ([`BatchError::Moved`]); a prefix that its moves name twice as one
    /// a subtree is taken from, or twice as one a subtree goes to
    /// ([`BatchError::SubtreeTwice`]); or, under a prefix that a move
    /// names on either side, an outcome of a key, a root, an enter of
    /// either kind, a clear, or a drop of anything but the subtree that
    /// is entered where one is moved to and that no move takes away
    /// ([`BatchError::Moved`]). That drop is one that the operation of
    /// the moves states, or an earlier one: the batch applies its drops
    /// before its moves, and a drop that a later operation states is of
    /// the subtree that was moved there. It is asked of a batch that is
    /// settled.
    fn moved(&self) -> Option<BatchError> {
        let first = self.moves.first()?;
        let apart = |moving: &&Move| moving.generation != first.generation;
        if let Some(later) = self.moves.iter().find(apart) {
            return Some(BatchError::Moved {
                subtree: later.from,
            });
        }
        let sources: Vec<Subtree> = self.moves.iter().flat_map(Move::sources).collect();
        let targets: Vec<Subtree> = self.moves.iter().flat_map(Move::targets).collect();
        let twice = |names: &[Subtree]| {
            let again = |(at, name): (usize, &Subtree)| names[..at].contains(name).then_some(*name);
            names.iter().enumerate().find_map(again)
        };
        if let Some(subtree) = twice(&sources).or_else(|| twice(&targets)) {
            return Some(BatchError::SubtreeTwice { subtree });
        }
        let named = sources.iter().chain(&targets).copied();
        let refused = named.into_iter().find(|named| {
            let held = |column: &BTreeMap<Subtree, Keys<C>>| {
                column.get(named).is_some_and(|keys| !keys.is_empty())
            };
            let makes_room = |state: &SubtreeState| {
                let dropped = state.leave == Some(Leave::Drop) && state.enter.is_none();
                dropped && !sources.contains(named) && state.at <= first.generation
            };
            let stated = (self.subtrees.get(named)).is_some_and(|state| !makes_room(state));
            stated || self.roots.contains_key(named) || self.columns.iter().any(held)
        });
        refused.map(|subtree| BatchError::Moved { subtree })
    }

    /// What the leave takes the place of is taken out when the operation
    /// ends ([`Self::settle`]).
    pub(super) fn leave(&mut self, subtree: Subtree, leave: Leave) {
        if self.current(subtree).leaving.replace(leave).is_some() {
            self.refuse(BatchError::SubtreeTwice { subtree });
        }
    }

    /// The subtrees that leave in this operation, and every subtree below
    /// one of them, at any depth.
    fn leaving(&self) -> (Vec<(Subtree, Leave)>, Vec<Subtree>) {
        let leaving: Vec<(Subtree, Leave)> = (self.subtrees.iter())
            .filter(|(_, state)| state.at == self.generation)
            .filter_map(|(subtree, state)| Some((*subtree, state.leaving?)))
            .collect();
        let mut reached: Vec<Subtree> = leaving.iter().map(|(subtree, _)| *subtree).collect();
        let mut below = Vec::new();
        let mut at = 0;
        while at < reached.len() {
            let host = reached[at];
            // What hangs there by what was said of its elements, and what
            // hung there when the batch said it leaves.
            let left = (self.subtrees.iter())
                .filter(|(_, state)| state.parent == Some(host))
                .map(|(subtree, _)| *subtree);
            for hosted in self.hung_on(host).into_iter().chain(left) {
                if !below.contains(&hosted) {
                    below.push(hosted);
                }
                if !reached.contains(&hosted) {
                    reached.push(hosted);
                }
            }
            at += 1;
        }
        (leaving, below)
    }

    /// Takes out what the drops and the clears of this operation take the
    /// place of: what earlier operations staged in each subtree that
    /// leaves, every column for a drop and the data column for a clear,
    /// and in every subtree below one, with what they said of it as a
    /// whole. What this operation staged stays. It runs when the operation
    /// ends, when everything the operation says is known.
    fn settle(&mut self) {
        let generation = self.generation;
        let (leaving, below) = self.leaving();
        let parents: Vec<_> = (leaving.iter())
            .map(|(subtree, _)| (*subtree, self.hangs_in(*subtree)))
            .collect();
        for below in &below {
            self.cut(*below, &ORDER);
            // A subtree below goes with the one that leaves, and is not
            // named beside it: not by an earlier operation, and not by a
            // leave of this one. What was said of it is taken together a
            // last time, for what it refuses. An enter of this operation
            // is of a new subtree, and stays.
            if let Some(state) = self.subtrees.get_mut(below) {
                let said = match state.at < generation {
                    true => state.fold(*below),
                    false => state.outcome_below(*below),
                };
                if let Err(refusal) = said {
                    self.refused.get_or_insert(refusal);
                }
                (state.leave, state.enter, state.parent) = (None, None, None);
                state.leaving = None;
                if state.is_blank() {
                    self.subtrees.remove(below);
                }
            }
        }
        for (subtree, parent) in parents {
            if let Some(state) = self.subtrees.get_mut(&subtree)
                && !below.contains(&subtree)
            {
                state.parent = state.parent.or(parent);
            }
        }
        for (subtree, leave) in leaving {
            match leave {
                Leave::Drop => self.cut(subtree, &ORDER),
                Leave::Clear => self.cut(subtree, &[Column::Data]),
            }
        }
    }

    /// The subtree that `subtree` hangs in, by what was said of the
    /// elements.
    fn hangs_in(&self, subtree: Subtree) -> Option<Subtree> {
        let mut hosts = self.hosts.keys().copied();
        hosts.find(|host| self.hung_on(*host).contains(&subtree))
    }

    /// Every subtree that hung on an element of `subtree` before this
    /// operation, or that the operation says hangs on one. An element that
    /// an earlier operation deleted has nothing hanging on it.
    fn hung_on(&self, subtree: Subtree) -> Vec<Subtree> {
        let generation = self.generation;
        let deleted = |key: &[u8], host: &Host| match self.held(subtree, key) {
            Some(Entry {
                generation: at,
                write: Write::Remove { under, .. },
                ..
            }) => {
                *at < generation
                    || (matches!(under, Under::Removed) && host.generation < generation)
            }
            _ => false,
        };
        let hosts = self.hosts.get(&subtree).into_iter().flatten();
        hosts
            .filter(|(key, host)| !deleted(key, host))
            .flat_map(|(_, host)| {
                let before = (host.generation == generation).then_some(&host.replaced);
                host.hosted
                    .iter()
                    .chain(before.into_iter().flatten())
                    .copied()
            })
            .collect()
    }

    /// Takes out of `columns` of `subtree` what earlier operations staged.
    fn cut(&mut self, subtree: Subtree, columns: &[Column]) {
        let generation = self.generation;
        for column in columns {
            let held = &mut self.columns[slot(*column)];
            if let Some(keys) = held.get_mut(&subtree) {
                keys.retain(|_, entry| entry.generation >= generation);
                if keys.is_empty() {
                    held.remove(&subtree);
                }
            }
            if *column != Column::Data {
                continue;
            }
            // A node of this operation keeps no value that an earlier one
            // stated: the leave comes before it, in whichever order the
            // two were made.
            let keys = self.columns[DATA].get_mut(&subtree);
            for entry in keys.into_iter().flat_map(BTreeMap::values_mut) {
                match &mut entry.write {
                    Write::Node { node, stated, .. } if *stated < generation => node.value = None,
                    Write::Remove { under, .. } => *under = Under::Transaction,
                    Write::Node { .. } | Write::Record(_) => {}
                }
            }
            if self
                .roots
                .get(&subtree)
                .is_some_and(|root| root.generation < generation)
            {
                self.roots.remove(&subtree);
            }
            if let Some(hosts) = self.hosts.get_mut(&subtree) {
                hosts.retain(|_, host| host.generation >= generation);
                if hosts.is_empty() {
                    self.hosts.remove(&subtree);
                }
            }
        }
    }

    /// The batch with nothing left to settle: what it says of each subtree
    /// as a whole is what all its operations come to. With it comes the
    /// first call that no outcome can stand for.
    pub(super) fn finish(mut self) -> (Self, Option<BatchError>) {
        self.settle();
        let mut refused = self.refused.take();
        for (subtree, state) in &mut self.subtrees {
            if let Err(refusal) = state.fold(*subtree) {
                refused.get_or_insert(refusal);
            }
        }
        self.subtrees.retain(|_, state| !state.is_blank());
        // A node that stands and keeps the value of a node that an earlier
        // operation removed has no value.
        let lost = |entry: &Entry<C>| matches!(entry.write, Write::Node { lost: true, .. });
        let lost = self.columns[DATA]
            .iter()
            .find(|(_, keys)| keys.values().any(lost));
        let lost = lost.map(|(subtree, _)| BatchError::NoValue { subtree: *subtree });
        let moved = self.moved();
        (self, refused.or(lost).or(moved))
    }

    /// Makes every call that `other` holds an outcome of again, in the
    /// operation this batch is in: what it says of the elements and of the
    /// subtrees first, then the outcomes of its keys.
    pub(super) fn merge(&mut self, other: Self) {
        let (other, refused) = other.finish();
        if let Some(refusal) = refused {
            self.refuse(refusal);
        }
        // What `other` said of an element it deletes is no outcome of it.
        for (subtree, key, host) in other.hosts_in_order() {
            self.host(subtree, key, &host.hosted);
        }
        let mut enters = Vec::new();
        for (subtree, state) in other.subtrees {
            if let Some(leave) = state.leave {
                self.leave(subtree, leave);
                let parent = &mut self.current(subtree).parent;
                *parent = parent.or(state.parent);
            }
            enters.extend(state.enter.map(|enter| (enter.order, subtree, enter)));
        }
        enters.sort_by_key(|(order, ..)| *order);
        for (_, subtree, enter) in enters {
            self.enter(subtree, &enter.tree_type, enter.if_absent);
        }
        for (subtree, root) in other.roots {
            self.set_root(subtree, root.key.as_deref());
        }
        for moving in other.moves {
            self.move_subtree(moving.from, moving.to, moving.read, &moving.below);
        }
        for (column, held) in ORDER.into_iter().zip(other.columns) {
            for (subtree, keys) in held {
                let unit = Unit::new(column, subtree);
                for (key, entry) in keys {
                    match entry.write {
                        Write::Record(value) => self.put(unit, &key, &value, entry.tag),
                        Write::Node { node, .. } => self.put_staged(subtree, *node, entry.tag),
                        Write::Remove { node, .. } => self.delete(unit, &key, node, entry.tag),
                    }
                }
            }
        }
    }

    /// Makes `other` a later operation of this batch, and whatever follows
    /// later still.
    pub(super) fn merge_overwriting(&mut self, other: Self) {
        self.next_operation();
        self.merge(other);
        self.next_operation();
    }
}
