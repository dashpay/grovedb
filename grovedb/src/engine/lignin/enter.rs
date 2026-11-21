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
//!   and is read as the empty tree that it is.
//! - **Its host is the element its handle was opened under**: the key of
//!   the last segment of the path, in the subtree of the segments before
//!   ([`LigninSubtree::host`]). A subtree with no host is the root Merk of
//!   the grove, or a keyspace that no element names, opened by its prefix
//!   alone. A first write under a key that holds no element once the
//!   batch is applied is refused with `Error::NotSupported`: the
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
//! - **The Merks of an indexed tree are entered together**, by a group
//!   that holds a named write for one of them while none is entered, each
//!   at its position under the element: the primary as a tree of the
//!   element's type, a secondary as a tree of the type its axis keeps.
//!   Those that no batch of the group enters itself are entered with the
//!   batch of the group that enters the subtree the element is in, and
//!   with the first batch of the group where that subtree is entered
//!   already.
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
//!   where the subtree is not entered, and applied where it is: the
//!   delete of a tree removes the records of its subtree with its
//!   element. A handle that names no element, by prefix or beside the
//!   path of the root, is not held to an element.
//! - **What an element hosts is read from the group where it states the
//!   element**: from the last batch that states a value for the element's
//!   node or removes it, which gives the element or none. Otherwise it is
//!   read from the transaction, as a node of the Merk that holds the
//!   element.
//! - **The directory has all of them where the element names them, or
//!   none.** A subtree is not dropped when its element goes (GROVE-05),
//!   and a conversion does not move the subtrees of its element
//!   (MERK-09). So an indexed tree can find, under its key, subtrees that
//!   are entered already. A group that holds a named write for one of its
//!   Merks is refused with `Error::NotSupported` where a subtree the
//!   element hosts, and that no batch of the group enters itself, is
//!   entered at another position or under another element, and where one
//!   of those is entered and another is not.
//!
//! What this does not do:
//!
//! - A subtree is not dropped when its element goes (GROVE-05), and its
//!   entry keeps the tree type and the host it was entered with when its
//!   element is written again as a tree of another type.
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
//! than once is as the last of those reads found it: nothing is stated
//! for one that was entered then. A Merk of an indexed tree that was
//! entered then has to hang where its element names it, as at the read
//! that its element is worked out with, or the group is refused.
//!
//! So a transaction that lays the group out refuses a write into a
//! subtree that a wipe took since (`BatchError::NotEntered`), and it
//! refuses an enter that was stated for a subtree which another writer
//! entered otherwise since (`BatchError::EnteredOtherwise`). The engine
//! looks again, and applies the group with what that look states, where
//! all of these hold (`Engine::apply_write_group`):
//!
//! - **The transaction refused the group for the entry of a subtree, and
//!   found it otherwise than the look had** ([`Owed::found_otherwise`]):
//!   no entry, for a subtree that the look stated no enter for, and that
//!   it found entered or that holds the element of an enter it stated;
//!   or another entry than the one the look stated an enter for. A
//!   subtree that the look stated an enter for and that has no entry
//!   where it is needed waits for an enter of the group itself, as one
//!   that is named as the subtree of its own element does: no commit
//!   made that so.
//! - **No batch of the group enters, drops or clears a subtree
//!   itself**: the enters that the look states are the group's only
//!   writes of subtrees. Where a batch makes one itself, the group can
//!   be refused for an entry by its own doing, with nothing published
//!   beside it, and the engine does not tell that from a stale look:
//!   such a group is not looked for again. No operation of GroveDB
//!   makes such a group today.
//! - **A commit was published since before that look**: the latest
//!   version of the database is another than it was then.
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
fn element_of(value: &[u8]) -> Option<Element> {
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

fn named_twice() -> Error {
    not_supported(
        "writes into one subtree through handles that name two elements for it, or one element \
         with two subtrees of its own path",
    )
}

/// What the parts make of an element.
enum Host<'a> {
    /// The last part that states a value for it states this one.
    Stated(&'a [u8]),
    /// The last part that writes it removes it.
    Removed,
    /// No part writes it, or those that do keep the value it has: it is
    /// what the transaction holds.
    Stored,
}

fn host_of<'a>(parts: &'a [LigninSettled], host: &LigninHost) -> Host<'a> {
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
/// axis, in the element's order.
pub(super) fn hosting(element: &Element, own: [u8; 32]) -> Result<Hosting, Error> {
    let Some(tree_type) = element.tree_type() else {
        return Ok(Hosting::One(None));
    };
    if !tree_type.is_indexed_primary() {
        return Ok(Hosting::One(Some(tree_type)));
    }
    let axes = indexed_element_axes(element)
        .map_err(|error| failed(format!("an indexed tree: {error}")))?;
    let secondaries = axes.into_iter().map(|(axis, _)| {
        let prefix = secondary_prefix_for(&own, axis_storage_tag(axis)).value;
        (prefix, axis_secondary_tree_type(axis))
    });
    Ok(Hosting::Many(
        std::iter::once((own, tree_type))
            .chain(secondaries)
            .collect(),
    ))
}

fn entered(view: LigninView<'_>, prefix: [u8; 32]) -> Result<Option<LigninEntry>, Error> {
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
    /// The subtrees whose entry the look read and, at its last read of
    /// each, found entered: nothing is stated for them.
    pub(super) found: BTreeSet<[u8; 32]>,
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
pub(super) fn owed(view: LigninView<'_>, parts: &[LigninSettled]) -> Result<Owed, Error> {
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
    let mut owed = Owed::default();
    for subtree in written.into_values() {
        let prefix = subtree.prefix;
        if enters_itself(&prefix) {
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
                // The Merks of the element are entered together, so the
                // directory has all of them where the element names
                // them, or none.
                let (mut found, mut absent) = (0, Vec::new());
                for (position, (prefix, tree_type)) in hosted.into_iter().enumerate() {
                    if enters_itself(&prefix) {
                        continue;
                    }
                    let Some(entry) = entered(prefix)? else {
                        let subtree = LigninSubtree {
                            prefix,
                            host: Some(named.clone()),
                        };
                        absent.push((subtree, tree_type_bytes(tree_type)));
                        continue;
                    };
                    let hangs = entry.host.is_some_and(|at| {
                        (at.subtree, &at.key, usize::from(at.position))
                            == (named.subtree, &named.key, position)
                    });
                    if !hangs {
                        return Err(entered_elsewhere(host));
                    }
                    found += 1;
                }
                if found > 0 && !absent.is_empty() {
                    return Err(entered_elsewhere(host));
                }
                owed.enters.extend(absent);
                owed.hosts.push(named);
            }
            Hosting::One(_) | Hosting::Nothing if prefix != own => {
                return Err(beside_nothing(host));
            }
            Hosting::One(tree_type) => {
                if entered(prefix)?.is_none() {
                    let bytes = tree_type.map(tree_type_bytes).unwrap_or_default();
                    owed.enters.push((subtree.clone(), bytes));
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
    // element. Where the last read found it entered, it has to hang
    // where the element names it, as at the first.
    for named in &owed.hosts {
        for prefix in &named.hosted {
            if let Some(Some(entry)) = seen.get(prefix)
                && !hangs(entry, named, prefix)
            {
                return Err(entered_elsewhere(named));
            }
        }
    }
    // A subtree is as the last read of its entry found it: an enter that
    // an earlier read owed it is owed no more where it was entered then.
    owed.found = (seen.iter())
        .filter_map(|(prefix, entry)| entry.is_some().then_some(*prefix))
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
