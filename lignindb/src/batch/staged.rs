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
    /// A node whose value the batch held.
    Value(Box<[u8]>),
    /// The delete of an earlier operation: the node has no value.
    Removed,
}

/// The one outcome a batch holds for a key.
pub(super) enum Write {
    /// A put of a flat record.
    Record(Box<[u8]>),
    /// A put of a node.
    Node(Box<StagedNode>),
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
    generation: u64,
    pub(super) order: u64,
}

/// What the batch does to one subtree as a whole: to the subtree as the
/// transaction has it, and then the enter of a subtree that is new.
#[derive(Default)]
pub(super) struct SubtreeState {
    pub(super) leave: Option<(Leave, u64)>,
    pub(super) enter: Option<Enter>,
}

pub(super) struct Root {
    generation: u64,
    pub(super) key: Option<Box<[u8]>>,
}

pub(super) struct Host {
    generation: u64,
    pub(super) order: u64,
    pub(super) hosted: Vec<Subtree>,
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
    pub(super) fn len(&self) -> usize {
        let keys = self.columns.iter().flat_map(BTreeMap::values);
        keys.map(BTreeMap::len).sum()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.refused.is_none()
            && self.subtrees.is_empty()
            && self.columns.iter().all(BTreeMap::is_empty)
    }

    pub(super) fn next_operation(&mut self) {
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
        if node.value.is_none() {
            match self.kept(subtree, &node.key) {
                Ok(value) => node.value = value,
                Err(error) => self.refuse(error),
            }
        }
        let key = node.key.clone();
        self.insert(Unit::data(subtree), key, Write::Node(Box::new(node)), tag);
    }

    /// The value that a node which is put under `key` keeps: the value the
    /// node has in the batch as it is, or `None` for the value the
    /// transaction has when the batch is applied.
    fn kept(&mut self, subtree: Subtree, key: &[u8]) -> Result<Option<Box<[u8]>>, BatchError> {
        let generation = self.generation;
        let held = (self.columns[DATA].get_mut(&subtree)).and_then(|keys| keys.remove(key));
        let Some(held) = held else {
            return Ok(None);
        };
        match held.write {
            Write::Node(node) => Ok(node.value),
            Write::Record(_) => Ok(None),
            // The put wins over a delete of its own operation, so it keeps
            // what the node had before that delete.
            Write::Remove { under, .. } if held.generation == generation => match under {
                Under::Transaction => Ok(None),
                Under::Value(value) => Ok(Some(value)),
                Under::Removed => Err(BatchError::NoValue { subtree }),
            },
            // An earlier operation removed the node: it has no value.
            Write::Remove { .. } => Err(BatchError::NoValue { subtree }),
        }
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
        let under = match held {
            Some(None) => return,
            Some(Some(Entry {
                write: Write::Node(node),
                ..
            })) => node.value.map_or(Under::Transaction, Under::Value),
            Some(Some(Entry {
                write: Write::Remove { .. },
                ..
            })) => Under::Removed,
            _ => Under::Transaction,
        };
        if node {
            // What the node hosted goes with it: a node that is put under
            // the key afterwards hosts what that put says.
            if let Some(hosts) = self.hosts.get_mut(&unit.subtree) {
                hosts.remove(key);
            }
        }
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
        let host = Host {
            generation: self.generation,
            order: self.next_order(),
            hosted: hosted.to_vec(),
        };
        self.hosts
            .entry(subtree)
            .or_default()
            .insert(key.into(), host);
    }

    pub(super) fn enter(&mut self, subtree: Subtree, tree_type: &[u8]) {
        let generation = self.generation;
        let state = self.subtrees.get(&subtree);
        let entered = state.and_then(|state| state.enter.as_ref());
        let cleared = state.is_some_and(|state| matches!(state.leave, Some((Leave::Clear, _))));
        let refusal = match entered {
            Some(enter) if enter.generation == generation => {
                Some(BatchError::SubtreeTwice { subtree })
            }
            // The batch entered it, or cleared it, which leaves it entered.
            Some(_) => Some(BatchError::Entered { subtree }),
            None if cleared => Some(BatchError::Entered { subtree }),
            None => None,
        };
        if let Some(refusal) = refusal {
            return self.refuse(refusal);
        }
        let enter = Enter {
            tree_type: tree_type.into(),
            generation,
            order: self.next_order(),
        };
        self.subtrees.entry(subtree).or_default().enter = Some(enter);
    }

    pub(super) fn leave(&mut self, subtree: Subtree, leave: Leave) {
        let generation = self.generation;
        let state = self.subtrees.get(&subtree);
        let entered = (state.and_then(|state| state.enter.as_ref())).map(|enter| enter.generation);
        let left = state.and_then(|state| state.leave);
        // A subtree that an earlier operation entered is the batch's own:
        // the transaction has none to leave.
        let own = entered.is_some_and(|at| at < generation);
        let left = match (leave, left) {
            _ if own => Ok(left),
            (_, Some((_, at))) if at == generation => Err(BatchError::SubtreeTwice { subtree }),
            (_, Some((Leave::Drop, _))) => Err(BatchError::NotEntered { subtree }),
            // A clear leaves the subtree entered, so the same operation
            // does not enter it.
            (Leave::Clear, _) if entered.is_some() => Err(BatchError::Entered { subtree }),
            _ => Ok(Some((leave, generation))),
        };
        let left = match left {
            Ok(left) => left,
            Err(refusal) => return self.refuse(refusal),
        };
        self.purge(subtree, leave);
        let state = self.subtrees.entry(subtree).or_default();
        state.leave = left;
        if own && leave == Leave::Drop {
            state.enter = None;
        }
        if state.leave.is_none() && state.enter.is_none() {
            self.subtrees.remove(&subtree);
        }
    }

    /// Takes out what earlier operations staged in `subtree` and below it:
    /// a drop takes every column of the subtree, a clear its data column,
    /// and both take every subtree below.
    fn purge(&mut self, subtree: Subtree, leave: Leave) {
        let generation = self.generation;
        for below in self.below(subtree) {
            self.cut(below, &ORDER);
            if let Some(state) = self.subtrees.get_mut(&below) {
                // A subtree below goes with the one that leaves, and is
                // not named beside it.
                if state
                    .enter
                    .as_ref()
                    .is_some_and(|enter| enter.generation < generation)
                {
                    state.enter = None;
                }
                if state.leave.is_some_and(|(_, at)| at < generation) {
                    state.leave = None;
                }
                if state.leave.is_none() && state.enter.is_none() {
                    self.subtrees.remove(&below);
                }
            }
        }
        match leave {
            Leave::Drop => self.cut(subtree, &ORDER),
            Leave::Clear => self.cut(subtree, &[Column::Data]),
        }
    }

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

    /// Every subtree that the batch was told hangs below `subtree`, at any
    /// depth.
    fn below(&self, subtree: Subtree) -> Vec<Subtree> {
        let mut below = vec![subtree];
        let mut at = 0;
        while at < below.len() {
            let hosts = self.hosts.get(&below[at]);
            for hosted in hosts.into_iter().flat_map(BTreeMap::values) {
                for hosted in &hosted.hosted {
                    if !below.contains(hosted) {
                        below.push(*hosted);
                    }
                }
            }
            at += 1;
        }
        below.split_off(1)
    }

    /// Makes every call that `other` holds an outcome of again, in the
    /// operation this batch is in.
    pub(super) fn merge(&mut self, other: Self) {
        if let Some(refusal) = other.refused {
            self.refuse(refusal);
        }
        // What an element hosts is said first: a drop takes what hangs
        // below it as the batch knows it then.
        let mut hosts: Vec<_> = (other.hosts.into_iter())
            .flat_map(|(subtree, hosts)| hosts.into_iter().map(move |host| (subtree, host)))
            .collect();
        hosts.sort_by_key(|(_, (_, host))| host.order);
        for (subtree, (key, host)) in hosts {
            self.host(subtree, &key, &host.hosted);
        }
        let mut enters = Vec::new();
        for (subtree, state) in other.subtrees {
            if let Some((leave, _)) = state.leave {
                self.leave(subtree, leave);
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
                        Write::Node(node) => self.put_staged(subtree, *node, entry.tag),
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
