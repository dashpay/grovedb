//! The load of a migration: the writes that make the target, a commit at a
//! time.
//!
//! # What a commit holds
//!
//! One write transaction of the target's database, in two operations:
//!
//! 1. The subtrees that the walk entered since the last commit
//!    (`SubtreeWrite::Enter`, each under its element at its position and
//!    with its tree type, a subtree after the one its element is in); the
//!    Merks it left, each as one change set that is its whole Merk
//!    (`MerkWrite::whole`, a load of `transactions.md` LD1), with the
//!    Merks that each node's element hosts stated in the node's put; the
//!    flat, aux and meta records it handed over; and the journal as the
//!    walk stands after them.
//! 2. The node records that no tree links, each as a put that the root of
//!    its Merk does not reach, which the engine keeps as an unlinked
//!    record. They are an operation of their own because a change set
//!    that is its whole Merk holds the tree and nothing else.
//!
//! No commit drops or clears a subtree: by key a batch with a load beside
//! one is refused (`BatchError::LoadBeside`).
//!
//! **How much a commit takes** is counted as the projection of a run
//! counts it. For each node of a Merk: its key and value, and what a node
//! is measured to take beside them (`survey::PEAK_A_NODE`). That figure
//! is the most the process holds for a node over the steps of a commit:
//! the migrator's nodes are held until the transaction has taken them,
//! and the transaction's records and the encoded changes of their pages
//! until the commit is made. For each record that is no node: the same
//! figure, and its key and value `survey::COPIES_OF_A_RECORD` times. For
//! each Merk: a page of the store, since the loads of a batch are laid
//! out in pages of their own. So a grove of many small Merks is committed
//! after a few thousand of them, one of many nodes after some tens of
//! thousands of nodes, and one of large values after as many bytes. A
//! commit ends after the Merk that fills it, and a prefix that nothing
//! leads to is carried in one commit whatever it holds, with the commit
//! ended after it where it filled it.
//!
//! **A Merk is handed over after the Merks its nodes host.** So each node
//! is written once, with the place of every pointer it has: the cell of a
//! tree's element points at the root of each Merk the element hosts from
//! the load of its own Merk on, and nothing names a Merk afterwards. A
//! Merk whose host's Merk is not in the same commit is written anchored:
//! its root waits for that load.
//!
//! # What a stop leaves
//!
//! A commit is whole or not there, and holds the journal of its own end
//! ([`super::journal`]). A Merk is one change set of one commit, so it is
//! in the target whole or not at all, and a load that goes on hands over
//! no Merk a second time: the journal says which were left. Flat, aux and
//! meta records and the records of root keys are puts, and a put of a
//! record over itself changes nothing that a read shows: a subtree of flat
//! records whose load was cut is handed over from its first record again.
//! What was carried of a subtree is counted when the walk leaves it, so
//! such a subtree is counted once.
//!
//! # After the grove
//!
//! The records of root keys are carried next, under every prefix of the
//! source, with the stored key of the last one in the journal. Then, where
//! the migration carries what the source holds beside its grove
//! ([`super::beside`]), each such prefix whole in one commit, which can
//! hold several of them: a prefix that is entered was carried by a run
//! before. The last commit holds the pending-drop records with their
//! marks, each mark once, and removes the journal.

use std::{
    collections::{BTreeSet, HashSet},
    io::Write,
    time::{Duration, Instant},
};

use grovedb::{Engine, LigninEngine};
use grovedb_merk::{
    cost_model::Keyspace as MerkKeyspace,
    store::lignin::{keeps_flat_records, left_behind_key, tree_type_bytes},
    TreeType,
};
use grovedb_storage::flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord};
use lignin_tree::hash::Stale;
use lignindb::{
    commit::{Committed, NoStaleHashes},
    txn::{
        Batch, Column, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, Subtree,
        SubtreeWrite, ValuePut,
    },
    Db,
};

use super::{
    beside::{beside, doomed, nodes},
    grove::{Link, Node},
    journal::{Carried, Journal, Phase, JOURNAL_KEY},
    source::{Keyspace, Prefix, Source},
    survey::{COPIES_OF_A_RECORD, PEAK_A_NODE},
    walk::{short_key, Chunk, Found, Frame, MerkHeld, Place, Side, Visit, ROOT, ROOT_KEY},
};
use crate::{args::Unreachable, hex, ToolError};

/// Enters the two subtrees that no element hosts and that every grove
/// has, where `db` has them not: the root Merk, a normal tree, and the
/// namespace of the pending-drop records, which keeps no tree. They are
/// what the engine of GroveDB enters at its making
/// (`grovedb::LigninEngine::new`), which then finds them.
pub(crate) fn base(db: &Db) -> Result<(), ToolError> {
    let normal = tree_type_bytes(TreeType::NormalTree);
    let none: &[u8] = &[];
    let namespace = Subtree::new(*pending_prefix_drops_namespace());
    let mut enters = Vec::new();
    {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        for (subtree, tree_type) in [(Subtree::new(ROOT), &normal[..]), (namespace, none)] {
            if operation.open(subtree)?.is_none() {
                enters.push(SubtreeWrite::EnterIfAbsent {
                    subtree,
                    tree_type,
                    host: None,
                });
            }
        }
    }
    if enters.is_empty() {
        return Ok(());
    }
    let batch = Batch {
        subtrees: &enters,
        ..Batch::default()
    };
    db.write(&batch, &mut NoStaleHashes)?;
    Ok(())
}

/// What a commit counts for `nodes` nodes of a Merk with `bytes` of keys
/// and values, as the projection of a run counts them: what a node is
/// measured to take beside its key and value for each, and the bytes. It
/// does not wrap.
fn weight_of_nodes(nodes: usize, bytes: usize) -> usize {
    (PEAK_A_NODE as usize)
        .saturating_mul(nodes)
        .saturating_add(bytes)
}

/// What a commit counts for one record that is no node, with `bytes` of
/// key and value, as the projection counts a record that is read whole
/// with a carried prefix: a node's figure, and the bytes as often as they
/// are held. It does not wrap.
fn weight_of_a_record(bytes: usize) -> usize {
    (COPIES_OF_A_RECORD as usize)
        .saturating_mul(bytes)
        .saturating_add(PEAK_A_NODE as usize)
}

/// A subtree to enter.
struct Entered {
    subtree: Subtree,
    tree_type: Vec<u8>,
    host: Option<(Subtree, Vec<u8>, u8)>,
}

/// A Merk to hand over.
struct Handed {
    subtree: Subtree,
    /// The subtree of the Merk its host is a node of.
    host: Option<Subtree>,
    merk: MerkHeld,
}

/// What the next commit writes.
#[derive(Default)]
struct Pending {
    enters: Vec<Entered>,
    merks: Vec<Handed>,
    records: Vec<(Column, Subtree, Vec<u8>, Vec<u8>)>,
    /// The bytes of the keys and values of all of it.
    bytes: usize,
}

fn link(link: &Option<Link>) -> Option<LinkPut<'_>> {
    link.as_ref().map(|link| LinkPut {
        key: &link.key,
        hash: &link.hash,
        heights: link.heights,
        aggregate: &link.aggregate,
    })
}

/// The put of `node`, which hosts `hosted`.
fn put<'a>(node: &'a Node, hosted: &'a [Subtree]) -> NodePut<'a> {
    NodePut {
        key: &node.key,
        feature: &node.feature,
        value_hash: &node.value_hash,
        kv_hash: &node.kv_hash,
        left: link(&node.left),
        right: link(&node.right),
        hosted: Hosted::Set(hosted),
        value: ValuePut::Value(&node.value),
        stale: Stale::NONE,
        at: None,
    }
}

/// The column of the target that a keyspace of the source is.
pub(crate) const fn column(keyspace: Keyspace) -> Column {
    match keyspace {
        Keyspace::Data => Column::Data,
        Keyspace::Aux => Column::Aux,
        Keyspace::Roots => Column::Roots,
        Keyspace::Meta => Column::Meta,
    }
}

/// The key of the mark that stands for what a flat drop left under
/// `prefix` in `keyspace`, in the aux keyspace of the namespace of the
/// pending-drop records.
pub(crate) fn mark_key(keyspace: Keyspace, prefix: &Prefix) -> Option<Vec<u8>> {
    let keyspace = match keyspace {
        Keyspace::Data => MerkKeyspace::Data,
        Keyspace::Aux => MerkKeyspace::Aux,
        Keyspace::Meta => MerkKeyspace::Meta,
        Keyspace::Roots => MerkKeyspace::Roots,
    };
    left_behind_key(keyspace, prefix)
}

/// Whether the latest version of `db` has the subtree `prefix` entered.
pub(crate) fn is_entered(db: &Db, prefix: &Prefix) -> Result<bool, ToolError> {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    Ok(operation.open(Subtree::new(*prefix))?.is_some())
}

/// Whether the latest version of `db` has the subtree `prefix` entered
/// under a host: every subtree of the grove is, but its root Merk.
fn is_hosted(db: &Db, prefix: &Prefix) -> Result<bool, ToolError> {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let opened = operation.open(Subtree::new(*prefix))?;
    Ok(opened.is_some_and(|opened| opened.entry().host().is_some()))
}

/// A pending-drop record as it is stored, its key and its value, with the
/// marks it leaves.
pub(crate) type Dropped = (Vec<u8>, Vec<u8>, Vec<Vec<u8>>);

/// The pending-drop records of `source` as they are stored, each with the
/// marks it leaves: one for each keyspace of each prefix it names that is
/// not `live` and that the source holds a record under.
pub(crate) fn pending_drops(
    source: &Source,
    mut live: impl FnMut(&Prefix) -> Result<bool, ToolError>,
) -> Result<Vec<Dropped>, ToolError> {
    let mut stored = Vec::new();
    source.subtree(
        Keyspace::Meta,
        pending_prefix_drops_namespace(),
        |key, value| {
            stored.push((key.to_vec(), value.to_vec()));
            Ok(())
        },
    )?;
    let mut drops = Vec::new();
    for (key, value) in stored {
        let record = PendingPrefixDropRecord::decode(&key, &value).map_err(|error| {
            ToolError::Migrate(format!(
                "the source {}: a pending-drop record: {error}",
                source.dir().display()
            ))
        })?;
        let mut marks = Vec::new();
        for prefix in &record.doomed_prefixes {
            if live(prefix)? {
                continue;
            }
            for keyspace in [Keyspace::Data, Keyspace::Aux, Keyspace::Meta] {
                if source.holds(keyspace, prefix)? {
                    marks.extend(mark_key(keyspace, prefix));
                }
            }
        }
        drops.push((key, value, marks));
    }
    Ok(drops)
}

/// The load of one target.
pub(crate) struct Load<'t> {
    db: &'t Db,
    journal: Journal,
    budget: usize,
    pending: Pending,
    /// What was carried of the subtree the walk of the grove is in. The
    /// journal counts it once the subtree is left: a subtree that a stop
    /// comes into is carried again from its first record, and is counted
    /// once.
    within: Carried,
    out: &'t mut dyn Write,
    /// How often the load says where it stands.
    every: Duration,
    began: Instant,
    said: Instant,
    /// The commits this run made.
    pub(crate) commits: u64,
}

impl<'t> Load<'t> {
    /// A load into `db` that goes on from `journal`, which one commit
    /// takes about `budget` bytes of records at a time. It says where it
    /// stands to `out`, at the commits that are `every` apart or more.
    pub(crate) fn new(
        db: &'t Db,
        journal: Journal,
        budget: usize,
        every: Duration,
        out: &'t mut dyn Write,
    ) -> Self {
        let now = Instant::now();
        Self {
            db,
            journal,
            budget,
            pending: Pending::default(),
            within: Carried::default(),
            out,
            every,
            began: now,
            said: now,
            commits: 0,
        }
    }

    /// Where the load stands.
    pub(crate) fn journal(&self) -> &Journal {
        &self.journal
    }

    fn record(&mut self, column: Column, subtree: Subtree, key: Vec<u8>, value: Vec<u8>) {
        let bytes = key.len() + value.len();
        self.charge(weight_of_a_record(bytes));
        self.within.bytes += bytes as u64;
        self.pending.records.push((column, subtree, key, value));
    }

    /// Counts `weight` for the commit that is pending.
    fn charge(&mut self, weight: usize) {
        self.pending.bytes = self.pending.bytes.saturating_add(weight);
    }

    fn say(&mut self, always: bool) -> Result<(), ToolError> {
        if !always && self.said.elapsed() < self.every {
            return Ok(());
        }
        self.said = Instant::now();
        let mut carried = self.journal.carried;
        carried.add(&self.within);
        writeln!(
            self.out,
            "migrate: phase={} subtrees={} merks={} nodes={} records={} bytes={} commits={} \
             seconds={:.1}",
            self.journal.phase.name(),
            carried.subtrees,
            carried.merks,
            carried.nodes,
            carried.records(),
            carried.bytes,
            self.commits,
            self.began.elapsed().as_secs_f64()
        )?;
        Ok(())
    }

    /// Commits what is pending with the journal as it stands, or, as the
    /// `last` commit of the load, without it.
    fn commit(&mut self, last: bool) -> Result<(), ToolError> {
        if self.journal.phase != Phase::Grove {
            // After the grove, the journal says for each record whether
            // it was carried.
            let within = std::mem::take(&mut self.within);
            self.journal.carried.add(&within);
        }
        let pending = std::mem::take(&mut self.pending);
        let txn = self.db.begin();
        {
            let journal = self.journal.encode();
            let root = Subtree::new(ROOT);
            let mut records: Vec<RecordWrite<'_>> = (pending.records.iter())
                .map(|(column, subtree, key, value)| {
                    RecordWrite::put(*column, *subtree, key, value)
                })
                .collect();
            records.push(match last {
                true => RecordWrite::delete(Column::Aux, root, JOURNAL_KEY),
                false => RecordWrite::put(Column::Aux, root, JOURNAL_KEY, &journal),
            });
            let enters: Vec<SubtreeWrite<'_>> = (pending.enters.iter())
                .map(|entered| SubtreeWrite::Enter {
                    subtree: entered.subtree,
                    tree_type: &entered.tree_type,
                    host: entered
                        .host
                        .as_ref()
                        .map(|(subtree, key, position)| HostPut {
                            subtree: *subtree,
                            key,
                            position: *position,
                        }),
                })
                .collect();

            let with_nodes = |handed: &&Handed| !handed.merk.nodes.is_empty();
            let here: HashSet<Subtree> = (pending.merks.iter())
                .filter(with_nodes)
                .map(|handed| handed.subtree)
                .collect();
            let puts: Vec<Vec<NodePut<'_>>> = (pending.merks.iter())
                .filter(with_nodes)
                .map(|handed| {
                    let nodes = handed.merk.nodes.iter();
                    nodes.map(|node| put(node, &node.hosted)).collect()
                })
                .collect();
            let loads: Vec<MerkWrite<'_>> = (pending.merks.iter().filter(with_nodes))
                .zip(&puts)
                .map(|(handed, nodes)| MerkWrite {
                    subtree: handed.subtree,
                    nodes,
                    removed: &[],
                    root: RootPut::Set(handed.merk.root.as_deref()),
                    // The Merk of the host comes after the Merks it hosts:
                    // where it is of a later commit, the root waits for it.
                    anchored: handed.host.is_some_and(|host| !here.contains(&host)),
                    whole: true,
                })
                .collect();

            let unlinked_of = |handed: &&Handed| !handed.merk.unlinked.is_empty();
            let left: Vec<Vec<NodePut<'_>>> = (pending.merks.iter())
                .filter(unlinked_of)
                .map(|handed| {
                    let records = handed.merk.unlinked.iter();
                    records.map(|node| put(node, &[])).collect()
                })
                .collect();
            let unlinked: Vec<MerkWrite<'_>> = (pending.merks.iter().filter(unlinked_of))
                .zip(&left)
                .map(|(handed, nodes)| MerkWrite {
                    subtree: handed.subtree,
                    nodes,
                    removed: &[],
                    root: RootPut::Keep,
                    anchored: handed.host.is_some(),
                    whole: false,
                })
                .collect();

            let scope = txn.scope()?;
            let first = Batch {
                records: &records,
                merks: &loads,
                subtrees: &enters,
            };
            scope.apply_by_kind(&first, keeps_flat_records)?;
            if !unlinked.is_empty() {
                let second = Batch {
                    merks: &unlinked,
                    ..Batch::default()
                };
                scope.apply_by_kind(&second, keeps_flat_records)?;
            }
            scope.merge();
        }
        // The transaction has its own of every record. The nodes that
        // were handed over, and the puts that named them, are let go of
        // before the commit makes its log group, which is as large as
        // they are for a Merk of many nodes.
        drop(pending);
        match self.db.commit(&txn, &mut NoStaleHashes)? {
            Committed::Published(_) => self.commits += 1,
            // The journal is written or removed by every commit, so only
            // the removal of one that is not there writes nothing.
            Committed::Empty => {}
        }
        self.say(last)
    }

    /// Commits what is pending if it has reached the budget of a commit.
    fn commit_if_due(&mut self) -> Result<(), ToolError> {
        match self.pending.bytes >= self.budget {
            true => self.commit(false),
            false => Ok(()),
        }
    }

    /// Carries the records of root keys of `source`, from where the
    /// journal says: each to where a handle of `engine` keeps the record
    /// of its prefix, also under a prefix with no tree.
    pub(crate) fn roots(
        &mut self,
        source: &Source,
        engine: &LigninEngine,
    ) -> Result<(), ToolError> {
        debug_assert_eq!(self.journal.phase, Phase::Roots);
        let mut records = source.records(Keyspace::Roots);
        if let Some(done) = self.journal.roots.clone() {
            records.seek(&done);
            if records.current()?.is_some_and(|(stored, _)| stored == done) {
                records.next();
            }
        }
        loop {
            {
                let transaction = engine.start_read_transaction();
                while let Some((stored, value)) = records.current()? {
                    let Some((prefix, key)) = stored.split_first_chunk::<32>() else {
                        return Err(short_key(source, Keyspace::Roots, stored));
                    };
                    if key != ROOT_KEY {
                        return Err(ToolError::Migrate(format!(
                            "the source {}: a record of the roots keyspace is no record of a \
                             root key",
                            source.dir().display()
                        )));
                    }
                    let handle = engine.subtree_by_prefix(*prefix, None, &transaction).value;
                    let (home, kept_under) = handle.root_key_place();
                    let (stored, value) = (stored.to_vec(), value.to_vec());
                    self.journal.carried.roots += 1;
                    self.record(Column::Roots, Subtree::new(home), kept_under, value);
                    self.journal.roots = Some(stored);
                    records.next();
                    if self.pending.bytes >= self.budget {
                        break;
                    }
                }
            }
            if records.current()?.is_none() {
                break;
            }
            self.commit(false)?;
        }
        self.journal.phase = Phase::Drops;
        self.journal.roots = None;
        self.commit(false)
    }

    /// Carries what `source` holds under the prefix `prefix`, which
    /// nothing leads to: its subtree is entered with no host, its data
    /// records are records that no tree links, and its aux and meta
    /// records are as they are.
    fn unreachable(&mut self, source: &Source, prefix: Prefix) -> Result<(), ToolError> {
        let (tree_type, nodes, _) = nodes(source, &prefix, true)?.map_err(|other| {
            ToolError::Migrate(format!(
                "the source {}: the records under the prefix {}, which nothing leads to, are \
                 not carried: {other}",
                source.dir().display(),
                hex(&prefix)
            ))
        })?;
        let subtree = Subtree::new(prefix);
        let held = nodes.iter();
        let bytes: usize = held.map(|node| node.key.len() + node.value.len()).sum();
        let carried = &mut self.journal.carried;
        carried.subtrees += 1;
        carried.unreachable += 1;
        carried.unlinked += nodes.len() as u64;
        carried.bytes += bytes as u64;
        self.charge(weight_of_nodes(nodes.len(), bytes).saturating_add(96));
        self.pending.enters.push(Entered {
            subtree,
            tree_type: tree_type_bytes(tree_type.unwrap_or(TreeType::NormalTree)),
            host: None,
        });
        self.pending.merks.push(Handed {
            subtree,
            host: None,
            merk: MerkHeld {
                root: None,
                nodes: Vec::new(),
                unlinked: nodes,
            },
        });
        for keyspace in [Keyspace::Aux, Keyspace::Meta] {
            let mut records = Vec::new();
            source.subtree(keyspace, &prefix, |key, value| {
                records.push((key.to_vec(), value.to_vec()));
                Ok(())
            })?;
            let carried = &mut self.journal.carried;
            match keyspace {
                Keyspace::Aux => carried.aux += records.len() as u64,
                _ => carried.meta += records.len() as u64,
            }
            for (key, value) in records {
                self.record(column(keyspace), subtree, key, value);
            }
        }
        Ok(())
    }

    /// Carries what `source` holds beside its grove, where the journal
    /// says that it is carried, and the pending-drop records with the
    /// marks of what they left, and ends the load: the last commit
    /// removes the journal. `side` is the source's aux and meta keyspaces.
    pub(crate) fn drops(&mut self, source: &Source, side: &Side) -> Result<(), ToolError> {
        debug_assert_eq!(self.journal.phase, Phase::Drops);
        let namespace = Subtree::new(*pending_prefix_drops_namespace());
        let db = self.db;
        if self.journal.unreachable == Unreachable::Carry {
            let doomed = doomed(source)?;
            let held = beside(source, side, |prefix| {
                Ok(*prefix == ROOT || is_hosted(db, prefix)?)
            })?;
            for prefix in held.into_keys().filter(|prefix| !doomed.contains(prefix)) {
                // A prefix is carried by one commit, whole: one that is
                // entered was carried by a run before this one, and the
                // journal of that commit counts it.
                if is_entered(db, &prefix)? {
                    continue;
                }
                self.unreachable(source, prefix)?;
                self.commit_if_due()?;
            }
        }
        // Two records can name one prefix: its mark is one record.
        let mut marks = BTreeSet::new();
        for (key, value, left) in pending_drops(source, |prefix| is_entered(db, prefix))? {
            self.journal.carried.meta += 1;
            self.record(Column::Meta, namespace, key, value);
            marks.extend(left);
        }
        for mark in marks {
            self.journal.carried.marks += 1;
            self.record(Column::Aux, namespace, mark, Vec::new());
        }
        self.commit(true)
    }
}

impl Visit for Load<'_> {
    fn takes_records(&self) -> bool {
        true
    }

    fn enter(&mut self, place: &Place, at: &[Frame]) -> Result<(), ToolError> {
        self.journal.frames = at.to_vec();
        // The engine entered the root Merk when it made the database.
        if let Some(host) = &place.host {
            self.journal.carried.subtrees += 1;
            self.pending.enters.push(Entered {
                subtree: Subtree::new(place.prefix),
                tree_type: tree_type_bytes(place.tree_type),
                host: Some((Subtree::new(host.parent), host.key.clone(), host.position)),
            });
            self.charge(host.key.len() + 96);
        }
        self.commit_if_due()
    }

    fn found(&mut self, place: &Place, found: &Found) -> Result<(), ToolError> {
        match found.faults.first() {
            None => Ok(()),
            Some(fault) => Err(ToolError::Migrate(format!(
                "the source is not what its survey found: {}: {fault}",
                place.named()
            ))),
        }
    }

    fn records(
        &mut self,
        place: &Place,
        keyspace: Keyspace,
        records: Chunk,
        at: &[Frame],
    ) -> Result<(), ToolError> {
        self.journal.frames = at.to_vec();
        let subtree = Subtree::new(place.prefix);
        let count = records.len() as u64;
        let carried = &mut self.within;
        match keyspace {
            Keyspace::Data => carried.flat += count,
            Keyspace::Aux => carried.aux += count,
            Keyspace::Meta => carried.meta += count,
            Keyspace::Roots => carried.roots += count,
        }
        for (key, value) in records {
            self.record(column(keyspace), subtree, key, value);
        }
        self.commit_if_due()
    }

    fn merk(&mut self, place: &Place, merk: MerkHeld) -> Result<(), ToolError> {
        let carried = &mut self.within;
        carried.merks += u64::from(!merk.nodes.is_empty());
        carried.nodes += merk.nodes.len() as u64;
        carried.unlinked += merk.unlinked.len() as u64;
        let held = merk.nodes.iter().chain(&merk.unlinked);
        let bytes: usize = held.map(|node| node.key.len() + node.value.len()).sum();
        carried.bytes += bytes as u64;
        // What the nodes take, with their keys and values, and the page a
        // Merk's load begins.
        let nodes = merk.nodes.len() + merk.unlinked.len();
        self.charge(weight_of_nodes(nodes, bytes).saturating_add(self.db.page_size().get()));
        self.pending.merks.push(Handed {
            subtree: Subtree::new(place.prefix),
            host: place.host.as_ref().map(|host| Subtree::new(host.parent)),
            merk,
        });
        Ok(())
    }

    fn leave(&mut self, place: &Place, at: &[Frame]) -> Result<(), ToolError> {
        self.journal.frames = at.to_vec();
        let within = std::mem::take(&mut self.within);
        self.journal.carried.add(&within);
        if place.host.is_none() {
            // The root Merk is left last: the grove is carried.
            self.journal.phase = Phase::Roots;
            return self.commit(false);
        }
        self.commit_if_due()
    }
}
