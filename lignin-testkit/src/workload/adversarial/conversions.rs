//! Trees that keep a 64-bit sum, converted in place to the trees that keep
//! it in 128 bits.

use grovedb::{AxisQuery, Element, ElementFlags, PathQuery, Query, QueryItem};
use grovedb_merk::proofs::query::{AggregateFold, IndexAxis};

use super::{near_max, DOMAIN_CONVERSIONS};
use crate::workload::{
    build::{self, batch, child, path, storage_flags, value},
    op::{self, BatchOp, Fetch, IndexedTree, Op, Path, Read, TransactionOp, Write},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"conversions";
const NESTED: &[u8] = b"nested";
const NOTE: &[u8] = b"note";
const WRAPPED: &[u8] = b"wrapped";
const OUTER: &[u8] = b"outer";
const INNER: &[u8] = b"inner";

/// The most rounds of [`Conversions`]: a key is made of a number of four
/// bytes, a round uses a few hundred of them, and a round takes an epoch.
pub const MAX_CONVERSION_ROUNDS: u32 = 10_000;

/// How far inside the `i64` range the amounts of a tree with a large credit
/// and a large debit stay, so that a write made before the conversion fits.
const HEADROOM: i64 = 4_000_000;

/// The seven trees that keep a 64-bit sum and are converted in place.
///
/// `GroveDb::convert_to_big_sum_tree`, which grove version 5 adds, turns a
/// `SumTree`, a `CountSumTree`, a `ProvableSumTree`, a
/// `ProvableCountSumTree`, a `ProvableCountProvableSumTree`, a
/// `ProvableSumIndexedTree` or a `ProvableCountProvableSumIndexedTree` into
/// the tree that holds the same children with the sum in 128 bits. Every
/// node of the tree is written again, an indexed tree gets the rows of its
/// sums under longer keys, and the element that describes the tree changes
/// its type.
///
/// A round builds one tree of each of the seven types in a tree of its own.
/// The trees of a round are empty, hold one node, or hold enough for a Merk
/// of a few levels, in turn. A tree of the last size holds amounts, an item
/// with an amount, an item without one, and a tree with amounts of its own,
/// which for an indexed tree is an entry that ranks by what it holds. Its
/// amounts add up to the largest `i64` exactly, to the smallest exactly, to
/// about nothing with one large credit and one large debit, or to little.
/// Two or three of the five trees that do not rank their entries are
/// created by the batch of the round, which also writes whatever the size
/// of the round puts into them, and each tree carries storage flags or
/// none, as the seed has it.
///
/// Where the grove version has the operation, the round then converts every
/// tree: half of them in the transaction of the block, among its writes and
/// some after a write of the same block to the tree, and half given no
/// transaction, which GroveDB then runs in one of its own. One conversion
/// of each round is first made and undone, by a rollback of the transaction
/// or to a savepoint. The converted trees go on being used: amounts take
/// their sums past either end of the `i64` range, an amount is replaced and
/// one deleted, an indexed tree takes entries through the methods of its new
/// type, among them a tree with a 128-bit sum of its own, and the trees
/// inside converted trees, which keep their type, are converted in turn or
/// go on adding their 64-bit sums. Before the first round the stream
/// converts trees their parents leave out of a count or a sum, inside their
/// wrappers, and a tree inside a tree that sums it, parent first.
///
/// The trees are read as built, as converted and as used: as elements, as
/// the rows of the tree that holds them and of each tree, as the sum and
/// the count of a range of keys where the type proves them, and for an
/// indexed tree in the order of its sums, its counts and its averages.
///
/// Where the grove version has no such operation, the stream builds the
/// same trees, reads them and ends each round there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conversions {
    /// How many rounds to run, at most [`MAX_CONVERSION_ROUNDS`].
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for Conversions {
    fn default() -> Self {
        Self {
            rounds: 6,
            features: Features::ALL,
        }
    }
}

/// One of the seven types.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sum,
    CountSum,
    ProvableSum,
    ProvableCountSum,
    ProvableCountProvableSum,
    SumIndexed,
    CountSumIndexed,
}

impl Kind {
    const ALL: [Kind; 7] = [
        Kind::Sum,
        Kind::CountSum,
        Kind::ProvableSum,
        Kind::ProvableCountSum,
        Kind::ProvableCountProvableSum,
        Kind::SumIndexed,
        Kind::CountSumIndexed,
    ];

    fn key(self) -> &'static [u8] {
        match self {
            Kind::Sum => b"sum",
            Kind::CountSum => b"count-sum",
            Kind::ProvableSum => b"provable-sum",
            Kind::ProvableCountSum => b"provable-count-sum",
            Kind::ProvableCountProvableSum => b"provable-count-provable-sum",
            Kind::SumIndexed => b"sum-indexed",
            Kind::CountSumIndexed => b"count-sum-indexed",
        }
    }

    /// The empty tree, as it is before a conversion.
    fn tree(self, flags: Option<ElementFlags>) -> Element {
        match self {
            Kind::Sum => Element::SumTree(None, 0, flags),
            Kind::CountSum => Element::CountSumTree(None, 0, 0, flags),
            Kind::ProvableSum => Element::ProvableSumTree(None, 0, flags),
            Kind::ProvableCountSum => Element::ProvableCountSumTree(None, 0, 0, flags),
            Kind::ProvableCountProvableSum => {
                Element::ProvableCountProvableSumTree(None, 0, 0, flags)
            }
            Kind::SumIndexed => Element::ProvableSumIndexedTree(None, None, 0, flags),
            Kind::CountSumIndexed => Element::ProvableCountProvableSumIndexedTree(
                None,
                0,
                0,
                [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
                    .map(|axis| (axis.tag(), None))
                    .to_vec(),
                flags,
            ),
        }
    }

    /// Whether the tree ranks its entries.
    fn indexed(self) -> bool {
        matches!(self, Kind::SumIndexed | Kind::CountSumIndexed)
    }

    /// The methods that write an entry of the tree, for the two that rank
    /// their entries.
    fn methods(self, wide: bool) -> Option<IndexedTree> {
        match (self, wide) {
            (Kind::SumIndexed, false) => Some(IndexedTree::Sum),
            (Kind::SumIndexed, true) => Some(IndexedTree::BigSum),
            (Kind::CountSumIndexed, false) => Some(IndexedTree::CountSum),
            (Kind::CountSumIndexed, true) => Some(IndexedTree::CountBigSum),
            _ => None,
        }
    }

    /// An empty tree with amounts of its own, as a child of a tree of this
    /// type: what an indexed tree takes as an entry, and for the others a
    /// `SumTree`.
    fn pot(self, wide: bool) -> Element {
        match (self, wide) {
            (Kind::CountSumIndexed, false) => Element::empty_count_sum_tree(),
            (Kind::CountSumIndexed, true) => Element::empty_count_big_sum_tree(),
            (_, false) => Element::empty_sum_tree(),
            (_, true) => Element::empty_big_sum_tree(),
        }
    }
}

/// How much a tree of a round holds when it is converted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    Empty,
    OneNode,
    Levels,
}

const SIZES: [Size; 3] = [Size::Empty, Size::OneNode, Size::Levels];

/// What the amounts of a tree add up to before it is converted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lean {
    /// The largest `i64`, with no debit.
    Top,
    /// The smallest `i64`, with no credit.
    Bottom,
    /// A credit and a debit near the bounds, and small amounts of both
    /// signs, which add up to about nothing. A tree of one node has one of
    /// the two large amounts and nothing else.
    Balanced,
    /// Small amounts of both signs.
    Small,
}

const LEANS: [Lean; 4] = [Lean::Top, Lean::Bottom, Lean::Balanced, Lean::Small];

impl Lean {
    /// The lean whose small amounts move a total away from the bound this
    /// one is at.
    fn opposite(self) -> Lean {
        match self {
            Lean::Top => Lean::Bottom,
            Lean::Bottom => Lean::Top,
            Lean::Balanced | Lean::Small => self,
        }
    }
}

/// A tree of the workload and what it holds.
struct Held {
    /// The subtree that holds the tree.
    parent: Path,
    key: Vec<u8>,
    kind: Kind,
    lean: Lean,
    converted: bool,
    /// The keys of its amounts.
    amounts: Vec<Vec<u8>>,
    /// The key of the tree inside it that has amounts of its own.
    pot: Option<Vec<u8>>,
}

impl Held {
    /// A tree at `key` in the subtree at `parent`, not converted and with
    /// nothing in it.
    fn new(parent: Path, key: &[u8], kind: Kind, lean: Lean) -> Held {
        Held {
            parent,
            key: key.to_vec(),
            kind,
            lean,
            converted: false,
            amounts: Vec::new(),
            pot: None,
        }
    }

    fn path(&self) -> Path {
        child(&self.parent, &self.key)
    }

    /// The write of a child of the tree: through the methods of an indexed
    /// tree, or an insert.
    fn put(&self, key: &[u8], element: Element) -> Op {
        match self.kind.methods(self.converted) {
            Some(methods) => Op::Write(Write::IndexedInsert {
                path: self.path(),
                key: key.to_vec(),
                element,
                tree: methods,
            }),
            None => build::insert(&self.path(), key, element),
        }
    }

    /// The removal of a child of the tree, likewise.
    fn remove(&self, key: &[u8]) -> Op {
        match self.kind.methods(self.converted) {
            Some(methods) => Op::Write(Write::IndexedDelete {
                path: self.path(),
                key: key.to_vec(),
                tree: methods,
            }),
            None => build::delete(&self.path(), key),
        }
    }

    fn convert(&self, own_transaction: bool) -> Op {
        convert_at(&self.parent, &self.key, own_transaction)
    }
}

struct Generator {
    rng: Rng,
    features: Features,
    /// Whether a batch writes entries of an indexed tree.
    batches: bool,
    root: Path,
    /// The number the next key is made of.
    next: u32,
    /// The epoch the flags of the round name.
    epoch: u16,
    ops: Vec<Op>,
}

impl Conversions {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            self.rounds <= MAX_CONVERSION_ROUNDS,
            "at most {MAX_CONVERSION_ROUNDS} rounds"
        );
        let mut generator = Generator {
            rng: Rng::derive(seed, DOMAIN_CONVERSIONS, 0),
            features: self.features,
            batches: self.features.indexed_batches_in_step,
            root: path(&[ROOT]),
            next: 0,
            epoch: 0,
            ops: Vec::new(),
        };
        generator.create();
        for round in 0..self.rounds {
            generator.round(round);
        }
        Prepared::stream(generator.ops)
    }
}

/// A small amount of the sign `lean` allows.
fn small(rng: &mut Rng, lean: Lean) -> i64 {
    let amount = build::sum(rng);
    match lean {
        Lean::Top => amount.abs(),
        Lean::Bottom => -amount.abs(),
        Lean::Balanced | Lean::Small => amount,
    }
}

/// Every row of the tree at `tree`, proven.
fn every_row(tree: Path) -> Op {
    build::proven(PathQuery::new_unsized(tree, Query::new_range_full()))
}

/// The conversion of the tree at `key` in the subtree at `parent`.
fn convert_at(parent: &Path, key: &[u8], own_transaction: bool) -> Op {
    Op::Write(Write::ConvertToBigSumTree {
        path: parent.to_vec(),
        key: key.to_vec(),
        own_transaction,
    })
}

impl Generator {
    /// A key no tree of the workload holds yet, starting with `tag`.
    fn key(&mut self, tag: u8) -> Vec<u8> {
        self.next += 1;
        let mut key = vec![tag];
        key.extend_from_slice(&(self.next - 1).to_be_bytes());
        key
    }

    /// An amount close to an end of the `i64` range: two of the same sign
    /// are more than an `i64` holds.
    fn wide(&mut self, credit: bool) -> i64 {
        let amount = near_max(&mut self.rng);
        if credit {
            amount
        } else {
            -amount
        }
    }

    /// An amount that leaves [`HEADROOM`] and what is `reserved` to an end
    /// of the `i64` range.
    fn near(&mut self, reserved: i64) -> i64 {
        near_max(&mut self.rng) - reserved - HEADROOM
    }

    /// An amount as a child of a tree of `kind`: a sum item, or for the
    /// tree that ranks by count, average and sum an item that carries one.
    fn amount(&mut self, kind: Kind, sum: i64) -> Element {
        match kind {
            Kind::CountSumIndexed => {
                Element::new_item_with_sum_item(value(&mut self.rng, 1, 24), sum)
            }
            _ => Element::new_sum_item(sum),
        }
    }

    /// The root tree. Where the grove version converts, also the trees
    /// whose parents leave them out of a count or a sum, and a tree inside
    /// a tree that sums it.
    fn create(&mut self) {
        let root = self.root.clone();
        self.ops
            .push(build::insert(&[], ROOT, Element::empty_tree()));
        if !self.features.convert_to_big_sum_tree {
            self.ops.push(Op::Commit);
            return;
        }
        let wrapped = child(&root, WRAPPED);
        self.ops
            .push(build::insert(&root, WRAPPED, Element::empty_tree()));
        // A parent, the tree in it, and the wrapper that leaves the tree
        // out of what the parent keeps.
        type Wrap = fn(Element) -> Element;
        let parents: [(&[u8], Element, Kind, Wrap); 3] = [
            (b"in-sum", Element::empty_sum_tree(), Kind::Sum, |tree| {
                Element::new_not_summed(tree).expect("a sum tree can be left out of a sum")
            }),
            (
                b"in-count",
                Element::empty_count_tree(),
                Kind::CountSum,
                |tree| Element::new_non_counted(tree).expect("a tree can be left out of a count"),
            ),
            (
                b"in-count-sum",
                Element::empty_count_sum_tree(),
                Kind::ProvableSum,
                |tree| {
                    Element::new_not_counted_or_summed(tree).expect("a sum tree can be left out")
                },
            ),
        ];
        let mut trees = Vec::new();
        for (name, parent, kind, wrap) in parents {
            self.ops.push(build::insert(&wrapped, name, parent));
            let parent = child(&wrapped, name);
            // What the parent keeps a count or a sum of, beside the tree
            // it leaves out.
            let beside = self.key(b'a');
            let element = if kind == Kind::CountSum {
                Element::new_item(value(&mut self.rng, 1, 24))
            } else {
                Element::new_sum_item(build::sum(&mut self.rng))
            };
            self.ops.push(build::insert(&parent, &beside, element));
            let tree = Held::new(parent, b"left-out", kind, Lean::Small);
            let flags = storage_flags(self.epoch, None);
            self.ops.push(build::insert(
                &tree.parent,
                &tree.key,
                wrap(kind.tree(flags)),
            ));
            for _ in 0..3 {
                let key = self.key(b'a');
                let sum = build::sum(&mut self.rng);
                self.ops.push(tree.put(&key, Element::new_sum_item(sum)));
            }
            trees.push(tree);
        }

        // A tree whose parent sums it is converted after the parent.
        let outer = child(&root, OUTER);
        let inner = child(&outer, INNER);
        self.ops
            .push(build::insert(&root, OUTER, Element::empty_sum_tree()));
        self.ops
            .push(build::insert(&outer, INNER, Element::empty_sum_tree()));
        for into in [&outer, &inner] {
            for _ in 0..3 {
                let key = self.key(b'a');
                let sum = build::sum(&mut self.rng);
                self.ops
                    .push(build::insert(into, &key, Element::new_sum_item(sum)));
            }
        }
        self.ops.push(Op::Commit);
        self.read_special(&trees);

        // The wrapped trees: one in the block, the others each in a
        // transaction of its own.
        self.ops.push(trees[0].convert(false));
        self.ops.push(Op::Commit);
        for tree in &trees[1..] {
            self.ops.push(tree.convert(true));
        }
        self.ops.push(convert_at(&root, OUTER, true));
        for tree in &mut trees {
            tree.converted = true;
        }
        self.read_special(&trees);

        // The tree inside follows its parent, in the block that takes its
        // sum, and with it the parent's, past an `i64`.
        self.ops.push(convert_at(&outer, INNER, false));
        for (index, tree) in trees.iter().enumerate() {
            for _ in 0..2 {
                let key = self.key(b'w');
                let sum = self.wide(index != 1);
                self.ops.push(tree.put(&key, Element::new_sum_item(sum)));
            }
        }
        for _ in 0..2 {
            let key = self.key(b'w');
            let sum = self.wide(true);
            self.ops
                .push(build::insert(&inner, &key, Element::new_sum_item(sum)));
        }
        self.ops.push(Op::Commit);
        self.read_special(&trees);
    }

    /// The wrapped trees with their parents, and the tree with a tree
    /// inside it.
    fn read_special(&mut self, wrapped: &[Held]) {
        let root = self.root.clone();
        let outer = child(&root, OUTER);
        self.ops.push(build::query(build::select_raw(
            &child(&root, WRAPPED),
            vec![op::QueryItem::RangeFull],
            None,
        )));
        for tree in wrapped {
            // The tree as `get` returns it, without its wrapper, and as it
            // is stored.
            self.ops.push(build::get(&tree.parent, &tree.key));
            self.ops.push(Op::Read(build::read::fetch(
                &tree.parent,
                &tree.key,
                Fetch::Raw,
                true,
            )));
            self.ops.push(every_row(tree.parent.clone()));
            self.ops.push(every_row(tree.path()));
        }
        self.ops.push(build::get(&root, OUTER));
        self.ops.push(build::get(&outer, INNER));
        self.ops.push(every_row(outer.clone()));
        let mut inside = Query::new_single_key(INNER.to_vec());
        inside.set_subquery(Query::new_range_full());
        self.ops
            .push(build::proven(PathQuery::new_unsized(outer, inside)));
    }

    fn round(&mut self, round: u32) {
        self.epoch = round as u16 + 1;
        let size = SIZES[round as usize % SIZES.len()];
        let name = self.key(b'r');
        let holder = child(&self.root, &name);
        self.ops
            .push(build::insert(&self.root, &name, Element::empty_tree()));
        let mut pending = Vec::new();
        let mut trees = Vec::new();
        for (index, kind) in Kind::ALL.into_iter().enumerate() {
            let lean = LEANS[self.rng.index(LEANS.len())];
            let batched = (index + round as usize / SIZES.len()) % 2 == 1;
            trees.push(self.build(&holder, kind, size, lean, batched, &mut pending));
        }
        if !pending.is_empty() {
            self.ops.push(Op::Batch(pending));
        }
        self.ops.push(Op::Commit);
        self.reads(round, &holder, &trees);
        if !self.features.convert_to_big_sum_tree {
            return;
        }
        self.convert_all(round, &mut trees);
        self.reads(round, &holder, &trees);
        self.use_converted(round, &mut trees);
        self.reads(round, &holder, &trees);
    }

    /// A tree of `kind` in `holder`, with what its size gives it. A tree of
    /// another type than the two that rank their entries is created and
    /// filled by the batch of the round when `batched` is set.
    fn build(
        &mut self,
        holder: &Path,
        kind: Kind,
        size: Size,
        lean: Lean,
        batched: bool,
        pending: &mut Vec<BatchOp>,
    ) -> Held {
        let indexed = kind.indexed();
        let batched = batched && !indexed;
        let flags = if self.rng.chance(1, 2) {
            storage_flags(self.epoch, None)
        } else {
            None
        };
        let mut tree = Held::new(holder.clone(), kind.key(), kind, lean);
        batch::insert_or_queue(
            batched,
            &mut self.ops,
            pending,
            holder,
            kind.key(),
            kind.tree(flags),
        );
        let inside = tree.path();
        match size {
            Size::Empty => {}
            Size::OneNode => {
                let sum = match lean {
                    Lean::Top => i64::MAX,
                    Lean::Bottom => i64::MIN,
                    Lean::Balanced => {
                        let near = self.near(0);
                        if self.rng.chance(1, 2) {
                            near
                        } else {
                            -near
                        }
                    }
                    Lean::Small => build::sum(&mut self.rng),
                };
                let key = self.key(b'a');
                let amount = self.amount(kind, sum);
                self.child(&tree, batched, pending, &key, amount);
                tree.amounts.push(key);
            }
            Size::Levels => {
                let small_amounts = self.rng.between(7, 11) as usize;
                let mut sums: Vec<i64> = (0..small_amounts)
                    .map(|_| small(&mut self.rng, lean))
                    .collect();
                let in_pot = [small(&mut self.rng, lean), small(&mut self.rng, lean)];
                // What the small amounts could add to either side of the
                // total, the pot at any moment of its filling included.
                let reserved: i64 = sums.iter().chain(&in_pot).map(|sum| sum.abs()).sum();
                let large: Vec<i64> = match lean {
                    Lean::Top => vec![i64::MAX - reserved],
                    Lean::Bottom => vec![i64::MIN + reserved],
                    Lean::Balanced => vec![self.near(reserved), -self.near(reserved)],
                    Lean::Small => Vec::new(),
                };
                for sum in large {
                    let at = self.rng.index(sums.len() + 1);
                    sums.insert(at, sum);
                }
                for (index, sum) in sums.into_iter().enumerate() {
                    let key = self.key(b'a');
                    // The second amount is an item that carries it, where
                    // the tree takes one beside its sum items.
                    let amount = if index == 1 && !indexed {
                        Element::new_item_with_sum_item(value(&mut self.rng, 1, 24), sum)
                    } else {
                        self.amount(kind, sum)
                    };
                    self.child(&tree, batched, pending, &key, amount);
                    tree.amounts.push(key);
                }
                if !indexed {
                    let note = Element::new_item(value(&mut self.rng, 1, 24));
                    self.child(&tree, batched, pending, NOTE, note);
                }
                self.child(&tree, batched, pending, NESTED, kind.pot(false));
                for sum in in_pot {
                    let key = self.key(b'i');
                    batch::insert_or_queue(
                        batched,
                        &mut self.ops,
                        pending,
                        &child(&inside, NESTED),
                        &key,
                        Element::new_sum_item(sum),
                    );
                }
                tree.pot = Some(NESTED.to_vec());
            }
        }
        tree
    }

    /// A child written as the tree is built: in the batch that creates the
    /// tree, or by an operation of its own.
    fn child(
        &mut self,
        tree: &Held,
        batched: bool,
        pending: &mut Vec<BatchOp>,
        key: &[u8],
        element: Element,
    ) {
        if batched {
            pending.push(batch::insert_only(&tree.path(), key, element));
        } else {
            self.ops.push(tree.put(key, element));
        }
    }

    /// Converts every tree of the round: one first made and undone, then
    /// half of them in one block and the others each in a transaction of
    /// its own.
    fn convert_all(&mut self, round: u32, trees: &mut [Held]) {
        let undone = &trees[round as usize % trees.len()];
        let looked_at = build::read::get(&undone.parent, &undone.key);
        let to_savepoint = round % 2 == 1;
        if to_savepoint {
            self.ops.push(build::step(TransactionOp::SetSavepoint));
        }
        self.ops.push(undone.convert(false));
        self.ops.push(build::in_block(looked_at.clone()));
        self.ops.push(build::step(if to_savepoint {
            TransactionOp::RollbackToSavepoint
        } else {
            TransactionOp::Rollback
        }));
        if !to_savepoint {
            self.ops.push(Op::Read(looked_at));
        }

        let in_block = |index: usize| (index + round as usize) % 2 == 0;
        for (index, tree) in trees.iter_mut().enumerate() {
            if !in_block(index) {
                continue;
            }
            // Half of them after a write of the same block to the tree,
            // of the sign its amounts leave room for.
            if index % 4 < 2 {
                let sum = small(&mut self.rng, tree.lean.opposite());
                let key = self.key(b'b');
                let amount = self.amount(tree.kind, sum);
                self.ops.push(tree.put(&key, amount));
                tree.amounts.push(key);
            }
            self.ops.push(tree.convert(false));
            tree.converted = true;
            // And half of them before one near an end of the `i64` range,
            // which a tree at that end takes only once it is converted.
            if index % 4 == 0 {
                let credit = tree.lean != Lean::Bottom;
                let key = self.key(b'w');
                let sum = self.wide(credit);
                let amount = self.amount(tree.kind, sum);
                self.ops.push(tree.put(&key, amount));
                tree.amounts.push(key);
            }
        }
        self.ops.push(Op::Commit);
        for (index, tree) in trees.iter_mut().enumerate() {
            if !in_block(index) {
                self.ops.push(tree.convert(true));
                tree.converted = true;
            }
        }
    }

    /// Goes on using the converted trees of the round.
    fn use_converted(&mut self, round: u32, trees: &mut [Held]) {
        let mut pending = Vec::new();
        let mut later = Vec::new();
        for (index, tree) in trees.iter_mut().enumerate() {
            let inside = tree.path();
            let indexed = tree.kind.indexed();
            // Two amounts past an end of the `i64` range: the end the tree
            // leans to, or either.
            let credit = match tree.lean {
                Lean::Top => true,
                Lean::Bottom => false,
                Lean::Balanced | Lean::Small => self.rng.chance(1, 2),
            };
            let in_batch = self.batches || !indexed;
            // One amount deleted and one written again, by the batch of
            // the block.
            if tree.amounts.len() >= 2 {
                let at = self.rng.index(tree.amounts.len());
                let deleted = tree.amounts.swap_remove(at);
                self.ops.push(tree.remove(&deleted));
                let at = self.rng.index(tree.amounts.len());
                let replaced = tree.amounts[at].clone();
                let sum = build::sum(&mut self.rng);
                let amount = self.amount(tree.kind, sum);
                if in_batch {
                    pending.push(batch::replace(&inside, &replaced, amount));
                } else {
                    self.ops.push(tree.put(&replaced, amount));
                }
            }
            for second in [false, true] {
                let key = self.key(b'w');
                let sum = self.wide(credit);
                let amount = self.amount(tree.kind, sum);
                if second && in_batch {
                    pending.push(batch::insert_or_replace(&inside, &key, amount));
                } else {
                    self.ops.push(tree.put(&key, amount));
                }
                tree.amounts.push(key);
            }
            if indexed {
                // An entry with a 128-bit sum of its own, which only the
                // converted tree ranks.
                let key = self.key(b'p');
                self.ops.push(tree.put(&key, tree.kind.pot(true)));
                for _ in 0..2 {
                    let item = self.key(b'i');
                    let sum = self.wide(credit);
                    self.ops.push(build::insert(
                        &child(&inside, &key),
                        &item,
                        Element::new_sum_item(sum),
                    ));
                }
            }
            // The tree inside, which kept its type: it goes on adding a
            // 64-bit sum, or follows its parent and passes an `i64` too,
            // in the block or in a transaction of its own. Which of the
            // three a type gets changes each time its trees have one.
            if let Some(pot) = tree.pot.clone() {
                let inner = child(&inside, &pot);
                let item = self.key(b'i');
                match (index + round as usize / SIZES.len()) % 3 {
                    0 => {
                        let sum = small(&mut self.rng, tree.lean);
                        self.ops
                            .push(build::insert(&inner, &item, Element::new_sum_item(sum)));
                    }
                    1 => {
                        self.ops.push(convert_at(&inside, &pot, false));
                        let sum = self.wide(credit);
                        self.ops
                            .push(build::insert(&inner, &item, Element::new_sum_item(sum)));
                    }
                    _ => {
                        let sum = self.wide(credit);
                        later.push((inside.clone(), pot, item, sum));
                    }
                }
            }
        }
        if !pending.is_empty() {
            self.ops.push(Op::Batch(pending));
        }
        self.ops.push(Op::Commit);
        if later.is_empty() {
            return;
        }
        for (inside, pot, ..) in &later {
            self.ops.push(convert_at(inside, pot, true));
        }
        for (inside, pot, item, sum) in later {
            self.ops.push(build::insert(
                &child(&inside, &pot),
                &item,
                Element::new_sum_item(sum),
            ));
        }
        self.ops.push(Op::Commit);
    }

    /// The trees of a round, read and proven.
    fn reads(&mut self, round: u32, holder: &Path, trees: &[Held]) {
        // Every row of the tree that holds the seven. An empty indexed
        // tree is among them in some rounds, and a grove version without
        // the later proofs does not prove such a row.
        self.ops.push(if self.features.v1_proofs {
            build::query(build::select_raw(
                holder,
                vec![op::QueryItem::RangeFull],
                None,
            ))
        } else {
            build::rows(
                PathQuery::new_unsized(holder.clone(), Query::new_range_full()),
                true,
            )
        });
        let mut all = Query::new();
        for tree in trees {
            all.insert_key(tree.key.clone());
        }
        self.ops.push(Op::Read(Read::ItemValues {
            query: Box::new(PathQuery::new_unsized(holder.clone(), all)),
            or_sums: true,
        }));
        for tree in trees {
            let inside = tree.path();
            // What these trees hold proves wherever a query hosted by a
            // tree of the type proves at all: no reference is among it.
            // Where the grove version refuses some such queries and not
            // others, by what the tree holds, the rows are read and no
            // proof is asked for.
            let proves = self.features.v1_proofs
                || !matches!(
                    tree.kind,
                    Kind::ProvableCountProvableSum | Kind::SumIndexed | Kind::CountSumIndexed
                );
            let rows = |tree: Path| {
                if proves {
                    every_row(tree)
                } else {
                    build::rows(PathQuery::new_unsized(tree, Query::new_range_full()), true)
                }
            };
            self.ops.push(build::get(holder, &tree.key));
            self.ops.push(rows(inside.clone()));
            if let Some(pot) = &tree.pot {
                self.ops.push(build::get(&inside, pot));
                self.ops.push(rows(child(&inside, pot)));
            }
            if tree.kind.indexed() {
                if self.features.read_modes {
                    self.rankings(round, tree);
                }
            } else {
                self.aggregate(tree);
            }
        }
    }

    /// The sum of a range of keys, with the count where the tree proves
    /// both, or the count alone where it proves no sum.
    fn aggregate(&mut self, tree: &Held) {
        let inside = tree.path();
        // The amounts written before a number and those written after it:
        // a range open above takes in the item and the tree inside too.
        let from = self.rng.below(u64::from(self.next)) as u32;
        let mut bound = vec![b'a'];
        bound.extend_from_slice(&from.to_be_bytes());
        let range = match self.rng.below(4) {
            0 => QueryItem::RangeFrom(bound..),
            1 => QueryItem::RangeTo(..bound),
            2 => QueryItem::RangeToInclusive(..=bound),
            _ => QueryItem::RangeAfter(bound..),
        };
        let proven = self.features.v1_proofs;
        let read = match (tree.kind, tree.converted) {
            (Kind::Sum | Kind::CountSum, _) | (Kind::SumIndexed | Kind::CountSumIndexed, false) => {
                return
            }
            (Kind::ProvableSum, false) => {
                build::path_query_read(PathQuery::new_aggregate_sum_on_range(inside, range), proven)
            }
            (Kind::ProvableCountSum, false) => build::path_query_read(
                PathQuery::new_aggregate_count_on_range(inside, range),
                proven,
            ),
            (Kind::ProvableCountProvableSum, false) => build::path_query_read(
                PathQuery::new_aggregate_count_and_sum_on_range(inside, range),
                proven,
            ),
            (kind, true) => {
                let with_count = kind != Kind::ProvableSum;
                let aggregate = if with_count {
                    Query::new_aggregate_count_and_sum_on_range(range)
                } else {
                    Query::new_aggregate_sum_on_range(range)
                };
                Op::Read(Read::BigSum {
                    query: Box::new(PathQuery::new_unsized(inside, aggregate)),
                    with_count,
                    per_key: false,
                })
            }
        };
        self.ops.push(read);
    }

    /// The rankings of an indexed tree, read and proven: on the axis of
    /// its sums, which a conversion replaces, and for the tree that has
    /// them on the axes of its counts and averages.
    fn rankings(&mut self, round: u32, tree: &Held) {
        let inside = tree.path();
        let sums = if tree.converted {
            IndexAxis::BigSum
        } else {
            IndexAxis::Sum
        };
        let axis = match tree.kind {
            Kind::CountSumIndexed => {
                [IndexAxis::Count, IndexAxis::Avg, sums][round as usize / SIZES.len() % 3]
            }
            _ => sums,
        };
        let page = self.rng.between(2, 6) as u16;
        let descending = self.rng.chance(1, 2);
        self.ops.push(build::proven(PathQuery::new_axis_top_k(
            inside.clone(),
            sums,
            page,
            0,
            descending,
        )));
        if axis != sums {
            self.ops.push(build::proven(PathQuery::new_axis_top_k(
                inside.clone(),
                axis,
                page,
                0,
                !descending,
            )));
        }
        self.ops.push(build::proven(PathQuery::new_axis(
            inside.clone(),
            AxisQuery::bottom_k(sums, page, self.rng.below(2)).keys_only(),
        )));
        // The entries below nothing, and those above: past the `i64` range
        // at either end once the tree is converted.
        let (max, min) = (i128::from(i64::MAX), i128::from(i64::MIN));
        let (low, high) = match (tree.converted, self.rng.below(3)) {
            (true, 0) => (max + 1, i128::MAX),
            (true, 1) => (i128::MIN, min - 1),
            (false, 0) => (0, max),
            (false, 1) => (min, -1),
            _ => (min, max),
        };
        self.ops.push(build::proven(PathQuery::new_axis_bounded(
            inside.clone(),
            sums,
            low,
            high,
            page,
            descending,
        )));
        if !tree.amounts.is_empty() {
            let ranked = tree.amounts[self.rng.index(tree.amounts.len())].clone();
            self.ops.push(build::proven(PathQuery::new_axis_rank_of_key(
                inside.clone(),
                sums,
                ranked,
                descending,
            )));
        }
        self.ops.push(build::proven(
            PathQuery::new_axis_aggregate_over_value_range(
                inside,
                sums,
                low,
                high,
                AggregateFold::Population,
            ),
        ));
    }
}
