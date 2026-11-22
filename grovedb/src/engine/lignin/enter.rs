//! Which subtrees a batch enters in the subtree directory.
//!
//! LigninDB keeps an entry for every subtree in use, and reads and writes
//! no record of a subtree without one (`transactions.md` CV9). On RocksDB
//! a subtree is its prefix, and GroveDB names none when it makes a tree.
//! So the engine says it for GroveDB, where a batch is applied:
//!
//! - **A subtree is entered by the first batch that writes into it**, in
//!   any column: a node of its Merk, a flat record, an aux or a meta
//!   record. A tree that was inserted and never written into has no entry,
//!   and is read as the empty tree that it is. No enter is stated on
//!   account of the writes that a group leaves out
//!   ([`gone`](super::gone)): those into a subtree that is dropped with
//!   the group, and removals alone in a subtree that is not entered.
//! - **Its host is the element its handle was opened under**: the key of
//!   the last segment of the path, in the subtree of the segments before
//!   ([`LigninSubtree::host`]). A subtree with no host is the root Merk of
//!   the grove, or a keyspace that no element names, opened by its prefix
//!   alone. A first write under a key that holds no element once the
//!   batch is applied, which is not left out so, is refused with
//!   `Error::NotSupported`: the
//!   directory would name a host that is no node, or name none for a
//!   subtree that an element inserted later points at.
//! - **Its tree type is the one of that element**, read from the value
//!   that the last batch of the group to state one states for the
//!   element, or with none from the transaction. Where the element is no
//!   tree, or the subtree has no host, the directory keeps no tree type
//!   for the subtree, and `Merk::open_from_directory` refuses it as it
//!   refuses any other bytes.
//!
//! An element that hosts more than one Merk is an indexed tree: the Merk
//! of its own path, the primary, and after it one secondary Merk for each
//! of its axes, in the order the element has them. A handle knows one
//! subtree, so what such an element hosts is said here, for the group,
//! from the element as the group leaves it ([`Owed::hosts`]).
//!
//! A handle is taken at its word. Those that [`Engine`](crate::Engine)
//! opens name the element of their path, the element under which their
//! subtree is entered as its first, or none. A handle that is made with
//! a description of the caller's own (`LigninNodeStore::deferred`) is
//! the caller's statement: the contradictions named below are refused,
//! and the engine looks for no other.
//!
//! All of it is of a **named write**: a write that the group holds for a
//! subtree through a handle that names an element, where no batch of the
//! group enters that subtree itself. For a subtree that a batch of the
//! group enters itself nothing is worked out: its host, its position and
//! its tree type are what that batch resolves them to, and the element
//! is not read for it.
//!
//! - **The Merks of an indexed tree are entered under its element**, by
//!   a group that holds a named write for one of them, each that is not
//!   entered at its position under the element: the primary as a tree of
//!   the element's type, a secondary as a tree of the type its axis
//!   keeps. Those that no batch of the group enters itself are entered
//!   with the batch of the group that enters the subtree the element is
//!   in, and with the first batch of the group where that subtree is
//!   entered already.
//! - **A Merk that is entered under the element at another place, or as
//!   a tree of another type, than the element names is stated anew**
//!   ([`Owed::restated`]), with the first batch of the group that puts
//!   the node of the element: the conversion of an indexed tree to its
//!   128-bit counterpart can name its axes in another order, and a tree
//!   that was emptied is converted by a write of its element alone. A
//!   Merk that a batch of the group states anew itself is as that batch
//!   states it.
//! - **A secondary that hangs under the element, of an axis the element
//!   has no more, leaves with the group** ([`Owed::strays`]): it is
//!   dropped with the first batch that puts the node of the element, and
//!   what the group removes from it is not handed over.
//! - **An axis that the element names twice has one secondary**, which
//!   is hosted once, at the place of its first naming ([`hosting`]).
//! - **The element's node names all of them** in every batch that puts
//!   it, of a group that holds a named write for one of them.
//! - **A handle that names an element says whose subtree it writes**:
//!   a handle by path, of the subtree of its path, and a handle beside
//!   that subtree ([`Engine::subtree_beside`](crate::Engine::subtree_beside)),
//!   of the subtree with its prefix. A named write into a subtree other
//!   than the one of the element's own path is refused with
//!   `Error::NotSupported` where the element hosts no subtree with that
//!   prefix once the group is applied, and so is a group whose handles
//!   name two elements for one subtree, or name one element with two
//!   subtrees of its own path, in the handles of one subtree or of two.
//!   A named write into the subtree of the element's own path under a
//!   key that holds no element once the group is applied is refused
//!   where the subtree is not entered and its writes are not left out
//!   ([`gone`](super::gone)): where the group puts a node or a record
//!   into it, or a batch of the group enters, drops or clears a subtree
//!   itself, or states one anew. A handle that names no
//!   element, by prefix or beside the
//!   path of the root, is not held to an element.
//! - **In a group of more than one batch, the element of a tree whose
//!   data is one Merk names that Merk in every batch that puts it**
//!   ([`Owed::apart`]). One batch can write the nodes of the Merk and a
//!   later one the node of its element, as the two segments of a partial
//!   batch do. The first then leaves the root of the Merk for the
//!   element's node to name (`tree-layout.md` T3), and a node that kept
//!   what it hosts would point at the root the Merk had before, or at
//!   none. A group in which a batch writes nodes of the Merk after the
//!   last batch that puts the element's node is refused with
//!   `Error::NotSupported`: where that batch moves the root, no node
//!   would name it.
//! - **What an element hosts is read from the group where it states the
//!   element**: from the last batch that states a value for the element's
//!   node or removes it, which gives the element or none. Otherwise, in
//!   a group of one batch, the entry of the subtree stands for the
//!   element where the subtree is entered as the one Merk of a tree: a
//!   subtree leaves the directory with its element
//!   ([`gone`](super::gone)), and nothing is read. For a subtree that is
//!   not entered, for one that is entered as a Merk of an indexed tree,
//!   and in a group of several batches, the element is read from the
//!   transaction, as a node of the Merk that holds it.
//! - **A Merk of an indexed tree hangs under its element or the group
//!   is refused.** A group that holds a named write for one of the Merks
//!   of an indexed tree is refused with `Error::NotSupported` where a
//!   subtree the element hosts, and that no batch of the group enters
//!   itself, is entered under another element, or under none; and where
//!   one is entered under the element at another place than the element
//!   names while no batch of the group puts the node of the element.
//! - **A subtree that is entered under no element takes no named
//!   write** (D-277): a write through a handle by prefix entered it, and
//!   no statement gives an entered subtree a host. RocksDB takes the
//!   write.
//!
//! What this does not do:
//!
//! - What a group drops when it writes another element over a tree's,
//!   and enters again, is worked out beside this ([`super::over`]), and
//!   what it drops when it removes a node in [`super::gone`].
//!
//! A subtree is entered **if it is absent**
//! (`lignindb::txn::SubtreeWrite::EnterIfAbsent`): the look here says
//! which subtrees the transaction does not hold as entered, and what the
//! batch then states is judged by the transaction where the write is laid
//! out. So of two transactions that each write first into one subtree,
//! the second to commit finds the entry of the first, whenever the first
//! committed: before the second looked, between its look and its
//! application, or after both were applied. An entry with the tree type
//! and the host that the second states satisfies its enter, and any other
//! refuses its write (`BatchError::EnteredOtherwise`). The enter is no
//! key of the conflict rule: the second meets the first as a conflict
//! where it had written a key before the first committed that the first
//! wrote too, as on RocksDB, and commits where it had written none
//! (`engine.md`, "Subtrees are entered by the first batch that writes
//! into them").
//!
//! **The look can be stale when the group is applied.** The look is
//! several reads of the transaction and the application an operation
//! after them, and a commit of another transaction can be published
//! between any two of them. A subtree whose entry the look reads more
//! than once is as the last of those reads found it: no enter is stated
//! for one that was entered then. A Merk of an indexed tree is judged by
//! that read too: one that was not entered then is entered if it is
//! absent, also where another Merk of the tree was found entered; one
//! that was entered then under its element stays, and is stated anew
//! where the element names it otherwise; and one that was entered then
//! under another element, or under none, refuses the group.
//!
//! So a transaction that lays the group out refuses a write into a
//! subtree that a wipe took since (`BatchError::NotEntered`), and it
//! refuses an enter that was stated for a subtree which another writer
//! entered otherwise since (`BatchError::EnteredOtherwise`). The engine
//! looks again, and applies the group with what that look states, where
//! all of these hold (`Engine::apply_write_group`):
//!
//! - **The transaction refused the group for the entry of a subtree, and
//!   found it otherwise than the look had** ([`Owed::found_otherwise`],
//!   and for a subtree that the look found entered and stated the drop
//!   of, `Gone::found_otherwise`):
//!   no entry, for a subtree that the look stated no enter for, and that
//!   it found entered or that holds the element of an enter it stated;
//!   or another entry than the one the look stated an enter for. A
//!   subtree that the look stated an enter for and that has no entry
//!   where it is needed waits for an enter of the group itself, as one
//!   that is named as the subtree of its own element does: no commit
//!   made that so.
//! - **No batch of the group enters, drops or clears a subtree
//!   itself, or states one anew**: the enters that the look states, and
//!   the drops that it
//!   states for the subtrees under removed nodes ([`gone`](super::gone)),
//!   are the group's only writes of subtrees. Where a batch makes one itself, the group can
//!   be refused for an entry by its own doing, with nothing published
//!   beside it, and the engine does not tell that from a stale look:
//!   such a group is not looked for again. One operation of GroveDB
//!   makes such a group: the conversion of a tree to its 128-bit
//!   counterpart, in whose batch Merk states the converted Merk anew
//!   where the Merk has a node (`NodeStore::state_tree_type`).
//! - **A commit was published since before that look**: the latest
//!   version of the database is another than it was then.
//!
//! That version is the one of the engine the group is applied through.
//! A transaction that another engine began is refused there before
//! anything is looked for: nothing is read through it, and nothing is
//! applied to it.
//!
//! Every other refusal is the caller's answer as it is, and nothing is
//! looked for after it: a refusal of the look, as the one of a first
//! write under a key that holds no element; a refusal of the transaction
//! that names no entry, or the entry of a subtree the look decided
//! nothing of; and a refusal for an entry where no commit was published
//! since before the look. The seam has one thread at a time write
//! through a transaction, and what that thread's own calls do to the
//! transaction is no commit.
//!
//! An application that the transaction refused left no write of the
//! group in it; a write scope of the caller's keeps what the application
//! claimed in it. A transaction that serves its records by key lays
//! nothing out, so the checks that the frames make of entries are not
//! made where it takes a group: what it holds is judged at its commit.
//! What it checks with no layout can still refuse the group where it is
//! applied.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::{
        lignin::{
            tree_type_bytes, LigninEntry, LigninEntryRefused, LigninHost, LigninNodeStore,
            LigninOutcome, LigninSettled, LigninSubtree, LigninView,
        },
        NodeStore,
    },
    tree::kv::ValueDefinedCostType,
    TreeType,
};
use grovedb_storage::{prefix::secondary_prefix_for, Error};
use grovedb_version::version::GroveVersion;

use super::{
    error::{failed, merk_error, not_supported},
    ROOT,
};
use crate::{
    operations::indexed_tree::{axis_secondary_tree_type, axis_storage_tag, indexed_element_axes},
    Element,
};

/// The element that `value` is the bytes of, or `None` for bytes of no
/// element. The bytes of an element are the same on every grove version.
pub(super) fn element_of(value: &[u8]) -> Option<Element> {
    Element::deserialize(value, GroveVersion::latest()).ok()
}

/// The node `host` as the transaction has it: `None` if there is no such
/// node, and otherwise the element it holds, if its bytes are one's.
pub(super) fn element_read(
    view: LigninView<'_>,
    host: &LigninHost,
) -> Result<Option<Option<Element>>, Error> {
    let parent = LigninNodeStore::read_only(view, LigninSubtree::alone(host.subtree));
    let no_cost = None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
    let node = parent.get_node(&host.key, no_cost, GroveVersion::latest());
    let node = node.unwrap().map_err(merk_error)?;
    Ok(node.as_ref().map(|node| element_of(node.value_as_slice())))
}

fn key_of(host: &LigninHost) -> String {
    host.key.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn entered_elsewhere(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "a write into the indexed tree under the key {key}, whose subtrees are not all entered \
         where it names them: some are entered at other places, or not entered"
    ))
}

fn under_no_element(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "a write into a subtree of the element under the key {key}, which is entered under no \
         element: a write through a handle by prefix entered it"
    ))
}

fn hosted_no_more(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "a write into a subtree that is entered under the key {key}, whose element hosts it no \
         more"
    ))
}

fn beside_nothing(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "a write into a subtree beside the one of the key {key}, whose element hosts no such \
         subtree"
    ))
}

/// Whether two hosts are one element, named alike: the same key of the
/// same Merk, with the same subtree as the one of the element's own path,
/// which what the element hosts is worked out from.
fn same_element(one: &LigninHost, other: &LigninHost) -> bool {
    (one.subtree, &one.key, one.hosted.first()) == (other.subtree, &other.key, other.hosted.first())
}

fn unnamed_root(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "a group of batches that writes nodes of the tree under the key {key} in a batch after \
         the last one that writes the tree's element"
    ))
}

fn named_twice() -> Error {
    not_supported(
        "writes into one subtree through handles that name two elements for it, or one element \
         with two subtrees of its own path",
    )
}

/// What the parts make of an element.
pub(super) enum Host<'a> {
    /// The last part that states a value for it states this one.
    Stated(&'a [u8]),
    /// The last part that writes it removes it.
    Removed,
    /// No part writes it, or those that do keep the value it has: it is
    /// what the transaction holds.
    Stored,
}

pub(super) fn host_of<'a>(parts: &'a [LigninSettled], host: &LigninHost) -> Host<'a> {
    for part in parts.iter().rev() {
        match part.node(&host.subtree, &host.key) {
            Some(LigninOutcome::Put(Some(value))) => return Host::Stated(value),
            Some(LigninOutcome::Removed) => return Host::Removed,
            // A put that keeps the value keeps the one that an earlier
            // part states, and with none the transaction's.
            Some(LigninOutcome::Put(None)) | None => {}
        }
    }
    Host::Stored
}

/// What the group makes of the element that `host` names, and what that
/// element hosts.
pub(super) enum Hosting {
    /// No element: the group removes it, or there never was one.
    Nothing,
    /// An element that hosts the subtree of its own path and no other,
    /// with its tree type if it is a tree.
    One(Option<TreeType>),
    /// An indexed tree: its Merks by position, each with its tree type.
    Many(Vec<([u8; 32], TreeType)>),
}

/// The Merks that `element` hosts, where `own` is the subtree of its own
/// path: for an indexed tree the primary and then the secondary of each
/// axis, in the element's order. An axis that the element names twice,
/// as a batch on `GROVE_V4` can write one, has one secondary, which is
/// hosted once, at the place of its first naming (`node-store.md`,
/// "Hashes").
pub(super) fn hosting(element: &Element, own: [u8; 32]) -> Result<Hosting, Error> {
    let Some(tree_type) = element.tree_type() else {
        return Ok(Hosting::One(None));
    };
    if !tree_type.is_indexed_primary() {
        return Ok(Hosting::One(Some(tree_type)));
    }
    let axes = indexed_element_axes(element)
        .map_err(|error| failed(format!("an indexed tree: {error}")))?;
    let mut merks = vec![(own, tree_type)];
    for (axis, _) in axes {
        let prefix = secondary_prefix_for(&own, axis_storage_tag(axis)).value;
        if merks.iter().all(|(known, _)| *known != prefix) {
            merks.push((prefix, axis_secondary_tree_type(axis)));
        }
    }
    Ok(Hosting::Many(merks))
}

/// The subtrees that the element with the bytes `value` hosts, by
/// position, where `own` is the subtree of its own path: none for what is
/// no tree.
pub(super) fn hosted_by(value: &[u8], own: [u8; 32]) -> Result<Vec<[u8; 32]>, Error> {
    let Some(element) = element_of(value) else {
        return Ok(Vec::new());
    };
    Ok(match hosting(&element, own)? {
        Hosting::Nothing | Hosting::One(None) => Vec::new(),
        Hosting::One(Some(_)) => vec![own],
        Hosting::Many(merks) => merks.into_iter().map(|(prefix, _)| prefix).collect(),
    })
}

pub(super) fn entered(
    view: LigninView<'_>,
    prefix: [u8; 32],
) -> Result<Option<LigninEntry>, Error> {
    let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(prefix));
    handle.entry().map_err(merk_error)
}

/// What the group makes of the element `host`: the element as the last
/// part to state its value states it, and as the transaction has it where
/// no part does. What the directory holds of its subtrees says nothing of
/// the element, which another may have taken the place of.
fn hosting_of(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    host: &LigninHost,
    own: [u8; 32],
) -> Result<Hosting, Error> {
    match host_of(parts, host) {
        Host::Stated(value) => match element_of(value) {
            Some(element) => hosting(&element, own),
            None => Ok(Hosting::One(None)),
        },
        Host::Removed => Ok(Hosting::Nothing),
        Host::Stored => match element_read(view, host)? {
            Some(Some(element)) => hosting(&element, own),
            Some(None) => Ok(Hosting::One(None)),
            None => Ok(Hosting::Nothing),
        },
    }
}

/// What a group owes the subtree directory when it is applied.
#[derive(Debug, Default)]
pub(super) struct Owed {
    /// The subtrees that are entered with the group: those a part writes
    /// into that neither a part nor the transaction has entered, and with
    /// one Merk of an indexed tree the others, each with the bytes of its
    /// tree type, and a subtree after the one its host is in.
    pub(super) enters: Vec<(LigninSubtree, Vec<u8>)>,
    /// Each indexed tree that the group writes a Merk of, with every Merk
    /// it hosts, by position.
    pub(super) hosts: Vec<LigninHost>,
    /// In a group of more than one batch, each tree whose data is one
    /// Merk that the group has a named write for, with that Merk. One batch can
    /// write the nodes of the Merk and a later one the node of its
    /// element, as the two segments of a partial batch do: the first
    /// leaves the root of the Merk for the element's node to name
    /// (`tree-layout.md` T3), and the batch that writes that node has to
    /// name it.
    pub(super) apart: Vec<LigninHost>,
    /// The subtrees whose entry the look read and, at its last read of
    /// each, found entered: no enter is stated for them.
    pub(super) found: BTreeSet<[u8; 32]>,
    /// The Merks of indexed trees that are stated anew with the group,
    /// each with the batch that states it, which is the first to put the
    /// node of its element, and the bytes of its tree type: those that
    /// are entered under their element at another place than the element
    /// names them at once the group is applied, or as a tree of another
    /// type.
    pub(super) restated: Vec<(usize, LigninSubtree, Vec<u8>)>,
    /// The secondaries that are entered under an indexed tree which the
    /// group writes, and that its element hosts no more, each with the
    /// batch that drops it: the first to put the node of the element.
    pub(super) strays: Vec<(usize, [u8; 32])>,
}

impl Owed {
    /// Whether `refused` is of a subtree whose entry this look found
    /// otherwise: no entry, for a subtree that no enter is stated for
    /// here, and that the look found entered or that holds the element
    /// of an enter stated here; or another entry than the one stated
    /// here for the subtree.
    pub(super) fn found_otherwise(&self, refused: LigninEntryRefused) -> bool {
        let stated = |subtree: [u8; 32]| {
            let mut enters = self.enters.iter();
            enters.any(|(entered, _)| entered.prefix == subtree)
        };
        match refused {
            // An enter that is stated here is made before anything that
            // needs its subtree. One that has no entry all the same
            // waits for an enter of the group itself.
            LigninEntryRefused::NotEntered(subtree) => {
                let hosts = |(entered, _): &(LigninSubtree, Vec<u8>)| {
                    (entered.host.as_ref()).is_some_and(|host| host.subtree == subtree)
                };
                !stated(subtree) && (self.found.contains(&subtree) || self.enters.iter().any(hosts))
            }
            LigninEntryRefused::EnteredOtherwise(subtree) => stated(subtree),
        }
    }
}

/// Whether `entry` is of a subtree that hangs under the element `host`,
/// at the place that `prefix` has among the subtrees `host` hosts.
fn hangs(entry: &LigninEntry, host: &LigninHost, prefix: &[u8; 32]) -> bool {
    let place = host.hosted.iter().position(|hosted| hosted == prefix);
    entry.host.as_ref().is_some_and(|at| {
        (at.subtree, &at.key, Some(usize::from(at.position))) == (host.subtree, &host.key, place)
    })
}

/// What `parts` owe the subtree directory.
///
/// `left` are the subtrees whose writes the group leaves out
/// ([`gone`](super::gone)): nothing is owed on account of a write into one
/// of them, which is not applied.
pub(super) fn owed(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    left: &BTreeSet<[u8; 32]>,
) -> Result<Owed, Error> {
    // Each subtree once: of the parts that write into it, as the first
    // names it whose handle named a host.
    // A subtree hangs on one element: handles that name two for it are
    // refused, in one part or in two.
    if parts.iter().any(LigninSettled::names_two_hosts) {
        return Err(named_twice());
    }
    let mut written: BTreeMap<_, &LigninSubtree> = BTreeMap::new();
    // The subtree of its own path that each named element was named
    // with: every handle of the group that names the element names it
    // with the same, whichever subtree the handle is of.
    let mut own: BTreeMap<(_, &[u8]), _> = BTreeMap::new();
    for subtree in parts.iter().flat_map(LigninSettled::written) {
        if let Some(host) = &subtree.host {
            let named = own.entry((host.subtree, host.key.as_slice()));
            if *named.or_insert(host.hosted.first()) != host.hosted.first() {
                return Err(named_twice());
            }
        }
        match written.get(&subtree.prefix) {
            Some(LigninSubtree {
                host: Some(known), ..
            }) => {
                if subtree
                    .host
                    .as_ref()
                    .is_some_and(|host| !same_element(known, host))
                {
                    return Err(named_twice());
                }
            }
            Some(_) if subtree.host.is_none() => {}
            _ => {
                written.insert(subtree.prefix, subtree);
            }
        }
    }
    let enters_itself = |prefix: &[u8; 32]| parts.iter().any(|part| part.enters(prefix));
    // The entry of each subtree as the last read of it found it: a
    // commit can be published between two reads of one look.
    let mut seen: BTreeMap<[u8; 32], Option<LigninEntry>> = BTreeMap::new();
    let mut entered = |prefix: [u8; 32]| {
        let entry = entered(view, prefix)?;
        seen.insert(prefix, entry.clone());
        #[cfg(test)]
        super::an_entry_was_read();
        Ok::<_, Error>(entry)
    };
    // A subtree is entered under an element that is there once the group
    // is applied: the directory names no host that is no node.
    let no_element = || not_supported("a write into a subtree under a key that holds no element");
    let states_itself = |prefix: &[u8; 32]| parts.iter().any(|part| part.states_anew(prefix));
    let mut owed = Owed::default();
    // The tree type of each Merk of an indexed tree of `owed.hosts`, in
    // the order of those.
    let mut kinds: Vec<Vec<TreeType>> = Vec::new();
    for subtree in written.into_values() {
        let prefix = subtree.prefix;
        if enters_itself(&prefix) || left.contains(&prefix) {
            continue;
        }
        let Some(host) = &subtree.host else {
            if entered(prefix)?.is_none() {
                let tree_type = (prefix == ROOT).then_some(TreeType::NormalTree);
                let bytes = tree_type.map(tree_type_bytes).unwrap_or_default();
                owed.enters.push((subtree.clone(), bytes));
            }
            continue;
        };
        // The subtree of the element's own path: the one a handle by path
        // is of, and the one a handle beside it names.
        let own = host.hosted.first().copied().unwrap_or(prefix);
        // A subtree of an element that was worked out for another of its
        // subtrees: its handle has to name that element.
        if let Some(known) = (owed.hosts.iter()).find(|known| known.hosted.contains(&prefix)) {
            match same_element(known, host) {
                true => continue,
                false => return Err(beside_nothing(host)),
            }
        }
        // A subtree leaves the directory with its element ([`gone`]), so
        // the entry of the one Merk of a tree stands for that element
        // where the one batch of the group does not state it: nothing is
        // owed, and the element is not read. A group of several batches
        // takes the way below, which looks at the element for all of
        // them.
        if parts.len() == 1 && prefix == own && matches!(host_of(parts, host), Host::Stored) {
            let one_merk = |entry: LigninEntry| {
                let indexed = (entry.tree_type()).is_ok_and(|tree| tree.is_indexed_primary());
                !indexed && entry.host.is_some_and(|at| at.position == 0)
            };
            if entered(prefix)?.is_some_and(one_merk) {
                continue;
            }
        }
        match hosting_of(view, parts, host, own)? {
            Hosting::Many(hosted) => {
                let named = LigninHost {
                    subtree: host.subtree,
                    key: host.key.clone(),
                    hosted: hosted.iter().map(|(prefix, _)| *prefix).collect(),
                };
                if !named.hosted.contains(&prefix) {
                    return Err(beside_nothing(host));
                }
                // The entry of each Merk of the element is read, and
                // judged below, as the last read of it found it.
                for (prefix, _) in &hosted {
                    if !enters_itself(prefix) {
                        entered(*prefix)?;
                    }
                }
                kinds.push(hosted.into_iter().map(|(_, kind)| kind).collect());
                owed.hosts.push(named);
            }
            Hosting::One(_) | Hosting::Nothing if prefix != own => {
                return Err(beside_nothing(host));
            }
            Hosting::One(tree_type) => {
                match entered(prefix)? {
                    None => {
                        let bytes = tree_type.map(tree_type_bytes).unwrap_or_default();
                        owed.enters.push((subtree.clone(), bytes));
                    }
                    // No statement gives a subtree that is entered a
                    // host (D-277).
                    Some(LigninEntry { host: None, .. }) => return Err(under_no_element(host)),
                    Some(_) => {}
                }
                // A tree that keeps its data flat has no Merk to name.
                let merk = tree_type.is_some_and(|tree| !tree.uses_non_merk_data_storage());
                if merk && parts.len() > 1 {
                    // The last batch that writes nodes of the Merk leaves
                    // its root for the element's node to name, in that
                    // batch or in a later one.
                    let puts = |part: &LigninSettled| {
                        let node = part.node(&host.subtree, &host.key);
                        matches!(node, Some(LigninOutcome::Put(_)))
                    };
                    let named = parts.iter().rposition(puts);
                    let written = parts.iter().rposition(|part| part.writes_merk(&own));
                    if written.is_some_and(|at| named.is_none_or(|named| named < at)) {
                        return Err(unnamed_root(host));
                    }
                    owed.apart.push(host.clone());
                }
            }
            Hosting::Nothing => {
                if entered(prefix)?.is_none() {
                    return Err(no_element());
                }
            }
        }
    }
    // A Merk of an indexed tree is read where its element is worked out,
    // and can be read again afterwards, through a handle that names no
    // element: each is as the last read of its entry found it. One that
    // is not entered is entered with the group. One that is entered
    // under its element stays, and is stated anew where the element
    // names it at another place or as a tree of another type. One that
    // is entered under another element, or under none, refuses the group.
    for (named, kinds) in owed.hosts.iter().zip(&kinds) {
        // A statement anew goes with the first batch that puts the node
        // of the element: the cell of that node names the Merks of the
        // element by their places from then on.
        let puts = |part: &LigninSettled| {
            let node = part.node(&named.subtree, &named.key);
            matches!(node, Some(LigninOutcome::Put(_)))
        };
        let put_by = parts.iter().position(puts);
        for (prefix, kind) in named.hosted.iter().zip(kinds) {
            if enters_itself(prefix) {
                continue;
            }
            let subtree = LigninSubtree {
                prefix: *prefix,
                host: Some(named.clone()),
            };
            let bytes = tree_type_bytes(*kind);
            let Some(Some(entry)) = seen.get(prefix) else {
                owed.enters.push((subtree, bytes));
                continue;
            };
            let Some(at) = &entry.host else {
                return Err(under_no_element(named));
            };
            if (at.subtree, &at.key) != (named.subtree, &named.key) {
                return Err(entered_elsewhere(named));
            }
            // A Merk that a batch of the group states anew itself is as
            // that batch states it.
            let placed = hangs(entry, named, prefix);
            if states_itself(prefix) || (placed && entry.tree_type == bytes) {
                continue;
            }
            match put_by {
                Some(part) => owed.restated.push((part, subtree, bytes)),
                // No batch writes the element: its cell names its Merks
                // as it did, and the tree type of a Merk alone is left
                // as it is entered.
                None if placed => {}
                None => return Err(entered_elsewhere(named)),
            }
        }
        // A secondary that hangs under the element, of an axis that the
        // element has no more, as after an indexed tree of other axes
        // was written over it, leaves with the batch that writes the
        // element: its place is one that a Merk of the element can take,
        // and the cell of the element names it no more. A secondary is
        // stored under the tag of its axis, and the big sum axis under
        // that of the sum axis: these are all.
        let (Some(part), Some(own)) = (put_by, named.hosted.first()) else {
            continue;
        };
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            let stray = secondary_prefix_for(own, axis.tag()).value;
            if named.hosted.contains(&stray) || enters_itself(&stray) {
                continue;
            }
            let hangs = self::entered(view, stray)?
                .and_then(|entry| entry.host)
                .is_some_and(|at| (at.subtree, &at.key) == (named.subtree, &named.key));
            if !hangs {
                continue;
            }
            // What the group removes from it leaves with it. A put into
            // it has nowhere to go.
            if parts
                .iter()
                .flat_map(LigninSettled::put_into)
                .any(|into| into == stray)
            {
                return Err(hosted_no_more(named));
            }
            owed.strays.push((part, stray));
        }
    }
    // A subtree is as the last read of its entry found it: an enter that
    // an earlier read owed it is owed no more where it was entered then.
    owed.found = (seen.iter())
        .filter_map(|(prefix, entry)| entry.is_some().then_some(*prefix))
        .chain(owed.strays.iter().map(|(_, stray)| *stray))
        .collect();
    owed.enters
        .retain(|(subtree, _)| !owed.found.contains(&subtree.prefix));
    // A subtree is entered once: as its element names it, where a write
    // through a handle that names no element owed it an enter too.
    let mut once: Vec<(LigninSubtree, Vec<u8>)> = Vec::with_capacity(owed.enters.len());
    for enter in std::mem::take(&mut owed.enters) {
        match (once.iter_mut()).find(|(known, _)| known.prefix == enter.0.prefix) {
            Some(known) if known.0.host.is_none() => *known = enter,
            Some(_) => {}
            None => once.push(enter),
        }
    }
    owed.enters = once;
    // A subtree whose host is in a subtree that is entered here too comes
    // after it: by how many of its hosts' subtrees are entered here.
    let depth = |subtree: &LigninSubtree| {
        let host = |subtree: &&LigninSubtree| {
            let host = subtree.host.as_ref()?.subtree;
            (owed.enters.iter())
                .map(|(owed, _)| owed)
                .find(|owed| owed.prefix == host)
        };
        let hosts = std::iter::successors(Some(subtree), host);
        hosts.take(owed.enters.len()).count()
    };
    let depths: Vec<usize> = (owed.enters.iter())
        .map(|(subtree, _)| depth(subtree))
        .collect();
    let mut ordered: Vec<_> = depths.into_iter().zip(owed.enters).collect();
    ordered.sort_by_key(|(depth, _)| *depth);
    owed.enters = ordered.into_iter().map(|(_, enter)| enter).collect();
    Ok(owed)
}
