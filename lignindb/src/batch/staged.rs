//! What a batch holds, and how each call changes it.

use std::collections::BTreeMap;

use super::node::StagedNode;
use crate::txn::{BatchError, Column, Hosted, NodePut, Subtree, Unit};

/// The columns in the order a batch is iterated in, and so charged in
/// (`cost-model.md` B1): the order `StorageBatch` iterates its maps in.
pub(super) const ORDER: [Column; 4] = [Column::Meta, Column::Aux, Column::Roots, Column::Data];

const DATA: usize = 3;

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
        let (mut leave, mut own) = (self.leave, self.enter.is_some());
        match (self.leaving, own, leave) {
            (None, ..) => {}
            // A subtree that an earlier operation entered is the batch's
            // own: a drop takes that enter back, and the transaction has
            // none to leave.
            (Some(leaving), true, _) => own = leaving == Leave::Clear,
            (Some(_), false, Some(Leave::Drop)) => {
                return Err(BatchError::NotEntered { subtree });
            }
            (Some(leaving), false, _) => leave = Some(leaving),
        }
        match (&self.entering, own) {
            (None, true) => Ok((leave, Entered::Before)),
            (None, false) => Ok((leave, Entered::No)),
            // A clear leaves the subtree entered, as an enter does.
            (Some(_), true) => Err(BatchError::Entered { subtree }),
            (Some(_), false) if leave == Some(Leave::Clear) => Err(BatchError::Entered { subtree }),
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
        (ORDER.iter().zip(&self.columns))
            .flat_map(|(column, held)| {
                held.iter()
                    .map(move |(subtree, keys)| (*column, subtree, keys))
            })
            .map(|(column, subtree, keys)| stays(column, subtree, keys))
            .sum()
    }

    pub(super) fn is_empty(&self) -> bool {
        let (_, below) = self.leaving();
        let generation = self.generation;
        let blank = |(subtree, state): (&Subtree, &SubtreeState)| match below.contains(subtree) {
            // What earlier operations said of a subtree below one that
            // leaves goes with it.
            true if state.at < generation => state.outcome(*subtree).is_ok(),
            true => state.leaving.is_none() && state.entering.is_none(),
            false => matches!(state.outcome(*subtree), Ok((None, Entered::No))),
        };
        self.refused.is_none() && self.len() == 0 && self.subtrees.iter().all(blank)
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
        let column = &mut self.columns[slot(unit.column)];
        let held = column.get_mut(&unit.subtree);
        let held = held.and_then(|keys| match keys.get(key) {
            Some(held) if held.generation >= generation => Some(None),
            Some(_) => keys.remove(key).map(Some),
            None => None,
        });
        let under = match held.map(|held| held.map(|held| held.write)) {
            Some(None) => return,
            Some(Some(Write::Node { node, stated, lost })) => match node.value {
                Some(value) => Under::Value(value, stated),
                None if lost => Under::Removed,
                None => Under::Transaction,
            },
            Some(Some(Write::Remove { .. })) => Under::Removed,
            _ => Under::Transaction,
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
        let held = (self.columns[DATA].get(&subtree)).and_then(|keys| keys.get(key));
        held.is_some_and(|held| match held.write {
            Write::Remove { .. } if held.generation < self.generation => true,
            Write::Remove { ref under, .. } => matches!(under, Under::Removed),
            _ => false,
        })
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

    pub(super) fn enter(&mut self, subtree: Subtree, tree_type: &[u8]) {
        let enter = Enter {
            tree_type: tree_type.into(),
            order: self.next_order(),
        };
        if self.current(subtree).entering.replace(enter).is_some() {
            self.refuse(BatchError::SubtreeTwice { subtree });
        }
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
            // What hangs there by what was said of its elements, and what
            // hung there when the batch said it leaves.
            let left = (self.subtrees.iter())
                .filter(|(_, state)| state.parent == Some(reached[at]))
                .map(|(subtree, _)| *subtree);
            let hung: Vec<Subtree> = self.hung_on(reached[at]).into_iter().chain(left).collect();
            for hosted in hung {
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
        for below in below {
            self.cut(below, &ORDER);
            // A subtree below goes with the one that leaves, and is not
            // named beside it: what earlier operations said of it is
            // taken together a last time, for what it refuses.
            if let Some(state) = self.subtrees.get_mut(&below) {
                if state.at < generation
                    && let Err(refusal) = state.fold(below)
                {
                    self.refused.get_or_insert(refusal);
                }
                (state.leave, state.enter, state.parent) = (None, None, None);
                if state.is_blank() {
                    self.subtrees.remove(&below);
                }
            }
        }
        for (subtree, parent) in parents {
            if let Some(state) = self.subtrees.get_mut(&subtree) {
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
        let keys = self.columns[DATA].get(&subtree);
        let deleted = |key: &[u8], host: &Host| {
            let held = keys.and_then(|keys| keys.get(key));
            let earlier = held.is_some_and(|held| {
                matches!(held.write, Write::Remove { .. }) && held.generation < generation
            });
            earlier || (self.removed_before(subtree, key) && host.generation < generation)
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
        let lost = |keys: &Keys<C>| {
            let lost = |entry: &Entry<C>| matches!(entry.write, Write::Node { lost: true, .. });
            keys.values().any(lost)
        };
        let lost = self.columns[DATA].iter().find(|(_, keys)| lost(keys));
        let lost = lost.map(|(subtree, _)| BatchError::NoValue { subtree: *subtree });
        (self, refused.or(lost))
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
        let deleted = |subtree: &Subtree, key: &[u8]| {
            let held = (other.columns[DATA].get(subtree)).and_then(|keys| keys.get(key));
            held.is_some_and(|held| matches!(held.write, Write::Remove { .. }))
        };
        let mut hosts: Vec<_> = (other.hosts.into_iter())
            .flat_map(|(subtree, hosts)| hosts.into_iter().map(move |host| (subtree, host)))
            .filter(|(subtree, (key, _))| !deleted(subtree, key))
            .collect();
        hosts.sort_by_key(|(_, (_, host))| host.order);
        for (subtree, (key, host)) in hosts {
            self.host(subtree, &key, &host.hosted);
        }
        let mut enters = Vec::new();
        for (subtree, state) in other.subtrees {
            if let Some(leave) = state.leave {
                self.leave(subtree, leave);
                let parent = &mut self.current(subtree).parent;
                *parent = parent.or(state.parent);
            }
            enters.extend(
                state
                    .enter
                    .map(|enter| (enter.order, subtree, enter.tree_type)),
            );
        }
        enters.sort_by_key(|(order, ..)| *order);
        for (_, subtree, tree_type) in enters {
            self.enter(subtree, &tree_type);
        }
        for (subtree, root) in other.roots {
            self.set_root(subtree, root.key.as_deref());
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
