//! Proofs of a Merk on the store (MERK-06): query, absence, aggregate and
//! count-offset proofs, and the trunk and branch proofs. The same calls on
//! a Merk on RocksDB give the same bytes and charge the same, and the walk
//! of a proof reads from the value log the values the proof emits and no
//! other, but for a count-offset proof, which also reads those of the rows
//! of its range that it gives as a digest.

use std::{
    collections::{BTreeSet, LinkedList},
    ffi::OsStr,
    io,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        Arc, Mutex,
    },
    thread::{self, ThreadId},
};

use ed::Encode;
use grovedb_element::{reference_path::ReferencePathType, BackwardReferences, Element};
use grovedb_query::QueryItem;
use grovedb_version::version::GROVE_VERSIONS;
use lignin_io::{DirEntry, OpenMode, StdVfs, Vfs, VfsFile, VfsLock};

use super::{
    cursor::{act, walked, with_both, Act, Both},
    *,
};
use crate::{
    proofs::{encode_into, Decoder, Node, Op as ProofOp},
    store::recording::{failed_call, Tape},
};

/// A proof of a Merk, or a read of it that a Merk answers from the nodes it
/// has in memory, which the walk of a proof leaves there on a store of
/// whole nodes.
#[derive(Clone, Debug)]
pub(super) enum Call {
    /// A query proof: the items, the limit, and whether left to right.
    Query(Vec<QueryItem>, Option<u16>, bool),
    Count(QueryItem),
    Sum(QueryItem),
    BigSum(QueryItem),
    CountAndSum(QueryItem),
    CountAndBigSum(QueryItem),
    /// A count-offset proof: the range, the offset, the limit, whether
    /// left to right, and whether it is the local one.
    CountOffset(QueryItem, u64, Option<u64>, bool, bool),
    /// A trunk proof: the greatest and the least depth of a chunk.
    Trunk(u8, Option<u8>),
    /// A branch proof: the key of its root and its depth.
    Branch(Vec<u8>, u8),
    /// The chunk of a restore under a chunk id (`chunk.rs`).
    Chunk(Vec<u8>),
    /// The chunk of a restore with an index.
    ChunkAt(usize),
    /// The chunks of a restore from a chunk id on, up to a limit of bytes.
    MultiChunk(Vec<u8>, Option<usize>),
    /// The value under a key, from the nodes in memory or from the store.
    Get(Vec<u8>, bool),
    Exists(Vec<u8>),
    ValueHash(Vec<u8>),
    FeatureType(Vec<u8>),
    /// The value and the value hash under a key, by the read that keeps
    /// the nodes it fetches, with a value-defined cost function.
    Observe(Vec<u8>),
    /// The walk of every link, which keeps every node.
    LinkedKeys,
}

/// A cost that a value defines, for the reads and writes that are given a
/// function: a node that is fetched with it is another node in memory
/// than one that is fetched without.
fn defined_cost(_value: &[u8], _: &GroveVersion) -> Option<ValueDefinedCostType> {
    Some(ValueDefinedCostType::SpecializedValueDefinedCost(11))
}

fn encoded<'o>(ops: impl IntoIterator<Item = &'o ProofOp>) -> String {
    let mut bytes = Vec::new();
    encode_into(ops.into_iter(), &mut bytes);
    hex::encode(bytes)
}

/// Makes one call, and returns what it answered: a proof as its bytes.
pub(super) fn answer<'db, S: NodeStore<'db>>(
    merk: &Merk<S>,
    call: &Call,
    grove_version: &GroveVersion,
    cost: &mut OperationCost,
) -> String {
    let ops = |ops: &LinkedList<ProofOp>| encoded(ops);
    // A read says how many seeks it made, so that a test can tell a read
    // of memory from a read of the store.
    let mut read = OperationCost::default();
    let answer = match call {
        Call::Query(items, limit, left_to_right) => {
            let proved =
                merk.prove_unchecked_query_items(items, *limit, *left_to_right, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            format!("{:?}", proved.map(|(proof, limit)| (ops(&proof), limit)))
        }
        Call::Count(range) => {
            let proved = merk.prove_aggregate_count_on_range(range, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            format!("{:?}", proved.map(|(proof, count)| (ops(&proof), count)))
        }
        Call::Sum(range) => {
            let proved = merk.prove_aggregate_sum_on_range(range, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            format!("{:?}", proved.map(|(proof, sum)| (ops(&proof), sum)))
        }
        Call::BigSum(range) => {
            let proved = merk.prove_aggregate_big_sum_on_range(range, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            format!("{:?}", proved.map(|(proof, sum)| (ops(&proof), sum)))
        }
        Call::CountAndSum(range) => {
            let proved = merk.prove_aggregate_count_and_sum_on_range(range, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            format!("{:?}", proved.map(|(proof, n, sum)| (ops(&proof), n, sum)))
        }
        Call::CountAndBigSum(range) => {
            let proved = merk.prove_aggregate_count_and_big_sum_on_range(range, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            format!("{:?}", proved.map(|(proof, n, sum)| (ops(&proof), n, sum)))
        }
        Call::CountOffset(range, offset, limit, left_to_right, local) => {
            let proved = match local {
                false => merk.prove_count_offset_on_range(
                    range,
                    *offset,
                    *limit,
                    *left_to_right,
                    grove_version,
                ),
                true => merk.prove_local_count_offset_on_range(
                    range,
                    *offset,
                    *limit,
                    *left_to_right,
                    grove_version,
                ),
            };
            let proved = proved.unwrap_add_cost(cost);
            let proved = proved.map(|page| (ops(&page.ops), page.returned, page.offset_remaining));
            format!("{proved:?}")
        }
        Call::Trunk(max_depth, min_depth) => {
            let proved = merk.trunk_query(*max_depth, *min_depth, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            let proved =
                proved.map(|trunk| (encoded(&trunk.proof), trunk.chunk_depths, trunk.tree_depth));
            format!("{proved:?}")
        }
        Call::Branch(key, depth) => {
            let proved = merk.branch_query(key, *depth, grove_version);
            let proved = proved.unwrap_add_cost(cost);
            let proved = proved.map(|branch| {
                let hash = hex::encode(branch.branch_root_hash);
                (encoded(&branch.proof), branch.returned_depth, hash)
            });
            format!("{proved:?}")
        }
        Call::Chunk(chunk_id) => {
            let chunk = merk.chunks().and_then(|mut producer| {
                let (chunk, next) = producer.chunk(chunk_id, grove_version)?;
                Ok((encoded(&chunk), next))
            });
            format!("{chunk:?}")
        }
        Call::ChunkAt(index) => {
            let chunk = merk.chunks().and_then(|mut producer| {
                let (chunk, next) = producer.chunk_with_index(*index, grove_version)?;
                Ok((encoded(&chunk), next))
            });
            format!("{chunk:?}")
        }
        Call::MultiChunk(chunk_id, limit) => {
            let chunks = merk.chunks().and_then(|mut producer| {
                let multi = producer.multi_chunk_with_limit(chunk_id, *limit, grove_version)?;
                let ops = multi
                    .chunk
                    .iter()
                    .flat_map(|op| op.encode().expect("an op"));
                let bytes = hex::encode(ops.collect::<Vec<u8>>());
                Ok((bytes, multi.next_index, multi.remaining_limit))
            });
            format!("{chunks:?}")
        }
        Call::Get(key, from_memory) => {
            let got = merk.get(key, *from_memory, NO_VALUE_DEFINED_COST, grove_version);
            format!("{:?}", got.unwrap_add_cost(&mut read))
        }
        Call::Exists(key) => {
            let exists = merk.exists_by_traversing_tree(key, NO_VALUE_DEFINED_COST, grove_version);
            format!("{:?}", exists.unwrap_add_cost(&mut read))
        }
        Call::ValueHash(key) => {
            let hash = merk.get_value_hash(key, true, NO_VALUE_DEFINED_COST, grove_version);
            format!("{:?}", hash.unwrap_add_cost(&mut read))
        }
        Call::FeatureType(key) => {
            let feature = merk.get_feature_type(key, true, NO_VALUE_DEFINED_COST, grove_version);
            format!("{:?}", feature.unwrap_add_cost(&mut read))
        }
        Call::Observe(key) => {
            let observed =
                merk.observe_value_and_value_hash(key, Some(&defined_cost), grove_version);
            format!("{:?}", observed.unwrap_add_cost(&mut read))
        }
        Call::LinkedKeys => {
            let keys = merk.linked_keys(Some(&defined_cost), grove_version);
            format!("{:?}", keys.unwrap_add_cost(&mut read))
        }
    };
    let seeks = read.seek_count;
    *cost += read;
    match kind(call) {
        Some(_) => answer,
        None => format!("{answer}, in {seeks} seeks"),
    }
}

/// The key of the `n`th of the nodes that hold an element of GroveDB.
fn element_key(n: u8) -> Vec<u8> {
    vec![0xe0, n]
}

/// Nodes whose values are elements of GroveDB of the kinds a proof tells
/// apart, a value that is no element, and an empty one. Two of them are
/// longer than a cell holds in line.
pub(super) fn elements(tree_type: TreeType) -> Vec<(Vec<u8>, Op)> {
    let serialized = |element: Element| element.serialize(version()).expect("an element");
    let reference = ReferencePathType::AbsolutePathReference(vec![vec![1], vec![2, 3]]);
    let referred = BackwardReferences::with_max_incoming(3);
    let values = [
        serialized(Element::new_item(vec![5; 20])),
        serialized(Element::new_item(vec![6; 700])),
        serialized(Element::new_sum_item(-77)),
        serialized(Element::new_item_with_sum_item(vec![7; 9], 12)),
        serialized(Element::empty_tree()),
        serialized(Element::empty_sum_tree()),
        serialized(Element::empty_provable_count_tree()),
        serialized(Element::ItemWithBackwardsReferences(
            vec![8; 300],
            referred,
            None,
        )),
        vec![0xf7; 40],
        Vec::new(),
    ];
    let mut nodes: Vec<(Vec<u8>, Op)> = (values.into_iter().enumerate())
        .map(|(n, value)| {
            let feature = feature(tree_type, 50 + n as u64);
            (element_key(n as u8), Op::Put(value, feature))
        })
        .collect();
    nodes.push((
        element_key(nodes.len() as u8),
        Op::PutCombinedReference(
            serialized(Element::new_reference(reference)),
            [9; 32],
            feature(tree_type, 70),
        ),
    ));
    nodes
}

/// The proofs of one grove version over a Merk that holds `keys` and the
/// nodes of [`elements`]: every kind of proof, with each key a query can
/// name (one that is there, one that is not, between two keys and beyond
/// either end), ranges with and without a limit in both directions, and
/// the reads that a Merk answers from memory after them.
fn calls(keys: &[Vec<u8>], round: usize) -> Vec<Call> {
    let at = |n: usize| keys[(n + 7 * round) % keys.len()].clone();
    let (first, last) = (keys[0].clone(), keys[keys.len() - 1].clone());
    let between = |n: usize| {
        let mut key = at(n);
        key.push(0);
        key
    };
    let key = |key: Vec<u8>| QueryItem::Key(key);
    let (low, high) = (at(3).min(at(40)), at(3).max(at(40)));
    let range = || QueryItem::RangeInclusive(low.clone()..=high.clone());
    let all = || QueryItem::RangeFull(..);
    let mut calls = vec![
        Call::Query(vec![key(at(0))], None, true),
        // The same proof again: its nodes were fetched once.
        Call::Query(vec![key(at(0))], None, true),
        Call::Get(at(0), true),
        Call::Exists(at(0)),
        Call::Query(vec![key(between(1))], None, true),
        Call::Get(between(1), true),
        Call::Exists(between(1)),
        Call::ValueHash(at(1)),
        Call::Query(vec![key(vec![])], None, true),
        Call::Query(vec![key(vec![0xff; 9])], None, false),
        Call::Query(vec![key(first.clone()), key(last.clone())], None, true),
        Call::Query(vec![range()], None, true),
        Call::Query(vec![range()], Some(1), true),
        Call::Query(vec![range()], Some(3), false),
        Call::Query(
            vec![QueryItem::Range(low.clone()..high.clone())],
            Some(2),
            true,
        ),
        Call::Query(vec![QueryItem::RangeFrom(high.clone()..)], None, false),
        Call::Query(vec![QueryItem::RangeTo(..low.clone())], Some(4), true),
        Call::Query(
            vec![QueryItem::RangeAfterTo(low.clone()..high.clone())],
            None,
            true,
        ),
        Call::Query(vec![all()], Some(5), false),
        Call::Get(at(9), true),
        Call::Get(at(9), false),
        Call::Get(vec![0xff; 9], true),
        Call::Exists(at(11)),
        Call::FeatureType(at(12)),
        Call::Observe(at(13)),
        Call::Observe(between(13)),
        Call::Query(vec![key(at(13))], None, true),
    ];
    let element = |n: u8| key(element_key(n));
    calls.extend((0..11).map(|n| Call::Query(vec![element(n)], None, n % 2 == 0)));
    calls.push(Call::Query((0..11).map(element).collect(), None, true));
    calls.push(Call::Get(element_key(1), true));
    for aggregate in [range(), all(), QueryItem::RangeTo(..at(5))] {
        calls.extend([
            Call::Count(aggregate.clone()),
            Call::Sum(aggregate.clone()),
            Call::BigSum(aggregate.clone()),
            Call::CountAndSum(aggregate.clone()),
            Call::CountAndBigSum(aggregate),
        ]);
    }
    // The nodes of `elements` are left out of the count-offset proofs: a
    // row that is a tree with a count of its own is refused.
    let rows = || QueryItem::RangeInclusive(first.clone()..=at(usize::MAX / 2));
    for local in [false, true] {
        calls.extend([
            Call::CountOffset(rows(), 0, Some(2), true, local),
            Call::CountOffset(rows(), 3, Some(3), false, local),
            Call::CountOffset(range(), 2, None, true, local),
            Call::CountOffset(all(), 1, Some(1), true, local),
        ]);
    }
    calls.extend([
        Call::Trunk(3, None),
        Call::Trunk(2, Some(2)),
        Call::Branch(at(20), 2),
        Call::Branch(at(20), 2),
        Call::Branch(between(20), 1),
        Call::Branch(element_key(1), 3),
        Call::Get(at(20), true),
        Call::Exists(at(21)),
        // Every node goes into the tree, and what follows reads memory.
        Call::LinkedKeys,
        Call::Get(at(22), true),
        Call::Query(vec![range()], Some(3), true),
    ]);
    calls
}

/// The keys of the Merk both engines hold, without the nodes of
/// [`elements`], in key order.
fn plain_keys(both: &Both<'_, '_, '_>) -> Vec<Vec<u8>> {
    let store = LigninNodeStore::deferred(both.writer, ROOT, None);
    let keys = walked(&store).into_iter().map(|(key, _)| key);
    keys.filter(|key| key.first() != Some(&0xe0)).collect()
}

/// The kind of proof `call` makes, or `None` for a read.
fn kind(call: &Call) -> Option<&'static str> {
    Some(match call {
        Call::Query(..) => "a query proof",
        Call::Count(_) => "a count",
        Call::Sum(_) => "a sum",
        Call::BigSum(_) => "a big sum",
        Call::CountAndSum(_) => "a count and a sum",
        Call::CountAndBigSum(_) => "a count and a big sum",
        Call::CountOffset(.., false) => "a count-offset proof",
        Call::CountOffset(.., true) => "a local count-offset proof",
        Call::Trunk(..) => "a trunk",
        Call::Branch(..) => "a branch",
        Call::Chunk(_) | Call::ChunkAt(_) => "a chunk",
        Call::MultiChunk(..) => "chunks",
        Call::Get(..)
        | Call::Exists(_)
        | Call::ValueHash(_)
        | Call::FeatureType(_)
        | Call::Observe(_)
        | Call::LinkedKeys => return None,
    })
}

#[test]
fn a_proof_is_the_bytes_rocksdb_gives_and_costs_what_it_costs_there() {
    let (mut proved, mut proofs) = (BTreeSet::new(), 0);
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        let before = [Act::Apply(elements(tree_type))];
        with_both(tree_type, 0x0600_f000 + seed as u64, 5, &before, |both| {
            let keys = plain_keys(both);
            assert!(keys.len() > 40, "{tree_type:?}: {} keys", keys.len());
            for (round, grove_version) in GROVE_VERSIONS.iter().enumerate() {
                let calls = calls(&keys, round);
                let acts: Vec<Act> = (calls.iter().cloned())
                    .map(|call| Act::Prove(grove_version, call))
                    .collect();
                // One Merk makes all the calls, so each is answered from
                // what the ones before it left in memory. Then another
                // does, on a handle that writes at once.
                for immediate in [false, true] {
                    let answers = both.run(immediate, &acts);
                    // The first answer is that of the open.
                    for (call, answer) in calls.iter().zip(&answers[1..]) {
                        if let Some(kind) = kind(call).filter(|_| answer.starts_with("Ok((")) {
                            proved.insert(kind);
                            proofs += 1;
                        }
                    }
                }
            }
        });
    }
    // Every kind of proof was made, and not only refused alike on both
    // engines, as a proof is that the tree type or the grove version has
    // not.
    let kinds = [
        "a query proof",
        "a count",
        "a sum",
        "a big sum",
        "a count and a sum",
        "a count and a big sum",
        "a count-offset proof",
        "a local count-offset proof",
        "a trunk",
        "a branch",
    ];
    assert_eq!(proved, BTreeSet::from(kinds));
    assert!(proofs > 4_000, "{proofs} proofs");
}

/// Proofs, and then a write of the Merk that made them, whose walk goes
/// over nodes the proofs were charged for: the node of a proof is
/// rewritten, the one beside it is removed, and a node is made far from
/// both. After each write come the reads and the proofs that a Merk
/// answers from what it has in memory.
fn writes_after_proofs(
    tree_type: TreeType,
    keys: &[Vec<u8>],
    near: usize,
    far: usize,
    made: Vec<u8>,
) -> [Vec<Act>; 3] {
    let prove = |call: Call| Act::Prove(version(), call);
    let point = |n: usize| Call::Query(vec![QueryItem::Key(keys[n].clone())], None, true);
    let all = |left_to_right| Call::Query(vec![QueryItem::RangeFull(..)], Some(9), left_to_right);
    let rewrite = Op::Put(vec![3; 40 + near], feature(tree_type, 3));
    let make = Op::Put(vec![1; 300], feature(tree_type, 4));
    [
        vec![
            prove(point(near)),
            // The read keeps the nodes on the way in the tree, as the
            // proof fetched them: with no cost that their values define.
            prove(Call::Observe(keys[near].clone())),
            Act::Apply(vec![(keys[near].clone(), rewrite)]),
            prove(Call::Get(keys[near].clone(), true)),
            prove(point(near)),
            prove(point(far)),
        ],
        vec![
            prove(point(near + 1)),
            prove(Call::Trunk(4, None)),
            Act::Apply(vec![(keys[near + 1].clone(), Op::Delete)]),
            prove(point(near + 1)),
            prove(Call::Exists(keys[near + 1].clone())),
            prove(Call::Get(keys[far].clone(), true)),
        ],
        vec![
            prove(all(true)),
            prove(point(far)),
            Act::Apply(vec![(made.clone(), make)]),
            prove(point(near)),
            prove(Call::Exists(made)),
            prove(all(false)),
        ],
    ]
}

#[test]
fn a_write_after_a_proof_costs_what_it_costs_on_rocksdb() {
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        with_both(tree_type, 0x0600_a000 + seed as u64, 4, &[], |both| {
            let keys = plain_keys(both);
            let (near, far) = (keys.len() / 3, keys.len() - 2);
            // A batch holds one change set of a Merk, so on a deferred
            // handle each write has a Merk and a batch of its own.
            for calls in writes_after_proofs(tree_type, &keys, near, far, vec![0xd0; 5]) {
                both.deferred(&calls);
            }
            // On a handle that writes at once, one Merk that stays open
            // makes them all, on other nodes.
            let calls = writes_after_proofs(tree_type, &keys, near + 5, far - 3, vec![0xd1; 4]);
            both.immediate(&calls.concat());

            // A write that is refused and a clear, each after a proof: the
            // Merk that is cleared forgets the nodes of its proofs with its
            // tree, and is written and proven again.
            let prove = |call: Call| Act::Prove(version(), call);
            let point = |key: &Vec<u8>| Call::Query(vec![QueryItem::Key(key.clone())], None, true);
            let unsorted = vec![
                (keys[far].clone(), Op::Delete),
                (keys[near].clone(), Op::Delete),
            ];
            let after = [
                prove(point(&keys[far])),
                Act::Apply(unsorted),
                prove(Call::Get(keys[far].clone(), true)),
                prove(point(&keys[near])),
                Act::Clear,
                prove(point(&keys[far])),
                prove(Call::Get(keys[far].clone(), true)),
            ];
            let answers = both.immediate(&after);
            assert!(answers[2].starts_with("Err("), "{}", answers[2]);
            assert!(answers[6].starts_with("Err("), "{}", answers[6]);
            assert_eq!(answers[7], "Ok(None), in 0 seeks");
            let again: Vec<(Vec<u8>, Op)> = (0..40u8)
                .map(|n| (vec![0xc0, n], Op::Put(vec![n; 300], feature(tree_type, 6))))
                .collect();
            let written = [
                Act::Apply(again),
                prove(point(&vec![0xc0, 7])),
                prove(Call::Get(vec![0xc0, 7], true)),
                prove(Call::Query(vec![QueryItem::RangeFull(..)], Some(5), false)),
            ];
            both.immediate(&written);
            // The same on a deferred handle, whose batch takes the clear.
            let after = [
                prove(point(&vec![0xc0, 9])),
                prove(Call::Get(vec![0xc0, 9], true)),
                Act::Clear,
                prove(point(&vec![0xc0, 9])),
                prove(Call::Exists(vec![0xc0, 9])),
            ];
            both.deferred(&after);
        });
    }
}

/// A node two levels below the root of the Merk on `store`, after the
/// nodes on the way to it from the root, and the key of the node beside
/// it: down the taller side, where a node that high has two children.
fn way_down(store: &LigninNodeStore<'_>) -> (Vec<TreeNode>, Vec<u8>) {
    let node = |key: &[u8]| {
        let node = store.get_node(key, NO_VALUE_DEFINED_COST, version());
        node.unwrap().unwrap().expect("a node that is linked")
    };
    let root_key = store.root_key().unwrap().unwrap().expect("a root");
    let mut way = vec![node(&root_key)];
    let mut sibling = None;
    while way.len() < 3 {
        let (below, beside) = {
            let at = way.last().expect("a node");
            let height = |left| at.link(left).map_or(0, Link::height);
            let left = height(true) >= height(false);
            let below = at.link(left).expect("a child on the taller side");
            (node(below.key()), at.link(!left).map(Link::key))
        };
        sibling = beside.map(<[u8]>::to_vec);
        way.push(below);
    }
    (way, sibling.expect("a node beside the target"))
}

/// The three ends of a read of the nodes in memory that comes to nodes a
/// proof was charged for and did not keep: at the node of the key, at a
/// side with no child, and at a child that is not in memory. The first
/// two are free, as they are on a Merk that kept the nodes, and the third
/// is one read by key.
#[test]
fn a_read_after_a_proof_walks_the_nodes_of_the_proof_as_if_they_were_kept() {
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        with_both(tree_type, 0x0600_b000 + seed as u64, 4, &[], |both| {
            let (way, sibling) = way_down(&LigninNodeStore::deferred(both.writer, ROOT, None));
            let target = way[2].key().to_vec();
            // A key that no Merk of these tests holds: the proof that it
            // is not there walks to where it would be.
            let absent = [target.as_slice(), &[0xff; 3]].concat();

            let prove = |call: Call| Act::Prove(version(), call);
            let point = |key: &Vec<u8>| Call::Query(vec![QueryItem::Key(key.clone())], None, true);
            let calls = [
                prove(point(&target)),
                prove(Call::Get(target.clone(), true)),
                prove(Call::Exists(target.clone())),
                prove(Call::ValueHash(target.clone())),
                prove(Call::Get(sibling.clone(), true)),
                prove(Call::Exists(sibling.clone())),
                prove(point(&absent)),
                prove(Call::Get(absent.clone(), true)),
                prove(Call::Exists(absent.clone())),
                // With no use of the nodes in memory, a read is one of
                // the store.
                prove(Call::Get(target.clone(), false)),
            ];
            // The first answer is that of the open.
            let answers = both.deferred(&calls);
            let seeks = |n: usize| answers[n].rsplit_once(", in ").expect("a read").1;
            // At the node of the key.
            assert_eq!([seeks(2), seeks(3), seeks(4)], ["0 seeks"; 3]);
            assert!(answers[2].starts_with("Ok(Some(") && answers[3].starts_with("Ok(true)"));
            assert!(answers[4].starts_with("Ok(Some("));
            // At a child that is not in memory: the proof gave the hash
            // of the node beside its target, and did not fetch it.
            assert_eq!([seeks(5), seeks(6)], ["1 seeks"; 2]);
            assert!(answers[5].starts_with("Ok(Some(") && answers[6].starts_with("Ok(true)"));
            // At a side with no child.
            assert_eq!([seeks(8), seeks(9)], ["0 seeks"; 2]);
            assert!(answers[8].starts_with("Ok(None)") && answers[9].starts_with("Ok(false)"));
            assert_eq!(seeks(10), "1 seeks");
            // A handle that writes at once reads for nothing, on both
            // engines.
            both.immediate(&calls);
        });
    }
}

/// What a read says that [`Refusing`] refused.
const REFUSED: &str = "the read of the node was refused";

/// The reads of a node by its key that [`Refusing`] refuses: every one, or
/// only one that asks for the node's value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reads {
    Every,
    OfTheValue,
}

/// A transaction whose read of a node by its key (`Access::node`) refuses
/// the node under one key for as long as it is told to, and counts the
/// reads it refused. Every other call is passed on.
struct Refusing<'a> {
    under: &'a dyn Access,
    refuses: Mutex<Option<(Vec<u8>, Reads)>>,
    refused: AtomicU64,
}

impl<'a> Refusing<'a> {
    fn over(under: &'a dyn Access) -> Self {
        Refusing {
            under,
            refuses: Mutex::new(None),
            refused: AtomicU64::new(0),
        }
    }

    /// From here on refuses the reads `refuses` names, of the node under a
    /// key, or none.
    fn refuse(&self, refuses: Option<(&[u8], Reads)>) {
        let refuses = refuses.map(|(key, reads)| (key.to_vec(), reads));
        *self.refuses.lock().expect("a lock that no thread left") = refuses;
    }

    /// How many reads were refused since this was last asked.
    fn refused(&self) -> u64 {
        self.refused.swap(0, Relaxed)
    }
}

impl Access for Refusing<'_> {
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error> {
        self.under.cursor(subtree)
    }

    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error> {
        let refuses = self.refuses.lock().expect("a lock that no thread left");
        let refused = refuses.as_ref().is_some_and(|(refused, reads)| {
            refused.as_slice() == key && (with_value || *reads == Reads::Every)
        });
        drop(refuses);
        if refused {
            self.refused.fetch_add(1, Relaxed);
            return Err(Error::StorageError(grovedb_storage::Error::StorageError(
                REFUSED.to_owned(),
            )));
        }
        self.under.node(subtree, key, hint, with_value)
    }

    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        self.under.committed_node(subtree, key)
    }

    fn child(&self, subtree: &Prefix, parent: Location, left: bool) -> Result<Child, Error> {
        self.under.child(subtree, parent, left)
    }

    fn record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.under.record(keyspace, subtree, key)
    }

    fn committed_record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.under.committed_record(keyspace, subtree, key)
    }

    fn subtree(&self, subtree: &Prefix) -> Result<Option<LigninEntry>, Error> {
        self.under.subtree(subtree)
    }

    fn apply(&self, write: &Write) -> Result<Written, Error> {
        self.under.apply(write)
    }

    fn apply_parts(
        &self,
        parts: &[&dyn BatchPart],
        enters: &[(usize, SubtreeChange)],
        entry: &mut Option<LigninEntryRefused>,
    ) -> Result<Vec<Written>, Error> {
        self.under.apply_parts(parts, enters, entry)
    }

    fn name_hosted(&self, hosts: &[LigninHost]) -> Result<(), Error> {
        self.under.name_hosted(hosts)
    }
}

impl FlatAccess for Refusing<'_> {
    fn records<'a>(
        &'a self,
        keyspace: Keyspace,
        subtree: &Prefix,
    ) -> Result<Box<dyn FlatWalk + 'a>, Error> {
        self.under.records(keyspace, subtree)
    }
}

/// A call as the message of an assertion names it.
fn named(call: &Act) -> String {
    match call {
        Act::Prove(_, call) => format!("{call:?}"),
        other => format!("{other:?}"),
    }
}

/// A Merk makes the calls of `proved`, and then `call` while the store
/// refuses the reads `refused` names. What that call answered, what it was
/// charged and how many reads were refused are returned.
///
/// Then the store answers again, and the Merk makes `call` once more and
/// the calls of `then`. Each succeeds, and answers and costs what it does
/// on a Merk that made `proved` and met no refusal, and the two Merks end
/// with the same root hash.
fn refused_then_answered(
    both: &Both<'_, '_, '_>,
    proved: &[Act],
    call: &Act,
    refused: (&[u8], Reads),
    then: &[Act],
) -> (String, OperationCost, u64) {
    let tree_type = both.tree_type;
    let case = format!("{tree_type:?}, {refused:?} refused at {}", named(call));
    let refusing = Refusing::over(both.writer);
    let staged = LigninBatch::new();
    let store = LigninNodeStore::new(&refusing, ROOT, Mode::Deferred(Some(&staged)));
    let mut merk = open(store, tree_type);
    let unrefused = LigninBatch::new();
    let store = LigninNodeStore::deferred(both.writer, ROOT, Some(&unrefused));
    let mut expected = open(store, tree_type);

    for call in proved {
        let found = act(&mut merk, call);
        assert_eq!(found, act(&mut expected, call), "{case}: before it");
    }
    refusing.refuse(Some(refused));
    let (failed, cost) = act(&mut merk, call);
    let refusals = refusing.refused();
    refusing.refuse(None);
    for call in std::iter::once(call).chain(then) {
        let found = act(&mut merk, call);
        assert!(found.0.starts_with("Ok("), "{case}: {}", named(call));
        let wanted = act(&mut expected, call);
        assert_eq!(found, wanted, "{case}: {} afterwards", named(call));
    }
    let root_hash = merk.root_hash().unwrap();
    assert_eq!(root_hash, expected.root_hash().unwrap(), "{case}");
    (failed, cost, refusals)
}

/// A Merk reads again, at no charge, a node that a proof was charged for
/// and did not keep. Where the store refuses that read, the call ends with
/// the store's error and is charged nothing. Once the store answers, the
/// same call succeeds, and it and the reads and the write that follow it
/// answer and cost what they do on a Merk that met no refusal.
///
/// A proof whose read is refused for a node the Merk was not charged for
/// is charged what the store charges for that read.
#[test]
fn a_reread_that_the_store_refuses_fails_the_call_at_no_charge() {
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        with_both(tree_type, 0x0600_c000 + seed as u64, 4, &[], |both| {
            let (way, sibling) = way_down(&LigninNodeStore::deferred(both.writer, ROOT, None));
            let (above, target) = (way[1].key().to_vec(), way[2].key().to_vec());
            let absent = [target.as_slice(), &[0xff; 3]].concat();
            let prove = |call: Call| Act::Prove(version(), call);
            let point =
                |key: &Vec<u8>| prove(Call::Query(vec![QueryItem::Key(key.clone())], None, true));
            // The proof of the target is charged for the node above it and
            // for the target, and keeps neither.
            let proved = [point(&target)];
            // What follows the refused call: reads of the two nodes and
            // of what is beside them, a write of the target, whose walk
            // goes through the node above it, and reads of what the write
            // left in memory.
            let rewrite = Op::Put(vec![7; 60], feature(tree_type, 3));
            let then = [
                prove(Call::Get(target.clone(), true)),
                prove(Call::Exists(target.clone())),
                point(&target),
                prove(Call::Get(sibling.clone(), true)),
                point(&absent),
                prove(Call::Get(above.clone(), true)),
                Act::Apply(vec![(target.clone(), rewrite)]),
                prove(Call::Get(target.clone(), true)),
                point(&target),
                prove(Call::Observe(above.clone())),
            ];
            let refused_as = |failed: &str| {
                failed.starts_with("Err(StorageError(StorageError(") && failed.contains(REFUSED)
            };

            // Four callers that read such a node again. A proof reads the
            // nodes on its way without their values, and then the value
            // of the node of its key. A read of the nodes in memory reads
            // the nodes on its way without their values and the node of
            // its key whole. The same walk for whether a key is there does
            // not read the node of the key. A read that keeps the nodes
            // it fetches reads each whole, as the walk of a write does.
            let callers = [
                point(&target),
                prove(Call::Get(target.clone(), true)),
                prove(Call::Exists(target.clone())),
                prove(Call::Observe(target.clone())),
            ];
            let refused_reads = [
                (above.as_slice(), Reads::Every),
                (target.as_slice(), Reads::Every),
                (target.as_slice(), Reads::OfTheValue),
            ];
            for call in &callers {
                for refused in refused_reads {
                    let (failed, cost, refusals) =
                        refused_then_answered(both, &proved, call, refused, &then);
                    let case = format!("{tree_type:?}, {refused:?} refused at {}", named(call));
                    assert_eq!(cost, OperationCost::default(), "{case}");
                    let unread =
                        matches!(call, Act::Prove(_, Call::Exists(_))) && refused.0 == target;
                    match unread {
                        false => {
                            assert!(refused_as(&failed), "{case}: {failed}");
                            assert_eq!(refusals, 1, "{case}");
                        }
                        true => {
                            assert_eq!(failed, "Ok(true), in 0 seeks", "{case}");
                            assert_eq!(refusals, 0, "{case}");
                        }
                    }
                }
            }

            // A Merk that was charged for neither node is charged for the
            // read that failed what the store charges for it, and is
            // charged for the node when the store answers.
            let refusing = Refusing::over(both.writer);
            let store = LigninNodeStore::new(&refusing, ROOT, Mode::Deferred(None));
            let link = way[0].link(above.as_slice() < way[0].key());
            let link = link.filter(|link| link.key() == above).expect("a link");
            refusing.refuse(Some((above.as_slice(), Reads::Every)));
            let unread = store.fetch_node_without_value(link, version());
            assert!(matches!(
                &unread.value,
                Err(Error::StorageError(grovedb_storage::Error::StorageError(text)))
                    if text == REFUSED
            ));
            assert_ne!(unread.cost, OperationCost::default());
            let refused = (above.as_slice(), Reads::Every);
            let (failed, cost, refusals) =
                refused_then_answered(both, &[], &proved[0], refused, &then);
            assert!(refused_as(&failed), "{tree_type:?}: {failed}");
            assert_eq!((cost, refusals), (unread.cost, 1), "{tree_type:?}");
        });
    }
}

/// `merk` is a Merk whose write failed in its walk: it holds no tree.
/// Opened again on its handle, it makes `write` as a Merk that is opened
/// on `control` does: with the same answer, at the same cost and to the
/// same root hash. What the write answered and cost is returned.
fn written_once_opened_again<'db, 'c, S: NodeStore<'db>, C: NodeStore<'c>>(
    merk: Merk<S>,
    control: C,
    tree_type: TreeType,
    write: &Act,
    case: &str,
) -> (String, OperationCost) {
    assert_eq!(merk.root_key(), None, "{case}");
    let mut merk = open(merk.storage, tree_type);
    let mut control = open(control, tree_type);
    let written = act(&mut merk, write);
    assert_eq!(written.0, "Ok(())", "{case}");
    assert_eq!(written, act(&mut control, write), "{case}");
    let root_hash = merk.root_hash().unwrap();
    assert_eq!(root_hash, control.root_hash().unwrap(), "{case}");
    written
}

/// A write whose walk comes to a node that a proof was charged for reads
/// the node again, whole and at no charge. Where the store refuses that
/// read, the write ends with the store's error, is charged nothing and
/// stages nothing. Where its Merk was not charged for the node, the write
/// is charged for the reads its walk made before and what the store
/// charges for the refused one, on LigninDB and on RocksDB.
///
/// A Merk is not to be used after a write of it failed in its walk: it
/// holds no tree, which is upstream behaviour and the same on both engines
/// (`merk/src/merk/apply.rs` takes the tree out for the walker and sets
/// one back only when the walk succeeds). A Merk that is opened again on
/// the handle makes the write, at the cost it has on a Merk that met no
/// refusal; on LigninDB that is the cost of the write on RocksDB.
#[test]
fn a_write_that_is_refused_a_read_fails_and_leaves_its_merk_with_no_tree() {
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        with_both(tree_type, 0x0600_d000 + seed as u64, 4, &[], |both| {
            let (way, _) = way_down(&LigninNodeStore::deferred(both.writer, ROOT, None));
            let (above, target) = (way[1].key().to_vec(), way[2].key().to_vec());
            let items = vec![QueryItem::Key(target.clone())];
            let point = Act::Prove(version(), Call::Query(items, None, true));
            // The walk of the write goes from the root, which a Merk
            // holds since its open, through the node above the target.
            let rewrite = Op::Put(vec![7; 60], feature(tree_type, 3));
            let write = Act::Apply(vec![(target.clone(), rewrite)]);

            // What the write is on RocksDB, on a Merk that just opened.
            let plain = StorageBatch::new();
            let on_rocks = act(&mut open(both.rocks_context(&plain), tree_type), &write);
            assert_eq!(on_rocks.0, "Ok(())", "{tree_type:?}");

            // What the store charges for the read of the node above the
            // target, and for a read that it refuses.
            let refusing = Refusing::over(both.writer);
            let store = LigninNodeStore::new(&refusing, ROOT, Mode::Deferred(None));
            let link = way[0].link(above.as_slice() < way[0].key());
            let link = link.filter(|link| link.key() == above).expect("a link");
            let read = store.fetch_node(link, NO_VALUE_DEFINED_COST.as_ref(), version());
            assert!(read.value.is_ok(), "{tree_type:?}");
            refusing.refuse(Some((above.as_slice(), Reads::Every)));
            let unread = store.fetch_node(link, NO_VALUE_DEFINED_COST.as_ref(), version());
            assert!(unread.value.is_err(), "{tree_type:?}");
            assert_ne!(unread.cost, OperationCost::default(), "{tree_type:?}");
            let nothing = OperationCost::default();

            // On LigninDB: after a proof that was charged for both nodes,
            // and with no proof before, each with the read of either
            // refused.
            let cases = [
                (true, &above, nothing.clone()),
                (true, &target, nothing),
                (false, &above, unread.cost.clone()),
                (false, &target, read.cost + unread.cost),
            ];
            for (proved, refused, charged) in cases {
                let case = format!("{tree_type:?}, proved: {proved}, refused: {refused:?}");
                let refusing = Refusing::over(both.writer);
                let staged = LigninBatch::new();
                let store = LigninNodeStore::new(&refusing, ROOT, Mode::Deferred(Some(&staged)));
                let mut merk = open(store, tree_type);
                if proved {
                    assert!(act(&mut merk, &point).0.starts_with("Ok("), "{case}");
                }
                refusing.refuse(Some((refused.as_slice(), Reads::Every)));
                let (failed, cost) = act(&mut merk, &write);
                let refusals = refusing.refused();
                refusing.refuse(None);
                let refused_as = format!("Err(StorageError(StorageError({REFUSED:?})))");
                assert_eq!(failed, refused_as, "{case}");
                assert_eq!((cost, refusals), (charged, 1), "{case}");
                assert!(staged.is_empty(), "{case}");

                let unrefused = LigninBatch::new();
                let control = LigninNodeStore::deferred(both.writer, ROOT, Some(&unrefused));
                let written = written_once_opened_again(merk, control, tree_type, &write, &case);
                assert_eq!(written, on_rocks, "{case}");
                assert!(!staged.is_empty(), "{case}");
            }

            // On RocksDB a write reads the nodes its Merk does not hold:
            // here the node above the target, and that read fails.
            let case = format!("{tree_type:?}, on RocksDB");
            let (tape, batch) = (Tape::default(), StorageBatch::new());
            let mut merk = open(tape.recording(both.rocks_context(&batch)), tree_type);
            tape.take();
            tape.fail(Some("get"));
            let (failed, cost) = act(&mut merk, &write);
            tape.fail(None);
            assert_eq!(
                failed, "Err(StorageError(StorageError(\"get failed\")))",
                "{case}"
            );
            assert_eq!(cost, failed_call(), "{case}");
            let calls = tape.take();
            assert_eq!(calls.len(), 1, "{case}");
            assert_eq!((calls[0].name, &calls[0].key), ("get", &above), "{case}");
            assert!(batch.is_empty(), "{case}");

            let (control_tape, control_batch) = (Tape::default(), StorageBatch::new());
            let control = control_tape.recording(both.rocks_context(&control_batch));
            written_once_opened_again(merk, control, tree_type, &write, &case);
            assert!(!batch.is_empty(), "{case}");
        });
    }
}

/// The files of the operating system, with a count of the reads of the
/// value log that the thread of the test makes: every read of a file in
/// the directory `val` on the thread that made this. The threads of the
/// database's background work read there too, when the cleaner copies
/// values, and those are no reads of a call of the test.
#[derive(Debug)]
struct CountingVfs {
    value_reads: Arc<AtomicU64>,
    counted: ThreadId,
}

impl CountingVfs {
    fn new() -> Self {
        Self {
            value_reads: Arc::default(),
            counted: thread::current().id(),
        }
    }
}

#[derive(Debug)]
struct CountedFile {
    file: Box<dyn VfsFile>,
    reads: Option<Arc<AtomicU64>>,
    counted: ThreadId,
}

impl CountedFile {
    fn count(&self) {
        if let Some(reads) = &self.reads
            && thread::current().id() == self.counted
        {
            reads.fetch_add(1, Relaxed);
        }
    }
}

impl Vfs for CountingVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let in_value_log = path.parent().and_then(Path::file_name) == Some(OsStr::new("val"));
        Ok(Box::new(CountedFile {
            file: StdVfs.open(path, mode)?,
            reads: in_value_log.then(|| self.value_reads.clone()),
            counted: self.counted,
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdVfs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        StdVfs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock_existing(path)
    }
}

impl VfsFile for CountedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.count();
        self.file.read_at(buf, offset)
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.count();
        self.file.read_exact_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.file.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.file.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        Ok(())
    }

    fn sync_all(&self) -> io::Result<()> {
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
}

/// A store whose reads of the value log are counted, with a Merk of
/// `nodes` nodes that is committed and written back: every value is in
/// the value log, since each is longer than a cell holds in line.
pub(super) struct Counted {
    pub(super) lignin: Lignin,
    value_reads: Arc<AtomicU64>,
    rocks: TempStorage,
}

const LONG: usize = 400;

/// The value of the node `n` of a [`Counted`] store: an item, and another
/// for each node.
pub(super) fn long_value(n: u64) -> Vec<u8> {
    let item = Element::new_item(vec![n as u8; LONG]);
    item.serialize(version()).expect("an item")
}

impl Counted {
    pub(super) fn new(tree_type: TreeType, nodes: u64) -> Self {
        let vfs = CountingVfs::new();
        let value_reads = vfs.value_reads.clone();
        // The database is checked at its close as every one of these
        // tests is: its values are put plainly.
        let lignin = Lignin::on(Arc::new(vfs), false);
        let rocks = TempStorage::new();

        let batch: Vec<(Vec<u8>, Op)> = (0..nodes)
            .map(|n| (be(n), Op::Put(long_value(n), feature(tree_type, n))))
            .collect();
        let transaction = lignin.path().begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
        apply(&mut open(store, tree_type), &batch);
        staged
            .apply(&writer)
            .unwrap()
            .expect("the batch is applied");
        writer.into_scope().merge();
        lignin.commit(&transaction);
        drop(transaction);

        let rocks_transaction = rocks.start_transaction();
        let storage_batch = StorageBatch::new();
        let context = rocks
            .get_transactional_storage_context(
                SubtreePath::empty(),
                Some(&storage_batch),
                &rocks_transaction,
            )
            .unwrap();
        apply(&mut open(context, tree_type), &batch);
        rocks
            .commit_multi_context_batch(storage_batch, Some(&rocks_transaction))
            .unwrap()
            .expect("the storage batch is applied");
        rocks
            .commit_transaction(rocks_transaction)
            .unwrap()
            .expect("the transaction commits");

        // The close of the session places every value in the value log,
        // and the reads are counted in the session after it.
        let lignin = lignin.reopened();
        Counted {
            lignin,
            value_reads,
            rocks,
        }
    }

    /// Makes `calls` on a Merk that is opened on a handle of each engine
    /// that only reads. The answers and the costs are the same, and what
    /// is returned is the answers, and how many reads of the value log
    /// the open made and then each call.
    pub(super) fn reads(&self, tree_type: TreeType, calls: &[Call]) -> (Vec<String>, Vec<u64>) {
        let rocks_transaction = self.rocks.start_transaction();
        let context = self
            .rocks
            .get_transactional_storage_context(SubtreePath::empty(), None, &rocks_transaction)
            .unwrap();
        let on_rocks = open(context, tree_type);

        let transaction = self.lignin.path().begin();
        let read = || self.value_reads.swap(0, Relaxed);
        read();
        let merk = open(LigninNodeStore::read_only(&transaction, ROOT), tree_type);
        let (mut answers, mut reads) = (Vec::new(), vec![read()]);
        for call in calls {
            let (mut cost, mut expected_cost) = Default::default();
            let found = answer(&merk, call, version(), &mut cost);
            reads.push(read());
            let expected = answer(&on_rocks, call, version(), &mut expected_cost);
            assert_eq!(found, expected, "{tree_type:?}: {call:?}");
            assert_eq!(cost, expected_cost, "{tree_type:?}: the cost of {call:?}");
            answers.push(found);
        }
        (answers, reads)
    }
}

/// The acceptance test of MERK-06: a point proof loads no out-of-line
/// value other than the target's.
#[test]
fn a_point_proof_reads_no_value_from_the_value_log_but_the_targets() {
    const NODES: u64 = 200;
    for tree_type in TREE_TYPES {
        let counted = Counted::new(tree_type, NODES);
        let root = {
            let transaction = counted.lignin.path().begin();
            let merk = open(LigninNodeStore::read_only(&transaction, ROOT), tree_type);
            merk.root_key().expect("a root")
        };
        let point = |key: Vec<u8>| Call::Query(vec![QueryItem::Key(key)], None, true);
        let targets = [0, 1, 77, 130, NODES - 1].into_iter();
        for target in targets.filter(|target| be(*target) != root) {
            // A proof that the target is there.
            let (answers, reads) = counted.reads(tree_type, &[point(be(target))]);
            assert!(answers[0].contains(&hex::encode(long_value(target))));
            // The open of the Merk reads its root node whole, as every
            // open does. The proof then reads one value: the target's.
            assert_eq!(reads, [1, 1], "{tree_type:?}: the proof of {target}");

            // A proof that a key is not there emits no value, and reads
            // none.
            let mut absent = be(target);
            absent.push(1);
            let (_, reads) = counted.reads(tree_type, &[point(absent.clone())]);
            assert_eq!(reads, [1, 0], "{tree_type:?}: no key after {target}");

            // On a Merk that has made the proof, the proof reads the
            // target's value again and no value of the nodes on its way.
            let again = [point(be(target)), point(be(target)), point(absent)];
            let (answers, reads) = counted.reads(tree_type, &again);
            assert_eq!(answers[0], answers[1]);
            assert_eq!(reads, [1, 1, 1, 0], "{tree_type:?}: {target} again");
        }
        // The root's value is in memory since the open.
        let (_, reads) = counted.reads(tree_type, &[point(root)]);
        assert_eq!(reads, [1, 0], "{tree_type:?}: the proof of the root");
    }
}

/// What the other proofs read from the value log: the values they emit,
/// and for a count-offset proof also the value of every row in its range
/// that it gives by key, which it reads to refuse a row that is a tree.
#[test]
fn a_proof_reads_the_values_it_emits_from_the_value_log() {
    const NODES: u64 = 200;
    let tree_type = TreeType::ProvableCountSumTree;
    let counted = Counted::new(tree_type, NODES);
    let range = |from: u64, to: u64| QueryItem::RangeInclusive(be(from)..=be(to));
    let (answers, reads) = counted.reads(
        tree_type,
        &[
            // Five rows.
            Call::Query(vec![range(40, 44)], None, true),
            // The first two rows of sixty.
            Call::Query(vec![range(100, 159)], Some(2), false),
            // An aggregate proof emits no value.
            Call::Count(range(10, 190)),
            // A trunk of three levels emits seven nodes, and the hashes
            // of the eight below them.
            Call::Trunk(3, None),
            // A branch proof walks to its root and emits what is under it
            // down to its depth: three nodes.
            Call::Branch(be(77), 2),
            // Two rows after an offset of thirty.
            Call::CountOffset(range(20, 120), 30, Some(2), true, false),
        ],
    );
    assert!(answers.iter().all(|answer| answer.starts_with("Ok((")));
    let root = {
        let transaction = counted.lignin.path().begin();
        let merk = open(LigninNodeStore::read_only(&transaction, ROOT), tree_type);
        let root = merk.root_key().expect("a root");
        u64::from_be_bytes(root.try_into().expect("a key of eight bytes"))
    };
    // The values a proof emits, and those among them that the Merk does
    // not hold: the root's is in memory since the open.
    let emitted = |answer: &String| -> (u64, u64) {
        let emits = |n: &u64| answer.contains(&hex::encode(long_value(*n)));
        let nodes = (0..NODES).filter(emits).count() as u64;
        (nodes, nodes - u64::from(emits(&root)))
    };
    assert_eq!(reads[0], 1, "the open");
    // Five rows, and the first two of sixty.
    assert_eq!(
        (emitted(&answers[0]).0, reads[1]),
        (5, emitted(&answers[0]).1)
    );
    assert_eq!(
        (emitted(&answers[1]).0, reads[2]),
        (2, emitted(&answers[1]).1)
    );
    assert_eq!(
        (emitted(&answers[2]), reads[3]),
        ((0, 0), 0),
        "an aggregate"
    );
    // A trunk holds the root and the levels under it, and not the nodes
    // it ends at, of which it gives the hashes.
    let trunk = emitted(&answers[3]);
    assert!(
        trunk.0 >= 3 && trunk.0 < 40 && trunk.1 == trunk.0 - 1,
        "{trunk:?}"
    );
    assert_eq!(reads[4], trunk.1);
    // A branch holds its root and what is under it down to its depth, and
    // not the nodes on the way to it.
    let branch = emitted(&answers[4]);
    assert!((1..=3).contains(&branch.0), "{branch:?}");
    assert_eq!(reads[5], branch.1);
    // A count-offset proof reads the value of every row of its range that
    // it gives by key: the two it returns among them, and far fewer than
    // the hundred and one the range holds. The rows it skips in whole
    // subtrees it gives as one hash each, and reads no value of.
    assert_eq!(emitted(&answers[5]).0, 2);
    let (_, proof) = answers[5].split_once('"').expect("a proof");
    let proof = hex::decode(proof.split_once('"').expect("a proof").0).expect("bytes");
    let named: Vec<u64> = Decoder::new(&proof)
        .filter_map(|op| match op.expect("an op") {
            ProofOp::Push(Node::KVCount(key, ..) | Node::KVDigestCount(key, ..)) => Some(key),
            _ => None,
        })
        .map(|key| u64::from_be_bytes(key.try_into().expect("a key of eight bytes")))
        .filter(|key| (20..=120).contains(key))
        .collect();
    assert!((2..40).contains(&named.len()), "{named:?}");
    assert_eq!(
        reads[6],
        named.len() as u64 - u64::from(named.contains(&root))
    );
}

/// A value is read for the node that was fetched, or not at all.
#[test]
fn a_value_is_not_read_for_a_node_that_was_written_since() {
    let lignin = Lignin::new();
    let transaction = lignin.path().begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut merk = open(
        LigninNodeStore::immediate(&writer, ROOT),
        TreeType::NormalTree,
    );
    let batch: Vec<(Vec<u8>, Op)> = (0..40).map(|n| basic(n, &long_value(n))).collect();
    apply(&mut merk, &batch);

    let reader = LigninNodeStore::deferred(&writer, ROOT, None);
    let root = reader
        .get_node(&merk.root_key().unwrap(), NO_VALUE_DEFINED_COST, version())
        .unwrap()
        .unwrap()
        .expect("the root");
    let link = root.link(true).expect("a child");
    let fetched = reader.fetch_node_without_value(link, version());
    // The fetch is charged what the fetch of the whole node is charged.
    let whole = reader.fetch_node(link, NO_VALUE_DEFINED_COST.as_ref(), version());
    assert_eq!(fetched.cost, whole.cost);
    let (mut node, whole) = (fetched.unwrap().unwrap(), whole.unwrap().unwrap());
    assert!(node.value_absent() && !whole.value_absent());
    assert_eq!(
        (node.key(), node.kv_hash(), node.value_hash()),
        (whole.key(), whole.kv_hash(), whole.value_hash())
    );
    assert_eq!(node.feature_type(), whole.feature_type());
    for left in [true, false] {
        assert_eq!(node.link(left), whole.link(left));
    }

    // The value is read once, at no charge, and a node that has its value
    // is left alone.
    let mut again = node.clone();
    let read = reader.load_node_value(&mut node);
    assert_eq!(read.cost, OperationCost::default());
    read.unwrap().unwrap();
    assert_eq!(node.value_as_slice(), whole.value_as_slice());
    let read = reader.load_node_value(&mut node);
    assert_eq!(read.cost, OperationCost::default());
    read.unwrap().unwrap();

    // The node gets another value: the value of the node that was fetched
    // is gone, and no other is put under its hashes.
    let n = u64::from_be_bytes(whole.key().try_into().unwrap());
    apply(&mut merk, &[basic(n, b"another value")]);
    let refused = reader.load_node_value(&mut again);
    assert_eq!(refused.cost, OperationCost::default());
    let Err(Error::StorageError(error)) = refused.unwrap() else {
        panic!("the value of a rewritten node was read");
    };
    assert!(error
        .to_string()
        .contains("written before its value was read"));
    assert!(again.value_absent());

    // And so for a node that is gone.
    apply(&mut merk, &[(be(n), Op::Delete)]);
    let Err(Error::StorageError(error)) = reader.load_node_value(&mut again).unwrap() else {
        panic!("the value of a removed node was read");
    };
    assert!(error
        .to_string()
        .contains("removed before its value was read"));

    // A link that leads to no node is the error it is for a whole fetch,
    // charged as a lookup that found nothing.
    let missing = reader.fetch_node_without_value(link, version());
    assert_eq!(
        missing.cost,
        reader
            .fetch_node(link, NO_VALUE_DEFINED_COST.as_ref(), version())
            .cost
    );
    assert!(matches!(
        missing.unwrap(),
        Err(Error::KeyNotFoundError("Key not found for fetch"))
    ));
}

/// A store that keeps one record a node returns it whole, and reads no
/// value afterwards.
#[test]
fn a_store_of_whole_nodes_fetches_whole_nodes_for_a_walk() {
    let rocks = TempStorage::new();
    let transaction = rocks.start_transaction();
    let context = rocks
        .get_immediate_storage_context(SubtreePath::empty(), &transaction)
        .unwrap();
    let mut merk = open(context, TreeType::NormalTree);
    let batch: Vec<(Vec<u8>, Op)> = (0..9).map(|n| basic(n, &long_value(n))).collect();
    apply(&mut merk, &batch);
    let context = rocks
        .get_immediate_storage_context(SubtreePath::empty(), &transaction)
        .unwrap();
    let root = NodeStore::get_node(
        &context,
        &merk.root_key().unwrap(),
        NO_VALUE_DEFINED_COST,
        version(),
    );
    let root = root.unwrap().unwrap().expect("the root");
    let link = root.link(false).expect("a child");
    let fetched = context.fetch_node_without_value(link, version());
    let mut node = fetched.unwrap().unwrap();
    assert!(!node.value_absent());
    let whole = context.fetch_node(link, NO_VALUE_DEFINED_COST.as_ref(), version());
    assert_eq!(node, whole.unwrap().unwrap());
    let read = context.load_node_value(&mut node);
    assert_eq!(read.cost, OperationCost::default());
    read.unwrap().unwrap();

    let mut without = TreeNode::new_without_value(
        node.key().to_vec(),
        *node.kv_hash(),
        *node.value_hash(),
        node.feature_type(),
        None,
        None,
    );
    let refused = context.load_node_value(&mut without).unwrap();
    assert!(matches!(refused, Err(Error::CorruptedCodeExecution(_))));
}
