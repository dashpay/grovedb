//! Workloads built to stress a storage engine, not to resemble an
//! application.
//!
//! Each one pushes a single dimension to its limit: nesting depth, key
//! length, value size, the number of subtrees, the share of deletes, and the
//! length and kind of reference chains.
//!
//! The submodules do the same for the operations that reach storage in a way
//! of their own: flat drops ([`FlatDrops`]) and moves ([`Moves`]) of every
//! type of tree, batches applied in two segments ([`PartialBatches`]), and
//! every way a query pages, budgets and ranks ([`Pagination`]).

mod capped_layers;
mod chunks;
mod conversions;
mod drops;
mod epochs;
mod indexed;
mod moves;
mod pages;
mod partial;
mod stock;
mod tree_deletes;
mod typed;
mod wide_ranks;
mod wide_sums;

use std::collections::BTreeSet;

pub use self::{
    capped_layers::{CappedLayers, MAX_CAPPED_KEYS, MAX_CAPPED_ROUNDS, MIN_CAPPED_KEYS},
    chunks::{ChunkProofs, MIN_CHUNKED_ROWS},
    conversions::{Conversions, MAX_CONVERSION_ROUNDS},
    drops::FlatDrops,
    epochs::{FlagEpochs, MAX_LADDER_RUNGS},
    indexed::{IndexedWrites, MAX_INDEXED_ROUNDS},
    moves::Moves,
    pages::Pagination,
    partial::PartialBatches,
    tree_deletes::{TreeDeletes, MIN_TREE_DELETE_ITEMS},
    typed::TypedReads,
    wide_ranks::{WideRanks, MAX_WIDE_RANK_ENTRIES, MAX_WIDE_RANK_ROUNDS, MIN_WIDE_RANK_ENTRIES},
    wide_sums::{WideSums, MIN_WIDE_SUM_ENTRIES},
};

use grovedb::{element::reference_path::ReferencePathType, Element, TreeType};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::{
    build::{self, batch, batch::insert_or_queue, child, path, sum, value},
    op::{AppendEntry, BatchOp, Op, Path, Query, QueryItem},
    rng::Rng,
    zoo::TreeKind,
    Features, Prepared, Stream,
};

/// A sum close to the largest an `i64` holds, negative one time in four:
/// two of them that are credits add up to more than an `i64` holds.
fn wide_sum(rng: &mut Rng) -> i64 {
    let sum = near_max(rng);
    if rng.chance(1, 4) {
        -sum
    } else {
        sum
    }
}

/// A sum within 2^40 of the largest an `i64` holds.
fn near_max(rng: &mut Rng) -> i64 {
    i64::MAX - rng.below(1 << 40) as i64
}

/// The longest key GroveDB accepts.
pub const MAX_KEY_LEN: u32 = 255;

/// The deepest [`DeepNesting`] goes. GroveDB proves a query at most 128
/// trees below the root, and every chain hangs under one root tree.
pub const MAX_NESTING_DEPTH: u32 = 127;

/// The most references GroveDB follows from one read.
pub const MAX_REFERENCE_HOPS: u32 = 10;

const DOMAIN_DEEP: u64 = 0xAD01;
const DOMAIN_KEYS: u64 = 0xAD02;
const DOMAIN_VALUES: u64 = 0xAD03;
const DOMAIN_TINY: u64 = 0xAD04;
const DOMAIN_DELETES: u64 = 0xAD05;
const DOMAIN_REFERENCES: u64 = 0xAD06;
const DOMAIN_DROPS: u64 = 0xAD07;
const DOMAIN_MOVES: u64 = 0xAD08;
const DOMAIN_PARTIAL: u64 = 0xAD09;
const DOMAIN_PAGES: u64 = 0xAD0A;
const DOMAIN_INDEXED: u64 = 0xAD0B;
const DOMAIN_TYPED: u64 = 0xAD0C;
const DOMAIN_CHUNKS: u64 = 0xAD0D;
const DOMAIN_EPOCHS: u64 = 0xAD0E;
const DOMAIN_TREE_DELETES: u64 = 0xAD0F;
const DOMAIN_WIDE_SUMS: u64 = 0xAD10;
const DOMAIN_WIDE_RANKS: u64 = 0xAD11;
const DOMAIN_CAPPED_LAYERS: u64 = 0xAD12;
const DOMAIN_CONVERSIONS: u64 = 0xAD13;

/// Chains of trees nested `depth` levels deep.
///
/// Every write at the bottom of a chain rehashes `depth` Merks on its way to
/// the root. Chains are built level by level through direct inserts or all
/// at once in one batch, then written to at the bottom, then taken apart:
/// leaf by leaf, by a recursive delete from the top, or by clearing the
/// middle.
///
/// GroveDB recurses once per level when it queries and proves, so the
/// thread applying this stream needs stack to match. At the default depth
/// an unoptimized build uses about 5 MiB, more than the 2 MiB a spawned
/// thread or a test gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeepNesting {
    /// How many trees are nested in one chain, at most
    /// [`MAX_NESTING_DEPTH`].
    pub depth: u32,
    /// How many chains to build.
    pub chains: u32,
    /// How many items sit at the bottom of each chain.
    pub leaves: u32,
}

impl Default for DeepNesting {
    fn default() -> Self {
        Self {
            depth: 64,
            chains: 4,
            leaves: 4,
        }
    }
}

impl DeepNesting {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.depth > 0 && self.leaves > 0, "nothing to generate");
        assert!(
            self.depth <= MAX_NESTING_DEPTH,
            "GroveDB cannot prove a query {} trees deep",
            self.depth
        );
        let mut rng = Rng::derive(seed, DOMAIN_DEEP, 0);
        let root = path(&[b"deep"]);
        let mut ops = vec![
            build::insert(&[], b"deep", Element::empty_tree()),
            Op::Commit,
        ];
        for chain in 0..self.chains {
            self.chain(chain, &mut rng, &root, &mut ops);
        }
        Prepared::stream(ops)
    }

    /// Builds, writes to and takes apart chain number `chain`.
    fn chain(&self, chain: u32, rng: &mut Rng, root: &[Vec<u8>], ops: &mut Vec<Op>) {
        let batched = chain % 2 == 1;
        let mut levels: Vec<Path> = Vec::new();
        let mut pending = Vec::new();
        let mut parent = root.to_vec();
        for level in 0..self.depth {
            let key = if level == 0 {
                format!("chain-{chain}").into_bytes()
            } else {
                value(rng, 1, 16)
            };
            let tree = match level % 7 {
                3 => Element::empty_sum_tree(),
                5 => Element::empty_count_tree(),
                _ => Element::empty_tree(),
            };
            let sibling = Element::new_item(value(rng, 0, 32));
            insert_or_queue(batched, ops, &mut pending, &parent, &key, tree);
            let here = child(&parent, &key);
            if level > 0 {
                insert_or_queue(batched, ops, &mut pending, &parent, b"\xffside", sibling);
            }
            levels.push(here.clone());
            parent = here;
        }
        let bottom = parent;
        for leaf in 0..self.leaves {
            let element = Element::new_item(value(rng, 1, 64));
            let key = format!("leaf-{leaf}").into_bytes();
            insert_or_queue(batched, ops, &mut pending, &bottom, &key, element);
        }
        if batched {
            ops.push(Op::Batch(pending));
        }
        ops.push(Op::Commit);
        ops.push(build::get(&bottom, b"leaf-0"));
        ops.push(build::query(build::select(
            &bottom,
            vec![QueryItem::RangeFull],
            None,
        )));

        for round in 0..3u32 {
            let leaf = format!("leaf-{}", rng.below(u64::from(self.leaves))).into_bytes();
            let element = Element::new_item(value(rng, 1, 64));
            if (round + chain).is_multiple_of(2) {
                ops.push(build::insert(&bottom, &leaf, element));
            } else {
                ops.push(Op::Batch(vec![batch::replace(&bottom, &leaf, element)]));
            }
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &bottom,
            vec![QueryItem::Key(b"leaf-0".to_vec())],
            None,
        )));

        let middle = &levels[levels.len() / 2];
        match chain % 4 {
            0 => {
                for leaf in 0..self.leaves {
                    ops.push(build::delete(&bottom, format!("leaf-{leaf}").as_bytes()));
                }
                for level in (levels.len() / 2 + 1..levels.len()).rev() {
                    let (key, parent) = levels[level].split_last().expect("a level has a key");
                    if level + 1 < levels.len() {
                        ops.push(build::delete(&levels[level], b"\xffside"));
                    }
                    ops.push(build::delete(parent, key));
                }
                ops.push(Op::Commit);
                ops.push(build::query(build::select_raw(
                    middle,
                    vec![QueryItem::RangeFull],
                    None,
                )));
                let (key, parent) = levels[0].split_last().expect("a level has a key");
                ops.push(build::delete(parent, key));
            }
            1 => {
                let (key, parent) = levels[0].split_last().expect("a level has a key");
                ops.push(Op::Batch(vec![batch::delete_tree(
                    parent,
                    key,
                    TreeType::NormalTree,
                )]));
            }
            2 => {
                ops.push(build::clear(middle));
                ops.push(build::insert(
                    middle,
                    b"refilled",
                    Element::new_item(value(rng, 1, 32)),
                ));
            }
            _ => {}
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select_raw(
            root,
            vec![QueryItem::RangeFull],
            None,
        )));
    }
}

/// Keys and path segments at the 255-byte limit.
///
/// The stream fills one tree with random 255-byte keys, one with keys that
/// share a 254-byte prefix, one with the shortest, longest, lowest and
/// highest keys there are, and nests trees under 255-byte keys so that paths
/// grow long too. It then replaces, deletes and range-reads across them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LongKeys {
    /// How many random 255-byte keys to store.
    pub keys: u32,
    /// How many trees with 255-byte keys to nest inside each other.
    pub nesting: u32,
}

impl Default for LongKeys {
    fn default() -> Self {
        Self {
            keys: 200,
            nesting: 4,
        }
    }
}

impl LongKeys {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut rng = Rng::derive(seed, DOMAIN_KEYS, 0);
        let root = path(&[b"long-keys"]);
        let mut ops = vec![build::insert(&[], b"long-keys", Element::empty_tree())];
        for name in [&b"random"[..], b"prefix", b"edges", b"nested"] {
            ops.push(build::insert(&root, name, Element::empty_tree()));
        }
        ops.push(Op::Commit);

        self.random_keys(&mut rng, &root, &mut ops);
        self.shared_prefix_keys(&mut rng, &root, &mut ops);
        edge_keys(&mut rng, &root, &mut ops);
        self.nested_keys(&mut rng, &root, &mut ops);
        Prepared::stream(ops)
    }

    /// Random 255-byte keys: inserted, range-read, then replaced and deleted.
    fn random_keys(&self, rng: &mut Rng, root: &[Vec<u8>], ops: &mut Vec<Op>) {
        let random = child(root, b"random");
        let mut keys: BTreeSet<Vec<u8>> = BTreeSet::new();
        while (keys.len() as u64) < u64::from(self.keys) {
            keys.insert(rng.bytes(MAX_KEY_LEN));
        }
        let mut order: Vec<Vec<u8>> = keys.iter().cloned().collect();
        rng.shuffle(&mut order);
        for chunk in order.chunks(8) {
            if rng.chance(1, 2) {
                ops.push(Op::Batch(
                    chunk
                        .iter()
                        .map(|key| {
                            batch::insert_only(&random, key, Element::new_item(value(rng, 0, 48)))
                        })
                        .collect(),
                ));
            } else {
                for key in chunk {
                    ops.push(build::insert(
                        &random,
                        key,
                        Element::new_item(value(rng, 0, 48)),
                    ));
                }
            }
        }
        ops.push(Op::Commit);
        let middle = keys.iter().nth(keys.len() / 2);
        if let (Some(first), Some(middle), Some(last)) = (keys.first(), middle, keys.last()) {
            ops.push(build::get(&random, middle));
            ops.push(build::query(build::select(
                &random,
                vec![QueryItem::Range(first.clone(), middle.clone())],
                Some(20),
            )));
            ops.push(build::query(build::select(
                &random,
                vec![QueryItem::RangeAfter(middle.clone())],
                Some(20),
            )));
            ops.push(build::query(Query {
                left_to_right: false,
                ..build::select(
                    &random,
                    vec![QueryItem::RangeInclusive(middle.clone(), last.clone())],
                    Some(20),
                )
            }));
            let mut absent = middle.clone();
            absent[254] ^= 1;
            if !keys.contains(&absent) {
                ops.push(build::query(build::select(
                    &random,
                    vec![QueryItem::Key(absent)],
                    None,
                )));
            }
        }
        for (position, key) in order.iter().enumerate() {
            match position % 5 {
                0 => ops.push(build::delete(&random, key)),
                1 => ops.push(Op::Batch(vec![batch::replace(
                    &random,
                    key,
                    Element::new_item(value(rng, 0, 48)),
                )])),
                2 => ops.push(Op::Batch(vec![batch::delete(&random, key)])),
                _ => {}
            }
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &random,
            vec![QueryItem::RangeFull],
            Some(30),
        )));
    }

    /// Keys sharing a 254-byte prefix.
    fn shared_prefix_keys(&self, rng: &mut Rng, root: &[Vec<u8>], ops: &mut Vec<Op>) {
        let prefix = child(root, b"prefix");
        let shared = rng.bytes(MAX_KEY_LEN - 1);
        let mut tails: Vec<u8> = (0..=255u8).collect();
        rng.shuffle(&mut tails);
        tails.truncate(self.keys.min(256) as usize);
        for &tail in &tails {
            let mut key = shared.clone();
            key.push(tail);
            ops.push(build::insert(
                &prefix,
                &key,
                Element::new_item(value(rng, 0, 16)),
            ));
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &prefix,
            vec![QueryItem::RangeFrom(shared.clone())],
            Some(16),
        )));
        let mut below = shared.clone();
        below.push(0x80);
        ops.push(build::query(build::select(
            &prefix,
            vec![QueryItem::RangeTo(below)],
            None,
        )));
    }

    /// Paths of 255-byte segments, with a reference to their far end.
    fn nested_keys(&self, rng: &mut Rng, root: &[Vec<u8>], ops: &mut Vec<Op>) {
        let mut nested = child(root, b"nested");
        let top = nested.clone();
        let mut segments = Vec::new();
        for _ in 0..self.nesting {
            let key = rng.bytes(MAX_KEY_LEN);
            ops.push(build::insert(&nested, &key, Element::empty_tree()));
            nested.push(key.clone());
            segments.push(key);
        }
        let leaf = rng.bytes(MAX_KEY_LEN);
        ops.push(build::insert(
            &nested,
            &leaf,
            Element::new_item(value(rng, 1, 64)),
        ));
        let mut target = nested.clone();
        target.push(leaf.clone());
        ops.push(build::insert(
            &top,
            b"reference",
            Element::new_reference(ReferencePathType::AbsolutePathReference(target)),
        ));
        ops.push(Op::Commit);
        ops.push(build::get(&nested, &leaf));
        ops.push(build::get(&top, b"reference"));
        ops.push(build::query(build::select(
            &nested,
            vec![QueryItem::Key(leaf)],
            None,
        )));
        if let Some(first) = segments.first() {
            ops.push(build::query(Query {
                subquery_path: Some(segments[1..].to_vec()),
                subquery_all: true,
                ..build::select(&top, vec![QueryItem::Key(first.clone())], None)
            }));
        }
    }
}

/// The shortest, longest, lowest and highest keys there are.
fn edge_keys(rng: &mut Rng, root: &[Vec<u8>], ops: &mut Vec<Op>) {
    let edges = child(root, b"edges");
    let mut edge_keys: Vec<Vec<u8>> = vec![
        vec![0x00],
        vec![0xff],
        vec![0x00; 255],
        vec![0xff; 255],
        vec![0x00; 254],
        vec![0xff; 254],
    ];
    for len in [1u32, 2, 31, 32, 33, 127, 128, 129, 253, 254, 255] {
        edge_keys.push(rng.bytes(len));
    }
    let mut low = vec![0x00; 254];
    low.push(0x01);
    edge_keys.push(low);
    let edge_keys: BTreeSet<Vec<u8>> = edge_keys.into_iter().collect();
    ops.push(Op::Batch(
        edge_keys
            .iter()
            .map(|key| batch::insert_only(&edges, key, Element::new_item(value(rng, 0, 16))))
            .collect(),
    ));
    ops.push(Op::Commit);
    ops.push(build::query(build::select(
        &edges,
        vec![QueryItem::RangeFull],
        None,
    )));
    ops.push(build::get(&edges, &[0xff; 255]));
    ops.push(build::get(&edges, &[0x00]));
}

/// Values from empty to `max_value_size` bytes.
///
/// Sizes sit on and around the powers of two a page-based engine cares
/// about. Values are stored as items, as items with sums and flags, and as
/// entries of append-only trees, then grown, shrunk, replaced at the same
/// size and deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LargeValues {
    /// The largest value in bytes.
    pub max_value_size: u32,
    /// How many values of random size to add to the fixed sizes.
    pub random_values: u32,
}

impl Default for LargeValues {
    fn default() -> Self {
        Self {
            max_value_size: 1 << 20,
            random_values: 12,
        }
    }
}

impl LargeValues {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut rng = Rng::derive(seed, DOMAIN_VALUES, 0);
        let root = path(&[b"large"]);
        let items = child(&root, b"items");
        let sums = child(&root, b"sums");
        let mmr = child(&root, b"mmr");
        let bulk = child(&root, b"bulk");
        let mut ops = vec![
            build::insert(&[], b"large", Element::empty_tree()),
            build::insert(&root, b"items", Element::empty_tree()),
            build::insert(&root, b"sums", Element::empty_sum_tree()),
            build::insert(&root, b"mmr", Element::empty_mmr_tree()),
            build::insert(
                &root,
                b"bulk",
                Element::empty_bulk_append_tree(2).expect("the chunk power is in range"),
            ),
            Op::Commit,
        ];

        let mut sizes: Vec<u32> = [
            0u32, 1, 31, 32, 33, 255, 256, 257, 4_095, 4_096, 4_097, 8_192, 16_383, 16_384, 16_385,
            65_535, 65_536, 65_537, 262_144,
        ]
        .into_iter()
        .filter(|&size| size <= self.max_value_size)
        .collect();
        sizes.push(self.max_value_size);
        for _ in 0..self.random_values {
            sizes.push(rng.between(0, u64::from(self.max_value_size)) as u32);
        }

        let key = |index: usize| format!("value-{index:03}").into_bytes();
        for (index, &size) in sizes.iter().enumerate() {
            let element = Element::new_item(rng.bytes(size));
            if index % 2 == 0 {
                ops.push(build::insert(&items, &key(index), element));
            } else {
                ops.push(Op::Batch(vec![batch::insert_only(
                    &items,
                    &key(index),
                    element,
                )]));
            }
            if index % 4 == 3 {
                ops.push(Op::Commit);
                ops.push(build::get(&items, &key(index)));
                ops.push(build::query(build::select(
                    &items,
                    vec![QueryItem::Key(key(index))],
                    None,
                )));
            }
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &items,
            vec![QueryItem::RangeFull],
            Some(3),
        )));

        for (index, &size) in sizes.iter().enumerate() {
            let flags = Some(rng.bytes((size / 64).min(1_024)));
            let element =
                Element::new_item_with_sum_item_with_flags(rng.bytes(size), sum(&mut rng), flags);
            ops.push(Op::Batch(vec![batch::insert_or_replace(
                &sums,
                &key(index),
                element,
            )]));
            if size <= 65_536 {
                let entry = rng.bytes(size.max(1));
                ops.push(build::append(&mmr, AppendEntry::Mmr(entry)));
                let entry = rng.bytes(size.max(1));
                ops.push(Op::Batch(vec![batch::append(
                    &bulk,
                    AppendEntry::Bulk(entry),
                )]));
            }
        }
        ops.push(Op::Commit);
        ops.push(build::get(&sums, &key(sizes.len() - 1)));

        for (index, &size) in sizes.iter().enumerate() {
            let other = sizes[(index + sizes.len() / 2) % sizes.len()];
            let resized = match index % 3 {
                0 => other,
                1 => size,
                _ => size / 2,
            };
            let element = Element::new_item(rng.bytes(resized));
            if index % 2 == 0 {
                ops.push(Op::Batch(vec![batch::replace(
                    &items,
                    &key(index),
                    element,
                )]));
            } else {
                ops.push(build::insert(&items, &key(index), element));
            }
        }
        ops.push(Op::Commit);
        ops.push(build::get(&items, &key(0)));

        for index in 0..sizes.len() {
            match index % 3 {
                0 => ops.push(build::delete(&items, &key(index))),
                1 => ops.push(Op::Batch(vec![batch::delete(&sums, &key(index))])),
                _ => {}
            }
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &sums,
            vec![QueryItem::RangeFull],
            Some(2),
        )));
        Prepared::stream(ops)
    }
}

/// Thousands of subtrees holding one to three elements each.
///
/// Five in eight are plain trees of items, the shape of an index. Each of
/// the rest is a Merk tree of a type drawn at random from all but the
/// indexed ones. A third of the subtrees are then deleted, and half of those
/// are created again under the same key, the mixed ones with a newly drawn
/// type, so the engine sees a subtree path reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TinySubtrees {
    /// How many subtrees to create.
    pub subtrees: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for TinySubtrees {
    fn default() -> Self {
        Self {
            subtrees: 2_000,
            features: Features::ALL,
        }
    }
}

const TINY_GROUPS: u32 = 8;
const TINY_PLAIN_GROUPS: u32 = 5;

struct Tiny {
    group: u32,
    key: Vec<u8>,
    kind: TreeKind,
    alive: bool,
}

impl TinySubtrees {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut rng = Rng::derive(seed, DOMAIN_TINY, 0);
        let features = self.features;
        let kinds: Vec<TreeKind> = TreeKind::ALL
            .into_iter()
            .filter(|&kind| !kind.indexed() && kind.supported(features))
            .collect();
        let root = path(&[b"tiny"]);
        let group_path = |group: u32| child(&root, format!("group-{group}").as_bytes());
        let mut ops = vec![build::insert(&[], b"tiny", Element::empty_tree())];
        for group in 0..TINY_GROUPS {
            ops.push(build::insert(
                &root,
                format!("group-{group}").as_bytes(),
                tiny_group(group),
            ));
        }
        ops.push(Op::Commit);

        let mut subtrees: Vec<Tiny> = Vec::new();
        let mut in_block = 0u32;
        let mut index = 0u32;
        while index < self.subtrees {
            let burst = (rng.between(1, 4) as u32).min(self.subtrees - index);
            let direct = rng.chance(1, 4);
            let mut pending = Vec::new();
            for _ in 0..burst {
                let group = index % TINY_GROUPS;
                let kind = if group < TINY_PLAIN_GROUPS {
                    TreeKind::Tree
                } else {
                    kinds[rng.index(kinds.len())]
                };
                let mut key = index.to_be_bytes().to_vec();
                key.extend(value(&mut rng, 0, 8));
                let tree = Tiny {
                    group,
                    key,
                    kind,
                    alive: true,
                };
                create_tiny(
                    &mut rng,
                    &group_path(group),
                    &tree,
                    direct,
                    &mut ops,
                    &mut pending,
                );
                subtrees.push(tree);
                index += 1;
            }
            if !pending.is_empty() {
                ops.push(Op::Batch(pending));
            }
            in_block += 1;
            if in_block == 25 || index == self.subtrees {
                in_block = 0;
                ops.push(Op::Commit);
                tiny_reads(&mut rng, &group_path, &subtrees, &mut ops);
            }
        }

        let mut recreate = Vec::new();
        let mut in_block = 0u32;
        for position in 0..subtrees.len() {
            if !rng.chance(1, 3) {
                continue;
            }
            let tree = &mut subtrees[position];
            let parent = group_path(tree.group);
            let tree_type = tree
                .kind
                .element()
                .tree_type()
                .expect("a tiny subtree is a tree");
            let group_type = tiny_group(tree.group)
                .tree_type()
                .expect("a group is a tree");
            let direct = rng.chance(1, 2) && features.deletes_tree_directly(group_type, tree_type);
            if direct {
                ops.push(build::delete(&parent, &tree.key));
            } else {
                ops.push(Op::Batch(vec![batch::delete_tree(
                    &parent, &tree.key, tree_type,
                )]));
            }
            tree.alive = false;
            if rng.chance(1, 2) {
                recreate.push(position);
            }
            in_block += 1;
            if in_block == 40 {
                in_block = 0;
                ops.push(Op::Commit);
                tiny_reads(&mut rng, &group_path, &subtrees, &mut ops);
            }
        }
        ops.push(Op::Commit);

        for (count, position) in recreate.into_iter().enumerate() {
            let tree = &mut subtrees[position];
            if tree.group >= TINY_PLAIN_GROUPS {
                tree.kind = kinds[rng.index(kinds.len())];
            }
            tree.alive = true;
            let direct = rng.chance(1, 2);
            let mut pending = Vec::new();
            create_tiny(
                &mut rng,
                &group_path(tree.group),
                tree,
                direct,
                &mut ops,
                &mut pending,
            );
            if !pending.is_empty() {
                ops.push(Op::Batch(pending));
            }
            if count % 40 == 39 {
                ops.push(Op::Commit);
                tiny_reads(&mut rng, &group_path, &subtrees, &mut ops);
            }
        }
        ops.push(Op::Commit);
        tiny_reads(&mut rng, &group_path, &subtrees, &mut ops);
        Prepared::stream(ops)
    }
}

/// Creates one tiny subtree and its one to three elements.
fn create_tiny(
    rng: &mut Rng,
    parent: &[Vec<u8>],
    tree: &Tiny,
    direct: bool,
    ops: &mut Vec<Op>,
    pending: &mut Vec<BatchOp>,
) {
    let here = child(parent, &tree.key);
    insert_or_queue(
        !direct,
        ops,
        pending,
        parent,
        &tree.key,
        tree.kind.element(),
    );
    for slot in 0..rng.between(1, 3) as u8 {
        let element = if tree.kind.sums() && slot > 0 {
            Element::new_sum_item(sum(rng))
        } else {
            Element::new_item(value(rng, 0, 24))
        };
        insert_or_queue(!direct, ops, pending, &here, &[slot], element);
    }
}

fn tiny_reads(
    rng: &mut Rng,
    group_path: &impl Fn(u32) -> Path,
    subtrees: &[Tiny],
    ops: &mut Vec<Op>,
) {
    let plain = rng.below(u64::from(TINY_PLAIN_GROUPS)) as u32;
    ops.push(build::query(Query {
        subquery_all: true,
        ..build::select(&group_path(plain), vec![QueryItem::RangeFull], Some(30))
    }));
    let any = rng.below(u64::from(TINY_GROUPS)) as u32;
    ops.push(build::query(build::select_raw(
        &group_path(any),
        vec![QueryItem::RangeFull],
        Some(10),
    )));
    if subtrees.is_empty() {
        return;
    }
    let from = rng.index(subtrees.len());
    if let Some(tree) = subtrees[from..].iter().find(|tree| tree.alive) {
        ops.push(build::get(&child(&group_path(tree.group), &tree.key), &[0]));
    }
}

/// The tree every subtree of a group is stored in.
fn tiny_group(group: u32) -> Element {
    match group {
        5 => Element::empty_sum_tree(),
        6 => Element::empty_count_tree(),
        _ => Element::empty_tree(),
    }
}

/// Streams in which deletes outnumber inserts.
///
/// Four trees (plain, sum, count and provable count) are filled and then
/// emptied in ascending, descending and random key order, churned through
/// rounds that delete four keys for every one they put back, cleared, and
/// deleted whole while populated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeleteHeavy {
    /// How many keys each tree is filled with.
    pub keys: u32,
    /// How many churn rounds follow the first emptying.
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for DeleteHeavy {
    fn default() -> Self {
        Self {
            keys: 300,
            rounds: 3,
            features: Features::ALL,
        }
    }
}

impl DeleteHeavy {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.keys > 0, "nothing to generate");
        let mut rng = Rng::derive(seed, DOMAIN_DELETES, 0);
        let root = path(&[b"churn"]);
        let mut ops = vec![build::insert(&[], b"churn", Element::empty_tree())];
        let trees: [(&[u8], Element, bool); 4] = [
            (b"plain", Element::empty_tree(), false),
            (b"sums", Element::empty_sum_tree(), true),
            (b"counts", Element::empty_count_tree(), false),
            (b"provable", Element::empty_provable_count_tree(), false),
        ];
        for (name, element, _) in &trees {
            ops.push(build::insert(&root, name, element.clone()));
        }
        ops.push(Op::Commit);

        for (name, element, sums) in &trees {
            self.churn(&mut rng, &root, name, element, *sums, &mut ops);
        }
        Prepared::stream(ops)
    }

    /// Fills, empties and churns the tree `name`, then deletes it whole.
    fn churn(
        &self,
        rng: &mut Rng,
        root: &[Vec<u8>],
        name: &[u8],
        element: &Element,
        sums: bool,
        ops: &mut Vec<Op>,
    ) {
        let tree = child(root, name);
        let mut keys: BTreeSet<Vec<u8>> = BTreeSet::new();
        while (keys.len() as u64) < u64::from(self.keys) {
            keys.insert(rng.bytes(8));
        }
        let keys: Vec<Vec<u8>> = keys.into_iter().collect();
        let make = |rng: &mut Rng| {
            if sums {
                Element::new_sum_item(sum(rng))
            } else {
                Element::new_item(value(rng, 0, 40))
            }
        };

        let mut order = keys.clone();
        rng.shuffle(&mut order);
        for chunk in order.chunks(10) {
            ops.push(Op::Batch(
                chunk
                    .iter()
                    .map(|key| batch::insert_only(&tree, key, make(rng)))
                    .collect(),
            ));
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &tree,
            vec![QueryItem::RangeFull],
            Some(5),
        )));

        let third = keys.len() / 3;
        for key in &keys[..third] {
            ops.push(build::delete(&tree, key));
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &tree,
            vec![QueryItem::Key(keys[0].clone())],
            None,
        )));
        for chunk in keys[keys.len() - third..].rchunks(5) {
            ops.push(Op::Batch(
                chunk
                    .iter()
                    .rev()
                    .map(|key| batch::delete(&tree, key))
                    .collect(),
            ));
        }
        ops.push(Op::Commit);
        let mut rest: Vec<Vec<u8>> = keys[third..keys.len() - third].to_vec();
        rng.shuffle(&mut rest);
        for key in &rest {
            ops.push(build::delete(&tree, key));
        }
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &tree,
            vec![QueryItem::RangeFull],
            None,
        )));
        ops.push(build::get(root, name));

        let mut live: Vec<Vec<u8>> = Vec::new();
        let mut dead: Vec<Vec<u8>> = keys.clone();
        for _ in 0..self.rounds {
            rng.shuffle(&mut dead);
            let refill = dead.len() / 2;
            for key in dead.drain(..refill) {
                ops.push(build::insert(&tree, &key, make(rng)));
                live.push(key);
            }
            ops.push(Op::Commit);
            let keep = live.len() / 20;
            let mut step = 0u32;
            while live.len() > keep {
                step += 1;
                if step.is_multiple_of(5) && !dead.is_empty() {
                    let key = dead.swap_remove(rng.index(dead.len()));
                    let element = make(rng);
                    ops.push(Op::Batch(vec![if step.is_multiple_of(10) {
                        batch::insert_if_not_exists(&tree, &key, element)
                    } else {
                        batch::insert_or_replace(&tree, &key, element)
                    }]));
                    live.push(key);
                } else {
                    let key = live.swap_remove(rng.index(live.len()));
                    if step.is_multiple_of(2) {
                        ops.push(build::delete(&tree, &key));
                    } else {
                        ops.push(Op::Batch(vec![batch::delete(&tree, &key)]));
                    }
                    dead.push(key);
                }
                if step.is_multiple_of(60) {
                    ops.push(Op::Commit);
                    if let Some(key) = dead.last() {
                        ops.push(build::query(build::select(
                            &tree,
                            vec![QueryItem::Key(key.clone())],
                            None,
                        )));
                    }
                }
            }
            ops.push(Op::Commit);
            ops.push(build::query(build::select(
                &tree,
                vec![QueryItem::RangeFull],
                Some(5),
            )));
        }

        for key in dead.iter().take(50) {
            ops.push(build::insert(&tree, key, make(rng)));
        }
        ops.push(build::clear(&tree));
        ops.push(Op::Commit);
        for key in dead.iter().take(50) {
            ops.push(build::insert(&tree, key, make(rng)));
        }
        ops.push(Op::Commit);
        let tree_type = element.tree_type().expect("the churned element is a tree");
        if self
            .features
            .deletes_tree_directly(TreeType::NormalTree, tree_type)
        {
            ops.push(build::delete(root, name));
        } else {
            ops.push(Op::Batch(vec![batch::delete_tree(root, name, tree_type)]));
        }
        ops.push(build::insert(root, name, element.clone()));
        ops.push(build::insert(&tree, &keys[0], make(rng)));
        ops.push(Op::Commit);
        ops.push(build::query(build::select(
            &tree,
            vec![QueryItem::RangeFull],
            None,
        )));
    }
}

/// Chains of references of every length and every kind.
///
/// Chain `n` is `1 + n % 10` references long, so the chains reach the ten
/// hops GroveDB follows. Each hop uses the next of the seven reference path
/// types and is placed where that type can reach the hop before it. Chains
/// are read from their far end, their item is replaced and the references
/// refreshed, and they are deleted from the far end back. Where the target
/// supports them, chains of bidirectional references follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceChains {
    /// How many chains to build.
    pub chains: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for ReferenceChains {
    fn default() -> Self {
        Self {
            chains: 21,
            features: Features::ALL,
        }
    }
}

#[derive(Clone, Debug)]
struct Hop {
    path: Path,
    key: Vec<u8>,
    reference: ReferencePathType,
    max_hop: Option<u8>,
    sum: Option<i64>,
}

impl Hop {
    fn element(&self) -> Element {
        match self.sum {
            Some(sum) => Element::new_reference_with_sum_item_with_hops(
                self.reference.clone(),
                self.max_hop,
                sum,
            ),
            None => Element::new_reference_with_hops(self.reference.clone(), self.max_hop),
        }
    }
}

fn common_prefix(a: &[Vec<u8>], b: &[Vec<u8>]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}

/// Where hop `hop` of a chain lives and how it points at `target`, for the
/// reference path type numbered `kind`.
fn place_hop(
    base: &[Vec<u8>],
    hop: u32,
    kind: u32,
    target_path: &[Vec<u8>],
    target_key: &[u8],
) -> (Path, Vec<u8>, ReferencePathType) {
    let mut qualified = target_path.to_vec();
    qualified.push(target_key.to_vec());
    let name = |prefix: &str| format!("{prefix}{hop}").into_bytes();
    match kind % 7 {
        0 => (
            child(base, &name("absolute-")),
            b"hop".to_vec(),
            ReferencePathType::AbsolutePathReference(qualified),
        ),
        1 => {
            let here = child(&child(base, &name("from-root-")), b"inner");
            let shared = common_prefix(&here, &qualified);
            (
                here,
                b"hop".to_vec(),
                ReferencePathType::UpstreamRootHeightReference(
                    shared as u8,
                    qualified[shared..].to_vec(),
                ),
            )
        }
        2 => {
            let here = child(&child(base, &name("from-element-")), b"inner");
            let shared = common_prefix(&here, &qualified);
            (
                here.clone(),
                b"hop".to_vec(),
                ReferencePathType::UpstreamFromElementHeightReference(
                    (here.len() - shared) as u8,
                    qualified[shared..].to_vec(),
                ),
            )
        }
        3 => (
            target_path.to_vec(),
            name("sibling-"),
            ReferencePathType::SiblingReference(target_key.to_vec()),
        ),
        4 => {
            let (last, above) = target_path.split_last().expect("a hop is below the root");
            (
                child(above, &name("cousin-")),
                target_key.to_vec(),
                ReferencePathType::CousinReference(last.clone()),
            )
        }
        5 => {
            let split = target_path.len() - 2;
            (
                child(&target_path[..split], &name("removed-cousin-")),
                target_key.to_vec(),
                ReferencePathType::RemovedCousinReference(target_path[split..].to_vec()),
            )
        }
        _ => {
            let here = child(&child(base, &name("addition-")), target_key);
            let shared = common_prefix(&here, target_path);
            (
                here,
                b"hop".to_vec(),
                ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
                    shared as u8,
                    target_path[shared..].to_vec(),
                ),
            )
        }
    }
}

impl ReferenceChains {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut rng = Rng::derive(seed, DOMAIN_REFERENCES, 0);
        let root = path(&[b"references"]);
        let mut ops = vec![
            build::insert(&[], b"references", Element::empty_tree()),
            Op::Commit,
        ];
        let mut trees: BTreeSet<Path> = BTreeSet::new();
        trees.insert(root.clone());

        for chain in 0..self.chains {
            self.chain(chain, &mut rng, &root, &mut trees, &mut ops);
        }

        if self.features.backward_references {
            self.bidirectional(&mut rng, &root, &mut ops);
        }
        Prepared::stream(ops)
    }

    /// Builds, reads, refreshes and, for some, deletes chain number `chain`.
    fn chain(
        &self,
        chain: u32,
        rng: &mut Rng,
        root: &[Vec<u8>],
        trees: &mut BTreeSet<Path>,
        ops: &mut Vec<Op>,
    ) {
        let summed = chain % 3 == 2;
        let batched = chain % 2 == 1;
        let length = 1 + chain % MAX_REFERENCE_HOPS;
        let base = child(root, format!("chain-{chain}").as_bytes());
        let item_path = child(&child(&base, b"origin"), b"inner");
        let item_key = b"item".to_vec();

        let mut hops: Vec<Hop> = Vec::new();
        let (mut target_path, mut target_key) = (item_path.clone(), item_key.clone());
        for hop in 0..length {
            let (hop_path, key, reference) =
                place_hop(&base, hop, chain + hop, &target_path, &target_key);
            let remaining = hop + 1;
            hops.push(Hop {
                path: hop_path.clone(),
                key: key.clone(),
                reference,
                max_hop: (hop % 2 == 0).then_some(remaining as u8),
                sum: (summed && hop % 2 == 1).then(|| sum(rng)),
            });
            (target_path, target_key) = (hop_path, key);
        }

        let mut pending = Vec::new();
        let mut needed: Vec<Path> = vec![item_path.clone()];
        needed.extend(hops.iter().map(|hop| hop.path.clone()));
        for tree in needed {
            for depth in root.len() + 1..=tree.len() {
                let ancestor = tree[..depth].to_vec();
                if trees.insert(ancestor.clone()) {
                    let (key, parent) = ancestor.split_last().expect("below the root");
                    let element = if summed {
                        Element::empty_sum_tree()
                    } else {
                        Element::empty_tree()
                    };
                    insert_or_queue(batched, ops, &mut pending, parent, key, element);
                }
            }
        }
        let item = Element::new_item(value(rng, 1, 80));
        if batched {
            pending.push(batch::insert_only(&item_path, &item_key, item));
            for hop in &hops {
                pending.push(batch::insert_only(&hop.path, &hop.key, hop.element()));
            }
            ops.push(Op::Batch(pending));
        } else {
            ops.push(build::insert(&item_path, &item_key, item));
            for hop in &hops {
                ops.push(build::insert(&hop.path, &hop.key, hop.element()));
            }
        }
        ops.push(Op::Commit);
        let last = hops.last().expect("a chain has a hop");
        ops.push(build::get(&last.path, &last.key));
        ops.push(build::query(build::select(
            &last.path,
            vec![QueryItem::Key(last.key.clone())],
            None,
        )));
        ops.push(build::query(build::select_raw(
            &last.path,
            vec![QueryItem::Key(last.key.clone())],
            None,
        )));

        let item = Element::new_item(value(rng, 1, 80));
        let mut refresh = vec![batch::replace(&item_path, &item_key, item)];
        for (position, hop) in hops.iter().enumerate() {
            if hop.sum.is_some() {
                refresh.push(batch::replace(&hop.path, &hop.key, hop.element()));
            } else {
                refresh.push(BatchOp::RefreshReference {
                    path: hop.path.clone(),
                    key: hop.key.clone(),
                    reference: hop.reference.clone(),
                    max_hop: hop.max_hop,
                    flags: None,
                    non_counted: false,
                    trusted: position % 2 == 0,
                });
            }
        }
        ops.push(Op::Batch(refresh));
        ops.push(Op::Commit);
        ops.push(build::get(&last.path, &last.key));
        ops.push(build::query(build::select(
            &last.path,
            vec![QueryItem::Key(last.key.clone())],
            None,
        )));

        if chain % 4 == 3 {
            for (position, hop) in hops.iter().enumerate().rev() {
                if position % 2 == 0 {
                    ops.push(build::delete(&hop.path, &hop.key));
                } else {
                    ops.push(Op::Batch(vec![batch::delete(&hop.path, &hop.key)]));
                }
            }
            ops.push(Op::Commit);
            ops.push(build::get(&item_path, &item_key));
        }
    }

    /// Chains of bidirectional references, whose targets push their changes
    /// along the chain.
    fn bidirectional(&self, rng: &mut Rng, root: &[Vec<u8>], ops: &mut Vec<Op>) {
        let area = child(root, b"bidirectional");
        ops.push(build::insert(root, b"bidirectional", Element::empty_tree()));
        for chain in 0..self.chains.min(6) {
            let name = format!("chain-{chain}").into_bytes();
            let here = child(&area, &name);
            ops.push(build::insert(&area, &name, Element::empty_tree()));
            ops.push(build::insert(
                &here,
                b"item",
                Element::new_item_allowing_bidirectional_references(value(rng, 1, 60)),
            ));
            let length = 1 + chain % 3;
            let mut target = b"item".to_vec();
            let mut keys = Vec::new();
            for hop in 0..length {
                let key = format!("hop-{hop}").into_bytes();
                let mut qualified = here.clone();
                qualified.push(target.clone());
                let reference = if hop % 2 == 0 {
                    ReferencePathType::SiblingReference(target.clone())
                } else {
                    ReferencePathType::AbsolutePathReference(qualified)
                };
                ops.push(build::insert(
                    &here,
                    &key,
                    Element::new_bidirectional_reference_with_options(
                        reference,
                        None,
                        chain % 2 == 0,
                        None,
                    ),
                ));
                target = key.clone();
                keys.push(key);
            }
            ops.push(Op::Commit);
            let last = keys.last().expect("a chain has a hop");
            ops.push(build::get(&here, last));
            ops.push(build::query(build::select(
                &here,
                vec![QueryItem::Key(last.clone())],
                None,
            )));

            ops.push(build::insert(
                &here,
                b"item",
                Element::new_item_allowing_bidirectional_references(value(rng, 1, 60)),
            ));
            ops.push(Op::Commit);
            ops.push(build::get(&here, last));
            ops.push(build::query(build::select(
                &here,
                vec![QueryItem::Key(last.clone())],
                None,
            )));

            if chain % 2 == 1 {
                for key in keys.iter().rev() {
                    ops.push(build::delete(&here, key));
                }
                ops.push(Op::Commit);
                ops.push(build::get(&here, b"item"));
            }
        }
        ops.push(Op::Commit);
    }
}
