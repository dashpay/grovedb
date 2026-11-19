//! Indexed trees that rank their entries by a 128-bit sum.

use grovedb::{AxisQuery, Element, PathQuery, Query, QueryItem};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    proofs::query::{AggregateFold, IndexAxis},
};

use super::DOMAIN_WIDE_RANKS;
use crate::workload::{
    build::{self, batch, child, path, value},
    op::{BatchOp, IndexedTree, Op, Path, Read, Write},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"wide-ranks";
const BOARDS: &[u8] = b"boards";
const RANKED: &[u8] = b"ranked";

/// The fewest amounts a round of [`WideRanks`] adds to a tree: the next
/// round writes two of them again and deletes two others.
pub const MIN_WIDE_RANK_ENTRIES: u32 = 4;

/// How many rounds of [`WideRanks`] create a tree under `boards`.
const BOARD_ROUNDS: u32 = 3;

/// Two indexed trees and the rankings they keep.
///
/// Grove version 5 adds two indexed trees with a 128-bit sum: a
/// `ProvableBigSumIndexedTree`, which ranks its entries by their sums, and
/// a `ProvableCountProvableBigSumIndexedTree`, which ranks them by their
/// counts, their averages and their sums. The axis both rank their sums on
/// is the big sum axis. Where the grove version creates them, the trees of
/// this workload are these two, and an entry may be a tree whose own sum
/// no `i64` holds. On an older grove version they are the trees with an
/// `i64` sum those two are the wide forms of, with small amounts, ranked on
/// the sum axis.
///
/// An entry is an amount or a pot: a tree that keeps the sum of the amounts
/// put into it, and for the second tree their count as well. A round adds
/// amounts and a pot to each tree, through the methods GroveDB has for the
/// type of the tree and through a batch in turn. From the second round on
/// it writes two amounts again, which moves them in the rankings, adds to
/// an older pot and takes from one, and deletes two amounts and a key that
/// is not stored. Every other round it deletes a pot with what it holds.
/// The first rounds also create a tree of the first kind under `boards`,
/// each with its entries and a pot in the batch that creates it.
///
/// On a grove version whose batches leave the rankings out of step with
/// the entries they write ([`Features::indexed_batches_in_step`]), every
/// entry is written through the methods of its tree, and a batch only
/// creates the trees.
///
/// After each block the trees are read back as elements and as the numbers
/// `query_item_value_or_sum` returns, and their entries as stored. Where
/// the grove version reads by rank, the rankings are read and proven:
/// pages from either end, as entries and as keys, the entries in a band of
/// values, the rank of a key, how many entries a band holds, and one page
/// for each of several trees under `boards`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WideRanks {
    /// How many rounds to run.
    pub rounds: u32,
    /// How many amounts a round adds to each tree, at least
    /// [`MIN_WIDE_RANK_ENTRIES`].
    pub entries: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for WideRanks {
    fn default() -> Self {
        Self {
            rounds: 5,
            entries: 4,
            features: Features::ALL,
        }
    }
}

/// One of the two trees.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Board {
    /// The tree that ranks by sum.
    Summed,
    /// The tree that ranks by count, average and sum.
    Counted,
}

impl Board {
    const BOTH: [Board; 2] = [Board::Summed, Board::Counted];

    fn key(self) -> &'static [u8] {
        match self {
            Board::Summed => b"by-sum",
            Board::Counted => b"by-both",
        }
    }

    /// The empty tree, with a 128-bit sum when `wide` is set.
    fn tree(self, wide: bool) -> Element {
        let axes = |axes: [IndexAxis; 3]| axes.map(|axis| (axis.tag(), None)).to_vec();
        match (self, wide) {
            (Board::Summed, true) => Element::empty_provable_big_sum_indexed_tree(),
            (Board::Summed, false) => Element::empty_provable_sum_indexed_tree(),
            (Board::Counted, true) => Element::ProvableCountProvableBigSumIndexedTree(
                None,
                0,
                0,
                axes([IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]),
                None,
            ),
            (Board::Counted, false) => Element::ProvableCountProvableSumIndexedTree(
                None,
                0,
                0,
                axes([IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]),
                None,
            ),
        }
    }

    /// The methods that write an entry of the tree.
    fn methods(self, wide: bool) -> IndexedTree {
        match (self, wide) {
            (Board::Summed, true) => IndexedTree::BigSum,
            (Board::Summed, false) => IndexedTree::Sum,
            (Board::Counted, true) => IndexedTree::CountBigSum,
            (Board::Counted, false) => IndexedTree::CountSum,
        }
    }

    /// An empty pot of the kind the tree takes as an entry.
    fn pot(self, wide: bool) -> Element {
        match (self, wide) {
            (Board::Summed, true) => Element::empty_big_sum_tree(),
            (Board::Summed, false) => Element::empty_sum_tree(),
            (Board::Counted, true) => Element::empty_count_big_sum_tree(),
            (Board::Counted, false) => Element::empty_count_sum_tree(),
        }
    }
}

/// The axis a tree ranks its sums on.
fn sum_axis(wide: bool) -> IndexAxis {
    if wide {
        IndexAxis::BigSum
    } else {
        IndexAxis::Sum
    }
}

/// What one of the two trees holds.
#[derive(Default)]
struct Held {
    amounts: Vec<Vec<u8>>,
    /// The pots, oldest first, each with the keys of its amounts.
    pots: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
}

struct Generator {
    rng: Rng,
    features: Features,
    wide: bool,
    /// Whether a batch writes entries. Otherwise the methods of the trees
    /// write them all.
    batches: bool,
    root: Path,
    entries: u32,
    /// The number the next key is made of.
    next: u32,
    held: [Held; 2],
    /// The trees under `boards`, each with the keys of its entries.
    boards: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
    ops: Vec<Op>,
}

impl WideRanks {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            self.entries >= MIN_WIDE_RANK_ENTRIES,
            "a round adds at least {MIN_WIDE_RANK_ENTRIES} entries"
        );
        let mut generator = Generator {
            rng: Rng::derive(seed, DOMAIN_WIDE_RANKS, 0),
            features: self.features,
            wide: self.features.big_sum_tree_twins,
            batches: self.features.indexed_batches_in_step,
            root: path(&[ROOT]),
            entries: self.entries,
            next: 0,
            held: [Held::default(), Held::default()],
            boards: Vec::new(),
            ops: Vec::new(),
        };
        generator.create();
        for round in 0..self.rounds {
            generator.round(round);
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    /// A key no tree of the workload holds yet, starting with `tag`.
    fn key(&mut self, tag: u8) -> Vec<u8> {
        self.next += 1;
        let mut key = vec![tag];
        key.extend_from_slice(&(self.next - 1).to_be_bytes());
        key
    }

    /// A sum to rank by: three in four are credits. Two wide credits are
    /// more than an `i64` holds.
    fn sum(&mut self) -> i64 {
        if !self.wide {
            return build::sum(&mut self.rng);
        }
        let sum = i64::MAX - self.rng.below(1 << 40) as i64;
        if self.rng.chance(1, 4) {
            -sum
        } else {
            sum
        }
    }

    /// An amount as an entry of `board`.
    fn amount(&mut self, board: Board) -> Element {
        let sum = self.sum();
        match board {
            Board::Summed => Element::new_sum_item(sum),
            Board::Counted => Element::new_item_with_sum_item(value(&mut self.rng, 1, 24), sum),
        }
    }

    fn insert(&self, board: Board, tree: &Path, key: &[u8], element: Element) -> Op {
        Op::Write(Write::IndexedInsert {
            path: tree.clone(),
            key: key.to_vec(),
            element,
            tree: board.methods(self.wide),
        })
    }

    /// An entry written in the batch of the round, or through the methods
    /// of the tree where a batch writes none.
    fn in_batch(
        &mut self,
        board: Board,
        tree: &Path,
        key: &[u8],
        element: Element,
        pending: &mut Vec<BatchOp>,
        write: fn(&[Vec<u8>], &[u8], Element) -> BatchOp,
    ) {
        if self.batches {
            pending.push(write(tree, key, element));
        } else {
            self.ops.push(self.insert(board, tree, key, element));
        }
    }

    fn delete(&self, board: Board, tree: &Path, key: &[u8]) -> Op {
        Op::Write(Write::IndexedDelete {
            path: tree.clone(),
            key: key.to_vec(),
            tree: board.methods(self.wide),
        })
    }

    fn create(&mut self) {
        let root = self.root.clone();
        self.ops
            .push(build::insert(&[], ROOT, Element::empty_tree()));
        self.ops.push(build::insert(
            &root,
            Board::Summed.key(),
            Board::Summed.tree(self.wide),
        ));
        self.ops.push(Op::Batch(vec![
            batch::insert_only(&root, Board::Counted.key(), Board::Counted.tree(self.wide)),
            batch::insert_only(&root, BOARDS, Element::empty_tree()),
        ]));
        self.ops.push(Op::Commit);
    }

    fn round(&mut self, round: u32) {
        let root = self.root.clone();
        let mut pending = Vec::new();
        for (index, board) in Board::BOTH.into_iter().enumerate() {
            let tree = child(&root, board.key());
            if round > 0 {
                self.rewrite(round, index, board, &tree, &mut pending);
            }
            for entry in 0..self.entries {
                let key = self.key(b'a');
                let amount = self.amount(board);
                if entry % 2 == 0 {
                    self.ops.push(self.insert(board, &tree, &key, amount));
                } else {
                    let write = batch::insert_or_replace;
                    self.in_batch(board, &tree, &key, amount, &mut pending, write);
                }
                self.held[index].amounts.push(key);
            }
            self.pot(round, index, board, &tree, &mut pending);
            // Where a write can leave a row behind, the rankings are
            // rebuilt before they are read.
            if !self.batches || round as usize % 2 == index {
                self.ops
                    .push(Op::Write(Write::ReconcileIndexed { path: tree }));
            }
        }
        let filled = self.board(round, &mut pending);
        self.ops.push(Op::Batch(pending));
        self.ops.extend(filled);
        if !self.batches {
            let boards = child(&root, BOARDS);
            for (name, _) in &self.boards {
                let path = child(&child(&boards, name), RANKED);
                self.ops.push(Op::Write(Write::ReconcileIndexed { path }));
            }
        }
        self.ops.push(Op::Commit);
        self.reads(round);
    }

    /// Writes two amounts again and deletes two, one of each through the
    /// methods of the tree and the other in the batch of the round, where
    /// the grove version keeps the rankings in step with such a delete. Adds to
    /// a pot, takes from one, and in every other round deletes the oldest
    /// pot with what it holds.
    fn rewrite(
        &mut self,
        round: u32,
        index: usize,
        board: Board,
        tree: &Path,
        pending: &mut Vec<BatchOp>,
    ) {
        let mut chosen = Vec::new();
        for _ in 0..4 {
            let amounts = &mut self.held[index].amounts;
            let at = self.rng.index(amounts.len());
            chosen.push(amounts.swap_remove(at));
        }
        let [written, replaced, deleted, dropped] = chosen.try_into().expect("four amounts");
        let amount = self.amount(board);
        self.ops.push(self.insert(board, tree, &written, amount));
        let amount = self.amount(board);
        self.in_batch(board, tree, &replaced, amount, pending, batch::replace);
        self.ops.push(self.delete(board, tree, &deleted));
        if self.batches {
            pending.push(batch::delete(tree, &dropped));
        } else {
            self.ops.push(self.delete(board, tree, &dropped));
        }
        self.held[index].amounts.extend([written, replaced]);
        let absent = self.key(b'n');
        self.ops.push(self.delete(board, tree, &absent));

        let pots = self.held[index].pots.len();
        let grown = self.rng.index(pots);
        let key = self.key(b'i');
        let sum = self.sum();
        let pot = child(tree, &self.held[index].pots[grown].0);
        self.ops
            .push(build::insert(&pot, &key, Element::new_sum_item(sum)));
        self.held[index].pots[grown].1.push(key);
        let shrunk = self.rng.index(pots);
        let (pot, items) = &mut self.held[index].pots[shrunk];
        if items.len() > 1 {
            let item = items.swap_remove(self.rng.index(items.len()));
            self.ops.push(build::delete(&child(tree, pot), &item));
        }
        if round % 2 == 0 {
            let (pot, _) = self.held[index].pots.remove(0);
            self.ops.push(self.delete(board, tree, &pot));
        }
    }

    /// A pot with two or three amounts. In an even round the pot is
    /// inserted through the methods of the tree and filled by inserts, in
    /// an odd one the batch of the round creates and fills it where a
    /// batch writes entries.
    fn pot(
        &mut self,
        round: u32,
        index: usize,
        board: Board,
        tree: &Path,
        pending: &mut Vec<BatchOp>,
    ) {
        let key = self.key(b'p');
        let inside = child(tree, &key);
        let batched = self.batches && round % 2 == 1;
        if batched {
            pending.push(batch::insert_only(tree, &key, board.pot(self.wide)));
        } else {
            self.ops
                .push(self.insert(board, tree, &key, board.pot(self.wide)));
        }
        let mut items = Vec::new();
        for _ in 0..2 + round % 2 {
            let item = self.key(b'i');
            let amount = Element::new_sum_item(self.sum());
            batch::insert_or_queue(batched, &mut self.ops, pending, &inside, &item, amount);
            items.push(item);
        }
        self.held[index].pots.push((key, items));
    }

    /// A tree of the first kind under `boards`, created with its entries
    /// by the batch of the round, and an amount more for an older one.
    /// Where a batch writes no entries it creates the tree alone, and what
    /// is returned fills it once the batch is applied.
    fn board(&mut self, round: u32, pending: &mut Vec<BatchOp>) -> Vec<Op> {
        let boards = child(&self.root, BOARDS);
        let board = Board::Summed;
        if !self.boards.is_empty() {
            let older = self.rng.index(self.boards.len());
            let tree = child(&child(&boards, &self.boards[older].0), RANKED);
            let key = self.key(b'a');
            let amount = self.amount(board);
            self.ops.push(self.insert(board, &tree, &key, amount));
            self.boards[older].1.push(key);
        }
        if round >= BOARD_ROUNDS {
            return Vec::new();
        }
        let name = vec![b'b', b'0' + round as u8];
        let branch = child(&boards, &name);
        let tree = child(&branch, RANKED);
        pending.push(batch::insert_only(&boards, &name, Element::empty_tree()));
        pending.push(batch::insert_only(&branch, RANKED, board.tree(self.wide)));
        let mut entries = Vec::new();
        let mut filled = Vec::new();
        for _ in 0..3 {
            let key = self.key(b'a');
            let amount = self.amount(board);
            if self.batches {
                pending.push(batch::insert_only(&tree, &key, amount));
            } else {
                filled.push(self.insert(board, &tree, &key, amount));
            }
            entries.push(key);
        }
        let pot = self.key(b'p');
        if self.batches {
            pending.push(batch::insert_only(&tree, &pot, board.pot(self.wide)));
        } else {
            filled.push(self.insert(board, &tree, &pot, board.pot(self.wide)));
        }
        for _ in 0..2 {
            let item = self.key(b'i');
            let amount = Element::new_sum_item(self.sum());
            let inside = child(&tree, &pot);
            if self.batches {
                pending.push(batch::insert_only(&inside, &item, amount));
            } else {
                filled.push(build::insert(&inside, &item, amount));
            }
        }
        entries.push(pot);
        self.boards.push((name, entries));
        filled
    }

    /// A band of values on the axis of sums: past the largest `i64`, below
    /// the smallest, or around zero.
    fn band(&mut self) -> (i128, i128) {
        let (max, min) = (i128::from(i64::MAX), i128::from(i64::MIN));
        if !self.wide {
            let low = i128::from(build::sum(&mut self.rng));
            return (low, low + i128::from(self.rng.between(0, 1_500_000)));
        }
        match self.rng.below(4) {
            0 => (max + 1, i128::MAX),
            1 => (i128::MIN, min - 1),
            2 => (min, max),
            _ => (
                -i128::from(self.rng.between(0, 3)) * max,
                i128::from(self.rng.between(0, 3)) * max,
            ),
        }
    }

    fn reads(&mut self, round: u32) {
        let root = self.root.clone();
        let wide = self.wide;
        for board in Board::BOTH {
            self.ops.push(build::get(&root, board.key()));
        }
        let mut both = Query::new();
        for board in Board::BOTH {
            both.insert_key(board.key().to_vec());
        }
        self.ops.push(Op::Read(Read::ItemValues {
            query: Box::new(PathQuery::new_unsized(root.clone(), both)),
            or_sums: true,
        }));
        for (index, board) in Board::BOTH.into_iter().enumerate() {
            let tree = child(&root, board.key());
            let tree_type = board.tree(wide).tree_type().expect("a tree");
            let proves = self.features.proves_queries_in(tree_type);
            self.ops.push(build::path_query_read(
                PathQuery::new_unsized(tree.clone(), Query::new_range_full()),
                proves,
            ));
            // The entries through a query that starts above the tree.
            let mut through = Query::new_single_key(board.key().to_vec());
            let from = self.key(b'a');
            let limit = self.rng.between(2, 5) as u16;
            through.set_subquery(Query::new_single_query_item(QueryItem::RangeTo(..from)));
            self.ops.push(build::path_query_read(
                build::sized(&root, through, Some(limit), None),
                proves,
            ));
            if self.features.read_modes {
                self.rankings(round, index, board, &tree);
            }
        }
        if self.features.read_modes {
            let mut branches: Vec<Vec<u8>> =
                self.boards.iter().map(|board| board.0.clone()).collect();
            branches.push(b"b9".to_vec());
            let page = self.rng.between(1, 3) as u16;
            let descending = self.rng.chance(1, 2);
            self.ops.push(build::proven(PathQuery::new_branched_axis(
                child(&root, BOARDS),
                branches,
                vec![RANKED.to_vec()],
                AxisQuery::top_k(sum_axis(wide), page, 0, descending),
            )));
        }
    }

    /// The rankings of one tree, read and proven.
    fn rankings(&mut self, round: u32, index: usize, board: Board, tree: &Path) {
        let wide = self.wide;
        let sums = sum_axis(wide);
        let axis = match board {
            Board::Summed => sums,
            Board::Counted => [IndexAxis::Count, IndexAxis::Avg, sums][round as usize % 3],
        };
        let page = self.rng.between(1, 4) as u16;
        let offset = self.rng.below(4);
        let descending = self.rng.chance(1, 2);
        self.ops.push(build::proven(PathQuery::new_axis_top_k(
            tree.clone(),
            axis,
            page,
            offset,
            descending,
        )));
        let offset = self.rng.below(3);
        self.ops.push(build::proven(PathQuery::new_axis(
            tree.clone(),
            AxisQuery::bottom_k(sums, page, offset).keys_only(),
        )));
        let (low, high) = self.band();
        let limit = self.rng.between(1, 5) as u16;
        self.ops.push(build::proven(PathQuery::new_axis_bounded(
            tree.clone(),
            sums,
            low,
            high,
            limit,
            self.rng.chance(1, 2),
        )));
        let held = &self.held[index];
        let ranked = if self.rng.chance(1, 3) {
            held.pots[self.rng.index(held.pots.len())].0.clone()
        } else {
            held.amounts[self.rng.index(held.amounts.len())].clone()
        };
        self.ops.push(build::proven(PathQuery::new_axis_rank_of_key(
            tree.clone(),
            sums,
            ranked,
            self.rng.chance(1, 2),
        )));
        let (low, high) = self.band();
        // The axis of 128-bit sums has no total: a row of its secondary
        // tree carries an `i64`.
        let fold = if wide || self.rng.chance(1, 2) {
            AggregateFold::Population
        } else {
            AggregateFold::Total
        };
        self.ops.push(build::proven(
            PathQuery::new_axis_aggregate_over_value_range(tree.clone(), sums, low, high, fold),
        ));
    }
}
