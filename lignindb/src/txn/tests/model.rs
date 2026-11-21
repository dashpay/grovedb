//! A model of the records of a database and of what a write transaction
//! reads, in maps: the committed records, the writes of each open
//! transaction with their first-write versions, and the conflict rule
//! evaluated directly (`transactions.md` RD3, RD5, CF2).
//!
//! The Merks of the model are treaps whose priorities come from the keys,
//! so the shape of a Merk follows from its keys alone, and a change writes
//! the nodes whose records differ, as a Merk does.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::tree_page::{Hash, HashField, Side};
use lignin_tree::{hash::Stale, node::NodeHandle};

pub use super::store::hash_of;
use super::store::{mix, Rng};
use crate::txn::{
    Batch, Column, Entering, HostPut, Hosted, LinkPut, MerkWrite, NodePut, NodeView, RecordWrite,
    RootPut, Scope, Subtree, SubtreeWrite, TxnError, Unit, ValuePut,
};

pub fn subtree(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

/// What a write states for a hash it leaves stale.
pub const UNMADE: Hash = [0xee; 32];

/// A link of a node record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRec {
    pub key: Vec<u8>,
    pub hash: Hash,
    pub heights: (u8, u8),
    pub aggregate: Vec<u8>,
}

/// The record of one Merk node, as Merk hands it to storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRec {
    pub value: Vec<u8>,
    pub feature: Vec<u8>,
    pub value_hash: Hash,
    pub kv_hash: Hash,
    pub left: Option<LinkRec>,
    pub right: Option<LinkRec>,
}

impl NodeRec {
    pub fn link(&self, side: Side) -> Option<&LinkRec> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }

    /// Checks that `view` shows this record.
    pub fn check(&self, key: &[u8], view: &NodeView<'_>) {
        assert_eq!(view.key(), key);
        assert_eq!(super::value_of(view), self.value, "the value of {key:?}");
        assert_eq!(view.value_len() as usize, self.value.len());
        assert_eq!(view.feature(), self.feature, "the feature of {key:?}");
        assert_eq!(view.value_hash(), &self.value_hash);
        assert_eq!(view.kv_hash(), &self.kv_hash);
        for side in [Side::Left, Side::Right] {
            let (shown, expected) = (view.link(side), self.link(side));
            assert_eq!(shown.is_some(), expected.is_some(), "a link of {key:?}");
            let (Some(shown), Some(expected)) = (shown, expected) else {
                continue;
            };
            assert_eq!(shown.hash, &expected.hash, "a child hash of {key:?}");
            assert_eq!(shown.heights, expected.heights);
            assert_eq!(shown.child_key_len as usize, expected.key.len());
            assert_eq!(shown.aggregate, expected.aggregate);
            if let Some(child) = shown.key {
                assert_eq!(child, expected.key);
            }
        }
    }
}

/// The node records of the Merk that holds `items`, by key, and the key of
/// its root.
pub fn records(
    items: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> (Option<Vec<u8>>, BTreeMap<Vec<u8>, NodeRec>) {
    fn build(
        keys: &[(&Vec<u8>, &Vec<u8>)],
        out: &mut BTreeMap<Vec<u8>, NodeRec>,
    ) -> Option<(LinkRec, u8)> {
        let top = (0..keys.len()).max_by_key(|at| mix(7, keys[*at].0))?;
        let (key, value) = keys[top];
        let left = build(&keys[..top], out);
        let right = build(&keys[top + 1..], out);
        let count = keys.len() as u64;
        let value_hash = hash_of(&[value]);
        let kv_hash = hash_of(&[key, &value_hash]);
        let none = [0u8; 32];
        let hash = hash_of(&[
            &kv_hash,
            left.as_ref().map_or(&none, |(link, _)| &link.hash),
            right.as_ref().map_or(&none, |(link, _)| &link.hash),
        ]);
        let heights = (
            left.as_ref().map_or(0, |(_, height)| *height),
            right.as_ref().map_or(0, |(_, height)| *height),
        );
        let mut feature = vec![3];
        feature.extend_from_slice(&count.to_le_bytes());
        out.insert(
            key.clone(),
            NodeRec {
                value: value.clone(),
                feature: feature.clone(),
                value_hash,
                kv_hash,
                left: left.map(|(link, _)| link),
                right: right.map(|(link, _)| link),
            },
        );
        let link = LinkRec {
            key: key.clone(),
            hash,
            heights,
            aggregate: feature,
        };
        Some((link, heights.0.max(heights.1) + 1))
    }
    let keys: Vec<_> = items.iter().collect();
    let mut out = BTreeMap::new();
    let root = build(&keys, &mut out).map(|(link, _)| link.key);
    (root, out)
}

/// What a record key holds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Val {
    Flat(Vec<u8>),
    Node(NodeRec),
}

/// One write of a transaction (ST5).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Write {
    Put(Unit, Vec<u8>, Val),
    Delete(Unit, Vec<u8>),
    Range(Unit, Vec<u8>, Option<Vec<u8>>),
    /// A drop of a subtree, or a clear, which keeps the subtree and every
    /// column of it but the data: one write (CV9, CF5).
    Leave(Left),
    /// A subtree that is entered, with the subtree of the element that
    /// hosts it. It writes no record and is no write of the write set: it
    /// stands among the writes so that it is taken back with them, and
    /// says which subtree is below which.
    Enter(Subtree, Option<Subtree>),
    /// A subtree that is stated anew: its tree type, and its place among
    /// the subtrees of its host. Like an enter it writes no record and
    /// meets no write, and it stands among the writes so that it is taken
    /// back with them.
    Stated(Subtree),
    /// The moves of one batch, each of a subtree with those below it,
    /// which are made together: one write, of every key under every
    /// prefix their statements name.
    Move(Moved),
}

/// What the moves of one batch do.
///
/// What the write covers is settled when it is stated and stays: every
/// prefix the statements name, and the subtrees that left then
/// (`transactions.md` CF10). Which subtrees it takes where is worked out
/// when it is stated, and again whenever the transaction's writes are
/// applied over a commit (WT7): `pairs`, `gone` and `stopped` say that
/// ([`Self::lay`]), for a layout that is made. Whether a layout is made
/// is the engine's to say, but for the one case of `stopped`: the model
/// does not ask whether a moved subtree is there still, or whether a
/// prefix that a subtree goes to is free.
#[derive(Debug, Clone, Default)]
pub struct Moved {
    /// Every prefix the statements name, on either side.
    pub named: Vec<Subtree>,
    /// The subtrees that are moved, by the prefix each has before.
    pub tops: Vec<Subtree>,
    /// Every pair the statements make, of a moved subtree and of one
    /// below it: the prefix before and the prefix after, whether a
    /// subtree is there or not.
    pub stated: Vec<(Subtree, Subtree)>,
    /// The pairs of a layout that is made: the stated pair of each moved
    /// subtree, and those of the other stated pairs whose first prefix
    /// has a subtree below a moved one.
    pub pairs: Vec<(Subtree, Subtree)>,
    /// The subtrees below the moved ones that left with them when the
    /// move was stated: with `named`, what the write covers in the write
    /// set and in the key set of its commit.
    pub left: Vec<Subtree>,
    /// The subtrees below the moved ones that no stated pair names, and
    /// that leave with the move.
    pub gone: Vec<Subtree>,
    /// Whether a stated pair names a subtree below one that leaves: such
    /// a move is not made (MV2), and the model says nothing of what the
    /// transaction then reads. It is the one such case the model tells.
    pub stopped: bool,
}

impl Moved {
    /// The prefix that the subtree which the move brings to `to` had.
    fn source(&self, to: Subtree) -> Option<Subtree> {
        let brought = self.pairs.iter().find(|(_, new)| *new == to);
        brought.map(|(old, _)| *old)
    }

    /// The prefix that the subtree which the move takes from `from` has
    /// afterwards.
    fn target(&self, from: Subtree) -> Option<Subtree> {
        let taken = self.pairs.iter().find(|(old, _)| *old == from);
        taken.map(|(_, new)| *new)
    }

    /// Whether the move takes a subtree from `from`: to another prefix,
    /// or out.
    fn takes(&self, from: Subtree) -> bool {
        self.target(from).is_some() || self.gone.contains(&from)
    }

    /// Every prefix the write covers.
    pub fn covered(&self) -> impl Iterator<Item = Subtree> + '_ {
        self.named.iter().chain(&self.left).copied()
    }

    /// Whether the write covers every key under `subtree`.
    fn covers(&self, subtree: Subtree) -> bool {
        self.covered().any(|covered| covered == subtree)
    }

    /// Works out which subtrees the move takes where, from `hosts`, which
    /// has the subtrees as they hang where the move is made: of the
    /// subtrees below a moved one, those that a stated pair names go with
    /// it, and the others leave. A pair for a subtree below that is not
    /// there takes none. What the write covers is not changed.
    pub fn lay(&mut self, hosts: &Hosts) {
        let under: Vec<Subtree> = (self.tops.iter())
            .flat_map(|top| below(hosts, *top))
            .collect();
        let there = |old: &Subtree| self.tops.contains(old) || under.contains(old);
        let pairs: Vec<(Subtree, Subtree)> = (self.stated.iter().copied())
            .filter(|(old, _)| there(old))
            .collect();
        let named = |subtree: &Subtree| pairs.iter().any(|(old, _)| old == subtree);
        let gone: Vec<Subtree> = (under.iter().copied())
            .filter(|under| !named(under))
            .collect();
        let leaves = |old: &Subtree| hosts.get(old).is_some_and(|host| gone.contains(host));
        self.stopped = pairs.iter().any(|(old, _)| leaves(old));
        self.pairs = pairs;
        self.gone = gone;
    }
}

/// Which subtree each hosted subtree is below: the subtree of its host.
pub type Hosts = BTreeMap<Subtree, Subtree>;

/// Takes `write` into `hosts`: a subtree that is entered is below its
/// host, and one that leaves takes those below it out.
pub fn hosted(hosts: &mut Hosts, write: &Write) {
    match write {
        Write::Enter(subtree, Some(host)) => drop(hosts.insert(*subtree, *host)),
        Write::Enter(subtree, None) => drop(hosts.remove(subtree)),
        Write::Leave(left) => {
            for below in &left.below {
                hosts.remove(below);
            }
            if !left.cleared {
                hosts.remove(&left.subtree);
            }
        }
        Write::Move(moved) => {
            // Made together: every subtree is taken from where it hangs,
            // and then each hangs below the host it had, under the prefix
            // that one has afterwards.
            for gone in &moved.gone {
                hosts.remove(gone);
            }
            let taken: Vec<_> = (moved.pairs.iter())
                .map(|(old, new)| (*new, hosts.remove(old)))
                .collect();
            for (new, host) in taken {
                if let Some(host) = host {
                    hosts.insert(new, moved.target(host).unwrap_or(host));
                }
            }
        }
        Write::Put(..) | Write::Delete(..) | Write::Range(..) | Write::Stated(_) => {}
    }
}

/// Every subtree that is below `subtree`, at any depth.
pub fn below(hosts: &Hosts, subtree: Subtree) -> Vec<Subtree> {
    let mut below = vec![subtree];
    let mut at = 0;
    while at < below.len() {
        for (child, host) in hosts {
            if *host == below[at] && !below.contains(child) {
                below.push(*child);
            }
        }
        at += 1;
    }
    below.split_off(1)
}

/// What a drop or a clear takes.
#[derive(Debug, Clone)]
pub struct Left {
    pub subtree: Subtree,
    /// Whether the subtree is cleared, and not dropped.
    pub cleared: bool,
    /// Every subtree below it, which goes with it: the model works them
    /// out from the hosts of the subtrees that were entered, where the
    /// write is stated, and again where the transaction's writes are
    /// applied over a commit ([`TxnModel::lay`]).
    pub below: Vec<Subtree>,
    /// Those of them that the transaction had written when it stated the
    /// write, which the write covers in its write set and in the key set
    /// of its commit. They stay as they were stated.
    pub cut: Vec<Subtree>,
}

impl Left {
    /// Whether `unit` is one the write takes of its own subtree: every
    /// column of a drop, and the data of a clear.
    fn own(&self, unit: Unit) -> bool {
        match self.cleared {
            true => unit == Unit::data(self.subtree),
            false => unit.subtree == self.subtree,
        }
    }

    /// Whether the write deletes every key of `unit`.
    fn takes(&self, unit: Unit) -> bool {
        self.own(unit) || self.below.contains(&unit.subtree)
    }

    /// Whether the write meets every write of `unit` (CF5): it deletes
    /// every key of the unit, also of a subtree below that its transaction
    /// had not written and that the key set of its commit does not name.
    fn names(&self, unit: Unit) -> bool {
        self.takes(unit)
    }
}

impl Write {
    pub fn unit(&self) -> Unit {
        match self {
            Self::Put(unit, ..) | Self::Delete(unit, _) | Self::Range(unit, ..) => *unit,
            Self::Leave(left) => Unit::data(left.subtree),
            Self::Enter(subtree, _) | Self::Stated(subtree) => Unit::data(*subtree),
            Self::Move(moved) => Unit::data(moved.named[0]),
        }
    }

    /// Whether the write is one of `unit`.
    pub fn touches(&self, unit: Unit) -> bool {
        match self {
            Self::Leave(left) => left.takes(unit),
            Self::Enter(..) | Self::Stated(_) => false,
            Self::Move(moved) => moved.covers(unit.subtree),
            _ => self.unit() == unit,
        }
    }

    /// The subtrees whose place among the Merks the transaction that made
    /// this write keeps for it (the first rule of CF5): the one it
    /// writes under, drops or clears, and each one it moves.
    fn watched(&self) -> Vec<Subtree> {
        match self {
            Self::Put(unit, ..) | Self::Delete(unit, _) | Self::Range(unit, ..) => {
                vec![unit.subtree]
            }
            Self::Leave(left) => vec![left.subtree],
            Self::Move(moved) => moved.tops.clone(),
            Self::Enter(..) | Self::Stated(_) => Vec::new(),
        }
    }

    /// Whether the write is a range delete that contains no key.
    fn is_empty_range(&self) -> bool {
        matches!(self, Self::Range(_, start, Some(end)) if end <= start)
    }

    fn covers(&self, unit: Unit, key: &[u8]) -> bool {
        match self {
            Self::Put(own, at, _) | Self::Delete(own, at) => *own == unit && at == key,
            Self::Range(own, start, end) => {
                *own == unit && key >= &start[..] && end.as_ref().is_none_or(|end| key < &end[..])
            }
            Self::Leave(left) => left.takes(unit),
            Self::Enter(..) | Self::Stated(_) => false,
            Self::Move(moved) => moved.covers(unit.subtree),
        }
    }

    /// Whether this write, of a commit, meets `other`, a write of a
    /// transaction beside it: they write a key in common (CF2), or the
    /// commit takes a subtree whole that the other write is under (CF5,
    /// CF10).
    pub fn meets(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Enter(..) | Self::Stated(_), _) | (_, Self::Enter(..) | Self::Stated(_)) => {
                false
            }
            // A move writes every key under each prefix it covers: those
            // it names, and those of the subtrees that left with it when
            // it was stated.
            (Self::Move(moved), Self::Move(other)) => {
                moved.covered().any(|covered| other.covers(covered))
            }
            // A commit that moved subtrees, and a drop or a clear of the
            // transaction: what that takes below its subtree is the
            // model's to say.
            (Self::Move(moved), Self::Leave(left)) => {
                let mut covered = moved.covered();
                covered.any(|covered| left.names(Unit::data(covered)) || left.subtree == covered)
            }
            // A commit that dropped or cleared a subtree names it, and
            // those below it that its transaction had written under
            // (CF5): it meets a move that covers one of them. What it
            // took without naming it is `TxnModel::foreign`'s to say.
            (Self::Leave(left), Self::Move(moved)) => {
                moved.covers(left.subtree) || left.cut.iter().any(|cut| moved.covers(*cut))
            }
            // A commit that moved a subtree took it whole: it meets every
            // write under a prefix it covers, a range delete with no key
            // in it among them.
            (Self::Move(moved), other) => moved.covers(other.unit().subtree),
            // A write of a commit meets a move that covers its prefix
            // where it holds a key (CF2): a range delete with no key in
            // it meets no move by its keys.
            (other, Self::Move(moved)) => {
                moved.covers(other.unit().subtree) && !other.is_empty_range()
            }
            (Self::Leave(left), Self::Leave(other)) => {
                let units = |left: &Left| {
                    let own = left.subtree;
                    let named = std::iter::once(own).chain(left.below.iter().copied());
                    named
                        .flat_map(|subtree| Column::ALL.map(|column| Unit::new(column, subtree)))
                        .collect::<Vec<_>>()
                };
                (units(left).into_iter()).any(|unit| left.names(unit) && other.names(unit))
            }
            (Self::Leave(left), other) | (other, Self::Leave(left)) => left.names(other.unit()),
            (Self::Range(unit, start, end), Self::Range(other, from, to)) => {
                let below = |low: &Vec<u8>, high: &Option<Vec<u8>>| {
                    high.as_ref().is_none_or(|high| low < high)
                };
                unit == other
                    && below(start, end)
                    && below(from, to)
                    && below(start, to)
                    && below(from, end)
            }
            (range @ Self::Range(..), Self::Put(unit, key, _) | Self::Delete(unit, key))
            | (Self::Put(unit, key, _) | Self::Delete(unit, key), range) => {
                range.covers(*unit, key)
            }
        }
    }
}

/// The records of a database by record key.
pub type Records = BTreeMap<(Unit, Vec<u8>), Val>;

fn apply(records: &mut Records, write: &Write) {
    match write {
        Write::Put(unit, key, value) => {
            records.insert((*unit, key.clone()), value.clone());
        }
        Write::Delete(unit, key) => {
            records.remove(&(*unit, key.clone()));
        }
        Write::Range(..) | Write::Leave(_) => {
            records.retain(|(unit, key), _| !write.covers(*unit, key));
        }
        Write::Enter(..) | Write::Stated(_) => {}
        Write::Move(moved) => {
            // Made together: every record under a prefix that a subtree
            // is taken from goes, and then each is under the prefix its
            // subtree has afterwards, in the column it was in.
            let mut taken = Vec::new();
            records.retain(|(unit, key), value| {
                let takes = moved.takes(unit.subtree);
                if let Some(to) = moved.target(unit.subtree) {
                    taken.push(((Unit::new(unit.column, to), key.clone()), value.clone()));
                }
                !takes
            });
            records.extend(taken);
        }
    }
}

/// The model of one write transaction: its writes in order, each with its
/// first-write version and whether a later commit met it.
#[derive(Debug, Clone, Default)]
pub struct TxnModel {
    pub writes: Vec<(Write, u64, bool)>,
    /// Of the subtrees whose place among the Merks the transaction keeps
    /// ([`Write::watched`]), those that the committed state had below a
    /// Merk when the transaction last looked at it ([`Self::look`]).
    pub seen: BTreeSet<Subtree>,
}

impl TxnModel {
    /// What the transaction's newest write that covers the key says:
    /// `None` if it has none, `Some(None)` for a delete.
    pub fn own(&self, unit: Unit, key: &[u8]) -> Option<Option<&Val>> {
        self.traced(unit, key).0
    }

    /// What the transaction's newest write that covers the key says, and
    /// the unit the committed state has the key in if no write covers it:
    /// another one where the transaction moved the subtree.
    fn traced(&self, mut unit: Unit, key: &[u8]) -> (Option<Option<&Val>>, Unit) {
        for (write, ..) in self.writes.iter().rev() {
            if let Write::Move(moved) = write {
                // What is under a prefix after a move is what was under
                // the prefix its subtree was taken from, and nothing
                // where a subtree was taken and none brought.
                match moved.source(unit.subtree) {
                    Some(from) => unit = Unit::new(unit.column, from),
                    None if moved.takes(unit.subtree) => return (Some(None), unit),
                    None => {}
                }
                continue;
            }
            if write.covers(unit, key) {
                let own = match write {
                    Write::Put(_, _, value) => Some(value),
                    _ => None,
                };
                return (Some(own), unit);
            }
        }
        (None, unit)
    }

    /// The record the transaction reads under the key (RD3).
    pub fn read<'a>(&'a self, committed: &'a Records, unit: Unit, key: &[u8]) -> Option<&'a Val> {
        match self.traced(unit, key) {
            (Some(own), _) => own,
            (None, unit) => committed.get(&(unit, key.to_vec())),
        }
    }

    /// Whether the transaction moved a subtree.
    pub fn moves(&self) -> bool {
        (self.writes.iter()).any(|(write, ..)| matches!(write, Write::Move(_)))
    }

    /// The records of `unit` the transaction reads, in key order.
    pub fn view(&self, committed: &Records, unit: Unit) -> BTreeMap<Vec<u8>, Val> {
        // A move brings records from another unit: every record is asked.
        let all = self.moves();
        let mut records: Records = committed
            .iter()
            .filter(|((own, _), _)| all || *own == unit)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for (write, ..) in &self.writes {
            if all || write.touches(unit) {
                apply(&mut records, write);
            }
        }
        records
            .into_iter()
            .filter(|((own, _), _)| *own == unit)
            .map(|((_, key), value)| (key, value))
            .collect()
    }

    /// Which subtree is below which in the transaction's view: what is
    /// committed, with what its writes that are left entered and took out.
    pub fn hosts(&self, committed: &Hosts) -> Hosts {
        let mut hosts = committed.clone();
        for (write, ..) in &self.writes {
            hosted(&mut hosts, write);
        }
        hosts
    }

    /// Whether the conflict rule fails the transaction's commit (CF2).
    pub fn conflicts(&self) -> bool {
        self.writes.iter().any(|(.., met)| *met)
    }

    /// How many writes are in the overlap set.
    pub fn met(&self) -> usize {
        self.writes.iter().filter(|(.., met)| *met).count()
    }

    /// The transaction's newest write of the node under `key` keeps a
    /// value that a commit wrote after the writer read it, or may have:
    /// the write is met where it is made (CF2a).
    pub fn meet_kept(&mut self, unit: Unit, key: &[u8]) {
        let newest = (self.writes.iter_mut().rev())
            .find(|(write, ..)| write.covers(unit, key))
            .expect("a write of the key");
        assert!(matches!(newest.0, Write::Put(_, _, Val::Node(_))));
        newest.2 = true;
    }

    /// A commit of `version` outside the transaction wrote `writes`.
    pub fn foreign(&mut self, version: u64, writes: &[Write]) {
        for (own, first_write, met) in &mut self.writes {
            if *first_write < version && writes.iter().any(|write| write.meets(own)) {
                *met = true;
            }
        }
        // The first rule of CF5 for a move: a commit that took, with a
        // subtree above it, a subtree that the transaction wrote under,
        // dropped, cleared or moved meets every entry under that one, and
        // a move is under every prefix it covers. The rule goes by the
        // Merks that the transaction found the subtree below at its last
        // look: one that it did not see below a Merk is not held against
        // the commit.
        let took = |subtree: &&Subtree| {
            (writes.iter())
                .any(|write| matches!(write, Write::Leave(left) if left.below.contains(subtree)))
        };
        let taken: Vec<Subtree> = self.seen.iter().filter(took).copied().collect();
        for (own, first_write, met) in &mut self.writes {
            let Write::Move(moved) = own else {
                continue;
            };
            if *first_write < version && taken.iter().any(|taken| moved.covers(*taken)) {
                *met = true;
            }
        }
    }

    /// The transaction looked at the committed state, whose subtrees hang
    /// as `committed` has them: each of its operations does (WT7). Of the
    /// subtrees whose place it keeps, it saw those there that hang below
    /// a Merk. Whoever calls the transaction says so: the model is told
    /// of a commit when it is made, and the transaction learns of it at
    /// its next call.
    pub fn look(&mut self, committed: &Hosts) {
        let watched = self.writes.iter().flat_map(|(write, ..)| write.watched());
        self.seen = watched
            .filter(|subtree| committed.contains_key(subtree))
            .collect();
    }

    /// Works out again what each drop, clear and move of the transaction
    /// takes, with its writes applied in their order over the committed
    /// subtrees `committed`: what a layout of the transaction's writes
    /// over a commit does (WT7). A drop or a clear takes what hangs below
    /// its subtree there, and a move what [`Moved::lay`] says. What each
    /// write covers stays as it was stated.
    pub fn lay(&mut self, committed: &Hosts) {
        let mut hosts = committed.clone();
        for (write, ..) in &mut self.writes {
            match write {
                Write::Move(moved) => moved.lay(&hosts),
                Write::Leave(left) => left.below = below(&hosts, left.subtree),
                _ => {}
            }
            hosted(&mut hosts, write);
        }
    }

    /// Whether a move of the transaction names a subtree below one that
    /// leaves, over the committed subtrees it was last laid out over
    /// ([`Moved::stopped`]): the one refusal of a layout that the model
    /// tells.
    pub fn stopped(&self) -> bool {
        (self.writes.iter()).any(|(write, ..)| matches!(write, Write::Move(moved) if moved.stopped))
    }
}

/// The committed records of the model.
#[derive(Debug, Clone, Default)]
pub struct Committed {
    pub records: Records,
    pub version: u64,
    /// Which committed subtree is below which.
    pub hosts: Hosts,
}

impl Committed {
    /// Commits `writes` as the next version.
    pub fn commit(&mut self, writes: &[Write]) -> u64 {
        for write in writes {
            apply(&mut self.records, write);
            hosted(&mut self.hosts, write);
        }
        self.version += 1;
        self.version
    }

    /// The items of the Merk `subtree` as a reader of `records` has them.
    pub fn items(records: &BTreeMap<Vec<u8>, Val>) -> BTreeMap<Vec<u8>, Vec<u8>> {
        records
            .iter()
            .filter_map(|(key, value)| match value {
                Val::Node(node) => Some((key.clone(), node.value.clone())),
                Val::Flat(_) => None,
            })
            .collect()
    }
}

/// One operation, owned: what [`OwnedBatch::with`] makes a [`Batch`] of.
#[derive(Debug, Clone, Default)]
pub struct OwnedBatch {
    pub merks: Vec<OwnedMerk>,
    pub records: Vec<Write>,
    pub subtrees: Vec<OwnedSubtree>,
}

/// What a batch does to a subtree as a whole.
#[derive(Debug, Clone)]
pub enum OwnedSubtree {
    /// Enters the subtree, with the host's subtree, its key and the
    /// position.
    Enter(Subtree, Option<(Subtree, Vec<u8>, u8)>),
    /// Enters the subtree if it is absent. The model keeps no entry: for
    /// it the subtree hangs where the write says, as it does where the
    /// write found the subtree entered, and its records are those it had.
    EnterIfAbsent(Subtree, Option<(Subtree, Vec<u8>, u8)>),
    Drop(Subtree),
    Clear(Subtree),
    /// States the subtree anew: its tree type and, where one is given, its
    /// position among the subtrees of its host. The model keeps neither.
    Convert(Subtree, Vec<u8>, Option<u8>),
    /// Moves the subtree `from` to `to`, hosted by the node with the key
    /// `host.1` of the subtree `host.0` at the position `host.2`. `read`
    /// is the handle the mover read the node with that hosts it before,
    /// and `below` has the prefixes of the subtrees below, before and
    /// afterwards.
    Move {
        from: Subtree,
        to: Subtree,
        host: (Subtree, Vec<u8>, u8),
        read: Option<NodeHandle>,
        below: Vec<(Subtree, Subtree)>,
    },
}

#[derive(Debug, Clone)]
pub struct OwnedMerk {
    pub subtree: Subtree,
    /// The written nodes, each with whether its value is stated or kept.
    pub nodes: Vec<(Vec<u8>, NodeRec, bool)>,
    pub removed: Vec<Vec<u8>>,
    pub root: Option<Option<Vec<u8>>>,
    /// The Merks the nodes host, by key.
    pub hosted: BTreeMap<Vec<u8>, Vec<Subtree>>,
    /// The hashes the writer leaves stale, by the key of their node.
    pub stale: BTreeMap<Vec<u8>, Stale>,
    /// Whether the Merk has a host that the batch does not write.
    pub anchored: bool,
    /// The handles of the nodes that keep their values, by key: each names
    /// the read its value is kept from ([`OwnedBatch::reading`]). A node
    /// that the transaction wrote itself needs none.
    pub hints: BTreeMap<Vec<u8>, NodeHandle>,
}

impl OwnedMerk {
    /// The change set that takes the Merk `subtree` from the records `view`
    /// to the Merk of `items`: the nodes whose records differ, and the keys
    /// that are gone.
    pub fn change(
        subtree: Subtree,
        view: &BTreeMap<Vec<u8>, Val>,
        items: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Self {
        let (root, new) = records(items);
        let old_root = records(&Committed::items(view)).0;
        let nodes = new
            .into_iter()
            .filter(|(key, node)| view.get(key) != Some(&Val::Node(node.clone())))
            .map(|(key, node)| {
                let keeps =
                    matches!(view.get(&key), Some(Val::Node(old)) if old.value == node.value);
                (key, node, !keeps)
            })
            .collect();
        let removed = view
            .keys()
            .filter(|key| !items.contains_key(*key))
            .cloned()
            .collect();
        Self {
            subtree,
            nodes,
            removed,
            root: (root != old_root).then_some(root),
            hosted: BTreeMap::new(),
            stale: BTreeMap::new(),
            anchored: false,
            hints: BTreeMap::new(),
        }
    }

    /// The writes of the change set, for the model.
    pub fn writes(&self) -> Vec<Write> {
        let unit = Unit::data(self.subtree);
        let puts = (self.nodes.iter())
            .map(|(key, node, _)| Write::Put(unit, key.clone(), Val::Node(node.clone())));
        let deletes = (self.removed.iter()).map(|key| Write::Delete(unit, key.clone()));
        puts.chain(deletes).collect()
    }
}

impl OwnedBatch {
    /// Every write of the batch, in the order the write set takes them: a
    /// drop is one write and so is a clear, then a move is one, and a
    /// subtree that is entered is none. What is below a subtree that
    /// leaves or moves is the model's to say (`World::apply`).
    pub fn writes(&self) -> Vec<Write> {
        let leaving = self.subtrees.iter().filter_map(|subtree| {
            let (subtree, cleared) = match subtree {
                OwnedSubtree::Drop(subtree) => (*subtree, false),
                OwnedSubtree::Clear(subtree) => (*subtree, true),
                _ => return None,
            };
            Some(Write::Leave(Left {
                subtree,
                cleared,
                below: Vec::new(),
                cut: Vec::new(),
            }))
        });
        let entered = self.subtrees.iter().filter_map(|subtree| match subtree {
            OwnedSubtree::Enter(subtree, host) | OwnedSubtree::EnterIfAbsent(subtree, host) => {
                let host = host.as_ref().map(|(host, ..)| *host);
                Some(Write::Enter(*subtree, host))
            }
            _ => None,
        });
        let stated = self.subtrees.iter().filter_map(|subtree| match subtree {
            OwnedSubtree::Convert(subtree, ..) => Some(Write::Stated(*subtree)),
            _ => None,
        });
        // The moves of a batch are made together: one write.
        let mut all = Moved::default();
        for subtree in &self.subtrees {
            if let OwnedSubtree::Move {
                from, to, below, ..
            } = subtree
            {
                all.tops.push(*from);
                all.pairs.push((*from, *to));
                all.pairs.extend(below.iter().copied());
            }
        }
        let (sources, targets): (Vec<_>, Vec<_>) = all.pairs.iter().copied().unzip();
        all.named = sources.into_iter().chain(targets).collect();
        let moved = (!all.tops.is_empty()).then_some(Write::Move(all));
        let merks = self.merks.iter().flat_map(OwnedMerk::writes);
        (leaving
            .chain(moved)
            .chain(stated)
            .chain(entered)
            .chain(merks))
        .chain(self.records.iter().cloned())
        .collect()
    }

    /// The same batch, in which every node that keeps its value names the
    /// read the value is kept from: `read` is asked for the node's handle,
    /// which a writer has from its read of the node, through the
    /// transaction or the scope that the batch is then applied to. A write
    /// that keeps a committed value and names no read is refused
    /// (`BatchError::NoRead`).
    ///
    /// The model decides from its own records which values are kept, so
    /// its read is made here, in the place of the one a writer makes
    /// before it changes a node.
    pub fn reading(&self, mut read: impl FnMut(Subtree, &[u8]) -> Option<NodeHandle>) -> Self {
        let mut batch = self.clone();
        for merk in &mut batch.merks {
            let kept = merk.nodes.iter().filter(|(.., stated)| !stated);
            merk.hints = kept
                .filter_map(|(key, ..)| Some((key.clone(), read(merk.subtree, key)?)))
                .collect();
        }
        batch
    }

    /// [`Self::reading`], with each node read through `scope`, which the
    /// batch is then applied to. A read that is refused because another
    /// scope holds the unit names no handle: the write is refused for the
    /// same reason.
    pub fn through(&self, scope: &Scope<'_, '_>) -> Self {
        self.reading(|subtree, key| match scope.node(subtree, key, handle_of) {
            Ok(handle) => handle,
            Err(TxnError::ScopeOverlap { .. }) => None,
            Err(error) => panic!("the node {key:?} is read before it is written: {error}"),
        })
    }

    /// Calls `run` with the batch.
    pub fn with<R>(&self, run: impl FnOnce(&Batch<'_>) -> R) -> R {
        fn link(link: &Option<LinkRec>) -> Option<LinkPut<'_>> {
            link.as_ref().map(|link| LinkPut {
                key: &link.key,
                hash: &link.hash,
                heights: link.heights,
                aggregate: &link.aggregate,
            })
        }
        fn put<'a>(merk: &'a OwnedMerk, node: &'a (Vec<u8>, NodeRec, bool)) -> NodePut<'a> {
            let (key, node, stated) = node;
            // A hash that the writer leaves stale is stated as any bytes.
            let stale = merk.stale.get(key).copied().unwrap_or(Stale::NONE);
            let stated_hash = |field, hash| match stale.has(field) {
                true => &UNMADE,
                false => hash,
            };
            NodePut {
                key,
                feature: &node.feature,
                value_hash: stated_hash(HashField::Value, &node.value_hash),
                kv_hash: stated_hash(HashField::KeyValue, &node.kv_hash),
                left: link(&node.left),
                right: link(&node.right),
                hosted: match merk.hosted.get(key) {
                    Some(hosted) => Hosted::Set(hosted),
                    None => Hosted::Keep,
                },
                value: match stated {
                    false => ValuePut::Keep,
                    true => ValuePut::Value(&node.value),
                },
                stale,
                at: merk.hints.get(key).copied(),
            }
        }
        let nodes: Vec<Vec<NodePut<'_>>> = (self.merks.iter())
            .map(|merk| merk.nodes.iter().map(|node| put(merk, node)).collect())
            .collect();
        let removed: Vec<Vec<&[u8]>> = (self.merks.iter())
            .map(|merk| merk.removed.iter().map(|key| &key[..]).collect())
            .collect();
        let merks: Vec<MerkWrite<'_>> = (self.merks.iter().zip(&nodes).zip(&removed))
            .map(|((merk, nodes), removed)| MerkWrite {
                subtree: merk.subtree,
                nodes,
                removed,
                root: match &merk.root {
                    None => RootPut::Keep,
                    Some(root) => RootPut::Set(root.as_deref()),
                },
                anchored: merk.anchored,
            })
            .collect();
        let records: Vec<RecordWrite<'_>> = (self.records.iter())
            .map(|write| match write {
                Write::Put(unit, key, Val::Flat(value)) => RecordWrite::Put {
                    unit: *unit,
                    key,
                    value,
                },
                Write::Put(..) => panic!("a node is written in a change set"),
                Write::Leave(_) | Write::Enter(..) | Write::Stated(_) | Write::Move(_) => {
                    panic!("a subtree is written as a whole")
                }
                Write::Delete(unit, key) => RecordWrite::Delete { unit: *unit, key },
                Write::Range(unit, start, end) => RecordWrite::DeleteRange {
                    unit: *unit,
                    start,
                    end: end.as_deref(),
                },
            })
            .collect();
        let subtrees: Vec<SubtreeWrite<'_>> = (self.subtrees.iter())
            .map(|subtree| match subtree {
                OwnedSubtree::Enter(entered, host) | OwnedSubtree::EnterIfAbsent(entered, host) => {
                    let entering = Entering {
                        subtree: *entered,
                        tree_type: super::store::TREE_TYPE,
                        host: host.as_ref().map(|(subtree, key, position)| HostPut {
                            subtree: *subtree,
                            key,
                            position: *position,
                        }),
                        if_absent: matches!(subtree, OwnedSubtree::EnterIfAbsent(..)),
                    };
                    entering.write()
                }
                OwnedSubtree::Drop(subtree) => SubtreeWrite::Drop { subtree: *subtree },
                OwnedSubtree::Clear(subtree) => SubtreeWrite::Clear { subtree: *subtree },
                OwnedSubtree::Convert(subtree, tree_type, position) => SubtreeWrite::Convert {
                    subtree: *subtree,
                    tree_type,
                    position: *position,
                },
                OwnedSubtree::Move {
                    from,
                    to,
                    host: (subtree, key, position),
                    read,
                    below,
                } => SubtreeWrite::Move {
                    from: *from,
                    to: *to,
                    host: HostPut {
                        subtree: *subtree,
                        key,
                        position: *position,
                    },
                    read: *read,
                    below,
                },
            })
            .collect();
        run(&Batch {
            records: &records,
            merks: &merks,
            subtrees: &subtrees,
        })
    }
}

/// The handle of a node that is shown, to keep: what a writer has from its
/// read of a node.
pub fn handle_of(view: Option<NodeView<'_>>) -> Option<NodeHandle> {
    view.and_then(|view| view.handle())
}

/// A value of `len` bytes drawn from `rng`.
pub fn value(rng: &mut Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| rng.next() as u8).collect()
}

/// The flat unit of `column` under the subtree `name`.
pub fn unit(column: Column, name: u8) -> Unit {
    Unit::new(column, subtree(name))
}

/// The keys of every record of the model that a test may ask for.
pub fn keys_of(records: &Records, unit: Unit) -> BTreeSet<Vec<u8>> {
    records
        .keys()
        .filter(|(own, _)| *own == unit)
        .map(|(_, key)| key.clone())
        .collect()
}
