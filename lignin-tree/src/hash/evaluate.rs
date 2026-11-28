//! Evaluation: one marked node at a time, and one page at a time.

use lignin_format::{
    tree_page::{Hash, HashField, Side},
    value_log::ValuePlace,
    CellAddress,
};

use super::{
    error::HashError,
    rules::{HashRules, ValueReader},
    stale::Stale,
    Counters, Hashes,
};
use crate::{fragment::Pages, node::NodeRef};

/// Which pointer of a node names another node.
#[derive(Debug, Clone, Copy)]
enum Role {
    /// The child on a side.
    Child(Side),
    /// The root of the Merk the node hosts at a position.
    Hosted(usize),
}

/// A node whose hash a stored hash of another node is made from.
#[derive(Debug, Clone, Copy)]
struct Dep {
    role: Role,
    at: CellAddress,
}

/// What the nodes that are open keep, in lists they share: a node's part of
/// a list lies on top of the parts of the nodes it was reached from, and is
/// cut off when the node is done. So an evaluation allocates for its lists,
/// not for each node.
#[derive(Debug, Default)]
struct Lists {
    /// What each open node waits for.
    deps: Vec<Dep>,
    /// The hashes of the first of a node's `deps`, in their order.
    hashes: Vec<Hash>,
    /// The roots a host's value hash is made from, while it is made.
    hosted: Vec<Option<Hash>>,
}

/// One node whose hashes are being made, as its cell was read.
#[derive(Debug)]
struct Frame {
    at: CellAddress,
    fields: Stale,
    /// Where the node's part of [`Lists::deps`] starts.
    deps: usize,
    /// Where the node's part of [`Lists::hashes`] starts.
    hashes: usize,
    /// The place of the value, if a value hash is made and the value is not
    /// in the cell.
    place: Option<ValuePlace>,
    /// How many Merks the node hosts, if a value hash is made.
    hosted: usize,
}

/// The hashes that were made for one cell.
#[derive(Debug, Default)]
struct Made {
    value: Option<Hash>,
    kv: Option<Hash>,
    left: Option<Hash>,
    right: Option<Hash>,
}

impl Made {
    fn hashes(&self) -> impl Iterator<Item = (HashField, &Hash)> {
        [
            (HashField::Value, &self.value),
            (HashField::KeyValue, &self.kv),
            (HashField::Child(Side::Left), &self.left),
            (HashField::Child(Side::Right), &self.right),
        ]
        .into_iter()
        .filter_map(|(field, hash)| Some((field, hash.as_ref()?)))
    }
}

/// Reads the node at `at` in one step.
fn read_node<P: Pages, T>(
    pages: &mut P,
    at: CellAddress,
    read: impl FnOnce(NodeRef<'_>) -> Result<T, HashError>,
) -> Result<T, HashError> {
    let result = pages.read(at.page(), |page| {
        read(
            page.node(at.slot())
                .ok_or(HashError::NoNode { address: at })?,
        )
    });
    result.map_err(HashError::Pages)?
}

/// Reads what the hashes `fields` of the cell at `at` are made from, and
/// puts the nodes it waits for on top of `lists`. A field that names the
/// hash of a child the node does not have is an error if `strict`, and is
/// left out otherwise.
fn gather<P: Pages>(
    pages: &mut P,
    lists: &mut Lists,
    at: CellAddress,
    fields: Stale,
    strict: bool,
) -> Result<Frame, HashError> {
    if fields.has(HashField::Value) && pages.host_is_behind(at) {
        return Err(HashError::HostBehind { host: at });
    }
    let mut frame = Frame {
        at,
        fields,
        deps: lists.deps.len(),
        hashes: lists.hashes.len(),
        place: None,
        hosted: 0,
    };
    read_node(pages, at, |node| {
        for side in Side::BOTH {
            let field = HashField::Child(side);
            if !fields.has(field) {
                continue;
            }
            match node.link(side) {
                Some(link) => lists.deps.push(Dep {
                    role: Role::Child(side),
                    at: link.child,
                }),
                None if strict => return Err(HashError::NoHash { node: at, field }),
                None => frame.fields = frame.fields.without(field),
            }
        }
        if fields.has(HashField::Value) {
            frame.place = node.value().place();
            let roots = node.hosted_roots();
            frame.hosted = roots.len();
            let named = roots.enumerate().filter_map(|(position, root)| {
                let role = Role::Hosted(position);
                Some(Dep { role, at: root? })
            });
            lists.deps.extend(named);
        }
        Ok(frame)
    })
}

/// Reads the value of `frame`'s node into `into`, if a value hash is made
/// from a value that is not in its cell.
fn fetch<V: ValueReader>(
    values: &mut V,
    frame: &Frame,
    into: &mut Vec<u8>,
) -> Result<(), HashError> {
    into.clear();
    let Some(place) = frame.place else {
        return Ok(());
    };
    values
        .read(place, frame.at.page(), into)
        .map_err(HashError::Value)?;
    if into.len() != place.value_len() as usize {
        return Err(HashError::ValueLen {
            node: frame.at,
            len: place.value_len(),
            read: into.len(),
        });
    }
    Ok(())
}

/// The hash of the node at `at`, whose cell is up to date.
fn node_hash_at<P: Pages, R: HashRules>(
    pages: &mut P,
    rules: &mut R,
    at: CellAddress,
) -> Result<Hash, HashError> {
    read_node(pages, at, |node| {
        rules.node_hash(node).map_err(HashError::Rule)
    })
}

/// Makes the hashes of `frame` from the cell `node` shows, which is the
/// cell the frame was read from, from the hashes of what it points at, on
/// top of `lists`, and from `value`, the node's value if that is not in the
/// cell.
///
/// The key-value hash is made from the value hash that was just made if
/// `chain`, and otherwise from the one the cell stores.
fn make<R: HashRules>(
    node: NodeRef<'_>,
    frame: &Frame,
    lists: &mut Lists,
    value: &[u8],
    chain: bool,
    rules: &mut R,
    counters: &mut Counters,
) -> Result<Made, HashError> {
    let mut made = place(node, frame, lists)?;
    let hosted = &lists.hosted;
    if frame.fields.has(HashField::Value) {
        let value = node.value().inline().unwrap_or(value);
        let hash = rules.value_hash(node, value, hosted);
        made.value = Some(hash.map_err(HashError::Rule)?);
        counters.value_hashes += 1;
    }
    if frame.fields.has(HashField::KeyValue) {
        let value_hash = made.value.as_ref().filter(|_| chain);
        let value_hash = value_hash.unwrap_or(node.value_hash());
        let hash = rules.kv_hash(node.key(), value_hash);
        made.kv = Some(hash.map_err(HashError::Rule)?);
        counters.kv_hashes += 1;
    }
    Ok(made)
}

/// Puts the hashes of the nodes that the cell `node` shows points at
/// where the cell's own hashes are made from them: the hash of each child
/// of `frame` into what is returned, and the hashes of the roots of the
/// Merks it hosts, by position, into [`Lists::hosted`] if `frame` makes a
/// value hash. A cell that points elsewhere than when `frame` was read is
/// refused.
fn place(node: NodeRef<'_>, frame: &Frame, lists: &mut Lists) -> Result<Made, HashError> {
    let changed = HashError::Changed { node: frame.at };
    let (deps, hashes) = (&lists.deps[frame.deps..], &lists.hashes[frame.hashes..]);
    let hosted = &mut lists.hosted;
    let mut made = Made::default();
    let makes_value = frame.fields.has(HashField::Value);
    if makes_value {
        let roots = node.hosted_roots();
        let named = deps
            .iter()
            .filter(|dep| matches!(dep.role, Role::Hosted(_)));
        let same_roots = roots.len() == frame.hosted && roots.flatten().count() == named.count();
        if node.value().place() != frame.place || !same_roots {
            return Err(changed);
        }
        hosted.clear();
        hosted.resize(frame.hosted, None);
    }
    for (dep, hash) in deps.iter().zip(hashes) {
        let (named, slot) = match dep.role {
            Role::Child(side) => (
                node.link(side).map(|link| link.child),
                match side {
                    Side::Left => &mut made.left,
                    Side::Right => &mut made.right,
                },
            ),
            Role::Hosted(position) => (
                node.hosted_roots().nth(position).flatten(),
                &mut hosted[position],
            ),
        };
        if named != Some(dep.at) {
            return Err(changed);
        }
        *slot = Some(*hash);
    }
    Ok(made)
}

/// One evaluation: the marks, the pages, the values and the rules, and the
/// nodes that are open on the way down.
pub(super) struct Run<'a, P, V, R> {
    hashes: &'a mut Hashes,
    pages: &'a mut P,
    values: &'a mut V,
    rules: &'a mut R,
    stack: Vec<Frame>,
    lists: Lists,
    value: Vec<u8>,
}

impl<'a, P: Pages, V: ValueReader, R: HashRules> Run<'a, P, V, R> {
    pub(super) fn new(
        hashes: &'a mut Hashes,
        pages: &'a mut P,
        values: &'a mut V,
        rules: &'a mut R,
    ) -> Self {
        Self {
            hashes,
            pages,
            values,
            rules,
            stack: Vec::new(),
            lists: Lists::default(),
            value: Vec::new(),
        }
    }

    /// The lowest marked address.
    pub(super) fn first(&self) -> Option<CellAddress> {
        self.hashes.marks.keys().next().copied()
    }

    /// Brings the node at `at` up to date, with everything that is marked
    /// below it, and returns its hash if `want`.
    pub(super) fn bring(&mut self, at: CellAddress, want: bool) -> Result<Option<Hash>, HashError> {
        let result = self.descend(at, want);
        for frame in self.stack.drain(..) {
            if let Some(marks) = self.hashes.marks.get_mut(&frame.at) {
                marks.open = false;
            }
        }
        self.lists.deps.clear();
        self.lists.hashes.clear();
        result
    }

    fn descend(&mut self, at: CellAddress, want: bool) -> Result<Option<Hash>, HashError> {
        if !self.open(at)? {
            return match want {
                true => self.clean(at).map(Some),
                false => Ok(None),
            };
        }
        loop {
            let top = self.stack.last().expect("a node is open");
            let have = self.lists.hashes.len() - top.hashes;
            if let Some(dep) = self.lists.deps[top.deps..].get(have) {
                let below = dep.at;
                if !self.open(below)? {
                    let hash = self.clean(below)?;
                    self.lists.hashes.push(hash);
                }
                continue;
            }
            let hash = self.close(want || self.stack.len() > 1)?;
            if self.stack.is_empty() {
                return Ok(hash);
            }
            let hash = hash.expect("a node that another waits for is hashed");
            self.lists.hashes.push(hash);
        }
    }

    /// Opens the node at `at` if it is marked: reads what its marked hashes
    /// are made from.
    fn open(&mut self, at: CellAddress) -> Result<bool, HashError> {
        let Some(marks) = self.hashes.marks.get_mut(&at) else {
            return Ok(false);
        };
        if marks.open {
            return Err(HashError::Cycle { node: at });
        }
        let frame = gather(self.pages, &mut self.lists, at, marks.stale, true)?;
        marks.open = true;
        self.stack.push(frame);
        Ok(true)
    }

    /// The hash of a node that is not marked.
    fn clean(&mut self, at: CellAddress) -> Result<Hash, HashError> {
        let hash = node_hash_at(self.pages, self.rules, at)?;
        self.hashes.counters.node_hashes += 1;
        Ok(hash)
    }

    /// Makes the marked hashes of the node that was opened last, writes
    /// those that differ into its cell, takes its marks off, and returns
    /// the node's hash if `want`. The marks are off once the stored hashes
    /// are in the cell, also when the node's own hash is then refused.
    fn close(&mut self, want: bool) -> Result<Option<Hash>, HashError> {
        let frame = self.stack.last().expect("a node is open");
        let at = frame.at;
        fetch(self.values, frame, &mut self.value)?;
        let (rules, lists, value) = (&mut *self.rules, &mut self.lists, &self.value);
        let counters = &mut self.hashes.counters;
        let written = self.pages.write(at.page(), |page| {
            let Some(mut cell) = page.node_mut(at.slot()) else {
                return Ok(Err(HashError::NoNode { address: at }));
            };
            let made = match make(cell.node(), frame, lists, value, true, rules, counters) {
                Ok(made) => made,
                Err(error) => return Ok(Err(error)),
            };
            for (field, hash) in made.hashes() {
                if cell.node().hash(field) == Some(hash) {
                    counters.unchanged += 1;
                } else {
                    cell.set_hash(field, hash)?;
                    counters.written += 1;
                }
            }
            Ok(Ok(want.then(|| rules.node_hash(cell.node()))))
        });
        let hash = written.map_err(HashError::Pages)??;
        self.lists.deps.truncate(frame.deps);
        self.lists.hashes.truncate(frame.hashes);
        self.hashes.marks.remove(&at);
        self.hashes.counters.nodes += 1;
        self.stack.pop();
        let hash = hash.transpose().map_err(HashError::Rule)?;
        self.hashes.counters.node_hashes += u64::from(hash.is_some());
        Ok(hash)
    }
}

/// Whether the value hash that the cell at `node` stores is the one the
/// rules name as made with another hash of a hosted root.
pub(super) fn made_with_other<P: Pages, V: ValueReader, R: HashRules>(
    pages: &mut P,
    values: &mut V,
    rules: &mut R,
    node: CellAddress,
) -> Result<bool, HashError> {
    let mut lists = Lists::default();
    let frame = gather(pages, &mut lists, node, Stale::of(HashField::Value), false)?;
    let mut any = false;
    for dep in &lists.deps {
        let other = read_node(pages, dep.at, |root| {
            rules.other_node_hash(root).map_err(HashError::Rule)
        })?;
        any |= other.is_some();
        lists.hashes.push(match other {
            Some(other) => other,
            None => node_hash_at(pages, rules, dep.at)?,
        });
    }
    if !any {
        return Ok(false);
    }
    let mut value = Vec::new();
    fetch(values, &frame, &mut value)?;
    read_node(pages, node, |cell| {
        place(cell, &frame, &mut lists)?;
        let value = cell.value().inline().unwrap_or(&value);
        let made = rules.value_hash_with_other(cell, value, &lists.hosted);
        Ok(made.map_err(HashError::Rule)?.as_ref() == Some(cell.value_hash()))
    })
}

/// Which of the hashes the cell at `node` stores differ from their
/// recomputation.
pub(super) fn check<P: Pages, V: ValueReader, R: HashRules>(
    pages: &mut P,
    values: &mut V,
    rules: &mut R,
    node: CellAddress,
) -> Result<Stale, HashError> {
    let mut lists = Lists::default();
    let frame = gather(pages, &mut lists, node, Stale::ALL, false)?;
    for dep in &lists.deps {
        lists.hashes.push(node_hash_at(pages, rules, dep.at)?);
    }
    let mut value = Vec::new();
    fetch(values, &frame, &mut value)?;
    read_node(pages, node, |cell| {
        let mut none = Counters::default();
        let made = make(cell, &frame, &mut lists, &value, false, rules, &mut none)?;
        let differing = made
            .hashes()
            .filter(|(field, hash)| cell.hash(*field) != Some(*hash));
        Ok(differing.map(|(field, _)| field).collect())
    })
}
