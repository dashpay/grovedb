//! What a source holds beside its grove: the records under a prefix that
//! no element leads to and no pending-drop record names.
//!
//! RocksDB keeps such records where the element of a tree was removed and
//! the records of the tree were not: by a delete in a batch of a tree
//! that still held elements, and by a clear that was told to look for no
//! tree. The engine of GroveDB on LigninDB drops the subtree of such a
//! tree with its element, and keeps the record of its root key alone
//! (`engine.md`, "A subtree leaves with its element").
//!
//! So a migration is told what becomes of them
//! ([`Unreachable`](crate::args::Unreachable)), and the
//! survey says of each such prefix what it holds ([`Orphan`]), so that it
//! can be matched to the operation that left it.
//!
//! # What a migration that carries them writes
//!
//! For each such prefix, in one commit: its subtree is entered with no
//! host, every data record is written as a record that no tree links
//! (`lignindb::txn::MerkWrite`, "Unlinked records"), and its aux and meta
//! records are written as they are. The tree type of the entry is the one
//! the features of the nodes name, and a normal tree where the prefix has
//! no data record. A prefix whose data records are not the nodes of one
//! such type, each as Merk encodes it, is not carried by this: the source
//! is refused.
//!
//! The record of the root key under such a prefix is carried whatever the
//! choice is, with every other record of a root key ([`super::load`]).
//!
//! # What the two choices give a later operation
//!
//! With the records left behind, the target is what a database written on
//! LigninDB holds. Two reads by the path of such a tree answer otherwise
//! than on RocksDB: a read that asks for no element on the way
//! (`has_raw`) finds nothing, and a tree that is made again at the path
//! is empty (`is_empty_tree`). And a cursor of another subtree that steps
//! from no key comes to the records that lie beside its subtree in the
//! keyspace (`flat-store.md`, "A step from no key"), of which the records
//! under such a prefix are then none: that changes an answer where fewer
//! records of the grove lie on that side than the cursor steps. The
//! survey says how many prefixes of the grove lie before and after each
//! such prefix.
//!
//! With the records carried, those reads answer as on RocksDB. **A write
//! below a tree that is made again at such a path is refused by the engine
//! as it stands**: the subtree is entered under no element, and the
//! engine takes no write of an element's subtree that a write by its
//! prefix entered. The test
//! `records_that_nothing_leads_to_are_refused_left_or_carried` of
//! `lignin-testkit` holds both.

use std::collections::{BTreeMap, BTreeSet};

use grovedb::Element;
use grovedb_merk::{tree::AggregateData, TreeType};
use grovedb_storage::flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord};
use grovedb_version::version::GroveVersion;

use super::{
    grove::{decode, Node},
    source::{after, Count, Counts, Keyspace, Prefix, Source},
    walk::{short_key, Side, ROOT_KEY},
};
use crate::{hex, ToolError};

/// How many records of a prefix the survey describes.
const DESCRIBED: usize = 4;

/// How many bytes of a key or of an element the survey shows.
const SHOWN: usize = 96;

/// What the data records under a prefix that nothing leads to are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Held {
    /// No data record: aux or meta records alone.
    NoData,
    /// The nodes of a Merk of a tree of this type, each as Merk encodes
    /// it.
    Nodes(TreeType),
    /// Something else, in words.
    Other(String),
}

/// Records under a prefix that no element leads to and no pending-drop
/// record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Orphan {
    /// The prefix.
    pub(crate) prefix: Prefix,
    /// What the source holds under it in the data, the aux and the meta
    /// keyspace.
    pub(crate) counts: Counts,
    /// What its data records are.
    pub(crate) held: Held,
    /// The first data records, in words: the key, and the element its
    /// node holds.
    pub(crate) records: Vec<String>,
    /// The key that the record of the root key under the prefix names.
    pub(crate) root_key: Option<Vec<u8>>,
    /// The path the prefix is of, where it was found.
    pub(crate) path: Option<String>,
    /// Whether an element that is a tree stands at that path today.
    pub(crate) tree_stands: Option<bool>,
    /// How many prefixes of the grove's subtrees come before this one in
    /// the order of the prefixes, and how many after it.
    pub(crate) around: (u64, u64),
    /// The bytes of the keys and values of its nodes, where its data
    /// records are those of a Merk: what a walk counts for a node of a
    /// Merk it comes to.
    pub(crate) weighs: u64,
}

impl Orphan {
    /// Whether a migration that carries what nothing leads to carries
    /// this.
    pub(crate) fn is_carried(&self) -> bool {
        !matches!(self.held, Held::Other(_))
    }
}

fn shown(bytes: &[u8]) -> String {
    match bytes.len() > SHOWN {
        true => format!("{}.. ({} bytes)", hex(&bytes[..SHOWN]), bytes.len()),
        false => hex(bytes),
    }
}

/// What the source holds under `prefix` in `keyspace`, counted record by
/// record.
pub(crate) fn counted(
    source: &Source,
    keyspace: Keyspace,
    prefix: &Prefix,
) -> Result<Count, ToolError> {
    let mut count = Count::default();
    source.subtree(keyspace, prefix, |key, value| {
        count.add(key, value);
        Ok(())
    })?;
    Ok(count)
}

/// Every prefix that holds a record in the data, the aux or the meta
/// keyspace of `source` and that `is_place` does not say a subtree of the
/// grove is under, with what it holds. The namespace of the pending-drop
/// records is passed over. `side` is the source's aux and meta keyspaces.
pub(crate) fn beside(
    source: &Source,
    side: &Side,
    mut is_place: impl FnMut(&Prefix) -> Result<bool, ToolError>,
) -> Result<BTreeMap<Prefix, Counts>, ToolError> {
    let namespace = *pending_prefix_drops_namespace();
    let mut beside: BTreeMap<Prefix, Counts> = BTreeMap::new();
    // The data keyspace is the large one: the prefixes of the grove are
    // skipped by a seek, not read.
    let mut records = source.records(Keyspace::Data);
    while let Some((stored, _)) = records.current()? {
        let Some((prefix, _)) = stored.split_first_chunk::<32>() else {
            return Err(short_key(source, Keyspace::Data, stored));
        };
        let prefix = *prefix;
        if prefix != namespace && !is_place(&prefix)? {
            let count = counted(source, Keyspace::Data, &prefix)?;
            *beside.entry(prefix).or_default().of_mut(Keyspace::Data) = count;
        }
        match after(&prefix) {
            Some(next) => records.seek(&next),
            None => break,
        }
    }
    for keyspace in [Keyspace::Aux, Keyspace::Meta] {
        for (prefix, count) in side.prefixes(keyspace) {
            if *prefix != namespace && !is_place(prefix)? {
                *beside.entry(*prefix).or_default().of_mut(keyspace) = *count;
            }
        }
    }
    Ok(beside)
}

/// The prefixes that the pending-drop records of `source` name.
pub(crate) fn doomed(source: &Source) -> Result<BTreeSet<Prefix>, ToolError> {
    let mut doomed = BTreeSet::new();
    let namespace = pending_prefix_drops_namespace();
    source.subtree(Keyspace::Meta, namespace, |key, value| {
        let record = PendingPrefixDropRecord::decode(key, value).map_err(|error| {
            ToolError::Migrate(format!(
                "the source {}: a pending-drop record: {error}",
                source.dir().display()
            ))
        })?;
        doomed.extend(record.doomed_prefixes);
        Ok(())
    })?;
    Ok(doomed)
}

/// The data records under `prefix`, which nothing leads to, as the nodes
/// they are, with the tree type their features name: `None` where the
/// prefix has no data record. The inner error says why they are not the
/// nodes of one tree type, each as Merk encodes it.
///
/// With `keep` unset no node is kept, and the list is empty: the tree
/// type alone is asked for.
pub(crate) fn nodes(source: &Source, prefix: &Prefix, keep: bool) -> Result<NodesOf, ToolError> {
    let mut nodes = Vec::new();
    let mut tree_type = None;
    let mut other = None;
    let mut weighs = 0;
    source.subtree(Keyspace::Data, prefix, |key, record| {
        if other.is_some() {
            return Ok(());
        }
        let decoded = match decode(key, record) {
            Ok(decoded) if decoded.encode() == record => decoded,
            Ok(_) => {
                other = Some(format!(
                    "the record under the key {} is not the bytes Merk encodes of its node",
                    shown(key)
                ));
                return Ok(());
            }
            Err(error) => {
                other = Some(format!(
                    "the record under the key {} is no node of a Merk: {error}",
                    shown(key)
                ));
                return Ok(());
            }
        };
        let of = AggregateData::from(decoded.feature_type()).parent_tree_type();
        if tree_type.is_some_and(|before| before != of) {
            other = Some(format!(
                "the node under the key {} is of a {of}, and the nodes before it are of a {}",
                shown(key),
                tree_type.unwrap_or(of)
            ));
            return Ok(());
        }
        tree_type = Some(of);
        weighs += (key.len() + decoded.value_as_slice().len()) as u64;
        if keep {
            nodes.push(Node::of(&decoded, Vec::new()));
        }
        Ok(())
    })?;
    Ok(match other {
        Some(other) => Err(other),
        None => Ok((tree_type, nodes, weighs)),
    })
}

/// The tree type that the nodes under a prefix name, with the nodes and
/// the bytes of their keys and values, or why the records there are not
/// the nodes of one tree type.
pub(crate) type NodesOf = Result<(Option<TreeType>, Vec<Node>, u64), String>;

/// Looks at the records under the prefix of `orphan`: what its data
/// records are, the first of them in words, and the key that its record
/// of a root key names.
pub(crate) fn inspect(source: &Source, orphan: &mut Orphan) -> Result<(), ToolError> {
    orphan.root_key = source.get(Keyspace::Roots, &orphan.prefix, ROOT_KEY)?;
    (orphan.held, orphan.weighs) = match nodes(source, &orphan.prefix, false)? {
        Ok((None, ..)) => (Held::NoData, 0),
        Ok((Some(tree_type), _, weighs)) => (Held::Nodes(tree_type), weighs),
        Err(other) => (Held::Other(other), 0),
    };
    let mut described = Vec::new();
    source.subtree(Keyspace::Data, &orphan.prefix, |key, record| {
        if described.len() == DESCRIBED {
            return Ok(());
        }
        let what = match decode(key, record) {
            Err(_) => format!("{} bytes that are no node of a Merk", record.len()),
            Ok(node) => {
                let value = node.value_as_slice();
                match Element::deserialize(value, GroveVersion::latest()) {
                    Err(_) => format!("a node whose value of {} bytes is no element", value.len()),
                    Ok(element) => {
                        let mut element = element.to_string();
                        if element.len() > SHOWN {
                            let cut = (0..=SHOWN).rev().find(|at| element.is_char_boundary(*at));
                            element.truncate(cut.unwrap_or(0));
                            element.push_str("..");
                        }
                        format!("a node of a {} that holds {element}", {
                            AggregateData::from(node.feature_type()).parent_tree_type()
                        })
                    }
                }
            }
        };
        described.push(format!("key {}: {what}", shown(key)));
        Ok(())
    })?;
    orphan.records = described;
    Ok(())
}
