//! What a flat drop leaves behind on RocksDB until its drain, and the
//! marks that stand for it here (`engine.md`, "Flat drops and their
//! drain").
//!
//! A flat drop removes the element of a tree and stages a pending-drop
//! record that names the prefixes the tree owned. On RocksDB the records
//! under those prefixes stay until the drain deletes their ranges, and a
//! cursor of another subtree that steps from no key comes to them
//! (`flat-store.md`, rule 6). On LigninDB the subtree leaves with its
//! element ([`gone`](super::gone)). So where a group is applied:
//!
//! - **A group drops a subtree flat** where one of its batches puts a
//!   pending-drop record that names the prefix, the last of its batches
//!   to hold an outcome for the key of that record being one that puts
//!   it, and the group drops the subtree of the prefix or one that it
//!   hangs under.
//! - **For such a prefix, and each of the data, the aux and the meta
//!   keyspace, a mark is stated** where RocksDB holds a record there
//!   once the group is applied: the group puts one, or the transaction
//!   has one that the group does not remove. The batch that puts the
//!   pending-drop record is applied with the mark
//!   ([`LigninSettled::leave_behind`]), in the aux keyspace of the
//!   namespace of the pending-drop records.
//! - **The drain removes the marks of a prefix** with what it drops
//!   under it ([`marks_of`]).
//!
//! A mark says that there is a record, and not how many: a cursor counts
//! it as one. The roots keyspace has no marks.
//!
//! The look is made with the one that states the drops, in the
//! transaction before the group is applied, and is not judged again
//! where the group is laid out (`engine.md`, "A subtree leaves with its
//! element").

use std::collections::{BTreeMap, BTreeSet};

use grovedb_costs::cost_model::Keyspace;
use grovedb_merk::store::{
    lignin::{
        left_behind_key, LigninNodeStore, LigninOutcome, LigninSettled, LigninSubtree, LigninView,
    },
    NodeCursor, NodeStore,
};
use grovedb_storage::{
    flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord},
    Error, FlatStore,
};

use super::enter::entered;

/// The keyspaces that have marks.
const MARKED: [Keyspace; 3] = [Keyspace::Data, Keyspace::Aux, Keyspace::Meta];

/// The marks that a group is applied with: each with the batch that it
/// goes with, the keyspace and the prefix it is of. `dropped` are the
/// subtrees that are dropped with the group by name, and `view` is the
/// transaction before the group.
pub(super) fn left_behind(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    dropped: &BTreeSet<[u8; 32]>,
) -> Result<Vec<(usize, Keyspace, [u8; 32])>, Error> {
    let mut marks = Vec::new();
    if dropped.is_empty() {
        return Ok(marks);
    }
    let namespace = pending_prefix_drops_namespace();
    // The pending-drop records that the group puts: the last batch that
    // holds an outcome for the key of a record says whether it is put.
    let mut records: BTreeMap<&[u8], Option<(usize, &[u8])>> = BTreeMap::new();
    for (at, part) in parts.iter().enumerate() {
        for (key, outcome) in part.held(Keyspace::Meta, namespace) {
            let put = match outcome {
                LigninOutcome::Put(value) => value.map(|value| (at, value)),
                LigninOutcome::Removed => None,
            };
            records.insert(key, put);
        }
    }
    for (key, put) in records {
        let Some((at, value)) = put else {
            continue;
        };
        // A record that does not decode names no prefix: the drain is
        // what reports it, on both engines.
        let Ok(record) = PendingPrefixDropRecord::decode(key, value) else {
            continue;
        };
        for prefix in record.doomed_prefixes {
            if !leaves(view, dropped, prefix)? {
                continue;
            }
            for keyspace in MARKED {
                if stays(view, parts, keyspace, &prefix)? {
                    marks.push((at, keyspace, prefix));
                }
            }
        }
    }
    Ok(marks)
}

/// Whether the subtree `prefix` leaves with the group: it is one of
/// `dropped`, or hangs under one, by the hosts that the directory names.
fn leaves(
    view: LigninView<'_>,
    dropped: &BTreeSet<[u8; 32]>,
    prefix: [u8; 32],
) -> Result<bool, Error> {
    let (mut asked, mut at) = (BTreeSet::new(), prefix);
    // No subtree is asked twice, whatever the directory names.
    while asked.insert(at) {
        if dropped.contains(&at) {
            return Ok(true);
        }
        match entered(view, at)?.and_then(|entry| entry.host) {
            Some(host) => at = host.subtree,
            None => break,
        }
    }
    Ok(false)
}

/// Whether RocksDB holds a record of `keyspace` under `prefix` once the
/// group is applied: the group puts one, or the transaction shows one
/// that the group does not remove. In the data keyspace the nodes of a
/// Merk and the flat records are both records.
fn stays(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    keyspace: Keyspace,
    prefix: &[u8; 32],
) -> Result<bool, Error> {
    // What the group holds for each key: the last batch that holds an
    // outcome for it says whether it is put.
    let mut held: BTreeMap<&[u8], bool> = BTreeMap::new();
    for part in parts {
        for (key, outcome) in part.held(keyspace, prefix) {
            held.insert(key, matches!(outcome, LigninOutcome::Put(_)));
        }
    }
    if held.values().any(|put| *put) {
        return Ok(true);
    }
    // Each key that is passed is one that the group removes.
    let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(*prefix));
    macro_rules! first_that_stays {
        ($cursor:expr) => {{
            let mut cursor = $cursor;
            cursor.seek_to_first().unwrap();
            let mut stays = false;
            while let Some(key) = cursor.key().unwrap() {
                if !held.contains_key(key) {
                    stays = true;
                    break;
                }
                cursor.next().unwrap();
            }
            cursor.status()?;
            Ok(stays)
        }};
    }
    match keyspace {
        Keyspace::Data => first_that_stays!(NodeStore::cursor(&handle)),
        Keyspace::Aux => first_that_stays!(FlatStore::cursor(&handle.aux())),
        Keyspace::Meta => first_that_stays!(FlatStore::cursor(&handle.meta())),
        Keyspace::Roots => Ok(false),
    }
}

/// The keys of the marks that `view` holds for `prefix`, in the aux
/// keyspace of the namespace of the pending-drop records. None for the
/// prefix of the namespace itself: what is under it goes as one.
pub(super) fn marks_of(view: LigninView<'_>, prefix: &[u8; 32]) -> Result<Vec<Vec<u8>>, Error> {
    let namespace = pending_prefix_drops_namespace();
    let mut marks = Vec::new();
    if prefix == namespace {
        return Ok(marks);
    }
    let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(*namespace));
    let aux = handle.aux();
    for key in MARKED
        .iter()
        .filter_map(|keyspace| left_behind_key(*keyspace, prefix))
    {
        if aux.get(&key).unwrap()?.is_some() {
            marks.push(key);
        }
    }
    Ok(marks)
}
