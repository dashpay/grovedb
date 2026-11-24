//! The survey of a source: what it holds, and whether every record of it
//! can be carried, found out before anything is written.
//!
//! The survey walks the grove ([`super::walk`]) and then reads every
//! keyspace of the source for what the walk did not come to. Each record
//! of the source is one of these, and is counted as it:
//!
//! | The record | What becomes of it |
//! | --- | --- |
//! | a node of the Merk of a subtree that an element leads to | carried, with its Merk handed over whole |
//! | a node record under such a prefix that the Merk's tree does not link | carried as an unlinked record |
//! | a flat record of a tree that keeps flat records | carried as it is |
//! | the node of a keyed child among the flat records of such a tree (grove versions 1 to 3) | **not carried: the source is refused**. The engine keeps such a child as a node of a Merk under the prefix of the tree, which the tree's element hosts, and what a child that is a tree holds lies under a prefix of its own (`engine.md`, "What differs on grove versions 1 to 3 under a tree that keeps flat records"). Those versions hold the key of a child to nothing, so which records of the prefix are such nodes is not told by where they lie. The survey takes for one every flat record whose bytes are those Merk encodes of a node under that key with an element for its value. A record that the tree was given itself with those very bytes is taken for one too, and refuses the source |
//! | an aux or a meta record of such a subtree | carried |
//! | the record of a root key, under any prefix | carried, where the engine keeps it: also under a prefix with no tree |
//! | a pending-drop record | carried, with a mark for each keyspace of each prefix it names in which the source still holds a record |
//! | a record under a prefix that a pending-drop record names and no element leads to | left: the engine dropped that subtree with its element, and the mark stands for what RocksDB still holds |
//! | a data, aux or meta record under any other prefix | as the migration is told ([`Unreachable`]): **the source is refused**, the record is left behind, or it is carried ([`super::beside`]) |
//!
//! A source is also refused for a Merk whose records are not one tree
//! under the root that is named for it, a record that Merk does not decode
//! or encodes otherwise, an element that names a root while no record is
//! under the prefix of its subtree, a record of the roots keyspace that is
//! no record of a root key, a record under the prefix of the pending-drop
//! records that is none, an aux record of the root under the key of the
//! migrator's journal, the mark of a state-sync restore that did not
//! finish (a database with that mark is to be discarded, on either
//! engine), and a grove that is deeper than the walk follows
//! (`walk::DEEPEST`).

use std::collections::{BTreeMap, BTreeSet};

use grovedb::Element;
use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, tree::NULL_HASH, CryptoHash};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord},
    prefix::{build_prefix, secondary_prefix_for},
};
use grovedb_version::version::GroveVersion;

use super::{
    beside::{beside, inspect, Held, Orphan},
    grove::decode,
    journal::JOURNAL_KEY,
    source::{Count, Counts, Keyspace, Prefix, Source},
    walk::{
        named, short_key, Chunk, Found, Frame, MerkHeld, Place, Side, Visit, Walk, HELD_A_NODE,
        ROOT, ROOT_KEY,
    },
};
use crate::{args::Unreachable, hex, verify::INCOMPLETE_RESTORE_KEY, ToolError};

/// How many prefixes that nothing leads to a report describes.
const DESCRIBED: usize = 1000;

/// The keyspaces that a subtree's own records are in: the roots keyspace
/// holds the records of root keys, which are carried apart.
const OWN: [Keyspace; 3] = [Keyspace::Data, Keyspace::Aux, Keyspace::Meta];

/// The storage tags of the secondaries of an indexed tree: the count, the
/// sum and the average axis (`axis_storage_tag`).
const AXIS_TAGS: [u8; 3] = [0, 1, 2];

/// About the bytes the process holds for each node of a Merk while that
/// Merk is handed over, beside the node's key and value.
///
/// It is a measurement of this command as it is at this commit, and of
/// the engine it writes through (LigninDB task OPS-02). A migration whose
/// largest Merk has 760,101 nodes with 33 bytes of key and value each
/// held 1.35 GB at its peak: with 384 MiB of it counted as the floor, 1.21
/// KB a node, which is rounded up here. A Merk of 400,000 nodes with 474
/// bytes of key and value each held less than this projects. A change of
/// what the load or the engine holds for a node changes the figure.
pub(crate) const PEAK_A_NODE: u64 = 1_300;

/// A Merk of a source that a load reads whole and hands over, with what
/// it weighs. The heaviest decides the most memory a migration holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Largest {
    /// Its nodes.
    pub(crate) nodes: u64,
    /// The bytes of the keys and values of its nodes, and
    /// [`HELD_A_NODE`] for each.
    pub(crate) held: u64,
    /// Where it is.
    pub(crate) at: String,
}

impl Largest {
    /// The bytes of the keys and values of its nodes.
    pub(crate) fn keys_and_values(&self) -> u64 {
        self.held.saturating_sub(HELD_A_NODE * self.nodes)
    }

    /// About the bytes the process holds for it while it is handed over:
    /// [`PEAK_A_NODE`] for each node, and its keys and values.
    pub(crate) fn peak(&self) -> u64 {
        PEAK_A_NODE * self.nodes + self.keys_and_values()
    }
}

/// A prefix that a pending-drop record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Doomed {
    /// The prefix.
    pub(crate) prefix: Prefix,
    /// Whether an element leads to the prefix today: a tree was made
    /// again at the dropped path before the drop was drained. Its records
    /// are then carried as the records of that tree.
    pub(crate) live: bool,
    /// What the source holds under it.
    pub(crate) counts: Counts,
}

/// A pending-drop record of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// The record.
    pub(crate) record: PendingPrefixDropRecord,
    /// The prefixes it names.
    pub(crate) doomed: Vec<Doomed>,
}

/// The records of root keys of a source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RootKeys {
    /// All of them.
    pub(crate) count: Count,
    /// Those under a prefix that no element leads to.
    pub(crate) where_no_tree: u64,
    /// Those that name a key no record is under.
    pub(crate) naming_no_node: u64,
}

/// What a source holds.
#[derive(Debug, Clone, Default)]
pub(crate) struct Survey {
    /// The root hash GroveDB gives the grove: the hash of the node that
    /// the root-key record of the root Merk names.
    pub(crate) root_hash: CryptoHash,
    /// The subtrees that are entered: those an element leads to that hold
    /// a record, and the root Merk.
    pub(crate) subtrees: u64,
    /// The prefixes of those subtrees, in their order.
    pub(crate) places: Vec<Prefix>,
    /// The records of those subtrees, by keyspace.
    pub(crate) counts: Counts,
    /// The nodes of their Merks.
    pub(crate) nodes: u64,
    /// The node records no tree links.
    pub(crate) unlinked: Count,
    /// The flat records of the trees that keep those.
    pub(crate) flat: Count,
    /// The flat records that are nodes of keyed children.
    pub(crate) keyed_children: u64,
    /// The nodes whose value is the bytes of no element.
    pub(crate) no_element: u64,
    /// The Merks whose root-key record does not name their root (GroveDB
    /// #1065), by where they are.
    pub(crate) stale_roots: Vec<String>,
    /// The heaviest Merk of the subtrees that are entered.
    pub(crate) largest: Largest,
    /// The heaviest of the prefixes that nothing leads to whose records
    /// are the nodes of a Merk: a load that carries them reads each
    /// whole.
    pub(crate) largest_unreachable: Largest,
    /// The Merk with the most elements that lead to subtrees: how many,
    /// and where it is.
    pub(crate) widest: (u64, String),
    /// The records of root keys.
    pub(crate) root_keys: RootKeys,
    /// The pending-drop records.
    pub(crate) pending: Vec<Pending>,
    /// The records that are left under the prefixes of pending drops.
    pub(crate) left_behind: Counts,
    /// The records that nothing leads to.
    pub(crate) orphans: Vec<Orphan>,
    /// Whether the paths of their prefixes were looked for.
    pub(crate) paths: bool,
    /// Everything else that refuses the source, in words.
    pub(crate) faults: Vec<String>,
}

/// The mark of what a flat drop left: the prefix and the keyspace.
pub(crate) type Mark = (Keyspace, Prefix);

impl Survey {
    /// The heaviest Merk that a migration which is told `unreachable`
    /// hands over.
    pub(crate) fn heaviest(&self, unreachable: Unreachable) -> &Largest {
        let carried = unreachable == Unreachable::Carry;
        match carried && self.largest_unreachable.peak() > self.largest.peak() {
            true => &self.largest_unreachable,
            false => &self.largest,
        }
    }

    /// Whether a migration that is told `unreachable` takes the source.
    pub(crate) fn is_taken(&self, unreachable: Unreachable) -> bool {
        self.faults.is_empty()
            && match unreachable {
                Unreachable::Refuse => self.orphans.is_empty(),
                Unreachable::Leave => true,
                Unreachable::Carry => self.orphans.iter().all(Orphan::is_carried),
            }
    }

    /// The marks a migration states: one for each keyspace of each prefix
    /// that a pending-drop record names, no element leads to, and the
    /// source holds a record under.
    pub(crate) fn marks(&self) -> BTreeSet<Mark> {
        let mut marks = BTreeSet::new();
        for doomed in self.pending.iter().flat_map(|pending| &pending.doomed) {
            for keyspace in OWN {
                if !doomed.live && doomed.counts.of(keyspace).records > 0 {
                    marks.insert((keyspace, doomed.prefix));
                }
            }
        }
        marks
    }

    fn is_place(&self, prefix: &Prefix) -> bool {
        self.places.binary_search(prefix).is_ok()
    }

    fn doomed(&self, prefix: &Prefix) -> bool {
        let mut doomed = self.pending.iter().flat_map(|pending| &pending.doomed);
        doomed.any(|doomed| doomed.prefix == *prefix)
    }
}

/// The visitor of the survey's walk: it adds up what each subtree holds.
struct Counting<'v> {
    survey: &'v mut Survey,
}

impl Visit for Counting<'_> {
    fn takes_records(&self) -> bool {
        false
    }

    fn enter(&mut self, place: &Place, _: &[Frame]) -> Result<(), ToolError> {
        self.survey.subtrees += 1;
        self.survey.places.push(place.prefix);
        Ok(())
    }

    fn found(&mut self, place: &Place, found: &Found) -> Result<(), ToolError> {
        let survey = &mut *self.survey;
        survey.counts.merge(&found.counts);
        survey.nodes += found.nodes;
        survey.unlinked.merge(found.unlinked);
        survey.flat.merge(found.flat);
        survey.keyed_children += found.keyed_children;
        survey.no_element += found.no_element;
        if found.stale_root {
            survey.stale_roots.push(place.named());
        }
        if found.trees > survey.widest.0 {
            survey.widest = (found.trees, place.named());
        }
        if found.keyed_children > 0 && survey.faults.len() < 100 {
            survey.faults.push(format!(
                "{}: {} of the records of this tree of flat records are nodes of a Merk: keyed \
                 children (grove versions 1 to 3), which this command does not carry",
                place.named(),
                found.keyed_children
            ));
        }
        // Every record of a Merk is in memory when it is handed over, also
        // where no tree links it.
        let merk = Largest {
            nodes: found.nodes + found.unlinked.records,
            held: found.held,
            at: String::new(),
        };
        if found.held > 0 && merk.peak() >= survey.largest.peak() {
            survey.largest = Largest {
                at: place.named(),
                ..merk
            };
        }
        // A source can have a fault in every Merk: the first of a subtree
        // says what is wrong with it, and a hundred say that it is wrong.
        for fault in found.faults.iter().take(3) {
            if survey.faults.len() < 100 {
                survey.faults.push(format!("{}: {fault}", place.named()));
            }
        }
        Ok(())
    }

    fn records(&mut self, _: &Place, _: Keyspace, _: Chunk, _: &[Frame]) -> Result<(), ToolError> {
        Ok(())
    }

    fn merk(&mut self, _: &Place, _: MerkHeld) -> Result<(), ToolError> {
        Ok(())
    }

    fn leave(&mut self, _: &Place, _: &[Frame]) -> Result<(), ToolError> {
        Ok(())
    }
}

/// The root hash that GroveDB gives the grove of `source`: the root Merk
/// is opened by the record of its root key, and has the hash of the node
/// that record names, or no node.
pub(crate) fn root_hash(source: &Source) -> Result<CryptoHash, ToolError> {
    let Some(named) = source.get(Keyspace::Roots, &ROOT, ROOT_KEY)? else {
        return Ok(NULL_HASH);
    };
    let Some(record) = source.get(Keyspace::Data, &ROOT, &named)? else {
        return Ok(NULL_HASH);
    };
    let node = decode(&named, &record).map_err(|error| {
        ToolError::Migrate(format!(
            "the source {}: the root node of the root Merk is no node of a Merk: {error}",
            source.dir().display()
        ))
    })?;
    Ok(node.hash().unwrap())
}

/// The pending-drop records of `source`, and what else its keyspaces hold
/// under the prefix of their namespace, which is refused.
fn pending(source: &Source, survey: &mut Survey) -> Result<(), ToolError> {
    let namespace = pending_prefix_drops_namespace();
    for keyspace in [Keyspace::Data, Keyspace::Aux] {
        if source.holds(keyspace, namespace)? {
            survey.faults.push(format!(
                "the {} keyspace holds a record under the prefix of the pending-drop records, \
                 where GroveDB writes none",
                keyspace.name()
            ));
        }
    }
    let mut records = Vec::new();
    source.subtree(Keyspace::Meta, namespace, |key, value| {
        match PendingPrefixDropRecord::decode(key, value) {
            Ok(record) => records.push(record),
            Err(error) => survey.faults.push(format!(
                "the meta record {} under the prefix of the pending-drop records is none: {error}",
                hex(key)
            )),
        }
        Ok(())
    })?;
    for record in records {
        let doomed = record.doomed_prefixes.iter().map(|prefix| Doomed {
            prefix: *prefix,
            live: false,
            counts: Counts::default(),
        });
        survey.pending.push(Pending {
            doomed: doomed.collect(),
            record,
        });
    }
    Ok(())
}

/// The records of root keys: every record of the roots keyspace.
fn root_keys(source: &Source, survey: &mut Survey) -> Result<(), ToolError> {
    let mut records = source.records(Keyspace::Roots);
    while let Some((stored, value)) = records.current()? {
        let Some((prefix, key)) = stored.split_first_chunk::<32>() else {
            return Err(short_key(source, Keyspace::Roots, stored));
        };
        if key != ROOT_KEY {
            if survey.faults.len() < 100 {
                survey.faults.push(format!(
                    "the roots keyspace holds the key {} under the prefix {}, which is no \
                     record of a root key",
                    hex(key),
                    hex(prefix)
                ));
            }
        } else {
            survey.root_keys.count.add(key, value);
            survey.root_keys.where_no_tree += u64::from(!survey.is_place(prefix));
            let names_a_node = source.get(Keyspace::Data, prefix, value)?.is_some();
            survey.root_keys.naming_no_node += u64::from(!names_a_node);
        }
        records.next();
    }
    Ok(())
}

/// What the source holds beside the subtrees the walk came to: under the
/// prefixes of pending drops it is counted as what is left behind, and
/// the other prefixes are the orphans.
fn beside_the_walk(source: &Source, side: &Side, survey: &mut Survey) -> Result<(), ToolError> {
    let places = std::mem::take(&mut survey.places);
    let mut held = beside(source, side, |prefix| {
        Ok(places.binary_search(prefix).is_ok())
    })?;
    // What a pending drop names is live where the walk came to it, and
    // is left behind otherwise.
    for doomed in (survey.pending.iter_mut()).flat_map(|pending| &mut pending.doomed) {
        doomed.live = places.binary_search(&doomed.prefix).is_ok();
        if let Some(counts) = held.remove(&doomed.prefix) {
            doomed.counts = counts;
            survey.left_behind.merge(&counts);
        }
    }
    for (prefix, counts) in held {
        if survey.doomed(&prefix) {
            continue;
        }
        let before = places.partition_point(|place| *place < prefix) as u64;
        let mut orphan = Orphan {
            prefix,
            counts,
            held: Held::NoData,
            records: Vec::new(),
            root_key: None,
            path: None,
            tree_stands: None,
            around: (before, places.len() as u64 - before),
        };
        inspect(source, &mut orphan)?;
        // Where the data records under the prefix are the nodes of a
        // Merk, a load that carries them reads them whole.
        let data = orphan.counts.of(Keyspace::Data);
        let merk = Largest {
            nodes: data.records,
            held: data.bytes + HELD_A_NODE * data.records,
            at: String::new(),
        };
        let is_merk = matches!(orphan.held, Held::Nodes(_));
        if is_merk && merk.peak() >= survey.largest_unreachable.peak() {
            survey.largest_unreachable = Largest {
                at: format!("the prefix {}, which nothing leads to", hex(&prefix)),
                ..merk
            };
        }
        survey.orphans.push(orphan);
    }
    survey.places = places;
    Ok(())
}

/// Where an orphan was found to be.
#[derive(Debug, Clone)]
struct Explained {
    /// The path of its subtree.
    path: Vec<Vec<u8>>,
    /// Which subtree under that path: 0 the one of the path, and 1 to 3
    /// the secondaries beside it.
    beside: u8,
    /// Whether a node of the grove's trees holds the element of a tree at
    /// the path.
    tree_stands: bool,
}

/// The prefixes under the key `key` of the subtree at `path`: the one of
/// its own path, and those of the three secondaries beside it.
fn under(path: &[Vec<u8>], key: &[u8]) -> [(Prefix, u8); 4] {
    let mut below = path.to_vec();
    below.push(key.to_vec());
    let own = build_prefix(SubtreePath::from(below.as_slice())).value;
    let secondary = |tag| secondary_prefix_for(&own, tag).value;
    let [count, sum, average] = AXIS_TAGS;
    [
        (own, 0),
        (secondary(count), 1),
        (secondary(sum), 2),
        (secondary(average), 3),
    ]
}

/// What is known of the orphans' paths, and which prefixes are asked for.
struct Explaining<'v> {
    explained: &'v mut BTreeMap<Prefix, Explained>,
    wanted: &'v BTreeSet<Prefix>,
}

impl Explaining<'_> {
    /// Looks at the prefixes under `key` of the subtree at `path`, whose
    /// record holds `value`: `linked` says whether a tree of the grove
    /// links the record.
    fn node(&mut self, path: &[Vec<u8>], key: &[u8], value: &[u8], linked: bool) {
        for (prefix, beside) in under(path, key) {
            if !self.wanted.contains(&prefix) || self.explained.contains_key(&prefix) {
                continue;
            }
            let mut below = path.to_vec();
            below.push(key.to_vec());
            let element = Element::deserialize(value, GroveVersion::latest());
            let is_tree = element.is_ok_and(|element| element.tree_type().is_some());
            self.explained.insert(
                prefix,
                Explained {
                    path: below,
                    beside,
                    tree_stands: linked && is_tree,
                },
            );
        }
    }
}

impl Visit for Explaining<'_> {
    fn takes_records(&self) -> bool {
        true
    }

    fn enter(&mut self, _: &Place, _: &[Frame]) -> Result<(), ToolError> {
        Ok(())
    }

    fn found(&mut self, _: &Place, _: &Found) -> Result<(), ToolError> {
        Ok(())
    }

    fn records(&mut self, _: &Place, _: Keyspace, _: Chunk, _: &[Frame]) -> Result<(), ToolError> {
        Ok(())
    }

    fn merk(&mut self, place: &Place, merk: MerkHeld) -> Result<(), ToolError> {
        // The secondaries of an indexed tree hold rows, under whose keys
        // no path leads on.
        if place.host.as_ref().is_some_and(|host| host.position > 0) {
            return Ok(());
        }
        for node in &merk.nodes {
            self.node(&place.path, &node.key, &node.value, true);
        }
        for node in &merk.unlinked {
            self.node(&place.path, &node.key, &node.value, false);
        }
        Ok(())
    }

    fn leave(&mut self, _: &Place, _: &[Frame]) -> Result<(), ToolError> {
        Ok(())
    }
}

/// Finds the paths of the orphans of `survey`, as far as they can be
/// found: under the keys of the records of the grove, under the paths of
/// the pending drops, and from each orphan that was found under the keys
/// of its own records.
fn explain(source: &Source, side: &Side, survey: &mut Survey) -> Result<(), ToolError> {
    let wanted: BTreeSet<Prefix> = survey.orphans.iter().map(|orphan| orphan.prefix).collect();
    let mut explained: BTreeMap<Prefix, Explained> = BTreeMap::new();
    let explaining = Explaining {
        explained: &mut explained,
        wanted: &wanted,
    };
    Walk::new(source, side, explaining, false).run()?;

    // A subtree that nothing leads to and whose path is known leads on:
    // its records are read as the nodes of a Merk.
    let mut ahead: Vec<(Prefix, Vec<Vec<u8>>)> = (explained.iter())
        .filter(|(_, explained)| explained.beside == 0)
        .map(|(prefix, explained)| (*prefix, explained.path.clone()))
        .collect();
    let dropped = survey.pending.iter().map(|pending| &pending.record);
    ahead.extend(dropped.map(|record| (record.primary_prefix, record.path.clone())));
    while let Some((prefix, path)) = ahead.pop() {
        let mut records = Vec::new();
        source.subtree(Keyspace::Data, &prefix, |key, record| {
            if let Ok(node) = decode(key, record) {
                records.push((key.to_vec(), node.value_as_slice().to_vec()));
            }
            Ok(())
        })?;
        for (key, value) in records {
            let before = explained.len();
            let mut explaining = Explaining {
                explained: &mut explained,
                wanted: &wanted,
            };
            // No tree of the grove links a record of such a subtree.
            explaining.node(&path, &key, &value, false);
            if explained.len() > before {
                let mut below = path.clone();
                below.push(key.clone());
                let own = build_prefix(SubtreePath::from(below.as_slice())).value;
                if explained.contains_key(&own) {
                    ahead.push((own, below));
                }
            }
        }
    }
    for orphan in &mut survey.orphans {
        if let Some(explained) = explained.remove(&orphan.prefix) {
            orphan.path = Some(named(&explained.path, explained.beside));
            orphan.tree_stands = Some(explained.tree_stands);
        }
    }
    Ok(())
}

/// Surveys `source`. `side` is its aux and meta keyspaces. With `paths`,
/// the path of each prefix that nothing leads to is looked for, which is
/// one more walk of the grove.
pub(crate) fn survey(source: &Source, side: &Side, paths: bool) -> Result<Survey, ToolError> {
    let mut survey = Survey {
        root_hash: root_hash(source)?,
        paths,
        ..Survey::default()
    };
    if source
        .get(Keyspace::Aux, &ROOT, INCOMPLETE_RESTORE_KEY)?
        .is_some()
    {
        survey.faults.push(
            "the root holds the mark of a state-sync restore that did not finish: the database \
             is to be discarded, not migrated"
                .to_owned(),
        );
    }
    if source.get(Keyspace::Aux, &ROOT, JOURNAL_KEY)?.is_some() {
        survey.faults.push(format!(
            "the root holds an aux record under the key {:?}, which the migrator keeps its \
             journal under in the target",
            String::from_utf8_lossy(JOURNAL_KEY)
        ));
    }
    pending(source, &mut survey)?;
    let counting = Counting {
        survey: &mut survey,
    };
    Walk::new(source, side, counting, true).run()?;
    survey.places.sort_unstable();
    root_keys(source, &mut survey)?;
    // A walk that met what is not carried went no further there: what is
    // beside it is not known.
    if survey.faults.is_empty() {
        beside_the_walk(source, side, &mut survey)?;
        if paths && !survey.orphans.is_empty() {
            explain(source, side, &mut survey)?;
        }
    }
    Ok(survey)
}

fn count(count: Count) -> String {
    format!("{} ({} bytes)", count.records, count.bytes)
}

impl Survey {
    /// The report of the survey, for a reader: one line for each kind of
    /// record, with what a migration that is told `unreachable` does with
    /// the records nothing leads to, and after them everything that
    /// refuses the source.
    pub(crate) fn report(&self, unreachable: Unreachable) -> Vec<String> {
        let mut lines = vec![
            format!("root hash: {}", hex(&self.root_hash)),
            format!("subtrees entered: {}", self.subtrees),
            format!("Merk nodes: {}", self.nodes),
            format!(
                "largest Merk that is handed over: {} nodes with {} MiB of keys and values, at {}",
                self.heaviest(unreachable).nodes,
                self.heaviest(unreachable)
                    .keys_and_values()
                    .div_ceil(1 << 20),
                match self.heaviest(unreachable).at.is_empty() {
                    true => "(none)",
                    false => &self.heaviest(unreachable).at,
                }
            ),
            format!(
                "widest Merk: {} of its elements lead to subtrees, at {}",
                self.widest.0,
                match self.widest.1.is_empty() {
                    true => "(none)",
                    false => &self.widest.1,
                }
            ),
            format!("data records: {}", count(self.counts.of(Keyspace::Data))),
            format!("flat records: {}", count(self.flat)),
            format!("aux records: {}", count(self.counts.of(Keyspace::Aux))),
            format!("meta records: {}", count(self.counts.of(Keyspace::Meta))),
            format!(
                "root-key records: {}, {} of them where no tree is, {} of them naming no node",
                count(self.root_keys.count),
                self.root_keys.where_no_tree,
                self.root_keys.naming_no_node
            ),
            format!("records that no tree links: {}", count(self.unlinked)),
            format!(
                "nodes of keyed children under trees of flat records (grove versions 1 to 3): {}",
                self.keyed_children
            ),
            format!("node values that are no element: {}", self.no_element),
            format!(
                "pending drops: {}, which leave {} records behind and {} marks",
                self.pending.len(),
                self.left_behind.records(),
                self.marks().len()
            ),
        ];
        for pending in &self.pending {
            for doomed in &pending.doomed {
                let held: Vec<String> = (OWN.iter())
                    .map(|keyspace| {
                        format!(
                            "{} {}",
                            keyspace.name(),
                            doomed.counts.of(*keyspace).records
                        )
                    })
                    .collect();
                lines.push(format!(
                    "  pending drop of {}: prefix {}, {}{}",
                    named(&pending.record.path, 0),
                    hex(&doomed.prefix),
                    held.join(", "),
                    match doomed.live {
                        true => "; A TREE STANDS AT THE PATH AGAIN: its records are carried as that tree's",
                        false => "",
                    }
                ));
            }
        }
        lines.push(format!(
            "root-key records that do not name the root of their Merk (GroveDB #1065): {}",
            self.stale_roots.len()
        ));
        for at in &self.stale_roots {
            lines.push(format!(
                "  AT {at}: THE MERK IS READ AS ITS RECORD NAMES IT, ON ROCKSDB AS HERE, AND \
                 LIGNINDB TAKES NO FURTHER WRITE OF A NODE OF IT"
            ));
        }
        if !self.faults.is_empty() {
            lines.push(
                "the walk of the grove stopped at what is not carried: what is beside the \
                 grove was not looked for"
                    .to_owned(),
            );
        }
        lines.push(format!(
            "prefixes that nothing leads to: {}, with {} records ({} bytes)",
            self.orphans.len(),
            self.orphans
                .iter()
                .map(|orphan| orphan.counts.records())
                .sum::<u64>(),
            self.orphans
                .iter()
                .map(|orphan| orphan.counts.bytes())
                .sum::<u64>(),
        ));
        for orphan in self.orphans.iter().take(DESCRIBED) {
            let held: Vec<String> = (OWN.iter())
                .map(|keyspace| {
                    let held = orphan.counts.of(*keyspace);
                    format!(
                        "{} {} ({} bytes)",
                        keyspace.name(),
                        held.records,
                        held.bytes
                    )
                })
                .collect();
            let becomes = match (unreachable, &orphan.held) {
                (Unreachable::Refuse, _) => "NOT CARRIED",
                (Unreachable::Leave, _) => "LEFT BEHIND",
                (Unreachable::Carry, Held::Other(_)) => "NOT CARRIED",
                (Unreachable::Carry, _) => "CARRIED AS RECORDS NO TREE LINKS",
            };
            lines.push(format!(
                "  {becomes}: prefix {}, {}; its data records are {}; its root-key record {}; \
                 path {}; a tree stands there today: {}; {} prefixes of the grove come before \
                 it and {} after it",
                hex(&orphan.prefix),
                held.join(", "),
                match &orphan.held {
                    Held::NoData => "none".to_owned(),
                    Held::Nodes(tree_type) => format!("the nodes of a {tree_type}"),
                    Held::Other(other) => format!("not the nodes of one Merk ({other})"),
                },
                match &orphan.root_key {
                    Some(key) => format!("names the key {}", hex(key)),
                    None => "is not there".to_owned(),
                },
                match (&orphan.path, self.paths) {
                    (Some(path), _) => path,
                    (None, true) => "not found",
                    (None, false) => "not looked for (--survey looks for it)",
                },
                match orphan.tree_stands {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "not known",
                },
                orphan.around.0,
                orphan.around.1,
            ));
            for record in &orphan.records {
                lines.push(format!("    {record}"));
            }
        }
        if self.orphans.len() > DESCRIBED {
            lines.push(format!("  and {} more", self.orphans.len() - DESCRIBED));
        }
        for fault in &self.faults {
            lines.push(format!("  NOT CARRIED: {fault}"));
        }
        lines
    }

    /// The counts of the survey on one line, as `name=value` words.
    pub(crate) fn line(&self) -> String {
        let orphaned: u64 = self
            .orphans
            .iter()
            .map(|orphan| orphan.counts.records())
            .sum();
        format!(
            "subtrees={} nodes={} largest_merk_nodes={} largest_merk_bytes={} \
             largest_unreachable_nodes={} largest_unreachable_bytes={} \
             widest_merk_trees={} data_bytes={} \
             flat={} aux={} meta={} root_keys={} root_keys_where_no_tree={} unlinked={} \
             keyed_children={} pending_drops={} left_behind={} marks={} stale_roots={} \
             orphan_prefixes={} orphan_records={} orphan_bytes={} faults={}",
            self.subtrees,
            self.nodes,
            self.largest.nodes,
            self.largest.keys_and_values(),
            self.largest_unreachable.nodes,
            self.largest_unreachable.keys_and_values(),
            self.widest.0,
            self.counts.of(Keyspace::Data).bytes,
            self.flat.records,
            self.counts.of(Keyspace::Aux).records,
            self.counts.of(Keyspace::Meta).records,
            self.root_keys.count.records,
            self.root_keys.where_no_tree,
            self.unlinked.records,
            self.keyed_children,
            self.pending.len(),
            self.left_behind.records(),
            self.marks().len(),
            self.stale_roots.len(),
            self.orphans.len(),
            orphaned,
            (self.orphans.iter())
                .map(|orphan| orphan.counts.bytes())
                .sum::<u64>(),
            self.faults.len(),
        )
    }
}
