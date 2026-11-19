//! What is checked of a batch before anything is changed.

use std::collections::HashSet;

use lignin_format::value_log::MAX_VALUE_LEN;

use super::{
    error::BatchError,
    key::{Column, Unit},
    write::{Batch, Hosted, RecordWrite, RootPut, ValuePut},
};

/// The longest key of a Merk node: a cell has one byte for its length
/// (`tree-layout.md` C1).
const MAX_NODE_KEY: usize = u8::MAX as usize;

/// The most Merks a node hosts: a pointer is named by one byte.
const MAX_HOSTED: usize = u8::MAX as usize;

/// Checks what can be checked of `batch` without reading a page: that it
/// writes each record key once, names each Merk once, sets no root without
/// a node and none that it deletes, writes no flat record among the nodes of
/// a Merk, and holds no key or value that is too long.
pub(crate) fn check(batch: &Batch<'_>) -> Result<(), BatchError> {
    let mut keys: HashSet<(Unit, &[u8])> = HashSet::new();
    let mut merks = HashSet::new();
    let value_len = |len: usize| match len > MAX_VALUE_LEN {
        true => Err(BatchError::ValueTooLong { len }),
        false => Ok(()),
    };
    for merk in batch.merks {
        let subtree = merk.subtree;
        if !merks.insert(subtree) {
            return Err(BatchError::MerkTwice { subtree });
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
    for record in batch.records {
        let unit = record.unit();
        if unit.column == Column::Data && merks.contains(&unit.subtree) {
            return Err(BatchError::NotFlat { unit });
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
