//! `lignin dump DIR (SUBTREE | --all) FILE` and `lignin load DIR FILE`: a
//! subtree, with every subtree below it, to a file and back.
//!
//! # What a dump holds
//!
//! The subtrees of one version, each with everything the engine keeps of
//! it: its entry (the tree type and, below the top, the element that hosts
//! it), every node of its Merk as stored (the key, the feature with its
//! aggregate, the value hash and the key-value hash, each link with the
//! child's key, hash, heights and aggregate, the Merks the node hosts and
//! the value), and every record of its aux, roots and meta columns, and of
//! its data column where it has no Merk (an append-only tree's entries).
//! `--all` dumps every subtree that no element hosts, each with every
//! subtree below it: the whole database.
//!
//! **A load writes every hash as the dump states it.** It writes the
//! nodes through a write transaction as Merk does on existing
//! GroveVersions, every hash made by the writer (`Stale::NONE`), so the
//! loaded Merks have the root hashes the dumped ones had and no rule is
//! needed. The values are written once each; the engine places them in
//! their cells or out of line by their length, as for any write.
//!
//! **Where a load puts the subtrees.** Each under the prefix it had, so a
//! subtree that the target already has is refused
//! ([`BatchError::Entered`](lignindb::txn::BatchError::Entered)), and
//! nothing is written. The top subtree of a dump is entered with no host:
//! the element that hosted it is not in the dump. A dump of a whole
//! database (`--all`) is so loaded as it was, and its tops had no host to
//! begin with. The subtrees below a top are entered under their hosts,
//! which the dump holds.
//!
//! **A load keeps the rule of anchored change sets itself.** Each Merk
//! below a top is written with its host, which names the Merk's new root,
//! and not with the node above the host. The change set of the host's
//! Merk is then marked anchored (`MerkWrite::anchored`), so the host's
//! Merk keeps its root where the node above it points, unless the host's
//! Merk is a top, which nothing points at. The transaction does not check
//! that mark: a batch that leaves it out where it belongs is taken, and
//! the store it commits is not sound.
//!
//! **A load is one commit.** It is one write transaction, applied a Merk
//! at a time and committed at its end, so after any crash the target has
//! all of the dump or none of it. The write transaction holds what it
//! wrote until the commit, the values out of line among it, so a load
//! takes memory in the size of the dump. (The engine's bulk load, TXN-10,
//! is what lays a large Merk out without that.)
//!
//! # The file
//!
//! All numbers are little-endian; `bytes8` is a length in one byte and the
//! bytes, `bytes32` a length in four.
//!
//! ```text
//! magic    "LIGNDUMP" and the format, one byte: 1
//! S        a subtree, for each in the order they are loaded, each after
//!          the one that hosts it:
//!          prefix (32), tree type (bytes8), host (0, or 1 and the host's
//!          prefix (32), key (bytes32), position (1)), root key (0, or 1
//!          and bytes32)
//! D        the data of the next subtree, in the same order: its prefix (32)
//!   N      a node: key, feature (bytes32 each), value hash, key-value hash
//!          (32 each), left and right link (0, or 1 and the child's key
//!          (bytes32), hash (32), heights (2), aggregate (bytes32)), the
//!          Merks it hosts (one byte for the count, then each prefix (32),
//!          by position), and the value (bytes32)
//!   R      a record: the column (1: data 0, aux 1, roots 2, meta 3), key
//!          and value (bytes32 each)
//! E        the end: the counts of subtrees, nodes and records (8 each)
//! ```
//!
//! The counts at the end tell a whole dump from a cut one. A load checks
//! that the file agrees with itself before it commits: each subtree's
//! root key is the key of one of its nodes (and a subtree with no root
//! has no node), and each Merk a node names is a subtree whose entry has
//! that node for its host, at that position, as each entry with a host is
//! named by its host's node. **The dump has no checksum.** `lignin
//! verify`, on the database a load made, makes again the hashes that
//! Merk's rules make from the pages (none where the database holds the
//! mark of an incomplete restore), and a changed byte is found where one
//! of them covers it. Changes to bytes outside hash coverage can go
//! undetected: bytes of a flat record (of the aux, roots or meta columns,
//! or of the data of a subtree with no Merk), of a reference's value,
//! whose value hash the rules take as stored, and of a part of a value
//! that Merk leaves out of its hash (`lignin verify`).

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
    path::Path,
};

use lignin_format::tree_page::{Hash, Side};
use lignin_tree::{hash::Stale, value::ValueBuf};
use lignindb::{
    commit::{Committed, NoStaleHashes},
    snapshot::{Listed, Operation},
    txn::{
        Batch, ChildKeys, Column, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite,
        RootPut, Subtree, SubtreeWrite, ValuePut,
    },
};

use crate::{args::Common, args::DumpWhat, hex, open::open, Outcome, ToolError};

const MAGIC: &[u8; 8] = b"LIGNDUMP";
const FORMAT: u8 = 1;
const SUBTREE: u8 = b'S';
const DATA: u8 = b'D';
const NODE: u8 = b'N';
const RECORD: u8 = b'R';
const END: u8 = b'E';

/// The columns as a dump numbers them.
const COLUMNS: [Column; 4] = [Column::Data, Column::Aux, Column::Roots, Column::Meta];

/// The element that hosts a subtree, as the dump holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Host {
    parent: Subtree,
    key: Vec<u8>,
    position: u8,
}

/// A subtree as the dump holds its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entered {
    subtree: Subtree,
    tree_type: Vec<u8>,
    host: Option<Host>,
    root: Option<Vec<u8>>,
}

/// A link of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Link {
    key: Vec<u8>,
    hash: Hash,
    heights: (u8, u8),
    aggregate: Vec<u8>,
}

/// A node as the dump holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    key: Vec<u8>,
    feature: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    left: Option<Link>,
    right: Option<Link>,
    hosted: Vec<Subtree>,
    value: Vec<u8>,
}

impl Node {
    fn link(link: &Option<Link>) -> Option<LinkPut<'_>> {
        link.as_ref().map(|link| LinkPut {
            key: &link.key,
            hash: &link.hash,
            heights: link.heights,
            aggregate: &link.aggregate,
        })
    }

    /// The write of the node, with its value or keeping the one this
    /// transaction stated.
    fn put<'a>(&'a self, value: ValuePut<'a>) -> NodePut<'a> {
        NodePut {
            key: &self.key,
            feature: &self.feature,
            value_hash: &self.value_hash,
            kv_hash: &self.kv_hash,
            left: Self::link(&self.left),
            right: Self::link(&self.right),
            hosted: Hosted::Set(&self.hosted),
            value,
            stale: Stale::NONE,
            at: None,
        }
    }
}

// The writing side.

struct Out<W> {
    file: W,
    bytes: u64,
}

impl<W: Write> Out<W> {
    fn raw(&mut self, bytes: &[u8]) -> Result<(), ToolError> {
        self.file.write_all(bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    fn bytes8(&mut self, bytes: &[u8]) -> Result<(), ToolError> {
        let len =
            u8::try_from(bytes.len()).map_err(|_| dump_error("a tree type over 255 bytes"))?;
        self.raw(&[len])?;
        self.raw(bytes)
    }

    fn bytes32(&mut self, bytes: &[u8]) -> Result<(), ToolError> {
        let len = u32::try_from(bytes.len()).map_err(|_| dump_error("a value over 4 GiB"))?;
        self.raw(&len.to_le_bytes())?;
        self.raw(bytes)
    }

    fn option32(&mut self, bytes: Option<&[u8]>) -> Result<(), ToolError> {
        match bytes {
            None => self.raw(&[0]),
            Some(bytes) => {
                self.raw(&[1])?;
                self.bytes32(bytes)
            }
        }
    }

    fn entered(&mut self, entered: &Entered) -> Result<(), ToolError> {
        self.raw(&[SUBTREE])?;
        self.raw(entered.subtree.as_bytes())?;
        self.bytes8(&entered.tree_type)?;
        match &entered.host {
            None => self.raw(&[0])?,
            Some(host) => {
                self.raw(&[1])?;
                self.raw(host.parent.as_bytes())?;
                self.bytes32(&host.key)?;
                self.raw(&[host.position])?;
            }
        }
        self.option32(entered.root.as_deref())
    }

    fn node(&mut self, node: &Node) -> Result<(), ToolError> {
        self.raw(&[NODE])?;
        self.bytes32(&node.key)?;
        self.bytes32(&node.feature)?;
        self.raw(&node.value_hash)?;
        self.raw(&node.kv_hash)?;
        for link in [&node.left, &node.right] {
            match link {
                None => self.raw(&[0])?,
                Some(link) => {
                    self.raw(&[1])?;
                    self.bytes32(&link.key)?;
                    self.raw(&link.hash)?;
                    self.raw(&[link.heights.0, link.heights.1])?;
                    self.bytes32(&link.aggregate)?;
                }
            }
        }
        let count = u8::try_from(node.hosted.len())
            .map_err(|_| dump_error("a node hosts over 255 Merks"))?;
        self.raw(&[count])?;
        for hosted in &node.hosted {
            self.raw(hosted.as_bytes())?;
        }
        self.bytes32(&node.value)
    }
}

fn dump_error(what: impl Into<String>) -> ToolError {
    ToolError::Dump(what.into())
}

/// The subtrees a dump of `what` holds, in the order they are loaded: each
/// top, then the subtrees it hosts, depth first, each after its host.
fn subtrees(listed: Vec<Listed>, what: DumpWhat) -> Result<Vec<Entered>, ToolError> {
    let live = listed.into_iter().filter(|listed| listed.live);
    let mut entries: BTreeMap<Subtree, Entered> = BTreeMap::new();
    let mut children: HashMap<Subtree, Vec<(Vec<u8>, u8, Subtree)>> = HashMap::new();
    for listed in live {
        let entry = &listed.entry;
        let host = entry.host().map(|host| Host {
            parent: Subtree::new(*host.parent),
            key: host.key.to_vec(),
            position: host.position,
        });
        if let Some(host) = &host {
            let under = children.entry(host.parent).or_default();
            under.push((host.key.clone(), host.position, listed.subtree));
        }
        entries.insert(
            listed.subtree,
            Entered {
                subtree: listed.subtree,
                tree_type: entry.tree_type().to_vec(),
                host,
                root: entry.root().map(|root| root.key.to_vec()),
            },
        );
    }
    let tops: Vec<Subtree> = match what {
        DumpWhat::All => (entries.values())
            .filter(|entered| entered.host.is_none())
            .map(|entered| entered.subtree)
            .collect(),
        DumpWhat::Subtree(subtree) if entries.contains_key(&subtree) => vec![subtree],
        DumpWhat::Subtree(subtree) => {
            return Err(dump_error(format!(
                "the latest version has no subtree {} in use",
                hex(subtree.as_bytes())
            )));
        }
    };
    let mut order = Vec::new();
    let mut stack: Vec<(Subtree, bool)> = tops.into_iter().rev().map(|top| (top, true)).collect();
    while let Some((subtree, top)) = stack.pop() {
        let mut entered = entries
            .remove(&subtree)
            .expect("each subtree is hosted once");
        if top {
            entered.host = None;
        }
        order.push(entered);
        if let Some(mut below) = children.remove(&subtree) {
            below.sort();
            stack.extend(below.into_iter().rev().map(|(_, _, child)| (child, false)));
        }
    }
    Ok(order)
}

/// The Merks the nodes of a subtree host, by the node's key, in the order
/// of their positions.
type HostedBy = BTreeMap<Vec<u8>, Vec<Subtree>>;

/// The Merks each node of each subtree of `order` hosts, from the entries
/// that name them.
fn hosted_by(order: &[Entered]) -> Result<HashMap<Subtree, HostedBy>, ToolError> {
    /// The Merks a node hosts, with their positions as their entries give
    /// them.
    type Positioned = BTreeMap<Vec<u8>, Vec<(u8, Subtree)>>;
    let mut hosted: HashMap<Subtree, Positioned> = HashMap::new();
    for entered in order {
        if let Some(host) = &entered.host {
            let at = hosted.entry(host.parent).or_default();
            at.entry(host.key.clone())
                .or_default()
                .push((host.position, entered.subtree));
        }
    }
    let by_subtree = hosted.into_iter().map(|(subtree, nodes)| {
        let nodes = nodes.into_iter().map(|(key, mut merks)| {
            merks.sort();
            let positions = merks.iter().enumerate();
            if positions
                .clone()
                .any(|(at, (position, _))| usize::from(*position) != at)
            {
                return Err(dump_error(format!(
                    "the node {} of {} hosts Merks at positions with a gap",
                    hex(&key),
                    hex(subtree.as_bytes())
                )));
            }
            Ok((key, merks.into_iter().map(|(_, merk)| merk).collect()))
        });
        Ok((subtree, nodes.collect::<Result<HostedBy, _>>()?))
    });
    by_subtree.collect()
}

/// Writes the nodes and the records of `entered` as `operation` reads
/// them. Returns how many of each.
fn write_data<W: Write>(
    out: &mut Out<W>,
    operation: &Operation<'_>,
    entered: &Entered,
    hosted: Option<&HostedBy>,
) -> Result<(u64, u64), ToolError> {
    out.raw(&[DATA])?;
    out.raw(entered.subtree.as_bytes())?;
    let opened = operation.open(entered.subtree)?;
    let opened = opened.ok_or_else(|| dump_error("a subtree went while it was dumped"))?;
    let (mut nodes, mut records) = (0, 0);
    let mut walk = opened.walk();
    walk.seek_to_first(operation)?;
    let mut children = ChildKeys::new();
    let mut buf = ValueBuf::new();
    while walk.key().is_some() {
        let read = walk.node_with_children(operation, &mut children, |view| {
            let view = view.expect("the walk stands on a node");
            let link = |side| {
                view.link(side).map(|link| Link {
                    key: Vec::new(),
                    hash: *link.hash,
                    heights: link.heights,
                    aggregate: link.aggregate.to_vec(),
                })
            };
            let value = view.value(&mut buf).map(<[u8]>::to_vec);
            value.map(|value| Node {
                key: view.key().to_vec(),
                feature: view.feature().to_vec(),
                value_hash: *view.value_hash(),
                kv_hash: *view.kv_hash(),
                left: link(Side::Left),
                right: link(Side::Right),
                hosted: Vec::new(),
                value,
            })
        })?;
        let mut node = read?;
        for (side, link) in [(Side::Left, &mut node.left), (Side::Right, &mut node.right)] {
            if let Some(link) = link {
                link.key = children.take(side);
            }
        }
        node.hosted = (hosted.and_then(|hosted| hosted.get(&node.key)))
            .cloned()
            .unwrap_or_default();
        out.node(&node)?;
        nodes += 1;
        walk.next(operation)?;
    }
    let columns = COLUMNS.iter().enumerate();
    for (code, &column) in columns {
        // The data column of a subtree with a Merk holds its nodes.
        if column == Column::Data && entered.root.is_some() {
            continue;
        }
        let mut cursor = opened.cursor(column);
        cursor.seek_to_first()?;
        let mut value = Vec::new();
        while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
            value.clear();
            if cursor.value(&mut value)? {
                out.raw(&[RECORD, code as u8])?;
                out.bytes32(&key)?;
                out.bytes32(&value)?;
                records += 1;
            }
            cursor.next()?;
        }
    }
    Ok((nodes, records))
}

/// Dumps `what` of the database in `common.dir` to `file`, which must not
/// be there, and writes what it dumped to `out`. A dump that returns an
/// error, an error of the close of the database or of the report
/// included, removes the file it made; where that removal fails, the
/// error says so and names the file.
pub fn dump(
    common: &Common,
    what: &DumpWhat,
    file: &Path,
    out: &mut dyn Write,
) -> Result<Outcome, ToolError> {
    let db = open(common)?;
    let created = OpenOptions::new().write(true).create_new(true).open(file)?;
    let written = (|| {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        let mut listed = Vec::new();
        let mut walk = operation.entries()?;
        while let Some(entry) = walk.next()? {
            listed.push(entry);
        }
        let order = subtrees(listed, *what)?;
        let mut dump = Out {
            file: BufWriter::new(&created),
            bytes: 0,
        };
        dump.raw(MAGIC)?;
        dump.raw(&[FORMAT])?;
        for entered in &order {
            dump.entered(entered)?;
        }
        let (mut nodes, mut records) = (0, 0);
        let hosted = hosted_by(&order)?;
        for entered in &order {
            let hosted = hosted.get(&entered.subtree);
            let (n, r) = write_data(&mut dump, &operation, entered, hosted)?;
            nodes += n;
            records += r;
        }
        dump.raw(&[END])?;
        for count in [order.len() as u64, nodes, records] {
            dump.raw(&count.to_le_bytes())?;
        }
        dump.file.flush()?;
        created.sync_all()?;
        Ok::<_, ToolError>((operation.at(), order.len(), nodes, records, dump.bytes))
    })();
    let finished = written.and_then(|(at, subtrees, nodes, records, bytes)| {
        db.close()?;
        writeln!(
            out,
            "dumped version {at}: {subtrees} subtrees, {nodes} nodes, {records} records, \
             {bytes} bytes to {}",
            file.display()
        )?;
        Ok(Outcome::Done)
    });
    let Err(error) = finished else {
        return finished;
    };
    drop(created);
    match fs::remove_file(file) {
        Ok(()) => Err(error),
        Err(left) => Err(ToolError::Dump(format!(
            "{error}; and the file it made, {}, could not be removed: {left}",
            file.display()
        ))),
    }
}

// The reading side.

struct In<R> {
    file: R,
}

impl<R: Read> In<R> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ToolError> {
        let mut bytes = [0; N];
        self.file
            .read_exact(&mut bytes)
            .map_err(|error| match error.kind() {
                ErrorKind::UnexpectedEof => dump_error("it ends before its end record: it was cut"),
                _ => ToolError::Io(error),
            })?;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, ToolError> {
        Ok(self.array::<1>()?[0])
    }

    fn vec(&mut self, len: usize) -> Result<Vec<u8>, ToolError> {
        let mut bytes = Vec::new();
        let read = (&mut self.file).take(len as u64).read_to_end(&mut bytes)?;
        match read == len {
            true => Ok(bytes),
            false => Err(dump_error("it ends before its end record: it was cut")),
        }
    }

    fn bytes8(&mut self) -> Result<Vec<u8>, ToolError> {
        let len = self.byte()?;
        self.vec(usize::from(len))
    }

    fn bytes32(&mut self) -> Result<Vec<u8>, ToolError> {
        let len = u32::from_le_bytes(self.array()?);
        self.vec(len as usize)
    }

    fn flag(&mut self) -> Result<bool, ToolError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(dump_error(format!("{other} where 0 or 1 belongs"))),
        }
    }

    fn subtree(&mut self) -> Result<Subtree, ToolError> {
        Ok(Subtree::new(self.array()?))
    }

    fn entered(&mut self) -> Result<Entered, ToolError> {
        let subtree = self.subtree()?;
        let tree_type = self.bytes8()?;
        let host = match self.flag()? {
            false => None,
            true => Some(Host {
                parent: self.subtree()?,
                key: self.bytes32()?,
                position: self.byte()?,
            }),
        };
        let root = match self.flag()? {
            false => None,
            true => Some(self.bytes32()?),
        };
        Ok(Entered {
            subtree,
            tree_type,
            host,
            root,
        })
    }

    fn node(&mut self) -> Result<Node, ToolError> {
        let key = self.bytes32()?;
        let feature = self.bytes32()?;
        let value_hash = self.array()?;
        let kv_hash = self.array()?;
        let mut link = || -> Result<Option<Link>, ToolError> {
            if !self.flag()? {
                return Ok(None);
            }
            let key = self.bytes32()?;
            let hash = self.array()?;
            let [low, high] = self.array()?;
            Ok(Some(Link {
                key,
                hash,
                heights: (low, high),
                aggregate: self.bytes32()?,
            }))
        };
        let (left, right) = (link()?, link()?);
        let count = self.byte()?;
        let hosted = (0..count)
            .map(|_| self.subtree())
            .collect::<Result<_, _>>()?;
        Ok(Node {
            key,
            feature,
            value_hash,
            kv_hash,
            left,
            right,
            hosted,
            value: self.bytes32()?,
        })
    }
}

/// What a load has to write of one subtree.
#[derive(Default)]
struct Data {
    nodes: Vec<Node>,
    records: Vec<(Column, Vec<u8>, Vec<u8>)>,
}

/// Refuses the data of `entered`, `nodes`, where the dump does not agree
/// with itself: a root key that is no node's key (or nodes and no root),
/// a Merk a node names whose entry does not have that node at that
/// position for its host, and an entry with a host whose node, read
/// before it in `hosts`, does not name it there.
fn agrees(
    entered: &Entered,
    nodes: &[Node],
    hosted: &HashMap<Subtree, &Host>,
    hosts: &HashMap<(Subtree, Vec<u8>), Node>,
) -> Result<(), ToolError> {
    let subtree = hex(entered.subtree.as_bytes());
    let rooted = match &entered.root {
        None => nodes.is_empty(),
        Some(root) => nodes.iter().any(|node| node.key == *root),
    };
    if !rooted {
        return Err(dump_error(format!(
            "the root its entry gives {subtree} is not one of the {} nodes it holds of it",
            nodes.len()
        )));
    }
    for node in nodes {
        for (position, merk) in node.hosted.iter().enumerate() {
            let named = hosted.get(merk).is_some_and(|host| {
                (host.parent, &host.key, usize::from(host.position))
                    == (entered.subtree, &node.key, position)
            });
            if !named {
                return Err(dump_error(format!(
                    "the node {} of {subtree} names {} at position {position}, and that entry \
                     does not name the node there for its host",
                    hex(&node.key),
                    hex(merk.as_bytes())
                )));
            }
        }
    }
    if let Some(host) = hosted.get(&entered.subtree) {
        let names = hosts
            .get(&(host.parent, host.key.clone()))
            .and_then(|node| node.hosted.get(usize::from(host.position)));
        if names != Some(&entered.subtree) {
            return Err(dump_error(format!(
                "the entry of {subtree} has the node {} of {} for its host, and that node, \
                 which comes before it, does not name it at position {}",
                hex(&host.key),
                hex(host.parent.as_bytes()),
                host.position
            )));
        }
    }
    Ok(())
}

/// Loads the dump `file` into the database in `common.dir`, in one
/// commit, and writes what it loaded to `out`.
pub fn load(common: &Common, file: &Path, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let mut dump = In {
        file: BufReader::new(File::open(file)?),
    };
    if dump.array::<8>()? != *MAGIC {
        return Err(dump_error("it is no dump of the lignin command"));
    }
    let format = dump.byte()?;
    if format != FORMAT {
        return Err(dump_error(format!(
            "format {format}, and this build reads {FORMAT}"
        )));
    }
    let mut order = Vec::new();
    let mut tag = dump.byte()?;
    while tag == SUBTREE {
        order.push(dump.entered()?);
        tag = dump.byte()?;
    }
    let db = open(common)?;
    let txn = db.begin();
    let scope = txn.scope()?;
    let enters: Vec<SubtreeWrite<'_>> = (order.iter())
        .map(|entered| SubtreeWrite::Enter {
            subtree: entered.subtree,
            tree_type: &entered.tree_type,
            host: entered.host.as_ref().map(|host| HostPut {
                subtree: host.parent,
                key: &host.key,
                position: host.position,
            }),
        })
        .collect();
    scope.apply(&Batch {
        subtrees: &enters,
        ..Batch::default()
    })?;
    // The subtrees that the load enters under a host, with the host.
    let hosted: HashMap<Subtree, &Host> = (order.iter())
        .filter_map(|entered| Some((entered.subtree, entered.host.as_ref()?)))
        .collect();
    // The nodes that host a Merk, kept to name it again once it has its
    // root.
    let mut hosts: HashMap<(Subtree, Vec<u8>), Node> = HashMap::new();
    let (mut nodes, mut records) = (0u64, 0u64);
    for entered in &order {
        if tag != DATA || dump.subtree()? != entered.subtree {
            return Err(dump_error(
                "the data of its subtrees are not in their order",
            ));
        }
        let mut data = Data::default();
        tag = dump.byte()?;
        loop {
            match tag {
                NODE => data.nodes.push(dump.node()?),
                RECORD => {
                    let code = dump.byte()?;
                    let column = *COLUMNS
                        .get(usize::from(code))
                        .ok_or_else(|| dump_error(format!("no column {code}")))?;
                    data.records
                        .push((column, dump.bytes32()?, dump.bytes32()?));
                }
                _ => break,
            }
            tag = dump.byte()?;
        }
        agrees(entered, &data.nodes, &hosted, &hosts)?;
        {
            nodes += data.nodes.len() as u64;
            records += data.records.len() as u64;
            let puts: Vec<NodePut<'_>> = (data.nodes.iter())
                .map(|node| node.put(ValuePut::Value(&node.value)))
                .collect();
            let mut merks = Vec::with_capacity(2);
            if !puts.is_empty() {
                merks.push(MerkWrite {
                    subtree: entered.subtree,
                    nodes: &puts,
                    removed: &[],
                    root: RootPut::Set(entered.root.as_deref()),
                    anchored: false,
                });
            }
            // The host names the Merk's root: it is written again, with the
            // value this transaction stated for it.
            let host = (entered.host.as_ref())
                .filter(|_| !puts.is_empty())
                .map(|host| {
                    let node = hosts.get(&(host.parent, host.key.clone()));
                    node.ok_or_else(|| dump_error("a Merk comes before the node that hosts it"))
                        .map(|node| (host.parent, [node.put(ValuePut::Keep)]))
                })
                .transpose()?;
            if let Some((parent, host)) = &host {
                // The batch does not write the node above the host, so
                // the host's Merk keeps its root where that node points.
                merks.push(MerkWrite {
                    subtree: *parent,
                    nodes: host,
                    removed: &[],
                    root: RootPut::Keep,
                    anchored: hosted.contains_key(parent),
                });
            }
            let writes: Vec<RecordWrite<'_>> = (data.records.iter())
                .map(|(column, key, value)| RecordWrite::put(*column, entered.subtree, key, value))
                .collect();
            scope.apply(&Batch {
                records: &writes,
                merks: &merks,
                subtrees: &[],
            })?;
        }
        for node in data.nodes {
            if !node.hosted.is_empty() {
                let value_less = Node {
                    value: Vec::new(),
                    ..node
                };
                hosts.insert((entered.subtree, value_less.key.clone()), value_less);
            }
        }
    }
    if tag != END {
        return Err(dump_error("its data do not end with its end record"));
    }
    let counts = [dump.array::<8>()?, dump.array()?, dump.array()?].map(u64::from_le_bytes);
    if counts != [order.len() as u64, nodes, records] {
        return Err(dump_error(format!(
            "its end record counts {counts:?}, and it holds {:?}",
            [order.len() as u64, nodes, records]
        )));
    }
    if dump.file.read(&mut [0])? != 0 {
        return Err(dump_error("bytes follow its end record"));
    }
    scope.merge();
    let version = match db.commit(&txn, &mut NoStaleHashes)? {
        Committed::Published(published) => published.version.to_string(),
        Committed::Empty => "none: the dump holds no subtree".to_owned(),
    };
    drop(txn);
    db.flush()?;
    db.close()?;
    writeln!(
        out,
        "loaded {} subtrees, {nodes} nodes, {records} records from {}; committed version {version}",
        order.len(),
        file.display()
    )?;
    Ok(Outcome::Done)
}
