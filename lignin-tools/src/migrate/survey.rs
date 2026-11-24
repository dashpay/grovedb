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
//! | a flat record of a tree that keeps flat records | carried as it is; a node of a keyed child (grove versions 1 to 3) among them is such a record |
//! | an aux or a meta record of such a subtree | carried |
//! | the record of a root key, under any prefix | carried, where the engine keeps it: also under a prefix with no tree |
//! | a pending-drop record | carried, with a mark for each keyspace of each prefix it names in which the source still holds a record |
//! | a record under a prefix that a pending-drop record names and no element leads to | left: the engine dropped that subtree with its element, and the mark stands for what RocksDB still holds |
//! | a data, aux or meta record under any other prefix | **not carried: the source is refused** ([`Orphan`]) |
//!
//! A source is also refused for a Merk whose records are not one tree
//! under the root that is named for it, a record that Merk does not decode
//! or encodes otherwise, a record of the roots keyspace that is no record
//! of a root key, a record under the prefix of the pending-drop records
//! that is none, and an aux record of the root under the key of the
//! migrator's journal.

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
    grove::decode,
    journal::JOURNAL_KEY,
    source::{after, Count, Counts, Keyspace, Prefix, Source},
    walk::{
        named, short_key, Chunk, Found, Frame, MerkHeld, Place, Side, Visit, Walk, ROOT, ROOT_KEY,
    },
};
use crate::{hex, ToolError};

/// The keyspaces that a subtree's own records are in: the roots keyspace
/// holds the records of root keys, which are carried apart.
const OWN: [Keyspace; 3] = [Keyspace::Data, Keyspace::Aux, Keyspace::Meta];

/// The storage tags of the secondaries of an indexed tree: the count, the
/// sum and the average axis (`axis_storage_tag`).
const AXIS_TAGS: [u8; 3] = [0, 1, 2];

/// The largest Merk of a source, by what it takes in memory when it is
/// handed over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Largest {
    /// Its nodes.
    pub(crate) nodes: u64,
    /// About the bytes its nodes take in memory.
    pub(crate) held: u64,
    /// Where it is.
    pub(crate) at: String,
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

/// Records under a prefix that no element leads to and no pending-drop
/// record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Orphan {
    /// The prefix.
    pub(crate) prefix: Prefix,
    /// What the source holds under it in the data, the aux and the meta
    /// keyspace.
    pub(crate) counts: Counts,
    /// The path the prefix is of, where it was found.
    pub(crate) path: Option<String>,
    /// Whether an element that is a tree stands at that path today.
    pub(crate) tree_stands: Option<bool>,
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
    /// The largest Merk.
    pub(crate) largest: Largest,
    /// The records of root keys.
    pub(crate) root_keys: RootKeys,
    /// The pending-drop records.
    pub(crate) pending: Vec<Pending>,
    /// The records that are left under the prefixes of pending drops.
    pub(crate) left_behind: Counts,
    /// The records that nothing leads to.
    pub(crate) orphans: Vec<Orphan>,
    /// Everything else that refuses the source, in words.
    pub(crate) faults: Vec<String>,
}

/// The mark of what a flat drop left: the prefix and the keyspace.
pub(crate) type Mark = (Keyspace, Prefix);

impl Survey {
    /// Whether every record of the source is carried.
    pub(crate) fn is_carried(&self) -> bool {
        self.faults.is_empty() && self.orphans.is_empty()
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
        if found.nodes > 0 && found.held >= survey.largest.held {
            survey.largest = Largest {
                nodes: found.nodes + found.unlinked.records,
                held: found.held,
                at: place.named(),
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

/// What the source holds under `prefix` in `keyspace`, counted record by
/// record.
fn counted(source: &Source, keyspace: Keyspace, prefix: &Prefix) -> Result<Count, ToolError> {
    let mut count = Count::default();
    source.subtree(keyspace, prefix, |key, value| {
        count.add(key, value);
        Ok(())
    })?;
    Ok(count)
}

/// Every prefix that holds a record in the data, the aux or the meta
/// keyspace and that the walk did not come to, with what it holds: the
/// prefixes of pending drops are counted as what is left behind, the
/// namespace of the pending-drop records is passed over, and the others
/// are the orphans.
fn beside_the_walk(source: &Source, side: &Side, survey: &mut Survey) -> Result<(), ToolError> {
    let namespace = *pending_prefix_drops_namespace();
    let mut beside: BTreeMap<Prefix, Counts> = BTreeMap::new();
    // The data keyspace is the large one: the prefixes that the walk came
    // to are skipped by a seek, not read.
    let mut records = source.records(Keyspace::Data);
    while let Some((stored, _)) = records.current()? {
        let Some((prefix, _)) = stored.split_first_chunk::<32>() else {
            return Err(short_key(source, Keyspace::Data, stored));
        };
        let prefix = *prefix;
        if !survey.is_place(&prefix) && prefix != namespace {
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
            if !survey.is_place(prefix) && *prefix != namespace {
                *beside.entry(*prefix).or_default().of_mut(keyspace) = *count;
            }
        }
    }
    // What a pending drop names is live where the walk came to it, and
    // is left behind otherwise.
    let places = std::mem::take(&mut survey.places);
    for doomed in (survey.pending.iter_mut()).flat_map(|pending| &mut pending.doomed) {
        doomed.live = places.binary_search(&doomed.prefix).is_ok();
        if let Some(counts) = beside.remove(&doomed.prefix) {
            doomed.counts = counts;
            survey.left_behind.merge(&counts);
        }
    }
    survey.places = places;
    let orphans = beside
        .into_iter()
        .filter(|(prefix, _)| !survey.doomed(prefix));
    survey.orphans = orphans
        .map(|(prefix, counts)| Orphan {
            prefix,
            counts,
            path: None,
            tree_stands: None,
        })
        .collect();
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

/// Surveys `source`. `side` is its aux and meta keyspaces.
pub(crate) fn survey(source: &Source, side: &Side) -> Result<Survey, ToolError> {
    let mut survey = Survey {
        root_hash: root_hash(source)?,
        ..Survey::default()
    };
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
    beside_the_walk(source, side, &mut survey)?;
    if !survey.orphans.is_empty() {
        explain(source, side, &mut survey)?;
    }
    Ok(survey)
}

fn count(count: Count) -> String {
    format!("{} ({} bytes)", count.records, count.bytes)
}

impl Survey {
    /// The report of the survey, for a reader: one line for each kind of
    /// record, and after them everything that refuses the source.
    pub(crate) fn report(&self) -> Vec<String> {
        let mut lines = vec![
            format!("root hash: {}", hex(&self.root_hash)),
            format!("subtrees entered: {}", self.subtrees),
            format!("Merk nodes: {}", self.nodes),
            format!(
                "largest Merk: {} nodes, about {} MiB in memory when it is handed over, at {}",
                self.largest.nodes,
                self.largest.held.div_ceil(1 << 20),
                match self.largest.at.is_empty() {
                    true => "(none)",
                    false => &self.largest.at,
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
                 LIGNINDB TAKES NO FURTHER WRITE OF IT"
            ));
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
        for orphan in self.orphans.iter().take(50) {
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
            lines.push(format!(
                "  NOT CARRIED: prefix {}, {}; path {}; a tree stands there today: {}",
                hex(&orphan.prefix),
                held.join(", "),
                orphan.path.as_deref().unwrap_or("not found"),
                match orphan.tree_stands {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "not known",
                }
            ));
        }
        if self.orphans.len() > 50 {
            lines.push(format!("  and {} more", self.orphans.len() - 50));
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
            "subtrees={} nodes={} largest_merk_nodes={} largest_merk_bytes={} data_bytes={} \
             flat={} aux={} meta={} root_keys={} root_keys_where_no_tree={} unlinked={} \
             keyed_children={} pending_drops={} left_behind={} marks={} stale_roots={} \
             orphan_prefixes={} orphan_records={} faults={}",
            self.subtrees,
            self.nodes,
            self.largest.nodes,
            self.largest.held,
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
            self.faults.len(),
        )
    }
}
