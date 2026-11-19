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

/// What a stored hash of a node is made from the hash of.
#[derive(Debug, Clone, Copy)]
enum Role {
    /// The child on a side.
    Child(Side),
    /// The root of the Merk the node hosts at a position.
    Hosted(usize),
}

#[derive(Debug, Clone, Copy)]
struct Dep {
    role: Role,
    at: CellAddress,
}

/// One node whose hashes are being made, as its cell was read.
#[derive(Debug)]
struct Frame {
    at: CellAddress,
    fields: Stale,
    deps: Vec<Dep>,
    /// The hashes of the first of `deps`, in their order.
    hashes: Vec<Hash>,
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
    children: [Option<Hash>; 2],
}

const fn index(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}

impl Made {
    fn hashes(&self) -> impl Iterator<Item = (HashField, &Hash)> {
        let [left, right] = &self.children;
        [
            (HashField::Value, &self.value),
            (HashField::KeyValue, &self.kv),
            (HashField::Child(Side::Left), left),
            (HashField::Child(Side::Right), right),
        ]
        .into_iter()
        .filter_map(|(field, hash)| Some((field, hash.as_ref()?)))
    }
}

/// Reads what the hashes `fields` of the cell at `at` are made from. A
/// field that names the hash of a child the node does not have is an error
/// if `strict`, and is left out otherwise.
fn gather<P: Pages>(
    pages: &mut P,
    at: CellAddress,
    fields: Stale,
    strict: bool,
) -> Result<Frame, HashError> {
    if fields.has(HashField::Value) && pages.host_is_behind(at) {
        return Err(HashError::HostBehind { host: at });
    }
    let read = pages.read(at.page(), |page| {
        let node = page
            .node(at.slot())
            .ok_or(HashError::NoNode { address: at })?;
        let mut frame = Frame {
            at,
            fields,
            deps: Vec::new(),
            hashes: Vec::new(),
            place: None,
            hosted: 0,
        };
        for side in Side::BOTH {
            let field = HashField::Child(side);
            if !fields.has(field) {
                continue;
            }
            match node.link(side) {
                Some(link) => frame.deps.push(Dep {
                    role: Role::Child(side),
                    at: link.child,
                }),
                None if strict => return Err(HashError::NoHash { node: at, field }),
                None => frame.fields = frame.fields.without(field),
            }
        }
        if fields.has(HashField::Value) {
            frame.place = node.value().place();
            for (position, root) in node.hosted_roots().enumerate() {
                frame.hosted += 1;
                frame.deps.extend(root.map(|at| Dep {
                    role: Role::Hosted(position),
                    at,
                }));
            }
        }
        Ok(frame)
    });
    read.map_err(HashError::Pages)?
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
    let read = pages.read(at.page(), |page| {
        let node = page
            .node(at.slot())
            .ok_or(HashError::NoNode { address: at })?;
        rules.node_hash(node).map_err(HashError::Rule)
    });
    read.map_err(HashError::Pages)?
}

/// Makes the hashes of `frame` from the cell `node` shows, which is the
/// cell the frame was read from, from the hashes of what it points at and
/// from `value`, the node's value if that is not in the cell.
///
/// The key-value hash is made from the value hash that was just made if
/// `chain`, and otherwise from the one the cell stores.
fn make<R: HashRules>(
    node: NodeRef<'_>,
    frame: &Frame,
    value: &[u8],
    chain: bool,
    rules: &mut R,
    counters: &mut Counters,
) -> Result<Made, HashError> {
    let changed = HashError::Changed { node: frame.at };
    let mut made = Made::default();
    let mut hosted = Vec::new();
    let makes_value = frame.fields.has(HashField::Value);
    if makes_value {
        let roots = node.hosted_roots();
        let named = frame.deps.iter();
        let named = named.filter(|dep| matches!(dep.role, Role::Hosted(_)));
        let same_roots = roots.len() == frame.hosted && roots.flatten().count() == named.count();
        if node.value().place() != frame.place || !same_roots {
            return Err(changed);
        }
        hosted.resize(frame.hosted, None);
    }
    for (dep, hash) in frame.deps.iter().zip(&frame.hashes) {
        let (named, slot) = match dep.role {
            Role::Child(side) => (
                node.link(side).map(|link| link.child),
                &mut made.children[index(side)],
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
    if makes_value {
        let value = node.value().inline().unwrap_or(value);
        let hash = rules.value_hash(node, value, &hosted);
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

/// One evaluation: the marks, the pages, the values and the rules, and the
/// nodes that are open on the way down.
pub(super) struct Run<'a, P, V, R> {
    hashes: &'a mut Hashes,
    pages: &'a mut P,
    values: &'a mut V,
    rules: &'a mut R,
    stack: Vec<Frame>,
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
            if let Some(dep) = top.deps.get(top.hashes.len()) {
                let below = dep.at;
                if !self.open(below)? {
                    let hash = self.clean(below)?;
                    let top = self.stack.last_mut().expect("a node is open");
                    top.hashes.push(hash);
                }
                continue;
            }
            let hash = self.close(want || self.stack.len() > 1)?;
            match self.stack.last_mut() {
                Some(above) => above
                    .hashes
                    .push(hash.expect("a node that another waits for is hashed")),
                None => return Ok(hash),
            }
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
        let frame = gather(self.pages, at, marks.stale, true)?;
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
    /// the node's hash if `want`.
    fn close(&mut self, want: bool) -> Result<Option<Hash>, HashError> {
        let frame = self.stack.last().expect("a node is open");
        let at = frame.at;
        fetch(self.values, frame, &mut self.value)?;
        let (rules, value) = (&mut *self.rules, &self.value);
        let counters = &mut self.hashes.counters;
        let written = self.pages.write(at.page(), |page| {
            let Some(mut cell) = page.node_mut(at.slot()) else {
                return Ok(Err(HashError::NoNode { address: at }));
            };
            let made = match make(cell.node(), frame, value, true, rules, counters) {
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
            if !want {
                return Ok(Ok(None));
            }
            let hash = rules.node_hash(cell.node());
            counters.node_hashes += u64::from(hash.is_ok());
            Ok(hash.map(Some).map_err(HashError::Rule))
        });
        let hash = written.map_err(HashError::Pages)??;
        self.hashes.marks.remove(&at);
        self.hashes.counters.nodes += 1;
        self.stack.pop();
        Ok(hash)
    }
}

/// Which of the hashes the cell at `node` stores differ from their
/// recomputation.
pub(super) fn check<P: Pages, V: ValueReader, R: HashRules>(
    pages: &mut P,
    values: &mut V,
    rules: &mut R,
    node: CellAddress,
) -> Result<Stale, HashError> {
    let mut frame = gather(pages, node, Stale::ALL, false)?;
    for index in 0..frame.deps.len() {
        let hash = node_hash_at(pages, rules, frame.deps[index].at)?;
        frame.hashes.push(hash);
    }
    let mut value = Vec::new();
    fetch(values, &frame, &mut value)?;
    let read = pages.read(node.page(), |page| {
        let cell = page
            .node(node.slot())
            .ok_or(HashError::NoNode { address: node })?;
        let made = make(cell, &frame, &value, false, rules, &mut Counters::default())?;
        let differing = made
            .hashes()
            .filter(|(field, hash)| cell.hash(*field) != Some(*hash));
        Ok(differing.map(|(field, _)| field).collect())
    });
    read.map_err(HashError::Pages)?
}
