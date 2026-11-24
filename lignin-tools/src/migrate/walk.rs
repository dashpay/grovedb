//! The walk of a source grove: every subtree that an element leads to,
//! from the root Merk down, with what each holds.
//!
//! The survey, the load and the comparison of a migration are three
//! [`Visit`]ors of this one walk, so each of them sees the same subtrees,
//! with the same roots, in the same order.
//!
//! # Which subtrees there are
//!
//! The root Merk, and below each node of a Merk's tree whose value is the
//! element of a tree, the subtrees that element hosts on the LigninDB
//! engine (`grovedb::engine::lignin::hosted_by_element`): the subtree of
//! its own path and, for an indexed tree, the secondary of each axis.
//!
//! **A subtree is entered where the source holds a record of it**, in the
//! data, the aux or the meta keyspace, and the Merks of an indexed tree
//! together where one of them holds one. That is what a state-sync restore
//! enters (`engine.md`, "The directory of a synced grove"), and what the
//! engine enters by itself for a tree that nothing was written into: no
//! subtree. A record of a root key alone enters nothing: the engine keeps
//! those outside the subtree.
//!
//! **The put of a tree's element names the Merks it hosts**, each at its
//! position: the subtrees that are entered for it whose data is a Merk.
//! The subtree of a tree that keeps flat records is entered under its
//! element and is no Merk: the element's cell has no pointer for it, as
//! in a store that GroveDB wrote.
//!
//! An element that a record no tree links holds hosts nothing here: an
//! unlinked record names no Merk. Where the source holds records under the
//! prefix such an element would host, no element leads to them, and the
//! survey reports the prefix.
//!
//! # The order, and the place of the walk
//!
//! A subtree is entered before the subtrees below it and left after them:
//! the children of a Merk in the order of their elements' keys, the
//! subtrees of one element in the order of their positions. So the Merk of
//! a subtree is handed over after the Merks below it, and each of its
//! nodes can name the roots of the Merks it hosts.
//!
//! [`Frame`] is one subtree that is entered and not left, with the last
//! subtree below it that was left. The frames from the root down are the
//! place of the walk: a walk that is given the frames of an earlier one
//! goes on right after the step that left them ([`Walk::resumed`]).
//!
//! # Memory
//!
//! A Merk is read whole to find its tree. Of a Merk with subtrees below it
//! the walk keeps, while it is below, the order of its nodes and the
//! subtrees its elements host, and reads its records a second time when it
//! comes back. A Merk with none below is handed over from its first read.
//! So what is held at once is the nodes of one Merk, and for each Merk
//! above it eight bytes for a node and the list of its subtrees.

use std::{
    collections::BTreeMap,
    hash::{Hash as _, Hasher as _},
};

use grovedb::{
    engine::lignin::{hosted_by_element, HostedSubtree},
    Element,
};
use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, TreeType};
use grovedb_path::SubtreePath;
use grovedb_storage::prefix::build_prefix;
use grovedb_version::version::GroveVersion;
use lignindb::txn::Subtree;

use super::{
    grove::{decode, shape, Keys, Node, Rooted, Shape},
    source::{Count, Counts, Keyspace, Prefix, Source},
};
use crate::ToolError;

/// The key of the record of a Merk's root key in the roots keyspace of its
/// subtree: Merk's `ROOT_KEY_KEY`. A test holds it to where a handle of
/// the engine keeps that record.
pub(crate) const ROOT_KEY: &[u8] = b"r";

/// The prefix of the root Merk: the one of the empty path.
pub(crate) const ROOT: Prefix = [0; 32];

/// About how many bytes of flat records the walk hands over at once.
const CHUNK_BYTES: usize = 8 << 20;

/// The element that hosts a subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Host {
    /// The subtree of the Merk the element is in.
    pub(crate) parent: Prefix,
    /// The element's key.
    pub(crate) key: Vec<u8>,
    /// Which of the element's subtrees it is.
    pub(crate) position: u8,
}

/// A subtree of the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    /// The 32 bytes that name the subtree.
    pub(crate) prefix: Prefix,
    /// The path of the element that hosts it: the empty one for the root
    /// Merk. A secondary of an indexed tree has the path of its tree.
    pub(crate) path: Vec<Vec<u8>>,
    /// The tree type it is entered with.
    pub(crate) tree_type: TreeType,
    /// Where the root of its Merk is read, or `None` for a tree that keeps
    /// its data as flat records.
    pub(crate) rooted: Option<Rooted>,
    /// The element that hosts it, or `None` for the root Merk.
    pub(crate) host: Option<Host>,
}

impl Place {
    /// The path as text, for a report.
    pub(crate) fn named(&self) -> String {
        let position = self.host.as_ref().map_or(0, |host| host.position);
        named(&self.path, position)
    }
}

/// `path` as text: each segment as it is where it is printable ASCII, and
/// in hex otherwise, with the position of a secondary after it.
pub(crate) fn named(path: &[Vec<u8>], position: u8) -> String {
    let segment = |segment: &Vec<u8>| {
        let printable = !segment.is_empty()
            && (segment.iter()).all(|byte| byte.is_ascii_graphic() && *byte != b'/');
        match printable {
            true => String::from_utf8_lossy(segment).into_owned(),
            false => format!("0x{}", crate::hex(segment)),
        }
    };
    let path: Vec<String> = path.iter().map(segment).collect();
    match position {
        0 => format!("/{}", path.join("/")),
        position => format!("/{} (secondary {position})", path.join("/")),
    }
}

/// One subtree that is entered and not left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The key of the element that hosts the subtree, in the subtree of
    /// the frame below: empty for the root Merk.
    pub(crate) key: Vec<u8>,
    /// Which of that element's subtrees it is.
    pub(crate) position: u8,
    /// The last subtree below this one that was left: the key of its
    /// element, and its position.
    pub(crate) done: Option<(Vec<u8>, u8)>,
}

/// What the walk found a subtree to hold, when it first read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Found {
    /// The records of its data, aux and meta keyspaces.
    pub(crate) counts: Counts,
    /// The nodes of its Merk's tree.
    pub(crate) nodes: u64,
    /// The key of the root of its Merk, if it has a node.
    pub(crate) root: Option<Vec<u8>>,
    /// The node records that its tree does not link.
    pub(crate) unlinked: Count,
    /// The flat records of a tree that keeps those.
    pub(crate) flat: Count,
    /// How many of those flat records are the record of a node whose
    /// value is an element: the keyed children that grove versions 1 to 3
    /// take under such a tree.
    pub(crate) keyed_children: u64,
    /// How many nodes of the tree hold the element of a tree.
    pub(crate) trees: u64,
    /// How many node values are the bytes of no element.
    pub(crate) no_element: u64,
    /// About the bytes its Merk takes in memory when it is handed over.
    pub(crate) held: u64,
    /// Whether its Merk has nodes and the record of its root key, which
    /// its root is read from, does not name its root (GroveDB #1065).
    pub(crate) stale_root: bool,
    /// Why the subtree is not carried, if it is not: each in words.
    pub(crate) faults: Vec<String>,
}

/// The Merk of a subtree, as it is handed over.
#[derive(Debug, Default)]
pub(crate) struct MerkHeld {
    /// The key of the root, or `None` for a Merk with no node.
    pub(crate) root: Option<Vec<u8>>,
    /// The nodes of the tree, a child before its parent.
    pub(crate) nodes: Vec<Node>,
    /// The node records that the tree does not link, in the order of
    /// their keys.
    pub(crate) unlinked: Vec<Node>,
}

/// Records of one keyspace of a subtree: each with its key and its value.
pub(crate) type Chunk = Vec<(Vec<u8>, Vec<u8>)>;

/// What a walk is made for.
pub(crate) trait Visit {
    /// Whether the walk hands the records of each subtree over
    /// ([`Self::records`], [`Self::merk`]), which takes a second read of a
    /// Merk with subtrees below it. Without, it reports what it found.
    fn takes_records(&self) -> bool;

    /// A subtree is entered, before any subtree below it. `at` is the
    /// place of the walk once it is.
    fn enter(&mut self, place: &Place, at: &[Frame]) -> Result<(), ToolError>;

    /// What the subtree holds, once for each subtree and before it is
    /// left.
    fn found(&mut self, place: &Place, found: &Found) -> Result<(), ToolError>;

    /// Records of `keyspace` of a subtree that is entered and not left, in
    /// the order of their keys: the flat records of the data keyspace, and
    /// the aux and the meta records. A keyspace is handed over in as many
    /// calls as it takes, and the place of the walk does not change
    /// between them.
    fn records(
        &mut self,
        place: &Place,
        keyspace: Keyspace,
        records: Chunk,
        at: &[Frame],
    ) -> Result<(), ToolError>;

    /// The Merk of a subtree that is about to be left, whole.
    fn merk(&mut self, place: &Place, merk: MerkHeld) -> Result<(), ToolError>;

    /// A subtree is left: everything of it and below it was handed over.
    /// `at` is the place of the walk once it is.
    fn leave(&mut self, place: &Place, at: &[Frame]) -> Result<(), ToolError>;
}

/// The prefixes that hold a record in the aux or in the meta keyspace,
/// each with its count.
#[derive(Debug, Clone, Default)]
pub(crate) struct Side {
    aux: BTreeMap<Prefix, Count>,
    meta: BTreeMap<Prefix, Count>,
}

impl Side {
    /// Reads the aux and the meta keyspace of `source` whole.
    pub(crate) fn read(source: &Source) -> Result<Self, ToolError> {
        let mut side = Self::default();
        for (keyspace, into) in [
            (Keyspace::Aux, &mut side.aux),
            (Keyspace::Meta, &mut side.meta),
        ] {
            let mut records = source.records(keyspace);
            while let Some((key, value)) = records.current()? {
                let Some((prefix, key)) = key.split_first_chunk::<32>() else {
                    return Err(short_key(source, keyspace, key));
                };
                into.entry(*prefix).or_default().add(key, value);
                records.next();
            }
        }
        Ok(side)
    }

    /// What `prefix` holds in `keyspace`, which is the aux or the meta
    /// one.
    pub(crate) fn of(&self, keyspace: Keyspace, prefix: &Prefix) -> Count {
        let held = match keyspace {
            Keyspace::Aux => self.aux.get(prefix),
            Keyspace::Meta => self.meta.get(prefix),
            Keyspace::Data | Keyspace::Roots => None,
        };
        held.copied().unwrap_or_default()
    }

    /// Whether `prefix` holds an aux or a meta record.
    pub(crate) fn holds(&self, prefix: &Prefix) -> bool {
        self.aux.contains_key(prefix) || self.meta.contains_key(prefix)
    }

    /// Every prefix that holds a record of `keyspace`, with its count.
    pub(crate) fn prefixes(&self, keyspace: Keyspace) -> impl Iterator<Item = (&Prefix, &Count)> {
        match keyspace {
            Keyspace::Meta => self.meta.iter(),
            _ => self.aux.iter(),
        }
    }
}

/// The refusal of a stored key that is shorter than a prefix.
pub(crate) fn short_key(source: &Source, keyspace: Keyspace, key: &[u8]) -> ToolError {
    ToolError::Migrate(format!(
        "the source {}: its {} keyspace holds the key {}, which is shorter than the prefix of \
         a subtree: it is no GroveDB that this command reads",
        source.dir().display(),
        keyspace.name(),
        crate::hex(key)
    ))
}

/// Whether `hosted` is the subtree of a tree that keeps its data as flat
/// records.
fn flat(hosted: &HostedSubtree) -> bool {
    hosted.tree_type.uses_non_merk_data_storage()
}

/// A subtree below a Merk: its place, without its path.
#[derive(Debug, Clone)]
struct Child {
    key: Vec<u8>,
    hosted: HostedSubtree,
    position: u8,
}

/// What the first read of a Merk keeps.
struct Read {
    found: Found,
    shape: Shape,
    /// The count of the records and a hash of their keys, which a second
    /// read has to come to as well.
    read_as: (usize, u64),
    /// For each node of the tree that hosts subtrees which are entered,
    /// by its place among the records: those subtrees, by position.
    hosting: BTreeMap<usize, Vec<Subtree>>,
    /// The subtrees below, in the order they are walked.
    children: Vec<Child>,
    /// The nodes, in the order of their keys, where the read kept them.
    nodes: Option<Vec<Node>>,
}

/// The walk of one source.
pub(crate) struct Walk<'s, V> {
    source: &'s Source,
    side: &'s Side,
    visitor: V,
    /// Whether each record is held to Merk's own encoding of it, and each
    /// flat record is looked at for a node: the survey's walk.
    surveying: bool,
    frames: Vec<Frame>,
    resume: Vec<Frame>,
    path: Vec<Vec<u8>>,
}

fn journal_error(what: &str) -> ToolError {
    ToolError::Migrate(format!(
        "the place that the journal of the target states does not fit the source: {what}. The \
         target was made from another state of the source: remove the directory it is in and \
         migrate anew"
    ))
}

impl<'s, V: Visit> Walk<'s, V> {
    /// A walk of `source` from its beginning.
    pub(crate) fn new(source: &'s Source, side: &'s Side, visitor: V, surveying: bool) -> Self {
        Self::resumed(source, side, visitor, surveying, Vec::new())
    }

    /// A walk of `source` that goes on from the place `frames`: the
    /// subtrees that are entered there are not entered again, and what
    /// was left is passed over.
    pub(crate) fn resumed(
        source: &'s Source,
        side: &'s Side,
        visitor: V,
        surveying: bool,
        frames: Vec<Frame>,
    ) -> Self {
        Self {
            source,
            side,
            visitor,
            surveying,
            frames: Vec::new(),
            resume: frames,
            path: Vec::new(),
        }
    }

    /// Walks the grove, and returns the visitor.
    pub(crate) fn run(mut self) -> Result<V, ToolError> {
        let root = Place {
            prefix: ROOT,
            path: Vec::new(),
            tree_type: TreeType::NormalTree,
            rooted: Some(Rooted::ByRecord(None)),
            host: None,
        };
        let resuming = !self.resume.is_empty();
        self.subtree(&root, resuming)?;
        Ok(self.visitor)
    }

    fn subtree(&mut self, place: &Place, resuming: bool) -> Result<(), ToolError> {
        let depth = self.frames.len();
        let (key, position) = match &place.host {
            Some(host) => (host.key.clone(), host.position),
            None => (Vec::new(), 0),
        };
        let saved = match resuming {
            true => {
                let frame = self.resume.get(depth).cloned();
                let frame = frame.ok_or_else(|| journal_error("a subtree is missing"))?;
                if (&frame.key, frame.position) != (&key, position) {
                    return Err(journal_error("another subtree is entered there"));
                }
                self.frames.push(frame.clone());
                Some(frame)
            }
            false => {
                self.frames.push(Frame {
                    key: key.clone(),
                    position,
                    done: None,
                });
                self.visitor.enter(place, &self.frames)?;
                None
            }
        };

        let mut read = match &place.rooted {
            Some(rooted) => Some(self.read(place, rooted)?),
            None => None,
        };
        if let Some(read) = &read {
            self.visitor.found(place, &read.found)?;
        }
        let children = read.as_mut().map(|read| std::mem::take(&mut read.children));
        let done = saved.as_ref().and_then(|frame| frame.done.clone());
        let mut in_progress = match resuming {
            true => (self.resume.get(depth + 1)).map(|frame| (frame.key.clone(), frame.position)),
            false => None,
        };
        for child in children.into_iter().flatten() {
            let id = (child.key.clone(), child.position);
            if done.as_ref().is_some_and(|done| id <= *done) {
                continue;
            }
            let child_resuming = match in_progress.take() {
                Some(expected) if expected == id => true,
                Some(_) => return Err(journal_error("the subtree that was entered is not next")),
                None => false,
            };
            let below = self.child(place, child);
            self.path.push(id.0);
            let walked = self.subtree(&below, child_resuming);
            self.path.pop();
            walked?;
        }
        if in_progress.is_some() {
            return Err(journal_error("the subtree that was entered is not there"));
        }

        match read {
            None => self.flat(place)?,
            Some(read) => {
                self.side_records(place)?;
                if self.visitor.takes_records() {
                    let merk = self.held(place, read)?;
                    self.visitor.merk(place, merk)?;
                }
            }
        }

        self.frames.pop();
        if let Some(above) = self.frames.last_mut() {
            above.done = Some((key, position));
        }
        self.visitor.leave(place, &self.frames)
    }

    /// The place of the subtree `child` below `place`.
    fn child(&self, place: &Place, child: Child) -> Place {
        let hosted = child.hosted;
        let flat = flat(&hosted);
        let mut path = self.path.clone();
        path.push(child.key.clone());
        Place {
            prefix: hosted.prefix,
            path,
            tree_type: hosted.tree_type,
            rooted: match (flat, child.position, hosted.root_key) {
                (true, ..) => None,
                // The secondary of an axis whose root the element does not
                // name is opened by the record of its root key, which the
                // read of the Merk reads.
                (false, 1.., None) => Some(Rooted::ByRecord(None)),
                (false, _, named) => Some(Rooted::ByElement(named)),
            },
            host: Some(Host {
                parent: place.prefix,
                key: child.key,
                position: child.position,
            }),
        }
    }

    /// The first read of the Merk of `place`.
    fn read(&mut self, place: &Place, rooted: &Rooted) -> Result<Read, ToolError> {
        // A root that is read from the record of the root key is read
        // from the source now.
        let rooted = match rooted {
            Rooted::ByRecord(_) => {
                Rooted::ByRecord(self.source.get(Keyspace::Roots, &place.prefix, ROOT_KEY)?)
            }
            by_element => by_element.clone(),
        };
        let mut found = Found::default();
        let mut keys: Vec<Keys> = Vec::new();
        // The bytes of each record, with its key.
        let mut lens: Vec<u64> = Vec::new();
        let mut nodes: Option<Vec<Node>> = self.visitor.takes_records().then(Vec::new);
        // The subtrees each record's element would host, by the record's
        // place, where one of them holds a record.
        let mut hosting: BTreeMap<usize, Vec<HostedSubtree>> = BTreeMap::new();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let (source, side, surveying) = (self.source, self.side, self.surveying);
        let path = &mut self.path;
        source.subtree(Keyspace::Data, &place.prefix, |key, record| {
            found.counts.of_mut(Keyspace::Data).add(key, record);
            let decoded = match decode(key, record) {
                Ok(decoded) => decoded,
                Err(error) => {
                    found.faults.push(format!(
                        "the record under the key {} is no node of a Merk: {error}",
                        crate::hex(key)
                    ));
                    return Ok(());
                }
            };
            if surveying && decoded.encode() != record {
                found.faults.push(format!(
                    "the record under the key {} is not the bytes Merk encodes of its node",
                    crate::hex(key)
                ));
            }
            key.hash(&mut hasher);
            let value = decoded.value_as_slice();
            let element = Element::deserialize(value, GroveVersion::latest());
            found.no_element += u64::from(element.is_err());
            let is_tree = element.is_ok_and(|element| element.tree_type().is_some());
            if is_tree {
                path.push(key.to_vec());
                let own = build_prefix(SubtreePath::from(path.as_slice())).value;
                path.pop();
                let hosted = hosted_by_element(value, own).map_err(|error| {
                    ToolError::Migrate(format!(
                        "the element under the key {} of {}: {error}",
                        crate::hex(key),
                        place.named()
                    ))
                })?;
                let mut entered = false;
                for hosted in &hosted {
                    entered = entered
                        || side.holds(&hosted.prefix)
                        || source.holds(Keyspace::Data, &hosted.prefix)?;
                }
                if entered {
                    hosting.insert(keys.len(), hosted);
                    // The Merk has subtrees below it: it is read again
                    // when the walk comes back.
                    nodes = None;
                }
            }
            found.held += (key.len() + value.len() + 300) as u64;
            if let Some(nodes) = &mut nodes {
                nodes.push(Node::of(&decoded, Vec::new()));
            }
            keys.push(Keys::of(&decoded));
            lens.push((key.len() + record.len()) as u64);
            Ok(())
        })?;
        for keyspace in [Keyspace::Aux, Keyspace::Meta] {
            *found.counts.of_mut(keyspace) = side.of(keyspace, &place.prefix);
        }

        let shape = match shape(&keys, &rooted) {
            Ok(shape) => shape,
            Err(fault) => {
                found.faults.push(fault.to_string());
                Shape::default()
            }
        };
        if !found.faults.is_empty() {
            // Nothing of a Merk that is not carried is walked.
            return Ok(Read {
                found,
                shape: Shape::default(),
                read_as: (0, 0),
                hosting: BTreeMap::new(),
                children: Vec::new(),
                nodes: None,
            });
        }
        found.nodes = shape.order.len() as u64;
        found.root = shape.root.map(|root| keys[root].key.clone());
        found.stale_root = shape.stale;
        let mut in_tree = vec![false; keys.len()];
        for at in &shape.order {
            in_tree[*at] = true;
        }
        for at in &shape.unlinked {
            found.unlinked.records += 1;
            found.unlinked.bytes += lens[*at];
        }
        let mut children = Vec::new();
        let mut hosts = BTreeMap::new();
        for (at, hosted) in hosting {
            if !in_tree[at] {
                continue;
            }
            found.trees += 1;
            // The cell of an element points at the Merks it hosts. A tree
            // that keeps its data as flat records hosts none: its subtree
            // is entered under the element, and the cell has no pointer
            // for it (`transactions.md` MV3).
            let merks = hosted.iter().filter(|hosted| !flat(hosted));
            hosts.insert(
                at,
                merks.map(|hosted| Subtree::new(hosted.prefix)).collect(),
            );
            let by_position = hosted.into_iter().enumerate();
            children.extend(by_position.map(|(position, hosted)| Child {
                key: keys[at].key.clone(),
                hosted,
                position: position as u8,
            }));
        }
        let read_as = (keys.len(), hasher.finish());
        // The nodes that were kept are handed over as they are: none of
        // them hosts a subtree.
        let nodes = nodes.filter(|_| children.is_empty());
        Ok(Read {
            found,
            shape,
            read_as,
            hosting: hosts,
            children,
            nodes,
        })
    }

    /// The Merk of `place` as it is handed over, from its first read: its
    /// records are read again where that read did not keep them.
    fn held(&mut self, place: &Place, read: Read) -> Result<MerkHeld, ToolError> {
        let Read {
            shape,
            read_as,
            mut hosting,
            nodes,
            ..
        } = read;
        let nodes = match nodes {
            Some(nodes) => nodes,
            None => {
                let mut nodes = Vec::with_capacity(read_as.0);
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                let changed = || {
                    ToolError::Migrate(format!(
                        "the source {}: the records of {} are not what they were when the \
                         migration first read them: something wrote the source",
                        self.source.dir().display(),
                        place.named()
                    ))
                };
                (self.source).subtree(Keyspace::Data, &place.prefix, |key, record| {
                    let decoded = decode(key, record).map_err(|_| changed())?;
                    key.hash(&mut hasher);
                    let hosted = hosting.remove(&nodes.len()).unwrap_or_default();
                    nodes.push(Node::of(&decoded, hosted));
                    Ok(())
                })?;
                if (nodes.len(), hasher.finish()) != read_as {
                    return Err(changed());
                }
                nodes
            }
        };
        let root = shape.root.map(|root| nodes[root].key.clone());
        let mut nodes: Vec<Option<Node>> = nodes.into_iter().map(Some).collect();
        let mut taken = |places: &[usize]| -> Vec<Node> {
            let taken = places.iter().map(|at| nodes[*at].take());
            taken
                .map(|node| node.expect("a record has one place"))
                .collect()
        };
        Ok(MerkHeld {
            root,
            nodes: taken(&shape.order),
            unlinked: taken(&shape.unlinked),
        })
    }

    /// The records of a tree that keeps its data flat, and its aux and
    /// meta records.
    fn flat(&mut self, place: &Place) -> Result<(), ToolError> {
        let mut found = Found::default();
        let takes = self.visitor.takes_records();
        let surveying = self.surveying;
        let mut chunk: Chunk = Vec::new();
        let mut held = 0;
        let (source, visitor, frames) = (self.source, &mut self.visitor, &self.frames);
        source.subtree(Keyspace::Data, &place.prefix, |key, value| {
            found.counts.of_mut(Keyspace::Data).add(key, value);
            found.flat.add(key, value);
            if surveying && is_keyed_child(key, value) {
                found.keyed_children += 1;
            }
            if takes {
                held += key.len() + value.len();
                chunk.push((key.to_vec(), value.to_vec()));
                if held >= CHUNK_BYTES {
                    held = 0;
                    let records = std::mem::take(&mut chunk);
                    visitor.records(place, Keyspace::Data, records, frames)?;
                }
            }
            Ok(())
        })?;
        if !chunk.is_empty() {
            visitor.records(place, Keyspace::Data, chunk, frames)?;
        }
        for keyspace in [Keyspace::Aux, Keyspace::Meta] {
            *found.counts.of_mut(keyspace) = self.side.of(keyspace, &place.prefix);
        }
        self.visitor.found(place, &found)?;
        self.side_records(place)
    }

    /// The aux and the meta records of `place`.
    fn side_records(&mut self, place: &Place) -> Result<(), ToolError> {
        if !self.visitor.takes_records() {
            return Ok(());
        }
        for keyspace in [Keyspace::Aux, Keyspace::Meta] {
            if self.side.of(keyspace, &place.prefix).records == 0 {
                continue;
            }
            let mut chunk: Chunk = Vec::new();
            let mut held = 0;
            let (source, visitor, frames) = (self.source, &mut self.visitor, &self.frames);
            source.subtree(keyspace, &place.prefix, |key, value| {
                held += key.len() + value.len();
                chunk.push((key.to_vec(), value.to_vec()));
                if held >= CHUNK_BYTES {
                    held = 0;
                    let records = std::mem::take(&mut chunk);
                    visitor.records(place, keyspace, records, frames)?;
                }
                Ok(())
            })?;
            if !chunk.is_empty() {
                visitor.records(place, keyspace, chunk, frames)?;
            }
        }
        Ok(())
    }
}

/// Whether the flat record under `key` is the record of a Merk node whose
/// value is an element: what a keyed child under a tree of flat records is
/// on RocksDB (grove versions 1 to 3).
fn is_keyed_child(key: &[u8], value: &[u8]) -> bool {
    let Ok(decoded) = decode(key, value) else {
        return false;
    };
    decoded.encode() == value
        && Element::deserialize(decoded.value_as_slice(), GroveVersion::latest()).is_ok()
}
