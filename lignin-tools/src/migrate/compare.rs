//! The comparison of a migrated target with its source: every record that
//! a migration carries, of every keyspace of every prefix, read from the
//! target as GroveDB reads it.
//!
//! The source is walked a second time ([`super::walk`]), and for each
//! subtree the records of its three keyspaces are read from the files of
//! RocksDB and held, byte for byte and in their order, to what the cursors
//! of a handle of the LigninDB engine show (`Engine::subtree_by_prefix`):
//! the node cursor for a Merk, which shows each node and each unlinked
//! record as the record Merk encodes of it, and the flat cursors for flat,
//! aux and meta records. The entry of the subtree in the directory is held
//! to its element, its position, its tree type and its root.
//!
//! Beside the walk:
//!
//! - **every record of a root key**, under every prefix of the source,
//!   is what a handle of that prefix reads (`NodeStore::root_key`), and the
//!   target keeps no other: the roots keyspace of the root Merk holds its
//!   own record and nothing else, that of the namespace of the
//!   pending-drop records holds the others, and that of every other
//!   subtree holds nothing;
//! - **the pending-drop records** are the source's, and **the marks** are
//!   those that the source's records leave, each with no value: one for
//!   each keyspace of a dropped prefix that no element leads to and that
//!   the source still holds a record under. The namespace holds no data
//!   record;
//! - **what the source holds beside its grove** ([`super::beside`]): the
//!   target has the records of such a prefix, all of them, under a subtree
//!   with no host and no root, or it has no subtree there. How many
//!   prefixes are of each kind is counted, and the migration holds the two
//!   counts to what it was told becomes of such records;
//! - **the directory** has an entry for each subtree of the walk, for each
//!   such prefix whose records the target has, and for the namespace of
//!   the pending-drop records, and no other;
//! - **the journal** of the migration is gone;
//! - **the root hash** that GroveDB gives the target is the source's.
//!
//! This reads the target through other code than the load wrote it with:
//! the load states cells to a write transaction, and the comparison reads
//! records through the node store of Merk.

use std::collections::BTreeSet;

use grovedb::{engine::lignin::LigninTransaction, Engine, GroveDb, LigninEngine};
use grovedb_merk::{
    store::{
        lignin::{tree_type_bytes, LigninNodeStore},
        NodeStore,
    },
    CryptoHash, TreeType,
};
use grovedb_storage::{flat::pending_prefix_drops_namespace, FlatStore, RawIterator};
use grovedb_version::version::GroveVersion;
use lignindb::txn::{Column, Subtree};

use super::{
    beside::{beside, doomed, nodes},
    journal::JOURNAL_KEY,
    load::pending_drops,
    source::{Keyspace, Prefix, Source},
    walk::{short_key, Chunk, Found, Frame, MerkHeld, Place, Side, Visit, Walk, ROOT, ROOT_KEY},
};
use crate::{hex, ToolError};

/// How many differences a comparison names before it only counts them.
const NAMED: usize = 20;

/// What a comparison read, and what it found to differ.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Compared {
    /// The subtrees it compared.
    pub(crate) subtrees: u64,
    /// The records of the data keyspaces: nodes, unlinked records and
    /// flat records.
    pub(crate) data: u64,
    /// The aux records.
    pub(crate) aux: u64,
    /// The meta records, the pending-drop records among them.
    pub(crate) meta: u64,
    /// The records of root keys.
    pub(crate) roots: u64,
    /// The marks of what flat drops left.
    pub(crate) marks: u64,
    /// The prefixes that nothing leads to whose records the target has,
    /// under a subtree with no host.
    pub(crate) unreachable_carried: u64,
    /// The prefixes that nothing leads to which the target has nothing
    /// of.
    pub(crate) unreachable_left: u64,
    /// How many differences it found.
    pub(crate) differing: u64,
    /// The first of them, in words.
    pub(crate) differences: Vec<String>,
}

impl Compared {
    /// Whether the target is the source.
    pub(crate) fn is_same(&self) -> bool {
        self.differing == 0
    }

    fn differs(&mut self, what: String) {
        self.differing += 1;
        if self.differences.len() < NAMED {
            self.differences.push(what);
        }
    }

    /// Every record of the source that was compared.
    pub(crate) fn of_the_source(&self) -> u64 {
        self.data + self.aux + self.meta + self.roots
    }
}

fn target_error(what: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Migrate(format!("a read of the target, of {what}: {error}"))
}

/// The visitor of the comparison's walk.
struct Comparing<'t, 'e> {
    source: &'t Source,
    engine: &'e LigninEngine,
    transaction: &'t LigninTransaction<'e>,
    /// The prefixes of the subtrees the walk came to.
    places: BTreeSet<Prefix>,
    compared: Compared,
}

impl<'t> Comparing<'t, '_> {
    /// Holds what `cursor` shows to the records of `keyspace` under
    /// `prefix` in the source, which `named` names, and returns how many
    /// records were the same.
    fn same<C: RawIterator>(
        &mut self,
        mut cursor: C,
        prefix: &Prefix,
        named: &str,
        keyspace: Keyspace,
    ) -> Result<u64, ToolError> {
        let _ = cursor.seek_to_first();
        let (mut same, mut differs) = (0, None);
        self.source.subtree(keyspace, prefix, |key, value| {
            if differs.is_some() {
                return Ok(());
            }
            let (found_key, found_value) = (cursor.key().value, cursor.value().value);
            if (found_key, found_value) == (Some(key), Some(value)) {
                same += 1;
                let _ = cursor.next();
                return Ok(());
            }
            differs = Some(match found_key {
                Some(found) if found == key => format!(
                    "the record under the key {} is other bytes ({} in the source, {} in the \
                     target)",
                    hex(key),
                    value.len(),
                    found_value.map_or(0, <[u8]>::len)
                ),
                Some(found) => format!(
                    "the source holds the key {} where the target holds {}",
                    hex(key),
                    hex(found)
                ),
                None => format!("the target does not hold the key {}", hex(key)),
            });
            Ok(())
        })?;
        if differs.is_none() {
            let left = cursor.key().value;
            differs = left.map(|key| {
                format!(
                    "the target holds the key {}, which the source does not",
                    hex(key)
                )
            });
        }
        cursor
            .status()
            .map_err(|error| target_error("a cursor", error))?;
        if let Some(differs) = differs {
            let at = format!("{named}, {} keyspace", keyspace.name());
            self.compared.differs(format!("{at}: {differs}"));
        }
        Ok(same)
    }

    /// A handle of the subtree `prefix`, as the engine hands one out.
    fn handle(&self, prefix: Prefix) -> LigninNodeStore<'t> {
        let (engine, transaction): (&'t LigninEngine, &'t LigninTransaction<'_>) =
            (self.engine, self.transaction);
        engine.subtree_by_prefix(prefix, None, transaction).value
    }

    /// Holds the records of the three keyspaces of the subtree `prefix`,
    /// which `named` names, to the source's: its data through the cursor
    /// over a Merk, or over flat records where `flat` says so.
    fn keyspaces(&mut self, prefix: Prefix, named: &str, flat: bool) -> Result<(), ToolError> {
        let handle = self.handle(prefix);
        let data = match flat {
            false => self.same(NodeStore::cursor(&handle), &prefix, named, Keyspace::Data)?,
            true => self.same(FlatStore::cursor(&handle), &prefix, named, Keyspace::Data)?,
        };
        self.compared.data += data;
        let handle = self.handle(prefix);
        self.compared.aux += self.same(handle.aux().cursor(), &prefix, named, Keyspace::Aux)?;
        let handle = self.handle(prefix);
        self.compared.meta += self.same(handle.meta().cursor(), &prefix, named, Keyspace::Meta)?;
        Ok(())
    }
}

impl Visit for Comparing<'_, '_> {
    fn takes_records(&self) -> bool {
        false
    }

    fn enter(&mut self, _: &Place, _: &[Frame]) -> Result<(), ToolError> {
        Ok(())
    }

    fn found(&mut self, place: &Place, found: &Found) -> Result<(), ToolError> {
        if let Some(fault) = found.faults.first() {
            return Err(ToolError::Migrate(format!(
                "the source is not what its survey found: {}: {fault}",
                place.named()
            )));
        }
        self.compared.subtrees += 1;
        self.places.insert(place.prefix);
        let entry = self.handle(place.prefix).entry();
        let entry = entry.map_err(|error| target_error("the directory", error))?;
        let host = place.host.as_ref();
        let stated = (
            tree_type_bytes(place.tree_type),
            host.map(|host| (host.parent, host.key.clone(), host.position)),
            found.root.clone(),
        );
        let entered = entry.map(|entry| {
            let host = entry.host;
            (
                entry.tree_type,
                host.map(|host| (host.subtree, host.key, host.position)),
                entry.root_key,
            )
        });
        if entered.as_ref() != Some(&stated) {
            self.compared.differs(format!(
                "{}: the directory has the subtree {} as {:?}, and the source has it as {:?} \
                 (tree type, host, root key)",
                place.named(),
                hex(&place.prefix),
                entered,
                stated
            ));
        }

        self.keyspaces(place.prefix, &place.named(), place.rooted.is_none())
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

/// Every record of `column` of `subtree` in the latest version of the
/// database of `engine`, as the store holds it: its key and its value.
/// `None` where the directory has no entry for the subtree.
fn records_of(
    engine: &LigninEngine,
    subtree: Subtree,
    column: Column,
) -> Result<Option<Chunk>, ToolError> {
    let snapshot = engine.database().snapshot();
    let operation = snapshot.operation();
    let Some(opened) = operation.open(subtree)? else {
        return Ok(None);
    };
    let mut records = Vec::new();
    let mut cursor = opened.cursor(column);
    cursor.seek_to_first()?;
    while let Some(key) = cursor.key() {
        let mut value = Vec::new();
        opened.get(column, key, &mut value)?;
        records.push((key.to_vec(), value));
        cursor.next()?;
    }
    Ok(Some(records))
}

/// Compares the grove `target` with `source`, whose root hash is
/// `root_hash`. `side` is the source's aux and meta keyspaces.
pub(crate) fn compare(
    source: &Source,
    side: &Side,
    target: &GroveDb<LigninEngine>,
    root_hash: &CryptoHash,
) -> Result<Compared, ToolError> {
    let engine = target.engine();
    let transaction = engine.start_read_transaction();
    let comparing = Comparing {
        source,
        engine,
        transaction: &transaction,
        places: BTreeSet::new(),
        compared: Compared::default(),
    };
    let mut comparing = Walk::new(source, side, comparing, false).run()?;

    // What the source holds beside its grove: the target has the records
    // of such a prefix under a subtree with no host and no root, or it
    // has no subtree there.
    let doomed = doomed(source)?;
    let places = std::mem::take(&mut comparing.places);
    let mut carried = Vec::new();
    let held = beside(source, side, |prefix| Ok(places.contains(prefix)))?;
    for prefix in held.into_keys().filter(|prefix| !doomed.contains(prefix)) {
        let named = format!("the prefix {}, which nothing leads to", hex(&prefix));
        let entry = comparing.handle(prefix).entry();
        let entry = entry.map_err(|error| target_error("the directory", error))?;
        let Some(entry) = entry else {
            comparing.compared.unreachable_left += 1;
            continue;
        };
        comparing.compared.unreachable_carried += 1;
        carried.push(prefix);
        // The tree type the load gave the entry: the one the nodes name.
        let stated = nodes(source, &prefix, false)?
            .ok()
            .map(|(tree_type, ..)| tree_type_bytes(tree_type.unwrap_or(TreeType::NormalTree)));
        if entry.host.is_some() || entry.root_key.is_some() || Some(entry.tree_type) != stated {
            comparing.compared.differs(format!(
                "{named}: the directory has its subtree under a host, with a root, or of \
                 another tree type than its records are of"
            ));
        }
        comparing.keyspaces(prefix, &named, false)?;
    }
    let mut compared = comparing.compared;

    // The roots keyspaces: the root Merk keeps the record of its own root
    // key, and no other subtree of the grove keeps a record there.
    {
        let snapshot = engine.database().snapshot();
        let operation = snapshot.operation();
        for prefix in places.iter().chain(&carried) {
            let Some(opened) = operation.open(Subtree::new(*prefix))? else {
                continue;
            };
            let mut cursor = opened.cursor(Column::Roots);
            cursor.seek_to_first()?;
            while let Some(key) = cursor.key() {
                if *prefix != ROOT || key != ROOT_KEY {
                    compared.differs(format!(
                        "the roots keyspace of the subtree {} holds the key {}, where the \
                         engine keeps no record",
                        hex(prefix),
                        hex(key)
                    ));
                    break;
                }
                cursor.next()?;
            }
        }
    }

    // The records of root keys, under every prefix.
    let mut records = source.records(Keyspace::Roots);
    while let Some((stored, value)) = records.current()? {
        let Some((prefix, key)) = stored.split_first_chunk::<32>() else {
            return Err(short_key(source, Keyspace::Roots, stored));
        };
        let handle = engine.subtree_by_prefix(*prefix, None, &transaction).value;
        let kept = NodeStore::root_key(&handle).value;
        let kept = kept.map_err(|error| target_error("the record of a root key", error))?;
        match key == ROOT_KEY && kept.as_deref() == Some(value) {
            true => compared.roots += 1,
            false => compared.differs(format!(
                "the record of the root key of the prefix {} is {} in the source and {} in the \
                 target",
                hex(prefix),
                hex(value),
                kept.map_or("not there".to_owned(), |kept| hex(&kept))
            )),
        }
        records.next();
    }
    let namespace = Subtree::new(*pending_prefix_drops_namespace());
    let held = |column| -> Result<Chunk, ToolError> {
        Ok(records_of(engine, namespace, column)?.unwrap_or_default())
    };
    // The namespace is entered as the engine enters it: under no element,
    // with no tree type and with no Merk.
    {
        let prefix = *pending_prefix_drops_namespace();
        let handle = engine.subtree_by_prefix(prefix, None, &transaction).value;
        let entry = handle.entry();
        match entry.map_err(|error| target_error("the directory", error))? {
            None => compared.differs(
                "the directory of the target has no entry for the namespace of the \
                 pending-drop records"
                    .to_owned(),
            ),
            Some(entry) => {
                let as_entered =
                    entry.host.is_none() && entry.root_key.is_none() && entry.tree_type.is_empty();
                if !as_entered {
                    compared.differs(
                        "the directory of the target has the namespace of the pending-drop \
                         records under a host, with a root or with a tree type: the engine \
                         enters it with none of the three"
                            .to_owned(),
                    );
                }
            }
        }
    }
    if let Some((key, _)) = held(Column::Data)?.first() {
        compared.differs(format!(
            "the namespace of the pending-drop records holds a data record under the key {}",
            hex(key)
        ));
    }
    let own = records_of(engine, Subtree::new(ROOT), Column::Roots)?;
    let kept = held(Column::Roots)?.len() + own.map_or(0, |own| own.len());
    if kept as u64 != compared.roots {
        compared.differs(format!(
            "the target keeps {kept} records of root keys, and the source has {}",
            compared.roots
        ));
    }

    // The pending-drop records, and the marks they leave.
    let mut expected_marks = BTreeSet::new();
    let mut dropped = Vec::new();
    for (key, value, marks) in pending_drops(source, |prefix| {
        let handle = engine.subtree_by_prefix(*prefix, None, &transaction).value;
        let entry = handle.entry();
        Ok((entry.map_err(|error| target_error("the directory", error))?).is_some())
    })? {
        dropped.push((key, value));
        expected_marks.extend(marks);
    }
    // The records as the target stores them, in the order of their keys
    // as the source's are: a record there that is no pending-drop record
    // is a difference, and no read that failed.
    let read_back = held(Column::Meta)?;
    match read_back == dropped {
        true => compared.meta += dropped.len() as u64,
        false => compared.differs(format!(
            "the target has {} pending-drop records and the source {}, or they differ",
            read_back.len(),
            dropped.len()
        )),
    }
    // A mark is its key: it has no value.
    let marks = held(Column::Aux)?;
    compared.marks = marks.len() as u64;
    let expected: Chunk = (expected_marks.into_iter())
        .map(|mark| (mark, Vec::new()))
        .collect();
    if marks != expected {
        compared.differs(format!(
            "the target has {} marks of what flat drops left, and the source's records leave \
             {}, or a mark has a value",
            marks.len(),
            expected.len()
        ));
    }

    // The directory: each subtree of the walk, each prefix beside it that
    // is carried, the namespace, and no other.
    let mut entries = 0u64;
    {
        let snapshot = engine.database().snapshot();
        let operation = snapshot.operation();
        let mut walk = operation.entries()?;
        while let Some(listed) = walk.next()? {
            entries += u64::from(listed.live);
        }
        let root = operation.open(Subtree::new(ROOT))?;
        let mut journal = Vec::new();
        if root.is_some_and(|root| {
            root.get(Column::Aux, JOURNAL_KEY, &mut journal)
                .unwrap_or(true)
        }) {
            compared.differs("the target holds the journal of a migration".to_owned());
        }
    }
    let expected = compared.subtrees + 1 + compared.unreachable_carried;
    if entries != expected {
        compared.differs(format!(
            "the directory of the target has {entries} subtrees in use, and the source has \
             {expected} with the namespace of the pending-drop records"
        ));
    }

    let found = target.root_hash(None, GroveVersion::latest()).value;
    let found = found.map_err(|error| target_error("the root hash", error))?;
    if found != *root_hash {
        compared.differs(format!(
            "the root hash of the target is {}, and the source's is {}",
            hex(&found),
            hex(root_hash)
        ));
    }
    Ok(compared)
}
