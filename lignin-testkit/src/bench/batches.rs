//! Batches as large as a caller makes them.
//!
//! The reference workload ([`mod@super::run`]) applies batches of some
//! tens of records. A caller of GroveDB also makes batches of thousands of
//! operations. This module builds two such batches and times each on an
//! engine: the application of the batch in a write transaction, and the
//! commit of that transaction.
//!
//! - **moves**: [`Sizes::parents`] trees under one tree, [`Sizes::moves`]
//!   trees spread evenly under them, each with one item, and one batch
//!   that moves each of those trees to another key of its parent.
//! - **mixed**: a tree of items under each parent, and one batch of
//!   [`Sizes::mixed`] operations over those trees: a third of them
//!   replaces an item, a third deletes one and the rest puts a new one.
//!
//! Each time is of one batch: a result of one run, with no summary of
//! samples. The charges and the root hash are GroveDB's, and are the same
//! on every engine.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use grovedb::Element;
use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;

use super::{BenchError, Block, Engine};
use crate::workload::{BatchOp, Op};

/// How large the two batches are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    /// The trees that the batch of moves moves.
    pub moves: usize,
    /// The parents that those trees, and the trees of items, are under.
    pub parents: usize,
    /// The operations of the mixed batch.
    pub mixed: usize,
}

/// One large batch as an engine applied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LargeBatch {
    /// `moves` or `mixed`.
    pub name: &'static str,
    /// The operations of the batch.
    pub operations: usize,
    /// The application of the batch in a write transaction, with the
    /// making of GroveDB's operations out of the workload's, which is the
    /// same work on both engines.
    pub apply: Duration,
    /// The commit of that transaction.
    pub commit: Duration,
    /// What GroveDB charged for the batch.
    pub cost: OperationCost,
    /// The root hash after the commit.
    pub root_hash: [u8; 32],
}

/// The most operations of a batch that makes what the large batches work
/// on.
const SETUP_BATCH: usize = 2_000;

const TOP: &[u8] = b"top";
const ITEMS: &[u8] = b"items";
const ITEM: &[u8] = b"item";

fn key(kind: u8, number: usize) -> Vec<u8> {
    let mut key = vec![kind];
    key.extend_from_slice(&(number as u32).to_be_bytes());
    key
}

/// The path of the parent `number`.
fn parent(number: usize) -> Vec<Vec<u8>> {
    vec![TOP.to_vec(), key(b'p', number)]
}

/// The path of the tree of items under the parent `number`.
fn items(number: usize) -> Vec<Vec<u8>> {
    let mut path = parent(number);
    path.push(ITEMS.to_vec());
    path
}

fn put(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> BatchOp {
    BatchOp::InsertOrReplace { path, key, element }
}

/// An item of 36 bytes that says its number and the write it is of.
fn item(number: usize, write: u8) -> Element {
    let mut value = vec![write; 32];
    value.extend_from_slice(&(number as u32).to_be_bytes());
    Element::new_item(value)
}

/// The writes that make what the two batches work on, a tree before
/// anything under it.
fn made(sizes: Sizes) -> Vec<BatchOp> {
    let mut ops = vec![put(Vec::new(), TOP.to_vec(), Element::empty_tree())];
    for number in 0..sizes.parents {
        ops.push(put(
            vec![TOP.to_vec()],
            key(b'p', number),
            Element::empty_tree(),
        ));
        ops.push(put(parent(number), ITEMS.to_vec(), Element::empty_tree()));
    }
    for number in 0..sizes.moves {
        let under = parent(number % sizes.parents);
        let mut inside = under.clone();
        inside.push(key(b't', number));
        ops.push(put(under, key(b't', number), Element::empty_tree()));
        ops.push(put(inside, ITEM.to_vec(), item(number, 0)));
    }
    for number in 0..2 * (sizes.mixed / 3) {
        let under = items(number % sizes.parents);
        ops.push(put(under, key(b'i', number), item(number, 0)));
    }
    ops
}

/// The batch that moves every tree to another key of its parent.
fn moves(sizes: Sizes) -> Vec<BatchOp> {
    (0..sizes.moves)
        .map(|number| BatchOp::Move {
            path: parent(number % sizes.parents),
            key: key(b't', number),
            new_key: key(b'm', number),
        })
        .collect()
}

/// The mixed batch: a third of its operations replaces an item, a third
/// deletes one, and the rest puts a new one.
fn mixed(sizes: Sizes) -> Vec<BatchOp> {
    let third = sizes.mixed / 3;
    let replaced = (0..third).map(|number| {
        let under = items(number % sizes.parents);
        put(under, key(b'i', number), item(number, 1))
    });
    let deleted = (third..2 * third).map(|number| BatchOp::Delete {
        path: items(number % sizes.parents),
        key: key(b'i', number),
    });
    let new = (0..sizes.mixed - 2 * third).map(|number| {
        let under = items(number % sizes.parents);
        put(under, key(b'n', number), item(number, 2))
    });
    replaced.chain(deleted).chain(new).collect()
}

/// Applies `ops` as one batch in a write transaction of its own and
/// commits it. Returns the two times and what GroveDB charged.
fn one<E: Engine>(
    engine: &mut E,
    ops: Vec<BatchOp>,
    grove_version: &GroveVersion,
) -> Result<(Duration, Duration, OperationCost), BenchError> {
    let op = Op::Batch(ops);
    let mut block = engine.begin_block();
    let start = Instant::now();
    let cost = block.apply(&op, grove_version)?;
    let apply = start.elapsed();
    let start = Instant::now();
    block.commit()?;
    let commit = start.elapsed();
    engine.settle()?;
    Ok((apply, commit, cost))
}

/// Opens a database of `E` in `dir`, makes what the two batches work on,
/// and applies the batch of moves and then the mixed batch, each in a
/// write transaction of its own.
pub fn large_batches<E: Engine>(
    dir: &Path,
    sizes: Sizes,
    grove_version: &GroveVersion,
) -> Result<Vec<LargeBatch>, BenchError> {
    if sizes.parents == 0 {
        return Err(BenchError::Check(
            "the large batches need a parent".to_string(),
        ));
    }
    let mut engine = E::open(dir)?;
    engine.settle()?;
    for batch in made(sizes).chunks(SETUP_BATCH) {
        one(&mut engine, batch.to_vec(), grove_version)?;
    }
    let mut applied = Vec::new();
    for (name, ops) in [("moves", moves(sizes)), ("mixed", mixed(sizes))] {
        let operations = ops.len();
        let (apply, commit, cost) = one(&mut engine, ops, grove_version)?;
        applied.push(LargeBatch {
            name,
            operations,
            apply,
            commit,
            cost,
            root_hash: engine.root_hash(grove_version)?,
        });
    }
    Ok(applied)
}
