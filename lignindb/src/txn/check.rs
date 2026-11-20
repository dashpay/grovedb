//! What is checked of a batch before anything is changed.

use std::collections::HashSet;

use lignin_format::value_log::MAX_VALUE_LEN;

use lignin_tree::hash::Stale;

use super::{
    error::{BatchError, WholeFault},
    key::{Column, Subtree, Unit},
    load::load_order,
    write::{Batch, Entering, Hosted, MerkWrite, RecordWrite, RootPut, SubtreeWrite, ValuePut},
};

/// The longest key of a Merk node: a cell has one byte for its length
/// (`tree-layout.md` C1).
const MAX_NODE_KEY: usize = u8::MAX as usize;

/// The most Merks a node hosts: a pointer is named by one byte.
const MAX_HOSTED: usize = u8::MAX as usize;

/// The longest tree type the subtree directory keeps.
const MAX_TREE_TYPE: usize = u8::MAX as usize;

/// Checks what can be checked of `batch` without reading a page: that it
/// writes each record key once, names each Merk once, sets no root without
/// a node and none that it deletes, writes no flat record among the nodes of
/// a Merk, holds no key or value that is too long, enters no subtree
/// twice, by either kind of enter, drops or clears none twice, and states
/// none anew twice or beside its enter, its drop or its clear. A change set
/// that says it is the whole Merk is one the bulk loader can lay out
/// ([`whole`]).
///
/// Of its moves: that no prefix is named twice as one a subtree is taken
/// from, and none twice as one a subtree is brought to; that the batch
/// puts the node each move states as the host, and names the new prefix
/// among the Merks that node hosts at the stated position or not at all
/// (which of the two is asked where the batch is applied, of the cell
/// that hosts the subtree before); and that nothing else is stated of a
/// prefix a move
/// names, but the drop of a subtree that is entered where one is moved to
/// and that no move takes away. A subtree that is stated anew under such
/// a prefix is refused as that ([`BatchError::ConvertMoved`]).
pub(crate) fn check(batch: &Batch<'_>) -> Result<(), BatchError> {
    let mut keys: HashSet<(Unit, &[u8])> = HashSet::new();
    let mut merks = HashSet::new();
    let (mut entered, mut left) = (HashSet::new(), HashSet::new());
    let mut stated = HashSet::new();
    let (mut sources, mut targets) = (HashSet::new(), HashSet::new());
    for moving in batch.moves() {
        let twice = |subtree| Err(BatchError::SubtreeTwice { subtree });
        if let Some(subtree) = moving.sources().find(|from| !sources.insert(*from)) {
            return twice(subtree);
        }
        if let Some(subtree) = moving.targets().find(|to| !targets.insert(*to)) {
            return twice(subtree);
        }
    }
    let named = |subtree: &Subtree| sources.contains(subtree) || targets.contains(subtree);
    let moved = |subtree: Subtree| Err(BatchError::Moved { subtree });
    for moving in batch.moves() {
        let host = moving.host;
        if host.key.len() > MAX_NODE_KEY {
            return Err(BatchError::KeyTooLong {
                subtree: host.subtree,
                len: host.key.len(),
            });
        }
        let merk = batch.merks.iter().find(|merk| merk.subtree == host.subtree);
        let node = merk.and_then(|merk| merk.nodes.iter().find(|node| node.key == host.key));
        let at = usize::from(host.position);
        let elsewhere = |hosted: &[Subtree]| {
            let mut named = hosted.iter().enumerate();
            named.any(|(position, hosted)| *hosted == moving.to && position != at)
        };
        if !node
            .is_some_and(|node| !matches!(node.hosted, Hosted::Set(hosted) if elsewhere(hosted)))
        {
            let subtree = moving.from;
            return Err(BatchError::MoveHost { subtree });
        }
        if named(&host.subtree) {
            return moved(host.subtree);
        }
    }
    for write in batch.subtrees {
        if write.moving().is_some() {
            continue;
        }
        let subtree = write.subtree();
        // A drop makes room where a subtree is moved to.
        let makes_room = matches!(write, SubtreeWrite::Drop { .. }) && !sources.contains(&subtree);
        if named(&subtree) && !makes_room {
            return match write {
                SubtreeWrite::Convert { .. } => Err(BatchError::ConvertMoved { subtree }),
                _ => moved(subtree),
            };
        }
        let enter = write.entering();
        let once = match (enter, write) {
            (Some(_), _) => entered.insert(subtree),
            (None, SubtreeWrite::Convert { .. }) => stated.insert(subtree),
            (None, _) => left.insert(subtree),
        };
        if !once {
            return Err(BatchError::SubtreeTwice { subtree });
        }
        if let SubtreeWrite::Convert { tree_type, .. } = write
            && tree_type.len() > MAX_TREE_TYPE
        {
            let len = tree_type.len();
            return Err(BatchError::TreeTypeTooLong { subtree, len });
        }
        if let Some(Entering {
            tree_type, host, ..
        }) = enter
        {
            if tree_type.len() > MAX_TREE_TYPE {
                let len = tree_type.len();
                return Err(BatchError::TreeTypeTooLong { subtree, len });
            }
            if let Some(host) = host.filter(|host| host.key.len() > MAX_NODE_KEY) {
                return Err(BatchError::KeyTooLong {
                    subtree: host.subtree,
                    len: host.key.len(),
                });
            }
        }
    }
    // A subtree that is stated anew stays as it is otherwise.
    if let Some(subtree) =
        (stated.iter()).find(|subtree| entered.contains(subtree) || left.contains(subtree))
    {
        return Err(BatchError::SubtreeTwice { subtree: *subtree });
    }
    let value_len = |len: usize| match len > MAX_VALUE_LEN {
        true => Err(BatchError::ValueTooLong { len }),
        false => Ok(()),
    };
    for merk in batch.merks {
        let subtree = merk.subtree;
        if !merks.insert(subtree) {
            return Err(BatchError::MerkTwice { subtree });
        }
        if named(&subtree) {
            return moved(subtree);
        }
        let unit = Unit::data(subtree);
        let key_len = |key: &[u8]| match key.len() > MAX_NODE_KEY {
            true => Err(BatchError::KeyTooLong {
                subtree,
                len: key.len(),
            }),
            false => Ok(()),
        };
        for node in merk.nodes {
            key_len(node.key)?;
            for link in [&node.left, &node.right].into_iter().flatten() {
                key_len(link.key)?;
            }
            if matches!(node.hosted, Hosted::Set(hosted) if hosted.len() > MAX_HOSTED) {
                return Err(BatchError::TooManyHosted { subtree });
            }
            if let ValuePut::Value(value) = node.value {
                value_len(value.len())?;
            }
            once(&mut keys, unit, node.key)?;
        }
        for key in merk.removed {
            key_len(key)?;
            once(&mut keys, unit, key)?;
        }
        if let RootPut::Set(root) = merk.root {
            if merk.nodes.is_empty() && merk.removed.is_empty() {
                return Err(BatchError::RootAlone { subtree });
            }
            if let Some(root) = root {
                key_len(root)?;
                // The root is a node the change set leaves in the Merk.
                if merk.removed.contains(&root) {
                    return Err(BatchError::NoRoot { subtree });
                }
            }
        }
    }
    if batch.merks.iter().any(|merk| merk.whole) {
        for merk in batch.merks.iter().filter(|merk| merk.whole) {
            whole(merk)?;
        }
        load_order(batch)?;
    }
    for record in batch.records {
        let unit = record.unit();
        if unit.column == Column::Data && merks.contains(&unit.subtree) {
            return Err(BatchError::NotFlat { unit });
        }
        if named(&unit.subtree) {
            return moved(unit.subtree);
        }
        match *record {
            RecordWrite::Put { unit, key, value } => {
                value_len(value.len())?;
                once(&mut keys, unit, key)?;
            }
            RecordWrite::Delete { unit, key } => once(&mut keys, unit, key)?,
            RecordWrite::DeleteRange { .. } => {}
        }
    }
    Ok(())
}

/// Checks that `merk`, which says it is the whole Merk, is one tree handed
/// over a child before its parent, with every value stated and every hash
/// made, and that its root is the root of that tree
/// ([`MerkWrite::whole`]). A change set with no node loads nothing and is
/// taken as any other.
fn whole(merk: &MerkWrite<'_>) -> Result<(), BatchError> {
    let subtree = merk.subtree;
    let refused = |fault| Err(BatchError::NotWhole { subtree, fault });
    if !merk.removed.is_empty() {
        return refused(WholeFault::Removes);
    }
    if merk.nodes.is_empty() {
        return Ok(());
    }
    let RootPut::Set(Some(root)) = merk.root else {
        return refused(WholeFault::NoRoot);
    };
    // The nodes that no node names as its child yet: a link takes its
    // child out, so a child stands before its parent and has one parent.
    let mut free: HashSet<&[u8]> = HashSet::with_capacity(merk.nodes.len());
    for node in merk.nodes {
        if node.value == ValuePut::Keep {
            return refused(WholeFault::KeepsValue);
        }
        if node.stale != Stale::NONE {
            return refused(WholeFault::StaleHash);
        }
        for link in [&node.left, &node.right].into_iter().flatten() {
            if !free.remove(link.key) {
                return refused(WholeFault::ChildLater);
            }
        }
        free.insert(node.key);
    }
    match (free.contains(root), free.len()) {
        (false, _) => refused(WholeFault::NoRoot),
        (true, 1) => Ok(()),
        (true, _) => refused(WholeFault::NotBelowRoot),
    }
}

/// Adds the record key `key` of `unit` to `keys`, which holds the keys the
/// batch has written before it.
fn once<'a>(
    keys: &mut HashSet<(Unit, &'a [u8])>,
    unit: Unit,
    key: &'a [u8],
) -> Result<(), BatchError> {
    match keys.insert((unit, key)) {
        true => Ok(()),
        false => Err(BatchError::KeyTwice { unit }),
    }
}
