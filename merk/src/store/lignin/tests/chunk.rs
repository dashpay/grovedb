//! Chunks of a Merk on the store, and the restore of a Merk from chunks
//! (MERK-07). The chunks a Merk on the store produces are the bytes a Merk
//! on RocksDB produces, and a restore from them leaves the root hash, the
//! root key and the nodes it leaves on RocksDB. The store takes no link to
//! a node it does not hold, so it is handed the restored Merk whole, when
//! the restore is finalized, and holds nothing of it before.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_element::{reference_path::ReferencePathType, BackwardReferences, Element};
use grovedb_storage::rocksdb_storage::PrefixedRocksDbImmediateStorageContext;
use grovedb_version::version::GROVE_VERSIONS;

use super::{
    cursor::{acts, walked, with_both, Act, Both},
    proof::{elements, long_value, Call, Counted},
    views::Facade,
    *,
};
use crate::{
    element::tree_type::ElementTreeTypeExtensions,
    merk::{chunks::ChunkProducer, restore::Restorer},
    proofs::{
        chunk::{error::ChunkError, util::generate_traversal_instruction_as_vec_bytes},
        Node, Op as ProofOp,
    },
    tree::combine_hash,
};

/// The chunks of `merk`, by chunk id. The producer numbers the chunks a
/// tree of the Merk's height has when every node of it is there, so a
/// number can name a place where this tree has no node: such a chunk is
/// refused, and is not among those a restore asks for.
fn chunks_of<'db, S: NodeStore<'db>>(merk: &Merk<S>) -> BTreeMap<Vec<u8>, Vec<ProofOp>> {
    let height = merk.height().expect("the Merk has nodes") as usize;
    let mut producer = ChunkProducer::new(merk).expect("the Merk has nodes");
    let mut chunks = BTreeMap::new();
    for index in 1..=producer.len() {
        let chunk_id = generate_traversal_instruction_as_vec_bytes(height, index).expect("an id");
        match producer.chunk_with_index(index, version()) {
            Ok((chunk, _)) => {
                chunks.insert(chunk_id, chunk);
            }
            Err(Error::ChunkingError(ChunkError::BadTraversalInstruction(_))) => {}
            Err(error) => panic!("the chunk {index}: {error:?}"),
        }
    }
    chunks
}

/// The key of the node a chunk op puts, if it puts one with its value.
fn emitted_key(op: &ProofOp) -> Option<&[u8]> {
    match op {
        ProofOp::Push(
            Node::KV(key, ..)
            | Node::KVValueHash(key, ..)
            | Node::KVValueHashFeatureType(key, ..)
            | Node::KVCount(key, ..)
            | Node::KVSum(key, ..)
            | Node::KVBigSum(key, ..)
            | Node::KVCountSum(key, ..)
            | Node::KVCountBigSum(key, ..),
        ) => Some(key),
        _ => None,
    }
}

/// Chunk requests over the chunk ids of a Merk: every chunk by its id, by
/// index at both ends and beyond them, several at once with and without a
/// limit, ids that name no chunk, and then reads and proofs of the Merk
/// that produced them.
fn requests(chunk_ids: &[Vec<u8>], keys: &[Vec<u8>]) -> Vec<Call> {
    let count = chunk_ids.len();
    let key = |n: usize| keys[n % keys.len()].clone();
    let mut calls: Vec<Call> = chunk_ids.iter().cloned().map(Call::Chunk).collect();
    calls.extend([0, 1, 2, count, count + 1].map(Call::ChunkAt));
    calls.extend([
        Call::MultiChunk(vec![], None),
        // Too few bytes for the first chunk, and enough for a few.
        Call::MultiChunk(vec![], Some(1)),
        Call::MultiChunk(vec![], Some(6_000)),
        Call::MultiChunk(chunk_ids[count / 2].clone(), Some(30_000)),
        Call::MultiChunk(chunk_ids[count - 1].clone(), None),
        // An id that is no chunk's.
        Call::Chunk(vec![1]),
        Call::MultiChunk(vec![1], None),
        // A step that is neither left nor right, and more steps than the
        // tree is deep.
        Call::Chunk(vec![2]),
        Call::Chunk(vec![1; 60]),
        // What the Merk answers once it has produced chunks.
        Call::Get(key(3), true),
        Call::Get(key(4), false),
        Call::Exists(key(5)),
        Call::Query(vec![grovedb_query::QueryItem::Key(key(6))], None, true),
        Call::Trunk(3, None),
        Call::Chunk(vec![]),
        Call::Observe(key(7)),
        Call::LinkedKeys,
        Call::Chunk(chunk_ids[count / 3].clone()),
        Call::Get(key(8), true),
    ]);
    calls
}

/// Makes the requests of [`requests`] on the Merk both engines hold, on
/// every grove version, on a deferred and on an immediate handle of each.
/// Every answer is the same on both engines. Returns how many chunks were
/// given by their id, and how many requests for several chunks were
/// answered with chunks.
fn answered(both: &Both<'_, '_, '_>) -> (usize, usize) {
    let tree_type = both.tree_type;
    let store = LigninNodeStore::deferred(both.writer, ROOT, None);
    let keys: Vec<Vec<u8>> = walked(&store).into_iter().map(|(key, _)| key).collect();
    let produced = chunks_of(&open(store, tree_type));
    let chunk_ids: Vec<Vec<u8>> = produced.keys().cloned().collect();
    assert!(chunk_ids.len() > 8, "{tree_type:?}: {}", chunk_ids.len());
    // The chunks hold every node of the Merk once.
    let emitted: Vec<&[u8]> = (produced.values().flatten())
        .filter_map(emitted_key)
        .collect();
    assert_eq!(emitted.len(), keys.len(), "{tree_type:?}");
    let emitted: BTreeSet<&[u8]> = emitted.into_iter().collect();
    assert_eq!(emitted, keys.iter().map(Vec::as_slice).collect());

    let calls = requests(&chunk_ids, &keys);
    let (mut chunks, mut several) = (0, 0);
    for grove_version in GROVE_VERSIONS.iter() {
        let acts: Vec<Act> = (calls.iter().cloned())
            .map(|call| Act::Prove(grove_version, call))
            .collect();
        // One Merk answers all the requests, so each is answered from what
        // the ones before it left in memory. Then another does, on a
        // handle that writes at once.
        for immediate in [false, true] {
            let answers = both.run(immediate, &acts);
            // The first answer is that of the open.
            for (call, answer) in calls.iter().zip(&answers[1..]) {
                let given = answer.starts_with("Ok((");
                match call {
                    Call::Chunk(chunk_id) if produced.contains_key(chunk_id) => {
                        assert!(given, "{tree_type:?}: {call:?} gave {answer}");
                        chunks += 1;
                    }
                    Call::MultiChunk(..) if given => several += 1,
                    _ => {}
                }
            }
        }
    }
    (chunks, several)
}

/// The acceptance test of MERK-07 for chunk production: chunk bytes equal
/// to RocksDB's.
#[test]
fn a_chunk_is_the_bytes_rocksdb_gives() {
    let (mut chunks, mut several) = (0, 0);
    let mut count = |(given, together): (usize, usize)| {
        chunks += given;
        several += together;
    };
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        // A Merk that many batches wrote, with nodes as they were
        // committed and nodes the transaction rewrote, made and removed.
        // Its tree has leaves at several depths, so a tree of its height
        // has chunks that this one has no node for.
        let before = [Act::Apply(elements(tree_type))];
        with_both(tree_type, 0x0700_c000 + seed as u64, 5, &before, |both| {
            count(answered(both));
        });
        // A Merk of elements that one batch wrote: its tree has every
        // chunk a tree of its height has, so that several are given at
        // once.
        with_engines(tree_type, |both, _| {
            both.deferred(&[Act::Apply(element_nodes(tree_type, 330))]);
            count(answered(both));
        });
    }
    // The requests gave chunks, and were not only refused alike on both
    // engines.
    assert!(chunks > 10_000, "{chunks} chunks");
    assert!(several > 400, "{several} answers of several chunks");
}

/// A chunk reads from the value log the values of the nodes it emits: not
/// those of the nodes on the way to it, and not those of the nodes at the
/// depth below it, whose hashes it gives.
#[test]
fn a_chunk_reads_the_values_it_emits_from_the_value_log() {
    const NODES: u64 = 200;
    for tree_type in [TreeType::NormalTree, TreeType::ProvableCountSumTree] {
        let counted = Counted::new(tree_type, NODES);
        let (root, chunk_ids) = {
            let transaction = counted.lignin.path().begin();
            let merk = open(LigninNodeStore::read_only(&transaction, ROOT), tree_type);
            let root = merk.root_key().expect("a root");
            let root = u64::from_be_bytes(root.try_into().expect("a key of eight bytes"));
            (root, chunks_of(&merk).into_keys().collect::<Vec<_>>())
        };
        // A chunk under the root's, and one at the bottom of the tree.
        let deepest = chunk_ids.iter().map(Vec::len).max().expect("a chunk");
        let below = |depth: usize| -> Vec<u8> {
            let mut at = chunk_ids.iter().filter(|chunk_id| chunk_id.len() == depth);
            at.next().expect("a chunk at that depth").clone()
        };
        assert!(
            deepest >= 6,
            "{tree_type:?}: {deepest} steps to the deepest"
        );
        let (middle, bottom) = (below(3), below(deepest));
        let (answers, reads) = counted.reads(
            tree_type,
            &[
                Call::Chunk(vec![]),
                Call::Chunk(middle),
                Call::Chunk(bottom),
                Call::Chunk(vec![]),
                Call::MultiChunk(vec![], None),
            ],
        );
        assert!(answers.iter().all(|answer| answer.starts_with("Ok((")));
        // The values a chunk emits, and those among them that the Merk
        // does not hold: the root's is in memory since the open.
        let emitted = |answer: &String| -> (u64, u64) {
            let emits = |n: &u64| answer.contains(&hex::encode(long_value(*n)));
            let nodes = (0..NODES).filter(emits).count() as u64;
            (nodes, nodes - u64::from(emits(&root)))
        };
        assert_eq!(reads[0], 1, "{tree_type:?}: the open");
        // The chunk at the root is seven nodes, with the hashes of the
        // eight below them.
        assert_eq!(
            (emitted(&answers[0]), reads[1]),
            ((7, 6), 6),
            "{tree_type:?}"
        );
        // A chunk under it is seven nodes as well, none of them on the way
        // to it.
        assert_eq!(
            (emitted(&answers[1]), reads[2]),
            ((7, 7), 7),
            "{tree_type:?}"
        );
        // A chunk at the bottom is what is left of the tree there.
        let bottom = emitted(&answers[2]);
        assert!((1..=7).contains(&bottom.0), "{tree_type:?}: {bottom:?}");
        assert_eq!(reads[3], bottom.0, "{tree_type:?}");
        // The Merk kept none of the nodes whose values it read: the chunk
        // at the root reads them again.
        assert_eq!(answers[3], answers[0]);
        assert_eq!(reads[4], 6, "{tree_type:?}");
        // All chunks together emit every node, each read once.
        assert_eq!(
            (emitted(&answers[4]), reads[5]),
            ((NODES, NODES - 1), NODES - 1)
        );
    }
}

/// The handle of a Merk that is written at once, on RocksDB: where a
/// restore writes on that engine.
fn rocks_at<'a>(both: &Both<'a, '_, '_>, place: u8) -> PrefixedRocksDbImmediateStorageContext<'a> {
    let name = [place];
    let path = [name.as_slice()];
    both.rocks
        .get_immediate_storage_context(path.as_ref().into(), both.rocks_transaction)
        .unwrap()
}

fn subtree_at(place: u8) -> LigninSubtree {
    LigninSubtree::alone([place; 32])
}

/// The handle of a Merk that is written at once, on the store.
fn lignin_at<'a>(both: &Both<'a, '_, '_>, place: u8) -> LigninNodeStore<'a> {
    LigninNodeStore::immediate(both.writer, subtree_at(place))
}

/// A key that carries `n`: of several lengths, and the empty key for 0.
fn scattered_key(n: u64) -> Vec<u8> {
    let mut key = n.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes().to_vec();
    key.truncate(8 - (n % 3) as usize);
    if n == 0 {
        key.clear();
    }
    key
}

/// Nodes whose values are no elements of GroveDB, some of them with a
/// value hash that is not the hash of the value, some with a value that is
/// longer than a cell holds in line, and some with no value but the mark.
fn raw_nodes(tree_type: TreeType, count: u64) -> Vec<(Vec<u8>, Op)> {
    let nodes = (0..count).map(|n| {
        let len = match n % 7 {
            0 => 300 + (n % 500) as usize,
            1 => 0,
            _ => 1 + (n % 120) as usize,
        };
        // No element begins with this byte, so no chunk node is chosen by
        // what a value happens to look like.
        let mut value = vec![0xf7];
        value.resize(1 + len, n as u8);
        let feature = feature(tree_type, n.wrapping_mul(977));
        let op = match n % 11 {
            0 => Op::PutCombinedReference(value, [n as u8; 32], feature),
            1 => Op::PutWithProvidedValueHash(value, [!(n as u8); 32], None, feature),
            _ => Op::Put(value, feature),
        };
        (scattered_key(n), op)
    });
    nodes.collect::<BTreeMap<_, _>>().into_iter().collect()
}

/// Nodes whose values are elements of GroveDB, each with the feature the
/// element has in a tree of `tree_type`: items, sum items, items with a
/// sum, an item that takes backward references, a tree and a reference.
fn element_nodes(tree_type: TreeType, count: u64) -> Vec<(Vec<u8>, Op)> {
    let serialized = |element: &Element| element.serialize(version()).expect("an element");
    let feature_of = |element: &Element| {
        let feature = element.get_feature_type(tree_type);
        feature.expect("the element has a feature in the tree")
    };
    let nodes = (0..count).map(|n| {
        let bytes = |len: u64| vec![n as u8; len as usize];
        let sum = (n as i64 % 301) - 150;
        let element = match n % 9 {
            0 => Element::new_item(bytes(300 + n % 400)),
            1 | 2 => Element::new_sum_item(sum),
            3 => Element::new_item_with_sum_item(bytes(1 + n % 50), sum),
            4 => Element::ItemWithBackwardsReferences(
                bytes(20),
                BackwardReferences::with_max_incoming(3),
                None,
            ),
            5 => Element::empty_tree(),
            6 => Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                vec![1],
                bytes(3),
            ])),
            _ => Element::new_item(bytes(1 + n % 90)),
        };
        let (value, feature) = (serialized(&element), feature_of(&element));
        let op = match element {
            // A reference is stored under the hash of what it refers to.
            Element::Reference(..) => Op::PutCombinedReference(value, [n as u8; 32], feature),
            _ => Op::Put(value, feature),
        };
        (scattered_key(n), op)
    });
    nodes.collect::<BTreeMap<_, _>>().into_iter().collect()
}

/// How a restore is driven.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Driven {
    /// Whether the values are finalized as elements of GroveDB.
    elements: bool,
    /// The value hash of the element that hosts the restored Merk, where
    /// the expected hash is of the two.
    parent: Option<CryptoHash>,
}

/// A copy of `chunk` that does not hash to what the original does: the
/// first node it puts has another key.
fn forged(chunk: &[ProofOp]) -> Vec<ProofOp> {
    let mut forged = chunk.to_vec();
    let node = forged.iter_mut().find_map(|op| match op {
        ProofOp::Push(
            Node::KV(key, ..)
            | Node::KVValueHash(key, ..)
            | Node::KVValueHashFeatureType(key, ..)
            | Node::KVCount(key, ..)
            | Node::KVSum(key, ..)
            | Node::KVBigSum(key, ..)
            | Node::KVCountSum(key, ..)
            | Node::KVCountBigSum(key, ..),
        ) => Some(key),
        _ => None,
    });
    node.expect("a chunk puts a node").push(0x5a);
    forged
}

/// What a restore answered and left.
#[derive(Debug, PartialEq)]
struct Restored {
    /// What every call answered and, for a restore that finalized, what
    /// the store then holds: the root, the record of the root key, and
    /// every node the tree links as its record.
    said: Vec<String>,
    /// For a restore that finalized, how many nodes the tree links, and
    /// how many keys the cursor of the store shows that the tree does not
    /// link. The cursor shows the key of every node the tree links.
    finalized: Option<(usize, usize)>,
}

/// Restores the Merk that `chunks` are of into `store`, in the order the
/// restore asks for them, with calls a restore refuses among them.
fn restore<'db, S: NodeStore<'db>>(
    store: S,
    tree_type: TreeType,
    root_hash: CryptoHash,
    chunks: &BTreeMap<Vec<u8>, Vec<ProofOp>>,
    driven: Driven,
) -> Restored {
    let expected = match driven.parent {
        Some(value_hash) => combine_hash(&value_hash, &root_hash).unwrap(),
        None => root_hash,
    };
    let mut restorer = Restorer::new(open(store, tree_type), expected, driven.parent);
    let mut said = vec![format!("empty {:?}", restorer.expects_an_empty_tree())];
    let mut process = |restorer: &mut Restorer<S>, what: &str, id: &[u8], chunk: Vec<ProofOp>| {
        let answered = restorer.process_chunk(id, chunk, version());
        said.push(format!("{what} {id:?}: {answered:?}"));
        answered.unwrap_or_default()
    };
    // A chunk before the chunk above it.
    let last = chunks.keys().next_back().expect("a chunk");
    process(&mut restorer, "early", last, chunks[last].clone());
    let mut pending = vec![vec![]];
    let mut written = 0;
    while let Some(chunk_id) = pending.pop() {
        let chunk = &chunks[&chunk_id];
        // A chunk that does not hash to what is expected is refused, and
        // the right one is taken after it.
        if written % 5 == 1 {
            process(&mut restorer, "forged", &chunk_id, forged(chunk));
        }
        pending.extend(process(&mut restorer, "chunk", &chunk_id, chunk.clone()));
        // A chunk that was written is expected no more.
        if written % 7 == 2 {
            process(&mut restorer, "again", &chunk_id, chunk.clone());
        }
        written += 1;
    }
    assert_eq!(written, chunks.len());

    let finalized = match driven.elements {
        true => restorer.finalize_with_grovedb_elements(version()),
        false => restorer.finalize(version()),
    };
    let finalized = match finalized {
        Err(error) => {
            said.push(format!("not finalized: {error:?}"));
            None
        }
        Ok(merk) => {
            let root = merk.root_hash_key_and_aggregate_data().unwrap();
            said.push(format!("finalized: {root:?}"));
            said.push(format!("root key {:?}", merk.storage.root_key().unwrap()));
            let nodes = records(&merk.storage, merk.root_key());
            let record = |(key, record): &(Vec<u8>, Vec<u8>)| {
                format!("{} {}", hex::encode(key), hex::encode(record))
            };
            said.extend(nodes.iter().map(record));
            let linked: BTreeSet<&[u8]> = nodes.iter().map(|(key, _)| key.as_slice()).collect();
            let shown = walked(&merk.storage);
            let shown: BTreeSet<&[u8]> = shown.iter().map(|(key, _)| key.as_slice()).collect();
            assert!(shown.is_superset(&linked), "{tree_type:?}");
            Some((nodes.len(), shown.difference(&linked).count()))
        }
    };
    Restored { said, finalized }
}

/// Fails on the first call two restores answered differently.
fn assert_same_restore(found: &Restored, expected: &Restored, at: &str) {
    for (found, expected) in found.said.iter().zip(&expected.said) {
        assert_eq!(found, expected, "{at}");
    }
    assert_eq!(found.said.len(), expected.said.len(), "{at}");
    assert_eq!(found.finalized, expected.finalized, "{at}");
}

/// What a Merk on a deferred handle of each engine answers to `calls`
/// where a restore left it, at `place`: the answers, the cost of every
/// call and the cost of the batch are the same on both.
fn same_after(both: &Both<'_, '_, '_>, place: u8, calls: &[Act]) {
    let tree_type = both.tree_type;
    let name = [place];
    let path = [name.as_slice()];
    let storage_batch = StorageBatch::new();
    let context = both
        .rocks
        .get_transactional_storage_context(
            path.as_ref().into(),
            Some(&storage_batch),
            both.rocks_transaction,
        )
        .unwrap();
    let mut expected = acts(context, tree_type, calls);
    let applied = both
        .rocks
        .commit_multi_context_batch(storage_batch, Some(both.rocks_transaction));
    expected.push((format!("{:?}", applied.value), applied.cost));

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(both.writer, subtree_at(place), Some(&staged));
    let mut found = acts(store, tree_type, calls);
    let applied = staged.apply(both.writer);
    found.push((format!("{:?}", applied.value.map(|_| ())), applied.cost));
    for (at, (found, expected)) in found.iter().zip(&expected).enumerate() {
        assert_eq!(
            found, expected,
            "{tree_type:?}: call {at} after the restore"
        );
    }
    assert_eq!(found.len(), expected.len());
}

/// A transaction of each engine, with nothing written.
fn with_engines(tree_type: TreeType, test: impl FnOnce(&Both<'_, '_, '_>, &WriteTxn<'_>)) {
    let rocks = TempStorage::new();
    let lignin = Lignin::with_stated_value_hashes();
    let path = lignin.path();
    let rocks_transaction = rocks.start_transaction();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let both = Both {
        tree_type,
        rocks: &rocks,
        rocks_transaction: &rocks_transaction,
        writer: &writer,
    };
    test(&both, &transaction);
    rocks
        .commit_transaction(rocks_transaction)
        .unwrap()
        .expect("the transaction commits");
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
}

const SOURCE: u8 = 10;

/// Writes `nodes` to a Merk at [`SOURCE`] on both engines, and returns its
/// root hash and its chunks, which are the same from both.
fn source(
    both: &Both<'_, '_, '_>,
    nodes: &[(Vec<u8>, Op)],
) -> (CryptoHash, BTreeMap<Vec<u8>, Vec<ProofOp>>) {
    let tree_type = both.tree_type;
    let mut on_rocks = open(rocks_at(both, SOURCE), tree_type);
    let mut on_lignin = open(lignin_at(both, SOURCE), tree_type);
    apply(&mut on_rocks, nodes);
    apply(&mut on_lignin, nodes);
    let (root_hash, chunks) = (on_lignin.root_hash().unwrap(), chunks_of(&on_lignin));
    assert_eq!(root_hash, on_rocks.root_hash().unwrap(), "{tree_type:?}");
    assert_eq!(chunks, chunks_of(&on_rocks), "{tree_type:?}");
    (root_hash, chunks)
}

/// A restore into `merk` that has been given every chunk, in the order it
/// asks for them.
fn fed<'db, S: NodeStore<'db>>(
    merk: Merk<S>,
    root_hash: CryptoHash,
    chunks: &BTreeMap<Vec<u8>, Vec<ProofOp>>,
) -> Restorer<S> {
    let mut restorer = Restorer::new(merk, root_hash, None);
    let mut pending = vec![vec![]];
    while let Some(chunk_id) = pending.pop() {
        let chunk = chunks[&chunk_id].clone();
        pending.extend(restorer.process_chunk(&chunk_id, chunk, version()).unwrap());
    }
    restorer
}

/// Reads and writes of a Merk that a restore left, over its keys.
fn calls_after(tree_type: TreeType, nodes: &[(Vec<u8>, Op)]) -> Vec<Act> {
    let key = |n: usize| nodes[n % nodes.len()].0.clone();
    let prove = |call: Call| Act::Prove(version(), call);
    let mut written = BTreeMap::from([
        (key(5), Op::Put(vec![0xf7; 80], feature(tree_type, 4))),
        (key(11), Op::Delete),
        (
            vec![0xfd; 5],
            Op::Put(vec![0xf7; 400], feature(tree_type, 6)),
        ),
    ]);
    written.remove(&key(40));
    vec![
        prove(Call::Get(key(40), true)),
        prove(Call::Get(key(41), false)),
        prove(Call::Exists(vec![0xfc; 4])),
        prove(Call::Query(
            vec![grovedb_query::QueryItem::Key(key(40))],
            None,
            true,
        )),
        prove(Call::Chunk(vec![])),
        prove(Call::MultiChunk(vec![], None)),
        Act::Scan(true),
        Act::Apply(written.into_iter().collect()),
        prove(Call::Get(key(40), true)),
        Act::Scan(false),
    ]
}

/// The acceptance test of MERK-07 for restore: a restored tree has the
/// root hash it has on RocksDB. It also has the root key and every node
/// record a restore on RocksDB leaves, each call of the restore answers
/// what it answers there, and the Merk that is left is read and written at
/// the costs it has there.
#[test]
fn a_restored_merk_is_the_merk_a_restore_on_rocksdb_leaves() {
    let mut restored = BTreeSet::new();
    for tree_type in TREE_TYPES {
        // Values that are no elements, and values that are.
        for (of_elements, nodes) in [
            (false, raw_nodes(tree_type, 330)),
            (true, element_nodes(tree_type, 330)),
        ] {
            with_engines(tree_type, |both, _| {
                let (root_hash, chunks) = source(both, &nodes);
                assert!(chunks.len() > 30, "{tree_type:?}: {} chunks", chunks.len());
                let parent = Some([0x51; 32]);
                let ways = [(false, None), (false, parent), (true, None), (true, parent)];
                for (place, (elements, parent)) in ways.into_iter().enumerate() {
                    let (place, driven) = (20 + place as u8, Driven { elements, parent });
                    let expected =
                        restore(rocks_at(both, place), tree_type, root_hash, &chunks, driven);
                    let found = restore(
                        lignin_at(both, place),
                        tree_type,
                        root_hash,
                        &chunks,
                        driven,
                    );
                    let at = format!("{tree_type:?}: {driven:?} of elements: {of_elements}");
                    assert_same_restore(&found, &expected, &at);
                    let refused = |what: &str| -> usize {
                        let refusals = found.said.iter().filter(|said| said.starts_with(what));
                        refusals.filter(|said| said.contains("Err(")).count()
                    };
                    assert_eq!(refused("early"), 1, "{at}");
                    assert!(refused("forged") > 5 && refused("again") > 3, "{at}");
                    assert_eq!(refused("chunk"), 0, "{at}");

                    // Values that are no elements cannot be finalized as
                    // elements where their features are made from them,
                    // which is in every tree that has aggregates.
                    if elements && !of_elements && tree_type != TreeType::NormalTree {
                        assert_eq!(found.finalized, None, "{at}");
                    }
                    if let Some(linked_and_not) = found.finalized {
                        // The store holds the nodes of the tree, and no
                        // other record under a key of the Merk.
                        assert_eq!(linked_and_not, (nodes.len(), 0), "{at}");
                        restored.insert((format!("{tree_type:?}"), of_elements, elements));
                        same_after(both, place, &calls_after(tree_type, &nodes));
                    }
                }
            });
        }
    }
    // Every tree type was restored from values that are no elements and
    // from elements, each finalized as what it is.
    let each = |of_elements: bool| -> usize {
        let finalized = restored
            .iter()
            .filter(|(_, of, as_elements)| *of == of_elements && *as_elements == of_elements);
        finalized.count()
    };
    assert_eq!(
        (each(false), each(true)),
        (TREE_TYPES.len(), TREE_TYPES.len())
    );
}

/// A copy of `chunk` that hashes to what the original does and that the
/// restore of a tree with no aggregates refuses at a node: the last node
/// it puts says that it is a node of a sum tree. The restore comes to that
/// node after the hash of a chunk below.
fn foreign(chunk: &[ProofOp]) -> Vec<ProofOp> {
    let mut foreign = chunk.to_vec();
    let at = (foreign.iter())
        .rposition(|op| emitted_key(op).is_some())
        .expect("a chunk puts a node");
    let below = |op: &ProofOp| matches!(op, ProofOp::Push(Node::Hash(_)));
    assert!(foreign[..at].iter().any(below));
    let sum = crate::TreeFeatureType::SummedMerkNode(0);
    foreign[at] = match &foreign[at] {
        ProofOp::Push(Node::KV(key, value)) => {
            let hash = crate::tree::value_hash(value).unwrap();
            ProofOp::Push(Node::KVValueHashFeatureType(
                key.clone(),
                value.clone(),
                hash,
                sum,
            ))
        }
        ProofOp::Push(
            Node::KVValueHash(key, value, hash) | Node::KVValueHashFeatureType(key, value, hash, _),
        ) => ProofOp::Push(Node::KVValueHashFeatureType(
            key.clone(),
            value.clone(),
            *hash,
            sum,
        )),
        other => panic!("{other:?} is no node of a tree with no aggregates"),
    };
    foreign
}

/// What a restore of `chunks` into `store` answers, and the records it
/// leaves, when three chunks that have chunks below them are each given
/// first as their [`foreign`] copy, which is refused at a node, and then
/// as they are.
fn refused_at_a_node<'db, S: NodeStore<'db>>(
    store: S,
    root_hash: CryptoHash,
    chunks: &BTreeMap<Vec<u8>, Vec<ProofOp>>,
) -> Vec<String> {
    let mut restorer = Restorer::new(open(store, TreeType::NormalTree), root_hash, None);
    let mut said = Vec::new();
    let mut pending = vec![vec![]];
    let mut refused = 0;
    while let Some(chunk_id) = pending.pop() {
        let chunk = &chunks[&chunk_id];
        let below = |op: &&ProofOp| matches!(op, ProofOp::Push(Node::Hash(_)));
        if refused < 3 && chunk.iter().filter(below).count() > 1 {
            let answered = restorer.process_chunk(&chunk_id, foreign(chunk), version());
            assert!(
                matches!(
                    &answered,
                    Err(Error::ChunkRestoringError(ChunkError::InvalidChunkProof(what)))
                        if what.contains("does not belong to the tree type")
                ),
                "{answered:?}"
            );
            said.push(format!("foreign {chunk_id:?}: {answered:?}"));
            refused += 1;
        }
        let answered = restorer.process_chunk(&chunk_id, chunk.clone(), version());
        said.push(format!("chunk {chunk_id:?}: {answered:?}"));
        pending.extend(answered.expect("the chunk is taken"));
    }
    assert_eq!(refused, 3);
    let merk = restorer.finalize(version()).expect("the restore finishes");
    let root = merk.root_hash_key_and_aggregate_data().unwrap();
    said.push(format!("finalized: {root:?}"));
    let nodes = records(&merk.storage, merk.root_key());
    let record = |(key, record): &(Vec<u8>, Vec<u8>)| {
        format!("{} {}", hex::encode(key), hex::encode(record))
    };
    said.extend(nodes.iter().map(record));
    said
}

/// A chunk can hash to what a restore expects and still be refused at one
/// of its nodes, after the restore has come by the hash of a chunk below
/// it. The chunk as it is is taken afterwards, and the restore finishes
/// with the answers and the records it has on RocksDB.
#[test]
fn a_chunk_that_is_refused_at_a_node_is_taken_as_it_is_afterwards() {
    let tree_type = TreeType::NormalTree;
    let nodes = raw_nodes(tree_type, 330);
    with_engines(tree_type, |both, transaction| {
        let (root_hash, chunks) = source(both, &nodes);
        const TARGET: u8 = 41;
        let expected = refused_at_a_node(rocks_at(both, TARGET), root_hash, &chunks);
        let found = refused_at_a_node(lignin_at(both, TARGET), root_hash, &chunks);
        assert_eq!(found, expected);
        assert_eq!(held_at(both, transaction, TARGET).0, nodes.len());
    });
}

/// The nodes and the root of the Merk at `place`, as the store's own
/// records have them, and how many writes the transaction holds.
fn held_at(
    both: &Both<'_, '_, '_>,
    transaction: &WriteTxn<'_>,
    place: u8,
) -> (usize, Option<Vec<u8>>, Option<Vec<u8>>, usize) {
    let store = LigninNodeStore::deferred(both.writer, subtree_at(place), None);
    let entry = store.entry().unwrap().expect("the subtree is entered");
    let record = store.root_key().unwrap().unwrap();
    let nodes = walked(&store).len();
    (nodes, entry.root_key, record, transaction.stats().writes)
}

#[test]
fn the_store_holds_nothing_of_a_restore_before_it_is_finalized() {
    let tree_type = TreeType::CountSumTree;
    let nodes = raw_nodes(tree_type, 200);
    with_engines(tree_type, |both, transaction| {
        let (root_hash, chunks) = source(both, &nodes);
        const TARGET: u8 = 30;
        let before = held_at(both, transaction, TARGET);
        assert_eq!((before.0, &before.1, &before.2), (0, &None, &None));

        // Every chunk arrives, and the restore is given up.
        let mut restorer = Restorer::new(open(lignin_at(both, TARGET), tree_type), root_hash, None);
        let mut pending = vec![vec![]];
        while let Some(chunk_id) = pending.pop() {
            let chunk = chunks[&chunk_id].clone();
            pending.extend(restorer.process_chunk(&chunk_id, chunk, version()).unwrap());
            assert_eq!(held_at(both, transaction, TARGET), before);
        }
        drop(restorer);
        assert_eq!(held_at(both, transaction, TARGET), before);

        // A restore that is finalized too early is refused, and the store
        // is as it was.
        let mut restorer = Restorer::new(open(lignin_at(both, TARGET), tree_type), root_hash, None);
        let chunk = chunks[&vec![]].clone();
        restorer.process_chunk(&[], chunk, version()).unwrap();
        let refused = restorer.finalize(version()).map(|_| ());
        assert!(
            matches!(
                refused,
                Err(Error::ChunkRestoringError(
                    ChunkError::RestorationNotComplete
                ))
            ),
            "{refused:?}"
        );
        assert_eq!(held_at(both, transaction, TARGET), before);

        // The hand-over is one write of every node, and the record of the
        // root key. It reads nothing of the Merk for the values it states:
        // the reads of a finalize do not grow with the nodes.
        let said = restore(
            lignin_at(both, TARGET),
            tree_type,
            root_hash,
            &chunks,
            Driven {
                elements: false,
                parent: None,
            },
        );
        assert_eq!(said.finalized, Some((nodes.len(), 0)));
        let after = held_at(both, transaction, TARGET);
        assert_eq!(after.0, nodes.len());
        assert!(after.1.is_some() && after.1 == after.2, "{after:?}");
        assert_eq!(after.3 - before.3, nodes.len() + 1);
    });
}

#[test]
fn a_finalize_reads_no_node_to_hand_the_merk_over() {
    let tree_type = TreeType::NormalTree;
    with_engines(tree_type, |both, transaction| {
        let reads = |transaction: &WriteTxn<'_>| {
            let stats = transaction.stats();
            stats.keyed_reads + stats.handles_followed
        };
        let mut read_by_finalize = Vec::new();
        for (place, count) in [(31, 60), (32, 600)] {
            let nodes = raw_nodes(tree_type, count);
            let mut on_lignin = open(lignin_at(both, place + 10), tree_type);
            apply(&mut on_lignin, &nodes);
            let (root_hash, chunks) = (on_lignin.root_hash().unwrap(), chunks_of(&on_lignin));
            drop(on_lignin);

            let merk = open(lignin_at(both, place), tree_type);
            let restorer = fed(merk, root_hash, &chunks);
            let before = reads(transaction);
            let merk = restorer.finalize(version()).expect("restore finalizes");
            read_by_finalize.push(reads(transaction) - before);
            assert_eq!(merk.root_hash().unwrap(), root_hash);
        }
        // The root is read back, and the key of each of its two children
        // from the child's cell. No other node is read: ten times the
        // nodes take the same three reads.
        assert_eq!(read_by_finalize, [3, 3]);
    });
}

/// What the two engines do with a restore into a Merk that holds nodes is
/// not the same. RocksDB writes the chunks over what is there, and a node
/// the restored tree does not link stays as a record under its key. A
/// restore that hands the store a Merk whole is refused in `finalize`
/// where the store's cursor shows a node, before it makes a change set.
#[test]
fn a_restore_into_a_merk_that_holds_nodes_is_refused_at_finalize() {
    let tree_type = TreeType::NormalTree;
    let nodes = raw_nodes(tree_type, 100);
    with_engines(tree_type, |both, transaction| {
        let (root_hash, chunks) = source(both, &nodes);
        const TARGET: u8 = 33;
        let held = [(vec![0xee; 6], Op::Put(vec![0xf7; 9], feature(tree_type, 1)))];
        apply(&mut open(rocks_at(both, TARGET), tree_type), &held);
        apply(&mut open(lignin_at(both, TARGET), tree_type), &held);
        let driven = Driven {
            elements: false,
            parent: None,
        };

        let on_rocks = restore(
            rocks_at(both, TARGET),
            tree_type,
            root_hash,
            &chunks,
            driven,
        );
        assert_eq!(on_rocks.finalized, Some((nodes.len(), 1)));

        let before = held_at(both, transaction, TARGET);
        let merk = open(lignin_at(both, TARGET), tree_type);
        let restorer = fed(merk, root_hash, &chunks);
        let refused = restorer.finalize(version()).map(|_| ());
        assert!(
            matches!(&refused, Err(Error::NotSupported(what)) if what.contains("holds nodes")),
            "{refused:?}"
        );
        assert_eq!(held_at(both, transaction, TARGET), before);
        assert_eq!(before.0, 1);
    });
}

/// A change set that says it is the whole Merk and puts a node is taken
/// with no read of the nodes it names, so the store takes it only where
/// the Merk has no root, whoever states it.
#[test]
fn the_store_refuses_a_whole_merk_where_the_merk_has_a_root() {
    let tree_type = TreeType::NormalTree;
    with_engines(tree_type, |both, transaction| {
        const TARGET: u8 = 36;
        let node = |n: u64| TreeNode::new(be(n), vec![0xf7; 5], None, feature(tree_type, n));
        // A change set that says it is the whole Merk, of one node, which
        // may also remove a key.
        let whole = |store: &LigninNodeStore<'_>, n: u64, change: NodeChange, removes: bool| {
            let node = node(n).unwrap();
            let mut change_set = store.change_set();
            change_set.state_root(Some(node.key()));
            change_set.state_whole_merk();
            let prepared = change_set.prepare_node(&node, change);
            change_set.put_node(node.key(), prepared, None).unwrap();
            if removes {
                change_set.delete_node(&be(n + 1), None);
            }
            store.commit(change_set).unwrap()
        };
        let refused_as = |refused: Result<(), Error>, why: &str| {
            assert!(
                matches!(&refused, Err(Error::StorageError(what)) if what.to_string().contains(why)),
                "{refused:?}"
            );
        };
        let store = lignin_at(both, TARGET);
        let empty = held_at(both, transaction, TARGET);

        // It states every value and removes nothing.
        let kept = whole(&store, 1, NodeChange::HashOnly, false);
        refused_as(kept, "keeps a value or removes a node");
        let removing = whole(&store, 1, NodeChange::ValueWritten, true);
        refused_as(removing, "keeps a value or removes a node");
        assert_eq!(held_at(both, transaction, TARGET), empty);

        // A Merk with no root takes it.
        whole(&store, 1, NodeChange::ValueWritten, false)
            .expect("a Merk with no node takes a whole Merk");
        let held = held_at(both, transaction, TARGET);
        assert_eq!((held.0, &held.1), (1, &Some(be(1))));

        // A Merk with a root does not, and stays as it is.
        let over = whole(&store, 2, NodeChange::ValueWritten, false);
        refused_as(over, "has a root");
        assert_eq!(held_at(both, transaction, TARGET), held);
    });
}

/// A restore into a subtree that is not entered is refused by the handle
/// when the Merk is handed over, as a whole change set for a subtree that
/// is not entered, and leaves no write.
#[test]
fn a_restore_into_a_subtree_that_is_not_entered_is_refused_at_the_hand_over() {
    let tree_type = TreeType::NormalTree;
    let nodes = raw_nodes(tree_type, 60);
    with_engines(tree_type, |both, transaction| {
        let (root_hash, chunks) = source(both, &nodes);
        let writes = transaction.stats().writes;
        // No store of the tests is made with this subtree entered.
        let store = LigninNodeStore::immediate(both.writer, subtree_at(240));
        let restorer = fed(open(store, tree_type), root_hash, &chunks);
        let refused = restorer.finalize(version()).map(|_| ());
        assert!(
            matches!(&refused, Err(Error::StorageError(what)) if what.to_string().contains("for a subtree that is not entered")),
            "{refused:?}"
        );
        assert_eq!(transaction.stats().writes, writes);
        let store = LigninNodeStore::deferred(both.writer, subtree_at(240), None);
        assert_eq!(store.entry().unwrap(), None);
        assert_eq!(store.root_key().unwrap().unwrap(), None);
    });
}

/// A handle that stages its writes holds one outcome for each key, and no
/// change set as it was stated. It takes no change set that says it is the
/// whole Merk: the hand-over is refused, and the batch holds nothing of the
/// restore.
#[test]
fn a_restore_through_a_deferred_handle_is_refused_at_the_hand_over() {
    let tree_type = TreeType::NormalTree;
    let nodes = raw_nodes(tree_type, 60);
    with_engines(tree_type, |both, transaction| {
        let (root_hash, chunks) = source(both, &nodes);
        const TARGET: u8 = 37;
        let before = held_at(both, transaction, TARGET);
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(both.writer, subtree_at(TARGET), Some(&staged));
        let restorer = fed(open(store, tree_type), root_hash, &chunks);
        assert!(staged.is_empty());
        let refused = restorer.finalize(version()).map(|_| ());
        assert!(
            matches!(
                &refused,
                Err(Error::StorageError(what))
                    if what.to_string().contains("through a handle that stages its writes")
            ),
            "{refused:?}"
        );
        assert!(staged.is_empty());
        assert_eq!(held_at(both, transaction, TARGET), before);
    });
}

/// A restore on an immediate handle of a write transaction, which opens a
/// scope for each of its writes: it finishes with the root hash of its
/// source and leaves no scope open, and a second whole change set for the
/// subtree is refused as for a Merk that has a root. With a writer's scope
/// of the transaction held open, so that the handle's scope for the write
/// opens beside it, a restore into a subtree that is not entered is
/// refused and writes nothing, and so is one through a second writer. The
/// database that is committed is sound.
#[test]
fn a_restore_on_a_write_transaction_finishes_and_leaves_no_scope_open() {
    let tree_type = TreeType::NormalTree;
    let facade = Facade::new();
    let transaction = facade.db.begin();
    let at = |place: u8| LigninSubtree::alone([place; 32]);
    // Values that are no elements: the mark, and bytes after it.
    let mut nodes: Vec<(Vec<u8>, Op)> = (0..330u64)
        .map(|n| {
            let mut value = vec![0xf7];
            value.resize(1 + (n % 90) as usize, n as u8);
            (scattered_key(n), Op::Put(value, feature(tree_type, n)))
        })
        .collect();
    nodes.sort_by(|a, b| a.0.cmp(&b.0));

    let source = LigninNodeStore::immediate(&transaction, at(1));
    source.enter(tree_type).expect("the subtree is entered");
    let mut source = open(source, tree_type);
    apply(&mut source, &nodes);
    let (root_hash, chunks) = (source.root_hash().unwrap(), chunks_of(&source));
    drop(source);

    let target = LigninNodeStore::immediate(&transaction, at(2));
    target.enter(tree_type).expect("the subtree is entered");
    let restorer = fed(open(target, tree_type), root_hash, &chunks);
    let merk = restorer.finalize(version()).expect("restore finalizes");
    assert!(!transaction.scopes_open());
    assert_eq!(merk.root_hash().unwrap(), root_hash);
    let root_key = merk.root_key();
    drop(merk);

    let again = LigninNodeStore::immediate(&transaction, at(2));
    let node = TreeNode::new(be(1), vec![0xf7; 5], None, feature(tree_type, 1)).unwrap();
    let mut change_set = again.change_set();
    change_set.state_root(Some(node.key()));
    change_set.state_whole_merk();
    let prepared = change_set.prepare_node(&node, NodeChange::ValueWritten);
    change_set.put_node(node.key(), prepared, None).unwrap();
    let refused = again.commit(change_set).unwrap();
    assert!(
        matches!(&refused, Err(Error::StorageError(what)) if what.to_string().contains("has a root")),
        "{refused:?}"
    );
    assert!(!transaction.scopes_open());
    assert_eq!(again.root_key().unwrap().expect("a read"), root_key);
    assert_eq!(records(&again, root_key).len(), nodes.len());
    drop(again);

    let beside = LigninWriter::new(transaction.scope().expect("a scope"));
    let writes = transaction.stats().writes;
    let not_entered = |refused: Result<(), Error>| {
        assert!(
            matches!(
                &refused,
                Err(Error::StorageError(what))
                    if what.to_string().contains("for a subtree that is not entered")
            ),
            "{refused:?}"
        );
    };
    let nowhere = LigninNodeStore::immediate(&transaction, at(9));
    let restorer = fed(open(nowhere, tree_type), root_hash, &chunks);
    not_entered(restorer.finalize(version()).map(|_| ()));
    // The same through a second writer, whose scope is open beside the
    // first one's.
    let second = LigninWriter::new(transaction.scope().expect("a scope"));
    let nowhere = LigninNodeStore::immediate(&second, at(9));
    let restorer = fed(open(nowhere, tree_type), root_hash, &chunks);
    not_entered(restorer.finalize(version()).map(|_| ()));
    second.into_scope().merge();
    beside.into_scope().merge();
    assert_eq!(transaction.stats().writes, writes);
    facade.commit(&transaction);
    facade.sound();
}

/// A Merk under an element is restored as one with no host is: the store
/// is handed it whole, and names its root for the directory.
#[test]
fn a_merk_under_an_element_is_restored_whole() {
    let tree_type = TreeType::SumTree;
    let nodes = element_nodes(tree_type, 150);
    with_engines(tree_type, |both, _| {
        let (root_hash, chunks) = source(both, &nodes);
        let parent = subtree_at(34);
        let child = LigninSubtree {
            prefix: [223; 32],
            host: Some(LigninHost {
                subtree: parent.prefix,
                key: be(50),
                hosted: vec![[223; 32]],
            }),
        };
        // The element is written first, as a restore of a grove writes
        // the Merk above before the Merk below, and enters its Merk.
        let staged = LigninBatch::new();
        staged.enter_tree(&child, tree_type).unwrap();
        let upper = LigninNodeStore::deferred(both.writer, parent.clone(), Some(&staged));
        let mut upper = open_layered(upper, None);
        apply(&mut upper, &[basic(50, b"a tree")]);
        drop(upper);
        staged
            .apply(both.writer)
            .unwrap()
            .expect("the batch is applied");

        let driven = Driven {
            elements: true,
            parent: Some([0x52; 32]),
        };
        const ON_ROCKS: u8 = 35;
        let expected = restore(
            rocks_at(both, ON_ROCKS),
            tree_type,
            root_hash,
            &chunks,
            driven,
        );
        let store = LigninNodeStore::immediate(both.writer, child.clone());
        let found = restore(store, tree_type, root_hash, &chunks, driven);
        assert_same_restore(&found, &expected, "under an element");
        assert_eq!(found.finalized, Some((nodes.len(), 0)));

        // The directory names the root, and the Merk opens from it.
        let store = LigninNodeStore::deferred(both.writer, child.clone(), None);
        let entry = store.entry().unwrap().expect("the subtree is entered");
        let opened = Merk::open_from_directory(store, NO_VALUE_DEFINED_COST, version());
        let opened = opened.unwrap().unwrap().expect("a Merk is there");
        assert_eq!(opened.root_hash().unwrap(), root_hash);
        assert_eq!(opened.root_key(), entry.root_key);
        assert_eq!(opened.tree_type, tree_type);
        drop(opened);

        // The hand-over wrote the Merk and not its host. A batch that then
        // writes the host, with a commit of the Merk that changes nothing,
        // is applied.
        let upper = LigninNodeStore::deferred(both.writer, parent.clone(), Some(&staged));
        let mut upper = open_layered(upper, Some(be(50)));
        apply(&mut upper, &[basic(50, b"a tree with a root")]);
        drop(upper);
        let lower = LigninNodeStore::deferred(both.writer, child.clone(), Some(&staged));
        let lower = Merk::open_from_directory(lower, NO_VALUE_DEFINED_COST, version());
        let mut lower = lower.unwrap().unwrap().expect("a Merk is there");
        apply(&mut lower, &[]);
        assert_eq!(lower.root_hash().unwrap(), root_hash);
        drop(lower);
        staged
            .apply(both.writer)
            .unwrap()
            .expect("the batch is applied");
    });
}
