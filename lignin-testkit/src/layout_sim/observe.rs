//! Reads the node structure Merk produced, straight from RocksDB.
//!
//! After every write the observer walks the grove from the GroveDB root,
//! inside the open transaction, and reads the node records Merk stored. It
//! decodes each record with Merk's own decoder and never looks at what the
//! workload asked for. It reimplements none of Merk: not the balancing, not
//! the hashing.
//!
//! The walk is a Merkle diff. A node's hash covers its whole subtree, and a
//! link stores the hash of the child it points at. So a child whose key and
//! hash in the link are the ones the mirror already has is unchanged with
//! everything below it, and the walk does not read it. A tree element's
//! value hash covers the root hash of the Merk it hosts, so the same holds
//! across Merks. The records the walk reads are therefore exactly the node
//! records the write changed, which are the ones Merk handed to storage.
//!
//! A node is found deleted when it was the child of a changed node and the
//! walk did not meet it again anywhere in its tree.
//!
//! A Merk that lost its last node is read as one delete. A Merk that lost
//! several nodes at once and is now empty is read as cleared: its nodes are
//! reported like those of a dropped Merk. The records cannot tell a clear
//! from a batch that deletes every key, and the first is the usual one.

use std::collections::HashMap;

use grovedb::{Element, GroveDb, Transaction};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    tree::{kv::ValueDefinedCostType, Link, TreeNode},
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use super::tree::{ChangeSet, Event, Grove, MerkId, NodeId};

/// The axis tag of the secondary Merk of a `ProvableCountIndexedTree`
/// (`IndexAxis::Count`).
const COUNT_AXIS: u8 = 0;
/// The axis tag of the secondary Merk of a `ProvableSumIndexedTree`
/// (`IndexAxis::Sum`).
const SUM_AXIS: u8 = 1;

type NoValueDefinedCost = fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>;

/// Keeps a [`Grove`] in step with a GroveDB.
pub struct Observer {
    grove: Grove,
    grove_version: &'static GroveVersion,
    observations: u32,
}

impl Observer {
    /// An observer of an empty GroveDB.
    pub fn new(grove_version: &'static GroveVersion) -> Self {
        Observer {
            grove: Grove::new(),
            grove_version,
            observations: 0,
        }
    }

    /// The mirror as of the last observation.
    pub fn grove(&self) -> &Grove {
        &self.grove
    }

    /// Brings the mirror up to date with what `transaction` sees, and
    /// returns what changed since the last observation.
    ///
    /// Panics if storage does not hold what Merk's own records say it
    /// holds, which would mean the database is damaged, and if a secondary
    /// Merk of an indexed tree holds a tree. GroveDB writes only reference
    /// rows there, and a tree under one would have no storage prefix to be
    /// found at.
    pub fn observe(&mut self, db: &GroveDb, transaction: &Transaction) -> ChangeSet {
        self.observations += 1;
        let root_key = db
            .root_key(Some(transaction), self.grove_version)
            .unwrap()
            .expect("the root Merk opens");
        let mut walk = Walk {
            storage: db.raw_storage(),
            transaction,
            grove: &mut self.grove,
            grove_version: self.grove_version,
            observation: self.observations,
            changes: ChangeSet::default(),
            old_roots: Vec::new(),
        };
        walk.merk(MerkId::ROOT, root_key.as_deref());
        let Walk {
            mut changes,
            old_roots,
            ..
        } = walk;
        changes.finish(&mut self.grove, &old_roots);
        changes
    }

    /// Compares the mirror with what `transaction` sees in storage. Under
    /// the prefix of every Merk the mirror knows, a raw scan must find the
    /// records of that Merk's nodes, no more and no fewer, and each record
    /// must be the one the mirror read: the same bytes, the same children
    /// with the same hashes, the same value length. The Merks an element
    /// hosts must be the ones the mirror has under its node, each with the
    /// root the element names.
    ///
    /// So the mirror misses no Merk: the root Merk is checked against the
    /// root key GroveDB states, and every other Merk is reached through an
    /// element of a Merk that was checked.
    ///
    /// This is the observer's check on itself. It reads and decodes every
    /// record, where an observation reads only the changed ones.
    pub fn verify(&self, db: &GroveDb, transaction: &Transaction) -> Result<(), String> {
        let root_key = db
            .root_key(Some(transaction), self.grove_version)
            .unwrap()
            .expect("the root Merk opens");
        if root_key.as_deref() != self.root_key_of(MerkId::ROOT) {
            return Err("the mirror has another root for the root Merk".into());
        }
        let mut mirrored: HashMap<MerkId, u64> = HashMap::new();
        for (_, node) in self.grove.nodes().filter(|(_, node)| node.live) {
            *mirrored.entry(node.merk).or_default() += 1;
        }
        for (id, merk) in self.grove.merks().filter(|(_, merk)| merk.live) {
            let hosted = match merk.host {
                Some(host) => {
                    let host = self.grove.node(host);
                    host.live && host.hosted.contains(&id)
                }
                None => id == MerkId::ROOT,
            };
            if !hosted {
                return Err(format!(
                    "the mirror keeps the Merk at {:?} without an element that hosts it",
                    merk.path
                ));
            }
            let context = db
                .raw_storage()
                .get_transactional_storage_context_by_subtree_prefix(merk.prefix, None, transaction)
                .unwrap();
            let mut iterator = context.raw_iter();
            let mut stored = 0u64;
            iterator.seek_to_first().unwrap();
            while iterator.valid().unwrap() {
                let key = iterator.key().unwrap().expect("a valid key");
                let record = iterator.value().unwrap().expect("a valid record");
                stored += 1;
                self.verify_record(id, key, record).map_err(|problem| {
                    format!("the Merk at {:?}, key {key:02x?}: {problem}", merk.path)
                })?;
                iterator.next().unwrap();
            }
            let mirrored = mirrored.get(&id).copied().unwrap_or(0);
            if stored != mirrored {
                return Err(format!(
                    "the mirror of the Merk at {:?} has {mirrored} keys, storage has {stored}",
                    merk.path,
                ));
            }
        }
        Ok(())
    }

    /// The key of the root of `merk` in the mirror.
    fn root_key_of(&self, merk: MerkId) -> Option<&[u8]> {
        let root = self.grove.merk(merk).root?;
        Some(&self.grove.node(root).key)
    }

    /// Checks one stored record against the node the mirror has for it.
    fn verify_record(&self, merk: MerkId, key: &[u8], record: &[u8]) -> Result<(), String> {
        let Some(id) = self.grove.find(merk, key) else {
            return Err("storage has it, the mirror does not".into());
        };
        let node = self.grove.node(id);
        if print(&[record]) != node.record_print {
            return Err("its record is not the one the mirror read".into());
        }
        let tree = TreeNode::decode_raw(
            record,
            key.to_vec(),
            None::<NoValueDefinedCost>,
            self.grove_version,
        )
        .expect("the mirror decoded this record");
        let value = tree.value_as_slice();
        if value.len() as u32 != node.value_len {
            return Err("the mirror has another value length".into());
        }
        let element =
            Element::deserialize(value, self.grove_version).expect("a Merk value is an element");
        let roots = hosted_roots(&element);
        let same_trees = roots.len() == node.hosted.len()
            && roots
                .iter()
                .zip(&node.hosted)
                .all(|((axis, root), hosted)| {
                    let hosted_merk = self.grove.merk(*hosted);
                    hosted_merk.live
                        && hosted_merk.host == Some(id)
                        && hosted_merk.axis == *axis
                        && self.root_key_of(*hosted) == root.as_deref()
                });
        if !same_trees {
            return Err("the mirror has other Merks under its element".into());
        }
        for (link, child) in [(tree.link(true), node.left), (tree.link(false), node.right)] {
            let same = match (link, child) {
                (None, None) => true,
                (Some(link), Some(child)) => {
                    let child = self.grove.node(child);
                    child.live
                        && child.merk == merk
                        && &*child.key == link.key()
                        && child.parent == Some(id)
                        && child.node_hash == *link.hash()
                }
                _ => false,
            };
            if !same {
                return Err("the mirror has another child".into());
            }
        }
        let above = match node.parent {
            Some(parent) => {
                let parent = self.grove.node(parent);
                parent.live && (parent.left == Some(id) || parent.right == Some(id))
            }
            None => self.grove.merk(merk).root == Some(id),
        };
        if !above {
            return Err("the mirror has it under a node that does not link to it".into());
        }
        Ok(())
    }
}

struct Walk<'a, 'db> {
    storage: &'db RocksDbStorage,
    transaction: &'db Transaction<'db>,
    grove: &'a mut Grove,
    grove_version: &'static GroveVersion,
    observation: u32,
    changes: ChangeSet,
    old_roots: Vec<(MerkId, NodeId)>,
}

type Context<'db> = <RocksDbStorage as Storage<'db>>::BatchTransactionalStorageContext;

/// What the mirror held for a node before its record was read again.
struct Old {
    parent: Option<NodeId>,
    left: Option<NodeId>,
    right: Option<NodeId>,
    value_print: u64,
    value_hash_print: u64,
}

impl<'db> Walk<'_, 'db> {
    /// Diffs one Merk against the mirror, given the root key its parent
    /// element (or the root record) now states.
    fn merk(&mut self, merk: MerkId, root_key: Option<&[u8]>) {
        let old_root = self.grove.merk(merk).root;
        let Some(root_key) = root_key else {
            if let Some(old_root) = old_root {
                if self.grove.merk(merk).keys.len() == 1 {
                    self.grove.merk_mut(merk).root = None;
                    self.sweep(vec![old_root]);
                } else {
                    self.grove.drop_merk(merk, false, &mut self.changes);
                }
                self.changes.roots_changed.push(merk);
            }
            return;
        };
        let prefix = self.grove.merk(merk).prefix;
        let context = self
            .storage
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, self.transaction)
            .unwrap();
        let mut candidates = Vec::new();
        let root = self.node(&context, merk, root_key, None, None, &mut candidates);
        self.grove.merk_mut(merk).root = Some(root);
        if old_root != Some(root) {
            self.changes.roots_changed.push(merk);
            if let Some(old_root) = old_root {
                candidates.push(old_root);
                self.old_roots.push((merk, old_root));
            }
        }
        self.sweep(candidates);
    }

    /// Retires the nodes among `candidates` that the walk did not meet in
    /// their tree, and what hung below them.
    fn sweep(&mut self, mut candidates: Vec<NodeId>) {
        while let Some(candidate) = candidates.pop() {
            let node = self.grove.node(candidate);
            if !node.live || node.seen == self.observation {
                continue;
            }
            candidates.extend(node.left.into_iter().chain(node.right));
            let hosted = node.hosted.clone();
            self.grove.retire_node(candidate);
            self.changes.removed.push(candidate);
            for merk in hosted {
                self.grove.drop_merk(merk, true, &mut self.changes);
            }
        }
    }

    /// Brings the node under `key` up to date and returns it. `link_hash`
    /// is the node's hash as the link of its parent states it.
    fn node(
        &mut self,
        context: &Context<'db>,
        merk: MerkId,
        key: &[u8],
        link_hash: Option<&[u8; 32]>,
        parent: Option<NodeId>,
        candidates: &mut Vec<NodeId>,
    ) -> NodeId {
        let existing = self.grove.find(merk, key);
        if let (Some(id), Some(hash)) = (existing, link_hash)
            && self.grove.node(id).node_hash == *hash
        {
            let node = self.grove.node_mut(id);
            node.seen = self.observation;
            node.parent = parent;
            return id;
        }
        let record = context
            .get(key)
            .unwrap()
            .expect("storage answers")
            .expect("a linked node is stored");
        let record_print = print(&[&record]);
        if let Some(id) = existing
            && self.grove.node(id).record_print == record_print
        {
            let node = self.grove.node_mut(id);
            node.seen = self.observation;
            node.parent = parent;
            node.node_hash = link_hash.copied().unwrap_or([0; 32]);
            return id;
        }
        let tree = TreeNode::decode_raw(
            &record,
            key.to_vec(),
            None::<NoValueDefinedCost>,
            self.grove_version,
        )
        .expect("a stored record decodes");
        let value = tree.value_as_slice();
        let value_print = print(&[value]);
        let value_hash_print = print(&[tree.value_hash()]);
        let links = [tree.link(true), tree.link(false)];
        let link_bytes: usize = links
            .iter()
            .flatten()
            .map(|link| 35 + link.key().len())
            .sum();
        let meta_len = record
            .len()
            .checked_sub(2 + link_bytes + 64 + value.len())
            .expect("a record holds two link markers, its links, two hashes and its value");
        let meta_fixed_len = tree.feature_type().encoding_cost()
            + links
                .iter()
                .flatten()
                .map(|link| {
                    let link_len = link.encoding_cost().expect("a stored link has a size");
                    link_len - 35 - link.key().len()
                })
                .sum::<usize>();

        let (id, created) = match existing {
            Some(id) => (id, false),
            None => (self.grove.add_node(merk, key), true),
        };
        let old = self.grove.node(id);
        let old = Old {
            parent: old.parent,
            left: old.left,
            right: old.right,
            value_print: old.value_print,
            value_hash_print: old.value_hash_print,
        };
        let child_changed = |old: Option<NodeId>, link: Option<&Link>| match (old, link) {
            (None, None) => false,
            (Some(old), Some(link)) => &*self.grove.node(old).key != link.key(),
            _ => true,
        };
        let links_changed = created
            || old.parent != parent
            || child_changed(old.left, links[0])
            || child_changed(old.right, links[1]);
        let value_changed = created || old.value_print != value_print;
        let value_hash_changed = created || old.value_hash_print != value_hash_print;
        self.changes.events.push(Event {
            node: id,
            created,
            old_parent: old.parent,
            old_left: old.left,
            old_right: old.right,
            value_changed,
            links_changed,
        });
        self.changes.record_bytes += (32 + key.len() + record.len()) as u64;
        let node = self.grove.node_mut(id);
        node.seen = self.observation;
        node.event = self.changes.events.len() as u32;
        node.parent = parent;
        node.node_hash = link_hash.copied().unwrap_or([0; 32]);
        node.record_print = record_print;
        node.value_print = value_print;
        node.value_hash_print = value_hash_print;
        node.value_len = value.len() as u32;
        node.meta_len = meta_len as u32;
        node.meta_fixed_len = meta_fixed_len as u32;

        let mut children = [None, None];
        for (child, link) in children.iter_mut().zip(links) {
            if let Some(link) = link {
                *child = Some(self.node(
                    context,
                    merk,
                    link.key(),
                    Some(link.hash()),
                    Some(id),
                    candidates,
                ));
            }
        }
        for (old, new) in [(old.left, children[0]), (old.right, children[1])] {
            if old != new {
                candidates.extend(old);
            }
        }
        let node = self.grove.node_mut(id);
        node.left = children[0];
        node.right = children[1];

        if value_changed || value_hash_changed {
            self.hosted(id, merk, key, value);
        }
        id
    }

    /// Follows the Merks that the element stored in node `id` hosts.
    fn hosted(&mut self, id: NodeId, merk: MerkId, key: &[u8], value: &[u8]) {
        let element =
            Element::deserialize(value, self.grove_version).expect("a Merk value is an element");
        let roots = hosted_roots(&element);
        let mut hosted = self.grove.node(id).hosted.clone();
        let same_trees = hosted.len() == roots.len()
            && hosted
                .iter()
                .zip(&roots)
                .all(|(merk, (axis, _))| self.grove.merk(*merk).axis == *axis);
        if !same_trees {
            for dropped in hosted.drain(..) {
                self.grove.drop_merk(dropped, true, &mut self.changes);
            }
            self.grove.node_mut(id).hosted = Vec::new();
        }
        if roots.is_empty() {
            return;
        }
        if hosted.is_empty() {
            let mut path = self
                .grove
                .merk(merk)
                .path
                .clone()
                .expect("a secondary Merk of an indexed tree holds no tree");
            path.push(key.to_vec());
            let primary = RocksDbStorage::build_prefix(SubtreePath::from(path.as_slice())).unwrap();
            for (axis, _) in &roots {
                let created = match axis {
                    None => self.grove.add_merk(primary, Some(path.clone()), None, id),
                    Some(axis) => {
                        let prefix = RocksDbStorage::secondary_prefix_for(&primary, *axis).unwrap();
                        self.grove.add_merk(prefix, None, Some(*axis), id)
                    }
                };
                hosted.push(created);
                self.changes.merks_created.push(created);
            }
            self.grove.node_mut(id).hosted = hosted.clone();
        }
        for (hosted, (_, root_key)) in hosted.into_iter().zip(roots) {
            self.merk(hosted, root_key.as_deref());
        }
    }
}

/// The Merks an element hosts: the axis tag (`None` for the primary Merk)
/// and the root key of each. Empty for an element that is not a tree and
/// for a tree whose data is not in a Merk.
fn hosted_roots(element: &Element) -> Vec<(Option<u8>, Option<Vec<u8>>)> {
    match element.underlying() {
        Element::ProvableSumIndexedTree(primary, secondary, ..) => {
            vec![(None, primary.clone()), (Some(SUM_AXIS), secondary.clone())]
        }
        Element::ProvableCountIndexedTree(primary, secondary, ..) => vec![
            (None, primary.clone()),
            (Some(COUNT_AXIS), secondary.clone()),
        ],
        Element::ProvableCountProvableSumIndexedTree(primary, _, _, axes, _) => {
            std::iter::once((None, primary.clone()))
                .chain(axes.iter().map(|(axis, root)| (Some(*axis), root.clone())))
                .collect()
        }
        tree if tree.uses_non_merk_data_storage() => Vec::new(),
        other => match other.root_key_and_tree_type() {
            Some((root_key, _)) => vec![(None, root_key.clone())],
            None => Vec::new(),
        },
    }
}

/// A 64-bit fingerprint of some byte strings, each with its length.
fn print(parts: &[&[u8]]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let mut first = [0u8; 8];
    first.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
    u64::from_le_bytes(first)
}
