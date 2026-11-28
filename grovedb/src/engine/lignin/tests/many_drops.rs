//! Many flat drops in one write transaction (TXN-03): the same calls on
//! LigninDB and, where the RocksDB engine is compiled in, on RocksDB,
//! with every answer, every `OperationCost` and the root hash after
//! every batch and every round of drops compared, on the grove versions
//! that have a flat drop, and the work of the LigninDB engine for the
//! drops held to the trees of a round. An element that a read returns is
//! compared as its serialized bytes, which are all of it.
//!
//! One transaction makes rounds. Each round inserts trees by a batch:
//! chains of four, a tree under one of twelve parents, a tree in it, and
//! so on. A second batch puts an item into the innermost tree of each
//! chain. The round reads what it made, by `has_raw`, `get_raw_optional`
//! and `query_raw`. From the round `lag` on it then drops flat, with a
//! call each and from the innermost tree outward, the trees that the
//! round `lag` rounds before made: each tree holds no tree when it is
//! dropped. Then the transaction is committed. The trees of the last
//! `lag` rounds are never dropped: they are there after the commit.
//!
//! The suite runs twelve rounds of forty-eight trees, which are sixty
//! inserts a round. Two hundred rounds of 386 trees, 483 inserts a round,
//! on each grove version that has a flat drop, are tests that are run
//! when they are asked for, in a release build: each took two and a half
//! minutes there on a machine of sixteen cores, and a debug build takes
//! many times as long.
//!
//! ```text
//! GROVEDB_TEST_ENGINE=lignin cargo test --release -p grovedb --all-features --lib -- many_drops --include-ignored --nocapture
//! ```
//!
//! A drop takes what the transaction wrote below the dropped tree, and
//! the LigninDB engine asks its subtree directory which subtrees those
//! are. It counts the entries it opens for that
//! (`lignindb::txn::TxnStats::below_opened`). An engine that asks a
//! subtree at the next drop of a tree with an element after the
//! transaction has come to write under it, and once more at the next
//! such drop after a batch took its entry, opens at most two for each
//! tree, in whichever round: one for a tree that is not dropped, or that
//! no such drop follows. One that asks every subtree the transaction has
//! written, at each drop, opens in one round as many as the round has
//! drops times the subtrees written so far, which is more with every
//! round. These tests hold what is opened to a fixed number for each
//! tree, over the rounds that `OPENED_FOR_A_TREE` says.

use std::{cell::RefCell, path::Path, time::Instant};

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    batches::{Calls, Op},
    nested::{assert_same, check, on_both_engines, open, Log},
    LigninEngine,
};
use crate::{
    engine::Engine, query_result_type::QueryResultType, BackwardsReferences, Element, PathQuery,
    Query, Transaction,
};

const TOP: &[u8] = b"top";
const ITEM: &[u8] = b"item";
const PARENTS: usize = 12;

/// How many trees a chain has: each is in the one before it, and the
/// innermost holds an item.
const DEPTH: usize = 4;

/// The most entries of its directory that the LigninDB engine opens for
/// each tree that is made or dropped: over all the rounds up to any one
/// of them, and in one round that comes after the first round with
/// drops. The first round with drops is held to the first of the two
/// alone: it asks what the transaction wrote in every round up to it.
///
/// A tree is asked at the next drop of a tree with an element after the
/// transaction has come to write under it, and once more at the next
/// such drop after a batch dropped it, if one follows. That is at most
/// one for each tree that a round makes and one for each that it drops.
/// The bound leaves as much again: it is room for a change of what a
/// call writes, and it catches an engine that asks every written subtree
/// at each drop. It is no measure of an engine that finds what is below
/// a tree otherwise.
const OPENED_FOR_A_TREE: u64 = 2;

/// An engine that says how many entries of its subtree directory a
/// write transaction opened to find what it had written below the
/// subtrees that it dropped or cleared.
trait Counted: Engine {
    /// Whether the engine counts them.
    const COUNTS: bool;

    fn opened(transaction: &Transaction<'_, Self>) -> u64;
}

impl Counted for LigninEngine {
    const COUNTS: bool = true;

    fn opened(transaction: &Transaction<'_, Self>) -> u64 {
        let stats = transaction.with_write_transaction(|written| written.stats());
        stats.expect("a write transaction").below_opened
    }
}

#[cfg(feature = "rocksdb_storage")]
impl Counted for crate::RocksDbEngine {
    const COUNTS: bool = false;

    fn opened(_: &Transaction<'_, Self>) -> u64 {
        0
    }
}

/// What one round did, on the engine that counts.
#[derive(Debug, Clone, Copy)]
struct Round {
    made: usize,
    dropped: usize,
    /// The entries of the directory that were opened in the round.
    opened: u64,
}

thread_local! {
    /// The rounds of the history that this thread ran last on the engine
    /// that counts.
    static ROUNDS: RefCell<Vec<Round>> = const { RefCell::new(Vec::new()) };
}

fn key(kind: u8, number: usize) -> Vec<u8> {
    let mut key = vec![kind];
    key.extend_from_slice(&(number as u32).to_be_bytes());
    key
}

/// The tree `number` of the round `round`: the path it is inserted at,
/// and its key. The trees of a round are numbered chain by chain, and
/// in a chain from the outermost in: a tree is in the trees of its chain
/// that are numbered before it.
fn tree(round: usize, number: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let (chain, level) = (number / DEPTH, number % DEPTH);
    let named = |number: usize| key(b'c', round * 100_000 + number);
    let mut at = vec![TOP.to_vec(), key(b'p', chain % PARENTS)];
    at.extend((chain * DEPTH..chain * DEPTH + level).map(named));
    (at, named(number))
}

/// Whether the tree `number` of a round of `trees` trees is the
/// innermost of its chain, which is given an item: the last of each four,
/// and the last of the round, whose chain is shorter where the round's
/// trees are no multiple of four.
fn innermost(number: usize, trees: usize) -> bool {
    number % DEPTH == DEPTH - 1 || number == trees - 1
}

fn slices(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// An element as its serialized bytes. The text of an element shows a
/// long value by its ends, and empty flags as none: two elements that
/// differ can have the same text. Their bytes differ.
fn whole(element: &Element, version: &GroveVersion) -> Vec<u8> {
    element
        .serialize(version)
        .expect("an element is serialized")
}

/// The two batches of a round: its trees, each chain from the outermost
/// in, and an item in the innermost tree of each chain.
fn made<E: Engine>(calls: &mut Calls<'_, '_, '_, E>, round: usize, trees: usize) {
    let insert = Op::insert_or_replace_op;
    let planted = (0..trees).map(|number| {
        let (at, key) = tree(round, number);
        insert(at, key, Element::empty_tree())
    });
    calls.batch("the trees of a round", planted.collect());
    let items = (0..trees).filter(|number| innermost(*number, trees));
    let items = items.map(|number| {
        let (mut inside, key) = tree(round, number);
        inside.push(key);
        insert(
            inside,
            ITEM.to_vec(),
            Element::new_item(vec![round as u8; 32]),
        )
    });
    calls.batch("an item in each innermost", items.collect());
}

/// What a round reads: each tree it made is there, seven in ten of them
/// are asked for the item, which the innermost have, and for all they
/// hold, and the outermost tree of every other chain that the round
/// before dropped is there no more. The log takes an element that is
/// read as its serialized bytes, and all that a query returns: how many
/// results, each key with its element, and how many it passed over.
fn read<E: Engine>(calls: &mut Calls<'_, '_, '_, E>, round: usize, trees: usize, lag: usize) {
    let (db, through, version) = (calls.db, calls.through, calls.version);
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    for number in 0..trees {
        let (at, key) = tree(round, number);
        let has = db.has_raw(slices(&at).as_slice(), &key, through, version);
        calls.log.answer("has".to_owned(), has);
        if number % 10 < 7 {
            let inside = [at, vec![key]].concat();
            let path = slices(&inside);
            let item = db.get_raw_optional(path.as_slice().into(), ITEM, through, version);
            let item = item.map_ok(|found| found.map(|element| whole(&element, version)));
            calls.log.answer("item".to_owned(), item);
            let mut every = Query::new();
            every.insert_all();
            let query = PathQuery::new_unsized(inside, every);
            let all = db.query_raw(&query, true, true, true, pairs, through, version);
            let all = all.map_ok(|(found, passed)| {
                let count = found.len();
                let found = found.to_key_elements().into_iter();
                let found: Vec<(Vec<u8>, Vec<u8>)> = found
                    .map(|(key, element)| (key, whole(&element, version)))
                    .collect();
                (count, found, passed)
            });
            calls.log.answer("all".to_owned(), all);
        }
    }
    let Some(gone) = round.checked_sub(lag + 1) else {
        return;
    };
    for number in (0..trees).step_by(2 * DEPTH) {
        let (at, key) = tree(gone, number);
        let has = db.has_raw(slices(&at).as_slice(), &key, through, version);
        calls.log.answer("has what was dropped".to_owned(), has);
    }
}

/// Drops flat each tree that the round `round` made, with a call each,
/// from the last to the first: a tree after the tree that was in it.
fn dropped<E: Engine>(calls: &mut Calls<'_, '_, '_, E>, round: usize, trees: usize) {
    let (db, through, version) = (calls.db, calls.through, calls.version);
    let unchecked = BackwardsReferences::DontCheck;
    for number in (0..trees).rev() {
        let (at, key) = tree(round, number);
        let at = slices(&at);
        let dropped = db.drop_flat_subtree(at.as_slice(), &key, unchecked, through, version);
        calls.log.answer("flat drop".to_owned(), dropped);
    }
    calls.root();
}

/// `rounds` rounds of `trees` trees in one transaction, and the commit.
/// A round from the round `lag` on drops the trees that the round `lag`
/// rounds before it made; the trees of the last `lag` rounds stay. The
/// committed state is then read as a round reads (`read`): what the last
/// round made, and the outermost tree of every other chain it dropped.
fn rounds_of<E: Counted>(
    dir: &Path,
    version: &'static GroveVersion,
    (rounds, trees, lag): (usize, usize, usize),
) -> Log {
    let began = Instant::now();
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let insert = Op::insert_or_replace_op;
    let mut parents = vec![insert(Vec::new(), TOP.to_vec(), Element::empty_tree())];
    for number in 0..PARENTS {
        let parent = key(b'p', number);
        parents.push(insert(vec![TOP.to_vec()], parent, Element::empty_tree()));
    }
    calls.batch("the parents", parents);
    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    let mut counted = Vec::with_capacity(rounds);
    for round in 0..rounds {
        let before = E::opened(&transaction);
        made(&mut calls, round, trees);
        read(&mut calls, round, trees, lag);
        let old = round.checked_sub(lag);
        if let Some(old) = old {
            dropped(&mut calls, old, trees);
        }
        counted.push(Round {
            made: trees,
            dropped: old.map_or(0, |_| trees),
            opened: E::opened(&transaction) - before,
        });
    }
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    // What the last round made stays, and what it dropped is gone.
    read(&mut calls, rounds - 1, trees, lag.saturating_sub(1));
    db.flush().expect("a flush");
    if E::COUNTS {
        ROUNDS.set(counted);
    }
    eprintln!(
        "{rounds} rounds of {trees} trees on {}: {:.1} s",
        std::any::type_name::<E>(),
        began.elapsed().as_secs_f64()
    );
    log
}

fn twelve_rounds<E: Counted>(dir: &Path, version: &'static GroveVersion) -> Log {
    rounds_of::<E>(dir, version, (12, 48, 3))
}

fn two_hundred_rounds<E: Counted>(dir: &Path, version: &'static GroveVersion) -> Log {
    rounds_of::<E>(dir, version, (200, 386, 25))
}

/// Holds what each round of the history that this thread ran last
/// opened to the bound.
///
/// - A round with no drop opens nothing.
/// - A round with drops opens at least an entry for every chain it
///   dropped: the count is alive. That is of this engine, which asks its
///   directory; one that knew what is below a tree with no read would
///   open none, and be as right.
/// - The first round with drops asks what the transaction wrote in all
///   the rounds up to it, and is held to the next bound alone. Over the
///   rounds up to any round, no more is opened than the bound for each
///   tree they made or dropped.
/// - Every round after that one opens no more than the bound for each
///   tree it made or dropped itself, however many rounds came before.
fn assert_bounded() {
    let rounds = ROUNDS.take();
    assert!(
        !rounds.is_empty(),
        "the history ran on the engine that counts"
    );
    let (mut opened, mut trees, mut dropped) = (0, 0, 0);
    let mut later: Vec<u64> = Vec::new();
    for (number, round) in rounds.iter().enumerate() {
        let of_the_round = (round.made + round.dropped) as u64;
        opened += round.opened;
        trees += of_the_round;
        assert!(
            opened <= OPENED_FOR_A_TREE * trees,
            "up to round {number}: {opened} opened for {trees} trees"
        );
        match round.dropped {
            0 => assert_eq!(round.opened, 0, "round {number}"),
            count => {
                let chains = (0..count).filter(|tree| innermost(*tree, count)).count();
                assert!(round.opened >= chains as u64, "round {number}: {round:?}");
                if dropped > 0 {
                    assert!(
                        round.opened <= OPENED_FOR_A_TREE * of_the_round,
                        "round {number}: {round:?}"
                    );
                    later.push(round.opened);
                }
            }
        }
        dropped += round.dropped;
    }
    let first = rounds.iter().find(|round| round.dropped > 0);
    let first = first.map_or(0, |round| round.opened);
    let (least, most) = (later.iter().min(), later.iter().max());
    eprintln!(
        "{opened} entries of the directory opened for {dropped} drops in {} rounds: {first} in the \
         first round with drops, and from {} to {} in each of the {} after it",
        rounds.len(),
        least.copied().unwrap_or(0),
        most.copied().unwrap_or(0),
        later.len()
    );
    assert!(dropped > 0 && !later.is_empty());
}

/// Runs `history` on LigninDB and, where the RocksDB engine is compiled
/// in, on RocksDB too, with every answer compared, and holds what its
/// rounds opened on LigninDB to the bound.
macro_rules! bounded {
    ($history:ident, $version:expr) => {{
        let log = on_both_engines!($history, $version);
        assert_eq!(log.refused, log.on_purpose);
        assert_bounded();
    }};
}

#[test]
fn rounds_of_flat_drops_in_one_transaction_are_applied_alike_within_a_bound_on_the_entries_opened()
{
    for version in &GROVE_VERSIONS[3..] {
        bounded!(twelve_rounds, version);
    }
}

#[test]
#[ignore = "minutes in a release build, many times as long in a debug build"]
fn two_hundred_rounds_of_flat_drops_in_one_transaction_are_applied_alike_on_grove_v4() {
    bounded!(two_hundred_rounds, &GROVE_VERSIONS[3]);
}

#[test]
#[ignore = "minutes in a release build, many times as long in a debug build"]
fn two_hundred_rounds_of_flat_drops_in_one_transaction_are_applied_alike_on_grove_v5() {
    bounded!(two_hundred_rounds, &GROVE_VERSIONS[4]);
}
