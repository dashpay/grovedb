//! Batches of many moves (GROVE-03): the same calls on LigninDB and on
//! RocksDB, with every answer, every `OperationCost` and the root hash
//! after every write compared, on the grove versions that take a move,
//! and the work of the engine's looks held to the size of what is moved.
//!
//! - `batch_of`: trees under twelve parents, each with an item, and one
//!   batch that moves every one of them to another key of its parent.
//! - `transaction_of`: fourteen batches of moves in one write
//!   transaction, from seven moves to ten thousand, the trees of each
//!   batch under parents of their own, from two to two thousand of them,
//!   and the trees of the two smallest batches with two hundred items
//!   each; then the commit.
//!
//! The suite runs the batch with two thousand trees and the transaction
//! at a quarter of its sizes, on the first grove version that takes a
//! move. The batch of ten thousand and the whole transaction, 21,244
//! moves, on each version that takes one, are tests that are run when
//! they are asked for, some minutes each in a debug build:
//!
//! ```text
//! GROVEDB_TEST_ENGINE=lignin cargo test -p grovedb --all-features --lib -- many_moves --include-ignored
//! ```
//!
//! The looks that say what a group of batches does to the subtree
//! directory count their steps in a test build
//! (`engine::lignin::look_step`): a pair, a node or a named element that
//! a look comes to, by an index or on a walk. A look that comes to each
//! a bounded number of times makes a few steps for each move, whatever
//! the size of the batch. One that looks through the batch for each move
//! makes as many as the batch is large, squared: for two thousand moves
//! more than a hundred times the bound that these tests hold the steps
//! to, and for ten thousand a thousand times.

use std::path::Path;

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    batches::{Calls, Op},
    nested::{assert_same, check, on_both_engines, open, Log},
    LigninEngine,
};
use crate::{
    engine::{lignin::look_steps_taken, Engine},
    Element,
};

/// The most steps that the looks make for one moved tree with nothing
/// below it: the pair is read, entered by its prefix and asked for the
/// pair above it; the move finds its node among those the batch states,
/// which are read once each, a few for each move; and it asks for what it
/// takes along, for its element and for the prefix it goes to, twice.
const STEPS_FOR_A_MOVE: u64 = 12;

const TOP: &[u8] = b"top";

fn key(kind: u8, number: usize) -> Vec<u8> {
    let mut key = vec![kind];
    key.extend_from_slice(&(number as u32).to_be_bytes());
    key
}

/// The path of the parent `number`.
fn parent(number: usize) -> Vec<Vec<u8>> {
    vec![TOP.to_vec(), key(b'p', number)]
}

fn tree(op: fn(Vec<Vec<u8>>, Vec<u8>, Element) -> Op, at: Vec<Vec<u8>>, key: Vec<u8>) -> Op {
    op(at, key, Element::empty_tree())
}

/// The trees of a history: `count` trees under the parents from `first`
/// on, `parents` of them, each tree with `items` items. The batches that
/// make them hold two thousand operations at the most.
fn made<E: Engine>(
    calls: &mut Calls<'_, '_, '_, E>,
    first: usize,
    parents: usize,
    count: usize,
    items: usize,
) {
    let insert = Op::insert_or_replace_op;
    let mut ops: Vec<Op> = (first..first + parents)
        .map(|number| tree(insert, vec![TOP.to_vec()], key(b'p', number)))
        .collect();
    for number in 0..count {
        let at = parent(first + number % parents);
        ops.push(tree(insert, at.clone(), key(b'c', number)));
        let inside = [at, vec![key(b'c', number)]].concat();
        for item in 0..items {
            let element = Element::new_item(vec![item as u8; 32]);
            ops.push(insert(inside.clone(), key(b'i', item), element));
        }
    }
    for batch in ops.chunks(2_000) {
        calls.batch("made", batch.to_vec());
    }
}

/// The batch that moves each of the `count` trees under the parents from
/// `first` on to another key of its parent.
fn moves(first: usize, parents: usize, count: usize) -> Vec<Op> {
    let moved = |number: usize| {
        let at = parent(first + number % parents);
        Op::move_op(at, key(b'c', number), key(b'm', number))
    };
    (0..count).map(moved).collect()
}

/// What is read of the tree `number` under the parents from `first` on
/// once it is moved: its item under the new key, and nothing under the
/// old one.
fn read_moved<E: Engine>(
    calls: &mut Calls<'_, '_, '_, E>,
    first: usize,
    parents: usize,
    number: usize,
) {
    let at = parent(first + number % parents);
    let (old, new) = (key(b'c', number), key(b'm', number));
    let item = key(b'i', 0);
    calls.get(&[TOP, &at[1], &new], &item);
    calls.refused(|calls| calls.get(&[TOP, &at[1], &old], &item));
    calls.get(&[TOP, &at[1]], &new);
}

const PARENTS: usize = 12;

/// One batch that moves `trees` trees.
fn batch_of<E: Engine>(dir: &Path, version: &'static GroveVersion, trees: usize) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let top = tree(Op::insert_or_replace_op, Vec::new(), TOP.to_vec());
    calls.batch("the top", vec![top]);
    made(&mut calls, 0, PARENTS, trees, 1);
    calls.batch("many moves", moves(0, PARENTS, trees));
    for number in [0, 1, PARENTS, trees / 2, trees - 1] {
        read_moved(&mut calls, 0, PARENTS, number);
    }
    db.flush().expect("a flush");
    log
}

fn two_thousand<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    batch_of::<E>(dir, version, 2_000)
}

fn ten_thousand<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    batch_of::<E>(dir, version, 10_000)
}

/// The batches of `transaction_of`, whole: for each how many trees it
/// moves, and how many parents they are under.
const BATCHES: [(usize, usize); 14] = [
    (2_884, 2_126),
    (128, 128),
    (235, 161),
    (251, 251),
    (10_000, 12),
    (6_037, 13),
    (111, 46),
    (491, 368),
    (20, 13),
    (123, 17),
    (254, 254),
    (459, 48),
    (244, 244),
    (7, 2),
];

/// The first parent of the batch `batch`.
fn first_of(batch: usize) -> usize {
    batch * 10_000
}

/// The batches at `percent` hundredths of their sizes: for each how many
/// trees it moves, how many parents they are under, and how many items
/// each tree holds, which is two hundred in the two smallest batches.
fn batches(percent: usize) -> Vec<(usize, usize, usize)> {
    let scaled = |count: usize| (count * percent / 100).max(1);
    let sized = |(count, parents): (usize, usize)| {
        let items = if count <= 20 { 200 } else { 1 };
        (scaled(count), scaled(parents).min(scaled(count)), items)
    };
    BATCHES.into_iter().map(sized).collect()
}

/// How many trees the batches at `percent` hundredths move.
fn moved_by(percent: usize) -> usize {
    batches(percent).iter().map(|(count, ..)| count).sum()
}

/// The fourteen batches in one transaction, at `percent` hundredths of
/// their sizes.
fn transaction_of<E: Engine>(dir: &Path, version: &'static GroveVersion, percent: usize) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let top = tree(Op::insert_or_replace_op, Vec::new(), TOP.to_vec());
    calls.batch("the top", vec![top]);
    let batches = batches(percent);
    for (batch, (count, parents, items)) in batches.iter().copied().enumerate() {
        made(&mut calls, first_of(batch), parents, count, items);
    }
    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    for (batch, (count, parents, _)) in batches.iter().copied().enumerate() {
        let name = format!("{count} moves under {parents} parents");
        calls.batch(&name, moves(first_of(batch), parents, count));
        read_moved(&mut calls, first_of(batch), parents, count - 1);
    }
    // What the first batch moved is read after the last.
    read_moved(&mut calls, first_of(0), batches[0].1, 0);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    for (batch, (count, parents, _)) in batches.iter().copied().enumerate() {
        read_moved(&mut calls, first_of(batch), parents, count / 2);
    }
    db.flush().expect("a flush");
    log
}

fn a_quarter<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    transaction_of::<E>(dir, version, 25)
}

fn whole<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    transaction_of::<E>(dir, version, 100)
}

/// Runs `history` on both engines, on LigninDB first, and gives the
/// steps that the looks of the LigninDB engine made for it.
macro_rules! steps_of {
    ($history:ident, $version:expr) => {{
        look_steps_taken();
        let log = on_both_engines!($history, $version);
        assert_eq!(log.refused, log.on_purpose);
        look_steps_taken()
    }};
}

/// Runs `run` on a thread with a stack to spare, and gives the steps it
/// gives: the looks count their steps on the thread that makes them.
///
/// Merk applies a batch that removes many keys of one Merk with a call
/// inside a call for removals that follow each other
/// (`Walker::apply_sorted`), on either engine, and a batch of moves
/// removes the old key of each tree it moves. How deep that goes depends
/// on the shape of the Merk, and in a debug build the stack of a test's
/// thread is too short for some of the shapes here.
fn on_a_stack_to_spare(run: impl FnOnce() -> u64 + Send) -> u64 {
    std::thread::scope(|scope| {
        let thread = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
        let thread = thread.spawn_scoped(scope, run).expect("a thread");
        thread.join().expect("the history ends")
    })
}

/// Holds `steps`, the steps of the looks for a history that moved `moved`
/// trees, to the bound. The batches that make the trees move nothing,
/// and their looks make no step.
fn assert_bounded(steps: u64, moved: usize) {
    eprintln!("{steps} steps of the looks for {moved} moves");
    assert!(steps >= moved as u64, "{steps}");
    assert!(steps <= STEPS_FOR_A_MOVE * moved as u64, "{steps}");
}

#[test]
fn a_batch_of_two_thousand_moves_is_applied_alike_within_a_bound_on_the_looks() {
    assert_bounded(
        on_a_stack_to_spare(|| steps_of!(two_thousand, &GROVE_VERSIONS[3])),
        2_000,
    );
}

#[test]
fn fourteen_batches_of_moves_in_one_transaction_are_applied_alike_at_a_quarter_of_their_sizes() {
    assert_eq!(moved_by(25), 5_305);
    assert_bounded(
        on_a_stack_to_spare(|| steps_of!(a_quarter, &GROVE_VERSIONS[3])),
        moved_by(25),
    );
}

#[test]
#[ignore = "minutes in a debug build"]
fn a_batch_of_ten_thousand_moves_is_applied_alike_within_a_bound_on_the_looks_on_grove_v4() {
    assert_bounded(
        on_a_stack_to_spare(|| steps_of!(ten_thousand, &GROVE_VERSIONS[3])),
        10_000,
    );
}

#[test]
#[ignore = "minutes in a debug build"]
fn a_batch_of_ten_thousand_moves_is_applied_alike_within_a_bound_on_the_looks_on_grove_v5() {
    assert_bounded(
        on_a_stack_to_spare(|| steps_of!(ten_thousand, &GROVE_VERSIONS[4])),
        10_000,
    );
}

#[test]
#[ignore = "minutes in a debug build"]
fn fourteen_batches_of_moves_in_one_transaction_are_applied_alike_on_grove_v4() {
    assert_eq!(moved_by(100), 21_244);
    assert_bounded(
        on_a_stack_to_spare(|| steps_of!(whole, &GROVE_VERSIONS[3])),
        moved_by(100),
    );
}

#[test]
#[ignore = "minutes in a debug build"]
fn fourteen_batches_of_moves_in_one_transaction_are_applied_alike_on_grove_v5() {
    assert_bounded(
        on_a_stack_to_spare(|| steps_of!(whole, &GROVE_VERSIONS[4])),
        moved_by(100),
    );
}
