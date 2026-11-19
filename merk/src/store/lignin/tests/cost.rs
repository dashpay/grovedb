//! What the store charges (MERK-03): every charge of the LigninDB path is
//! the one RocksDB makes for the same event, and comes from the cost model.
//!
//! - A Merk on the store and a Merk on RocksDB are opened, written, read
//!   and proved with the same calls, on every grove version, and every
//!   call costs the same on both.
//! - Each operation of the seam is made on a handle of each engine, with
//!   the writes Merk's own operations never issue among them, and is
//!   charged the same.
//! - The costs the store reports add up to what the cost model handed out
//!   (`cost-model.md` 14.2).

use std::{collections::HashMap, fmt};

use ed::Encode;
use grovedb_costs::{
    cost_model::ledger,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{
            use_basic_sectioned_removal_addition_version, StorageRemovalPerEpochByIdentifier,
            StorageRemovedBytes,
        },
        transition::ElementFlagsUpdate,
        StorageCost,
    },
    CostResult,
};
use grovedb_query::QueryItem;
use grovedb_storage::{rocksdb_storage::RocksDbStorage, Error as StorageError, StorageContext};
use grovedb_version::version::GROVE_VERSIONS;

use super::*;
use crate::{
    cost_model::HANDLE_COST_MODEL,
    proofs::{query::count_offset::ProverCountOffsetResult, Query},
    store::NodeWriteCost,
    tree::kv::KV,
    tree::tree_feature_type::NodeType,
};

type Tx<'db> = <RocksDbStorage as Storage<'db>>::Transaction;

type CostFn = fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>;

const SPECIALIZED: u32 = 20;
const LAYERED: u32 = 30;

/// The cost a value defines, by its first byte. It stands in for the
/// elements of GroveDB, which Merk does not know: `S` for a value whose
/// cost is fixed, as a sum item's is, and `T` for one that is charged as a
/// tree.
fn value_defined_cost(value: &[u8], _: &GroveVersion) -> Option<ValueDefinedCostType> {
    match value.first() {
        Some(b'S') => Some(ValueDefinedCostType::SpecializedValueDefinedCost(
            SPECIALIZED,
        )),
        Some(b'T') => Some(ValueDefinedCostType::LayeredValueDefinedCost(LAYERED)),
        _ => None,
    }
}

const VALUE_DEFINED_COST: Option<CostFn> = Some(value_defined_cost);

/// What a node of a tree of `node_type` costs as it is stored, from its key
/// and its value: what a client gives Merk to bill a node against.
fn stored_cost(key: &[u8], value: &[u8], node_type: NodeType) -> Result<u32, Error> {
    let key_len = key.len() as u32;
    Ok(match value.first() {
        Some(b'S') => KV::node_value_byte_cost_size(key_len, SPECIALIZED, node_type),
        Some(b'T') => {
            KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, LAYERED, node_type)
        }
        _ => KV::node_value_byte_cost_size(key_len, value.len() as u32, node_type),
    })
}

/// The flags of a value that carries any: `F`, then the epoch it was
/// written in.
fn epoch(value: &[u8]) -> Option<u8> {
    match value {
        [b'F', epoch, ..] => Some(*epoch),
        _ => None,
    }
}

/// The flags callback of a client: a value that replaces one with flags
/// keeps the epoch of the value it replaces.
fn keep_the_epoch(
    old: &[u8],
    new: &mut [u8],
) -> Result<(ElementFlagsUpdate, Option<ValueDefinedCostType>), Error> {
    match (epoch(old), epoch(new)) {
        (Some(old), Some(new_epoch)) if old != new_epoch => {
            new[1] = old;
            Ok((ElementFlagsUpdate::Changed, None))
        }
        _ => Ok((ElementFlagsUpdate::Unchanged, None)),
    }
}

/// The removal callback of a client: the bytes a value with flags frees
/// are removed by the owner and epoch its flags name.
fn section_by_epoch(
    old: &[u8],
    key_bytes: u32,
    value_bytes: u32,
) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
    Ok(match epoch(old) {
        Some(epoch) => (sectioned(epoch, key_bytes), sectioned(epoch, value_bytes)),
        None => (
            StorageRemovedBytes::BasicStorageRemoval(key_bytes),
            StorageRemovedBytes::BasicStorageRemoval(value_bytes),
        ),
    })
}

fn sectioned(epoch: u8, bytes: u32) -> StorageRemovedBytes {
    let mut by_owner = StorageRemovalPerEpochByIdentifier::default();
    by_owner
        .entry([epoch % 3; 32])
        .or_default()
        .insert(u16::from(epoch), bytes);
    StorageRemovedBytes::SectionedStorageRemoval(by_owner)
}

/// What a call ended in, in a form two engines can be compared by.
#[derive(Clone, Debug, PartialEq)]
enum Ended {
    Done,
    Held(bool),
    Found(Option<Vec<u8>>),
    /// A commit whose change set refused this many puts as it was filled.
    Committed {
        refused: usize,
    },
    Failed(&'static str),
}

fn failed(error: Error) -> Ended {
    Ended::Failed(match error {
        Error::StorageError(StorageError::CostError(_)) | Error::CostsError(_) => "cost",
        Error::StorageError(_) => "storage",
        Error::KeyNotFoundError(_) => "key not found",
        Error::EdError(_) => "encoding",
        _ => "other",
    })
}

fn outcome<T>(result: Result<T, Error>, found: impl FnOnce(T) -> Ended) -> Ended {
    result.map_or_else(failed, found)
}

fn done(result: CostResult<(), Error>) -> (OperationCost, Ended) {
    (result.cost, outcome(result.value, |()| Ended::Done))
}

fn record(result: CostResult<Option<Vec<u8>>, Error>) -> (OperationCost, Ended) {
    (result.cost, outcome(result.value, Ended::Found))
}

/// What a key of the comparison holds: the kind of its value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Plain,
    Flagged,
    Specialized,
    Layered,
}

fn value_of(kind: Kind, n: u64) -> Vec<u8> {
    let payload = value(n, true);
    let mut value = match kind {
        Kind::Plain => vec![b'P'],
        Kind::Flagged => vec![b'F', (n % 5) as u8],
        Kind::Specialized => vec![b'S'],
        Kind::Layered => vec![b'T'],
    };
    value.extend(payload);
    value
}

fn put_of(tree_type: TreeType, kind: Kind, n: u64) -> Op {
    let (value, feature) = (value_of(kind, n), feature(tree_type, n));
    match kind {
        Kind::Plain if n.is_multiple_of(9) => {
            Op::PutCombinedReference(value, [n as u8; 32], feature)
        }
        Kind::Plain | Kind::Flagged => Op::Put(value, feature),
        Kind::Specialized => Op::PutWithSpecializedCost(value, SPECIALIZED, feature),
        Kind::Layered if n.is_multiple_of(2) => {
            Op::PutLayeredReference(value, LAYERED, [n as u8; 32], feature)
        }
        Kind::Layered => Op::ReplaceLayeredReference(value, LAYERED, [n as u8; 32], feature),
    }
}

fn delete_of(kind: Kind, n: u64) -> Op {
    match (kind, n % 2) {
        (Kind::Plain | Kind::Flagged, _) => Op::Delete,
        (Kind::Specialized, _) => Op::DeleteMaybeSpecialized,
        (Kind::Layered, 0) => Op::DeleteLayered,
        (Kind::Layered, _) => Op::DeleteLayeredMaybeSpecialized,
    }
}

/// One batch over a small key space: new keys, values of another kind put
/// over what a key holds, values put again as they are, and removals, in
/// key order.
fn mixed_batch(
    random: &mut Random,
    tree_type: TreeType,
    model: &mut BTreeMap<u64, (Kind, u64)>,
) -> Vec<(Vec<u8>, Op)> {
    let mut batch = BTreeMap::new();
    for _ in 0..1 + random.below(16) {
        let k = random.below(120);
        let op = match (model.get(&k).copied(), random.below(10)) {
            (Some((kind, n)), 0..=2) => {
                model.remove(&k);
                delete_of(kind, n)
            }
            (Some((kind, n)), 3) => put_of(tree_type, kind, n),
            _ => {
                let kind = match random.below(8) {
                    0 | 1 => Kind::Flagged,
                    2 => Kind::Specialized,
                    3 => Kind::Layered,
                    _ => Kind::Plain,
                };
                let n = random.below(1 << 40);
                model.insert(k, (kind, n));
                put_of(tree_type, kind, n)
            }
        };
        batch.insert(key(k), op);
    }
    batch.into_iter().collect()
}

fn apply_as_a_client<'db, S: NodeStore<'db>>(
    merk: &mut Merk<S>,
    batch: &[(Vec<u8>, Op)],
    aux: &[(Vec<u8>, Op, Option<KeyValueStorageCost>)],
    grove_version: &GroveVersion,
) -> (OperationCost, Ended) {
    let node_type = merk.tree_type.inner_node_type();
    done(merk.apply_with_costs_just_in_time_value_update(
        batch,
        aux,
        None,
        &|key, value| stored_cost(key, value, node_type),
        VALUE_DEFINED_COST.as_ref(),
        &|_old, _new| Ok(None),
        &mut |_cost, old, new| keep_the_epoch(old, new),
        &mut |old, key_bytes, value_bytes| section_by_epoch(old, key_bytes, value_bytes),
        grove_version,
    ))
}

/// How the Merk of a comparison is opened.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Opened {
    /// With its root key in a record of its own.
    Base,
    /// With its root key given, as under a parent element.
    Layered,
    /// With its root key in a record of its own that it gives a cost for.
    Standalone,
}

fn open_as<'db, S: NodeStore<'db>>(
    store: S,
    opened: Opened,
    root: &Option<Vec<u8>>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> (OperationCost, Merk<S>) {
    let merk = match opened {
        Opened::Base => Merk::open_base(store, tree_type, VALUE_DEFINED_COST, grove_version),
        Opened::Layered => Merk::open_layered_with_root_key(
            store,
            root.clone(),
            tree_type,
            VALUE_DEFINED_COST,
            grove_version,
        ),
        Opened::Standalone => {
            Merk::open_standalone(store, tree_type, VALUE_DEFINED_COST, grove_version)
        }
    };
    (merk.cost, merk.value.expect("the Merk opens"))
}

/// The meta record a round of the comparison writes, reads or removes, with
/// what that cost.
fn meta_as_a_client<'db, S: NodeStore<'db>>(
    merk: &mut Merk<S>,
    round: usize,
) -> (OperationCost, String) {
    let key = b"meta".to_vec();
    match round % 4 {
        0 => {
            let put = merk.put_meta(key, vec![round as u8; round % 23]);
            (put.cost, format!("{:?}", put.value.is_ok()))
        }
        3 => {
            let deleted = merk.delete_meta(&key);
            (deleted.cost, format!("{:?}", deleted.value.is_ok()))
        }
        _ => {
            let read = merk.get_meta(key);
            (read.cost, format!("{:?}", read.value))
        }
    }
}

/// What a read gave, with what it cost.
fn told<T: fmt::Debug>(read: CostResult<T, Error>) -> (OperationCost, String) {
    (read.cost, format!("{:?}", read.value))
}

/// The range between two keys, in whichever order they come.
fn between(a: &[u8], b: &[u8]) -> QueryItem {
    QueryItem::RangeInclusive(a.min(b).to_vec()..=a.max(b).to_vec())
}

/// How many reads [`read_as_a_client`] makes of the tree, and then of each
/// key.
const TREE_READS: usize = 16;
const KEY_READS: usize = 9;

/// Which kinds of read were charged a seek.
type Charged = [bool; TREE_READS + KEY_READS];

/// Marks in `charged` the kinds of `reads` that were charged a seek.
fn mark_charged(charged: &mut Charged, reads: &[(OperationCost, String)]) {
    for (n, (cost, _)) in reads.iter().enumerate() {
        let kind = match n.checked_sub(TREE_READS) {
            None => n,
            Some(of_a_key) => TREE_READS + of_a_key % KEY_READS,
        };
        charged[kind] |= cost.seek_count > 0;
    }
}

/// Every read of a Merk the comparison makes, each with what it cost. Of
/// the tree: an aux record, a proof, the aggregates of a range with their
/// proofs, which a tree type that keeps none of the kind refuses, the
/// query of a trunk, the proofs of a page that is skipped to by count, and
/// the root hash. Then, of each of the four `keys`, the reads by key and
/// the query of a branch.
///
/// A read that walks the tree is charged the fetches of the nodes no read
/// before it loaded. So the reads of the tree are made in another order
/// with every `turn`, and each of them is the first read of a Merk in one
/// turn of sixteen.
fn read_as_a_client<'db, S: NodeStore<'db>>(
    merk: &Merk<S>,
    keys: &[Vec<u8>; 4],
    turn: usize,
    grove_version: &GroveVersion,
) -> Vec<(OperationCost, String)> {
    let [a, b, c, d] = keys;
    let mut query = Query::new();
    query.insert_item(between(a, d));
    let page =
        |proved: ProverCountOffsetResult| (proved.ops, proved.returned, proved.offset_remaining);
    let of_the_tree: [&dyn Fn() -> (OperationCost, String); TREE_READS] = [
        &|| told(merk.get_aux(b"aux")),
        &|| {
            let proved = merk.prove(query.clone(), Some(7), grove_version);
            told(proved.map_ok(|proved| proved.proof))
        },
        &|| told(merk.count_aggregate_on_range(&between(a, b), grove_version)),
        &|| told(merk.sum_aggregate_on_range(&between(b, c), grove_version)),
        &|| told(merk.count_and_sum_aggregate_on_range(&between(c, d), grove_version)),
        &|| told(merk.big_sum_aggregate_on_range(&between(a, c), grove_version)),
        &|| told(merk.count_and_big_sum_aggregate_on_range(&between(b, d), grove_version)),
        &|| told(merk.prove_aggregate_count_on_range(&between(c, d), grove_version)),
        &|| told(merk.prove_aggregate_sum_on_range(&between(a, b), grove_version)),
        &|| told(merk.prove_aggregate_count_and_sum_on_range(&between(b, c), grove_version)),
        &|| told(merk.prove_aggregate_big_sum_on_range(&between(b, d), grove_version)),
        &|| told(merk.prove_aggregate_count_and_big_sum_on_range(&between(a, c), grove_version)),
        &|| told(merk.trunk_query(3, None, grove_version)),
        &|| {
            let range = between(a, d);
            let proved = merk.prove_count_offset_on_range(&range, 1, Some(3), true, grove_version);
            told(proved.map_ok(page))
        },
        &|| {
            let range = between(b, c);
            let proved =
                merk.prove_local_count_offset_on_range(&range, 1, Some(2), false, grove_version);
            told(proved.map_ok(page))
        },
        &|| {
            let hash = merk.root_hash();
            (hash.cost, format!("{:?}", hash.value))
        },
    ];
    let mut reads: Vec<_> = (0..TREE_READS)
        .map(|n| (n + turn) % TREE_READS)
        .map(|kind| (kind, of_the_tree[kind]()))
        .collect();
    reads.sort_by_key(|(kind, _)| *kind);
    let mut reads: Vec<_> = reads.into_iter().map(|(_, read)| read).collect();

    let cost_fn = VALUE_DEFINED_COST;
    for (n, key) in keys.iter().enumerate() {
        let cached = n.is_multiple_of(2);
        let of_the_key: [_; KEY_READS] = [
            told(merk.get(key, cached, cost_fn, grove_version)),
            told(merk.exists(key, cost_fn, grove_version)),
            told(merk.exists_by_traversing_tree(key, cost_fn, grove_version)),
            told(merk.get_feature_type(key, !cached, cost_fn, grove_version)),
            told(merk.get_hash(key, cached, cost_fn, grove_version)),
            told(merk.get_value_hash(key, !cached, cost_fn, grove_version)),
            told(merk.get_kv_hash(key, cached, cost_fn, grove_version)),
            told(merk.get_value_and_value_hash(key, !cached, cost_fn, grove_version)),
            told(merk.branch_query(key, 2, grove_version)),
        ];
        reads.extend(of_the_key);
    }
    reads
}

/// The same Merk on RocksDB and on LigninDB, written by the same batches:
/// every call costs the same on both.
fn a_merk_costs_the_same(
    grove_version: &GroveVersion,
    tree_type: TreeType,
    opened: Opened,
    immediate: bool,
    seed: u64,
    rounds: usize,
    charged: &mut Charged,
) {
    let at = |round: usize, what: &str| {
        format!(
            "{what}: grove version {}, {tree_type:?}, {opened:?}, immediate {immediate}, seed \
             {seed}, round {round}",
            grove_version.protocol_version
        )
    };
    let _arithmetic = use_basic_sectioned_removal_addition_version(
        grove_version
            .grovedb_versions
            .storage_costs
            .add_basic_storage_removal_to_sectioned_storage_removal,
    );
    let prefix = [7; 32];
    let subtree = LigninSubtree::alone(prefix);
    let rocks = TempStorage::new();
    let lignin = Lignin::new();
    let path = lignin.path();
    let mut random = Random(seed);
    let mut model = BTreeMap::new();
    let mut root: Option<Vec<u8>> = None;
    let aux_of = |round: usize| -> Vec<(Vec<u8>, Op, Option<KeyValueStorageCost>)> {
        let put = Op::Put(vec![round as u8; round % 9], TreeFeatureType::BasicMerkNode);
        match round % 4 {
            0 => vec![(b"aux".to_vec(), put, None)],
            1 => vec![(b"aux".to_vec(), Op::Delete, None)],
            _ => Vec::new(),
        }
    };

    if immediate {
        let rocks_tx = rocks.start_transaction();
        let context = rocks
            .get_immediate_storage_context_by_subtree_prefix(prefix, &rocks_tx)
            .unwrap();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let store = LigninNodeStore::immediate(&writer, subtree.clone());
        let (expected, mut on_rocks) = open_as(context, opened, &root, tree_type, grove_version);
        let (cost, mut on_lignin) = open_as(store, opened, &root, tree_type, grove_version);
        assert_eq!(cost, expected, "{}", at(0, "the open"));
        for round in 1..=rounds {
            let batch = mixed_batch(&mut random, tree_type, &mut model);
            let aux = aux_of(round);
            let expected = apply_as_a_client(&mut on_rocks, &batch, &aux, grove_version);
            let found = apply_as_a_client(&mut on_lignin, &batch, &aux, grove_version);
            assert_eq!(found, expected, "{}", at(round, "the batch"));
            assert_eq!(
                meta_as_a_client(&mut on_lignin, round),
                meta_as_a_client(&mut on_rocks, round),
                "{}",
                at(round, "the meta record")
            );
            let keys = [(); 4].map(|()| key(random.below(120)));
            assert_eq!(
                read_as_a_client(&on_lignin, &keys, round, grove_version),
                read_as_a_client(&on_rocks, &keys, round, grove_version),
                "{}",
                at(round, "the reads")
            );
        }
        return;
    }

    let mut transaction = path.begin();
    let mut writer = LigninWriter::new(transaction.scope().unwrap());
    let mut rocks_tx = rocks.start_transaction();
    for round in 1..=rounds {
        let batch = mixed_batch(&mut random, tree_type, &mut model);
        let aux = aux_of(round);

        // The Merk is opened, written and committed into a batch.
        let storage_batch = StorageBatch::new();
        let context = rocks
            .get_transactional_storage_context_by_subtree_prefix(
                prefix,
                Some(&storage_batch),
                &rocks_tx,
            )
            .unwrap();
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
        let (expected, mut on_rocks) = open_as(context, opened, &root, tree_type, grove_version);
        let (cost, mut on_lignin) = open_as(store, opened, &root, tree_type, grove_version);
        assert_eq!(cost, expected, "{}", at(round, "the open"));
        let expected = apply_as_a_client(&mut on_rocks, &batch, &aux, grove_version);
        let found = apply_as_a_client(&mut on_lignin, &batch, &aux, grove_version);
        assert_eq!(found, expected, "{}", at(round, "the batch"));
        assert_eq!(
            meta_as_a_client(&mut on_lignin, round),
            meta_as_a_client(&mut on_rocks, round),
            "{}",
            at(round, "the meta record")
        );
        root = on_rocks.root_key();
        assert_eq!(on_lignin.root_key(), root, "{}", at(round, "the root key"));
        drop((on_rocks, on_lignin));

        // The batch is applied.
        let expected = rocks.commit_multi_context_batch(storage_batch, Some(&rocks_tx));
        let found = staged.apply(&writer);
        assert_eq!(
            found.value.is_ok(),
            expected.value.is_ok(),
            "{}",
            at(round, "the applied batch")
        );
        assert_eq!(
            found.cost,
            expected.cost,
            "{}",
            at(round, "the cost of the applied batch")
        );

        // Every third round is committed, so that reads and writes meet
        // nodes of the committed state and nodes of the transaction.
        if round.is_multiple_of(3) {
            rocks.commit_transaction(rocks_tx).unwrap().unwrap();
            rocks_tx = rocks.start_transaction();
            writer.into_scope().merge();
            assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
            transaction = path.begin();
            writer = LigninWriter::new(transaction.scope().unwrap());
        }

        // A Merk that was opened for the read fetches what it reads.
        let context = rocks
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, &rocks_tx)
            .unwrap();
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), None);
        let (expected, mut on_rocks) = open_as(context, opened, &root, tree_type, grove_version);
        let (cost, mut on_lignin) = open_as(store, opened, &root, tree_type, grove_version);
        assert_eq!(cost, expected, "{}", at(round, "the open for a read"));
        assert_eq!(
            meta_as_a_client(&mut on_lignin, 1),
            meta_as_a_client(&mut on_rocks, 1),
            "{}",
            at(round, "the meta record that is read")
        );
        let keys = [(); 4].map(|()| key(random.below(120)));
        let found = read_as_a_client(&on_lignin, &keys, round, grove_version);
        assert_eq!(
            found,
            read_as_a_client(&on_rocks, &keys, round, grove_version),
            "{}",
            at(round, "the reads")
        );
        mark_charged(charged, &found);
    }
}

#[test]
fn a_merk_on_lignindb_costs_what_it_costs_on_rocksdb_on_every_grove_version() {
    let mut charged = [false; TREE_READS + KEY_READS];
    for (v, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        for (n, tree_type) in TREE_TYPES.into_iter().enumerate() {
            let seed = 0x5851_F42D + (v * 16 + n) as u64;
            // Every tree type is written in each way on every grove
            // version, in turn as a base Merk, a layered one and one that
            // stands alone.
            let opened = match (v + n) % 3 {
                0 => Opened::Base,
                1 => Opened::Layered,
                _ => Opened::Standalone,
            };
            for (immediate, rounds) in [(false, 18), (true, 12)] {
                a_merk_costs_the_same(
                    grove_version,
                    tree_type,
                    opened,
                    immediate,
                    seed,
                    rounds,
                    &mut charged,
                );
            }
        }
    }
    // Each kind of read was charged a fetch of its own in some round, but
    // for the root hash, which reads nothing.
    let mut expected = [true; TREE_READS + KEY_READS];
    expected[TREE_READS - 1] = false;
    assert_eq!(charged, expected);
}

fn at(n: u8) -> Prefix {
    [n; 32]
}

/// A node with no child, so that a Merk of it alone is a whole tree.
#[derive(Clone, Debug)]
struct Lone {
    key: Vec<u8>,
    value: Vec<u8>,
    feature: TreeFeatureType,
}

impl Lone {
    fn node(&self) -> TreeNode {
        TreeNode::new(self.key.clone(), self.value.clone(), None, self.feature).unwrap()
    }
}

fn lone(key: &[u8], value_len: usize, feature: TreeFeatureType) -> Lone {
    Lone {
        key: key.to_vec(),
        value: vec![key.len() as u8; value_len],
        feature,
    }
}

fn plain(key: &[u8], value_len: usize) -> Lone {
    lone(key, value_len, TreeFeatureType::BasicMerkNode)
}

/// The bytes a writer says a write frees.
#[derive(Clone, Copy, Debug)]
enum Freed {
    Nothing,
    Plain(u32),
    Sectioned(u8, u32),
}

impl Freed {
    fn bytes(self) -> StorageRemovedBytes {
        match self {
            Freed::Nothing => StorageRemovedBytes::NoStorageRemoval,
            Freed::Plain(bytes) => StorageRemovedBytes::BasicStorageRemoval(bytes),
            Freed::Sectioned(epoch, bytes) => sectioned(epoch, bytes),
        }
    }
}

/// The cost a writer gives with the put of a node.
#[derive(Clone, Copy, Debug)]
enum Bill {
    /// None: a write Merk does not charge, as chunk restore makes.
    Unbilled,
    /// A new node, whose value's cost is checked against the record.
    New,
    /// An update that replaces as many bytes as it writes, checked.
    Replaced,
    /// A cost that is taken as given: added, replaced and freed bytes.
    Given(u32, u32, Freed),
    /// A checked cost that does not match the record.
    Mismatched,
    /// A put that was paid for before.
    Prepaid,
}

/// What a put of `node` is to pay for its value: what the storage layer
/// charges a put of its record that comes with no cost, less the stored
/// key (`cost-model.md` K1, K2).
fn paid_value_len(node: &TreeNode) -> u32 {
    let stored_key_len = HANDLE_COST_MODEL.stored_key_len(node.key().len());
    let sizes = (
        node.feature_type()
            .tree_feature_specialized_type_and_length(),
        node.child_ref_and_sum_size(true),
        node.child_ref_and_sum_size(false),
    );
    let mut unbilled = OperationCost::default();
    unbilled
        .add_key_value_storage_costs(
            stored_key_len as u32,
            node.encoding_length() as u32,
            Some(sizes),
            None,
        )
        .expect("a put with no cost of the writer's");
    let new_key = KeyValueStorageCost {
        new_node: true,
        ..Default::default()
    };
    let key_cost = (HANDLE_COST_MODEL.complete_new_key_cost(Some(new_key), stored_key_len))
        .expect("a cost stays a cost")
        .key_storage_cost
        .added_bytes;
    unbilled.storage_cost.added_bytes - key_cost
}

fn billed(node: &TreeNode, bill: Bill) -> Option<NodeWriteCost> {
    let value = |added_bytes, replaced_bytes, freed: Freed| StorageCost {
        added_bytes,
        replaced_bytes,
        removed_bytes: freed.bytes(),
    };
    let paid = paid_value_len(node);
    let storage_cost = match bill {
        Bill::Unbilled => return None,
        Bill::New | Bill::Mismatched => KeyValueStorageCost {
            value_storage_cost: value(
                paid + u32::from(matches!(bill, Bill::Mismatched)),
                0,
                Freed::Nothing,
            ),
            new_node: true,
            needs_value_verification: true,
            ..Default::default()
        },
        Bill::Replaced => KeyValueStorageCost {
            value_storage_cost: value(0, paid, Freed::Nothing),
            needs_value_verification: true,
            ..Default::default()
        },
        Bill::Given(added, replaced, freed) => KeyValueStorageCost {
            value_storage_cost: value(added, replaced, freed),
            ..Default::default()
        },
        Bill::Prepaid => KeyValueStorageCost::prepaid(),
    };
    Some(NodeWriteCost {
        feature: node
            .feature_type()
            .tree_feature_specialized_type_and_length(),
        left_child: node.child_ref_and_sum_size(true),
        right_child: node.child_ref_and_sum_size(false),
        storage_cost,
    })
}

/// The cost a writer gives with a removal, or with a record that is no
/// node: bytes freed from the key and from the value.
fn freeing(key: Freed, value: Freed) -> Option<KeyValueStorageCost> {
    let freed = |freed: Freed| StorageCost {
        removed_bytes: freed.bytes(),
        ..Default::default()
    };
    Some(KeyValueStorageCost {
        key_storage_cost: freed(key),
        value_storage_cost: freed(value),
        ..Default::default()
    })
}

/// One change of a change set.
#[derive(Clone, Debug)]
enum Change {
    Put(Lone, NodeChange, Bill),
    Delete(Vec<u8>, Option<KeyValueStorageCost>),
    Root(Option<Vec<u8>>),
    PutRootKey(Vec<u8>, Option<KeyValueStorageCost>),
    DeleteRootKey(Option<KeyValueStorageCost>),
    PutAux(Vec<u8>, Vec<u8>, Option<KeyValueStorageCost>),
    DeleteAux(Vec<u8>, Option<KeyValueStorageCost>),
}

impl Change {
    fn make<C: NodeChangeSet>(self, change_set: &mut C) -> Result<(), Error> {
        match self {
            Change::Put(lone, change, bill) => {
                let node = lone.node();
                let prepared = change_set.prepare_node(&node, change);
                return change_set.put_node(&lone.key, prepared, billed(&node, bill));
            }
            Change::PutRootKey(key, cost) => return change_set.put_root_key(&key, cost),
            Change::PutAux(key, value, cost) => {
                return change_set.put_aux_record(&key, &value, cost)
            }
            Change::Delete(key, cost) => change_set.delete_node(&key, cost),
            Change::Root(key) => change_set.state_root(key.as_deref()),
            Change::DeleteRootKey(cost) => change_set.delete_root_key(cost),
            Change::DeleteAux(key, cost) => change_set.delete_aux_record(&key, cost),
        }
        Ok(())
    }
}

/// One operation of the seam.
#[derive(Clone, Debug)]
enum Step {
    Get(Vec<u8>),
    GetForRead(Vec<u8>),
    Has(Vec<u8>),
    Fetch(Vec<u8>),
    RootKey,
    Aux(Vec<u8>),
    Meta(Vec<u8>),
    SetRootKey(Option<Vec<u8>>),
    PutMeta(Vec<u8>, Vec<u8>),
    DeleteMeta(Vec<u8>),
    WriteNode(Lone, NodeChange),
    Commit(Vec<Change>),
}

type Script = Vec<(Prefix, Step)>;

/// What each step of a script cost and ended in.
type Played = Vec<(OperationCost, Ended)>;

fn run_step<'db, S: NodeStore<'db>>(store: &S, step: &Step) -> (OperationCost, Ended) {
    match step {
        Step::Get(key) => {
            let read = store.get_node(key, NO_VALUE_DEFINED_COST, version());
            let found = |node: Option<TreeNode>| Ended::Found(node.map(|node| node.encode()));
            (read.cost, outcome(read.value, found))
        }
        Step::GetForRead(key) => {
            let read = store.get_node_for_read(key);
            let found = |node: Option<crate::tree::TreeNodeInner>| {
                Ended::Found(node.map(|node| node.encode().expect("a stored node is encoded")))
            };
            (read.cost, outcome(read.value, found))
        }
        Step::Has(key) => {
            let read = store.has_node(key);
            (read.cost, outcome(read.value, Ended::Held))
        }
        Step::Fetch(key) => {
            let link = Link::Reference {
                hash: [0; 32],
                child_heights: (0, 0),
                key: key.clone(),
                aggregate_data: AggregateData::NoAggregateData,
            };
            let read = store.fetch_node(&link, NO_VALUE_DEFINED_COST.as_ref(), version());
            let found = |node: TreeNode| Ended::Found(Some(node.encode()));
            (read.cost, outcome(read.value, found))
        }
        Step::RootKey => record(store.root_key()),
        Step::Aux(key) => record(store.get_aux_record(key)),
        Step::Meta(key) => record(store.get_meta_record(key)),
        Step::SetRootKey(key) => done(store.set_root_key(key.as_deref())),
        Step::PutMeta(key, value) => done(store.put_meta_record(key, value)),
        Step::DeleteMeta(key) => done(store.delete_meta_record(key)),
        Step::WriteNode(node, change) => done(store.write_node(&node.node(), *change)),
        Step::Commit(changes) => {
            let mut change_set = store.change_set();
            let mut refused = 0;
            for change in changes {
                refused += usize::from(change.clone().make(&mut change_set).is_err());
            }
            let committed = store.commit(change_set);
            let ended = outcome(committed.value, |()| Ended::Committed { refused });
            (committed.cost, ended)
        }
    }
}

/// Runs `script`, each step on the handle `open` gives for its subtree. A
/// subtree has one handle for the whole script.
fn run_script<'db, S: NodeStore<'db>>(open: impl Fn(Prefix) -> S, script: &Script) -> Played {
    let mut handles: HashMap<Prefix, S> = HashMap::new();
    let steps = script.iter().map(|(prefix, step)| {
        let store = handles.entry(*prefix).or_insert_with(|| open(*prefix));
        run_step(store, step)
    });
    steps.collect()
}

/// What a played act is compared by: each of its steps, and then the
/// application of its batch if it staged into one.
type Act = (Played, Option<(OperationCost, Ended)>);

fn deferred_on_rocks(rocks: &TempStorage, transaction: &Tx<'_>, script: &Script) -> Act {
    let batch = StorageBatch::new();
    let open = |prefix| {
        rocks
            .get_transactional_storage_context_by_subtree_prefix(prefix, Some(&batch), transaction)
            .unwrap()
    };
    let steps = run_script(open, script);
    let applied = rocks.commit_multi_context_batch(batch, Some(transaction));
    let ended = outcome(applied.value.map_err(Error::StorageError), |()| Ended::Done);
    (steps, Some((applied.cost, ended)))
}

fn deferred_on_lignin(writer: &LigninWriter<'_, '_>, script: &Script) -> Act {
    let staged = LigninBatch::new();
    let open =
        |prefix| LigninNodeStore::deferred(writer, LigninSubtree::alone(prefix), Some(&staged));
    let steps = run_script(open, script);
    let applied = staged.apply(writer);
    let ended = outcome(applied.value, |_| Ended::Done);
    (steps, Some((applied.cost, ended)))
}

fn unstaged_on_rocks(rocks: &TempStorage, transaction: &Tx<'_>, script: &Script) -> Played {
    let open = |prefix| {
        rocks
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, transaction)
            .unwrap()
    };
    run_script(open, script)
}

fn unstaged_on_lignin(writer: &LigninWriter<'_, '_>, script: &Script) -> Played {
    let open = |prefix| LigninNodeStore::deferred(writer, LigninSubtree::alone(prefix), None);
    run_script(open, script)
}

fn immediate_on_rocks(rocks: &TempStorage, transaction: &Tx<'_>, script: &Script) -> Played {
    let open = |prefix| {
        rocks
            .get_immediate_storage_context_by_subtree_prefix(prefix, transaction)
            .unwrap()
    };
    run_script(open, script)
}

fn immediate_on_lignin(writer: &LigninWriter<'_, '_>, script: &Script) -> Played {
    let open = |prefix| LigninNodeStore::immediate(writer, LigninSubtree::alone(prefix));
    run_script(open, script)
}

/// One part of a history that is played on each engine.
#[derive(Clone, Debug)]
enum Part {
    /// A script on deferred handles that stage into one batch, which is
    /// then applied.
    Deferred(Script),
    /// A script on deferred handles that were opened with no batch.
    Unstaged(Script),
    /// A script on immediate handles.
    Immediate(Script),
    /// A script whose batch the transaction does not take: on RocksDB it
    /// is applied to a transaction that only reads, and on LigninDB it
    /// names a subtree that was never entered.
    NotTaken(Script),
    /// The transaction is committed and the next one begins.
    Commit,
}

fn play_on_rocks(history: &[Part]) -> Vec<Act> {
    let rocks = TempStorage::new();
    let mut transaction = rocks.start_transaction();
    let mut acts = Vec::new();
    for part in history {
        acts.push(match part {
            Part::Deferred(script) => deferred_on_rocks(&rocks, &transaction, script),
            Part::Unstaged(script) => (unstaged_on_rocks(&rocks, &transaction, script), None),
            Part::Immediate(script) => (immediate_on_rocks(&rocks, &transaction, script), None),
            Part::NotTaken(script) => {
                deferred_on_rocks(&rocks, &rocks.start_snapshot_read_transaction(), script)
            }
            Part::Commit => {
                rocks.commit_transaction(transaction).unwrap().unwrap();
                transaction = rocks.start_transaction();
                continue;
            }
        });
    }
    acts
}

/// Plays `history` on LigninDB. `after` is given each act as it ends.
fn play_on_lignin(history: &[Part], mut after: impl FnMut(&Part, &Act)) -> Vec<Act> {
    let lignin = Lignin::new();
    let path = lignin.path();
    let mut transaction = path.begin();
    let mut writer = LigninWriter::new(transaction.scope().unwrap());
    let mut acts = Vec::new();
    for part in history {
        let act = match part {
            Part::Deferred(script) | Part::NotTaken(script) => deferred_on_lignin(&writer, script),
            Part::Unstaged(script) => (unstaged_on_lignin(&writer, script), None),
            Part::Immediate(script) => (immediate_on_lignin(&writer, script), None),
            Part::Commit => {
                writer.into_scope().merge();
                assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
                transaction = path.begin();
                writer = LigninWriter::new(transaction.scope().unwrap());
                continue;
            }
        };
        after(part, &act);
        acts.push(act);
    }
    acts
}

fn step(n: u8, step: Step) -> (Prefix, Step) {
    (at(n), step)
}

/// A commit of the Merk under `n` that is the lone node `node`.
fn put_lone(n: u8, node: &Lone, change: NodeChange, bill: Bill) -> (Prefix, Step) {
    let changes = vec![
        Change::Put(node.clone(), change, bill),
        Change::Root(Some(node.key.clone())),
    ];
    step(n, Step::Commit(changes))
}

/// A commit that removes the lone node of the Merk under `n`.
fn delete_lone(n: u8, node: &Lone, cost: Option<KeyValueStorageCost>) -> (Prefix, Step) {
    let changes = vec![Change::Delete(node.key.clone(), cost), Change::Root(None)];
    step(n, Step::Commit(changes))
}

/// Every read of the seam for `key` in the subtree `n`.
fn every_read(n: u8, key: &[u8]) -> Script {
    let key = key.to_vec();
    vec![
        step(n, Step::Get(key.clone())),
        step(n, Step::GetForRead(key.clone())),
        step(n, Step::Has(key.clone())),
        step(n, Step::Fetch(key.clone())),
        step(n, Step::RootKey),
        step(n, Step::Aux(key.clone())),
        step(n, Step::Meta(key)),
    ]
}

/// A history that makes every operation of the seam, with every kind of
/// cost a writer gives and with none, on records the committed state
/// holds, on records the transaction wrote, and on keys that hold none.
fn history() -> Vec<Part> {
    use TreeFeatureType::*;

    let a = plain(b"a", 40);
    let a_longer = plain(b"a", 900);
    let b = lone(b"bb", 3, SummedMerkNode(-70_000));
    let c = lone(b"", 300, CountedSummedMerkNode(1, 5));
    let d = plain(b"dddd", 0);
    let e = lone(
        &[9; 200],
        70,
        ProvableCountedAndProvableBigSummedMerkNode(1, -9),
    );
    let f = plain(b"f", 12);
    let g = plain(b"g", 77);
    let h = plain(b"h", 5);
    let n = plain(b"n", 64);
    let late = plain(b"late", 9);
    let z = plain(b"z", 6);
    let (k, m) = (b"k".to_vec(), b"m".to_vec());
    let key_freed = Freed::Plain(34);
    let root_cost = |old, new: &[u8]| {
        Some(KeyValueStorageCost::for_updated_root_cost(
            old,
            new.len() as u32,
        ))
    };

    // Nothing is stored yet: every read finds nothing, and the writes of
    // the batch are not read before it is applied.
    let mut first = every_read(10, &a.key);
    first.extend([
        put_lone(10, &a, NodeChange::ValueWritten, Bill::New),
        put_lone(11, &b, NodeChange::ValueWritten, Bill::New),
        put_lone(12, &c, NodeChange::ValueWritten, Bill::Unbilled),
        step(
            13,
            Step::Commit(vec![
                Change::Put(
                    d.clone(),
                    NodeChange::ValueWritten,
                    Bill::Given(40, 0, Freed::Nothing),
                ),
                Change::Root(Some(d.key.clone())),
                Change::PutRootKey(d.key.clone(), None),
                Change::PutAux(k.clone(), vec![1; 9], None),
            ]),
        ),
        step(
            14,
            Step::Commit(vec![
                Change::Put(e.clone(), NodeChange::ValueWritten, Bill::Prepaid),
                Change::Root(Some(e.key.clone())),
                Change::PutRootKey(e.key.clone(), root_cost(None, &e.key)),
                Change::PutAux(
                    k.clone(),
                    vec![2; 300],
                    freeing(Freed::Nothing, Freed::Nothing),
                ),
            ]),
        ),
        put_lone(
            15,
            &f,
            NodeChange::ValueWritten,
            Bill::Given(0, 0, Freed::Nothing),
        ),
        step(15, Step::SetRootKey(Some(f.key.clone()))),
        step(15, Step::PutMeta(m.clone(), vec![3; 9])),
        put_lone(16, &g, NodeChange::ValueWritten, Bill::New),
        put_lone(17, &h, NodeChange::ValueWritten, Bill::New),
        put_lone(23, &late, NodeChange::ValueWritten, Bill::New),
        put_lone(24, &z, NodeChange::ValueWritten, Bill::New),
        put_lone(25, &plain(b"a", 6), NodeChange::ValueWritten, Bill::New),
        step(10, Step::Has(a.key.clone())),
    ]);

    // The committed records are read, and the transaction rewrites some
    // of them and creates others. One outcome stands for a record that a
    // batch writes twice.
    let mut second: Script = [10, 11, 12, 13, 14, 15]
        .into_iter()
        .zip([&a.key, &b.key, &c.key, &d.key, &e.key, &f.key])
        .flat_map(|(n, key)| every_read(n, key))
        .collect();
    second.extend(every_read(13, &k));
    second.extend(every_read(15, &m));
    second.extend([
        put_lone(10, &a_longer, NodeChange::ValueWritten, Bill::Replaced),
        put_lone(20, &n, NodeChange::ValueWritten, Bill::New),
        put_lone(
            12,
            &c,
            NodeChange::HashOnly,
            Bill::Given(0, 77, Freed::Sectioned(4, 9)),
        ),
        put_lone(
            11,
            &b,
            NodeChange::StructureChanged,
            Bill::Given(3, 2, Freed::Plain(60)),
        ),
        step(
            15,
            Step::WriteNode(plain(b"f", 500), NodeChange::ValueWritten),
        ),
        step(13, Step::SetRootKey(Some(vec![7; 90]))),
        step(
            13,
            Step::Commit(vec![Change::PutAux(k.clone(), vec![1; 150], None)]),
        ),
        step(15, Step::PutMeta(m.clone(), vec![3; 200])),
        step(15, Step::PutMeta(b"twice".to_vec(), vec![1; 4])),
        step(15, Step::PutMeta(b"twice".to_vec(), vec![2; 44])),
        step(15, Step::DeleteMeta(b"put after".to_vec())),
        step(15, Step::PutMeta(b"put after".to_vec(), vec![5; 5])),
        step(15, Step::PutMeta(b"put before".to_vec(), vec![6; 6])),
        step(15, Step::DeleteMeta(b"put before".to_vec())),
        step(15, Step::DeleteMeta(b"never there".to_vec())),
    ]);

    // Removals, with the writer's cost and with none. One with none is
    // charged from the committed record, whatever the transaction wrote.
    let third = vec![
        step(10, Step::Get(a.key.clone())),
        delete_lone(10, &a, None),
        delete_lone(20, &n, None),
        delete_lone(16, &g, None),
        delete_lone(15, &f, None),
        delete_lone(17, &h, freeing(key_freed, Freed::Plain(80))),
        delete_lone(
            11,
            &b,
            freeing(Freed::Sectioned(2, 34), Freed::Sectioned(3, 70)),
        ),
        delete_lone(12, &c, freeing(key_freed, Freed::Sectioned(1, 400))),
        step(
            13,
            Step::Commit(vec![
                Change::DeleteRootKey(None),
                Change::DeleteAux(k.clone(), None),
                Change::DeleteAux(b"never there".to_vec(), None),
            ]),
        ),
        step(
            14,
            Step::Commit(vec![
                Change::DeleteRootKey(freeing(key_freed, Freed::Plain(200))),
                Change::DeleteAux(
                    k.clone(),
                    freeing(Freed::Sectioned(1, 33), Freed::Plain(300)),
                ),
            ]),
        ),
        step(15, Step::SetRootKey(None)),
        step(15, Step::DeleteMeta(m.clone())),
    ];

    // A batch is charged by subtree and then by key. The removal in the
    // first subtree frees plain bytes and the one in the second frees
    // bytes of the owner that the first way of adding removals up drops
    // when they join plain ones, so the sum says which came first. Their
    // keys are in the other order.
    let ordered = vec![
        delete_lone(
            25,
            &plain(b"a", 6),
            freeing(Freed::Sectioned(3, 34), Freed::Sectioned(0, 60)),
        ),
        delete_lone(24, &z, freeing(key_freed, Freed::Plain(50))),
    ];

    // A put whose cost does not match its record refuses the batch. What
    // came before it in the order of a batch stays charged, and what
    // comes after it is not reached: the removal after it would load a
    // committed record.
    let refused = vec![
        step(22, Step::PutMeta(b"before".to_vec(), vec![1; 30])),
        step(22, Step::Commit(vec![Change::DeleteAux(k.clone(), None)])),
        delete_lone(23, &late, None),
        put_lone(21, &plain(b"early", 9), NodeChange::ValueWritten, Bill::New),
        put_lone(
            22,
            &plain(b"q", 50),
            NodeChange::ValueWritten,
            Bill::Mismatched,
        ),
    ];
    let after_the_refusal = vec![
        step(21, Step::Has(b"early".to_vec())),
        step(23, Step::Has(late.key.clone())),
        step(22, Step::Meta(b"before".to_vec())),
    ];

    // A batch the transaction does not take is charged its seeks and what
    // it loaded, and no storage cost.
    let not_taken = vec![
        put_lone(250, &plain(b"s", 20), NodeChange::ValueWritten, Bill::New),
        step(250, Step::PutMeta(m.clone(), vec![3; 9])),
        step(250, Step::DeleteMeta(b"gone".to_vec())),
    ];

    // A handle with no batch refuses every write and reads as any other.
    let mut unstaged = every_read(14, &e.key);
    unstaged.extend([
        put_lone(14, &e, NodeChange::ValueWritten, Bill::New),
        step(14, Step::WriteNode(e.clone(), NodeChange::HashOnly)),
        step(14, Step::SetRootKey(Some(k.clone()))),
        step(14, Step::PutMeta(m.clone(), vec![1])),
        step(14, Step::DeleteMeta(m.clone())),
    ]);

    // An immediate handle reads for nothing, writes at once, and charges
    // a commit as its change set was filled.
    let p = plain(b"p", 30);
    let p_longer = plain(b"p", 3_000);
    let mut immediate = every_read(30, &p.key);
    immediate.extend([step(
        30,
        Step::Commit(vec![
            Change::Put(p.clone(), NodeChange::ValueWritten, Bill::New),
            Change::Root(Some(p.key.clone())),
            Change::PutRootKey(p.key.clone(), None),
            Change::PutAux(k.clone(), vec![8; 8], None),
            Change::PutAux(b"billed".to_vec(), vec![8; 8], root_cost(Some(3), &k)),
        ]),
    )]);
    immediate.extend(every_read(30, &p.key));
    immediate.extend([
        put_lone(30, &p_longer, NodeChange::ValueWritten, Bill::Replaced),
        put_lone(30, &p_longer, NodeChange::HashOnly, Bill::Mismatched),
        put_lone(30, &p_longer, NodeChange::HashOnly, Bill::Unbilled),
        put_lone(30, &p_longer, NodeChange::HashOnly, Bill::Prepaid),
        step(30, Step::WriteNode(p.clone(), NodeChange::ValueWritten)),
        step(30, Step::SetRootKey(Some(vec![1; 40]))),
        step(30, Step::PutMeta(m.clone(), vec![4; 40])),
        step(30, Step::Meta(m.clone())),
        step(30, Step::DeleteMeta(m.clone())),
        step(30, Step::SetRootKey(None)),
        step(
            30,
            Step::Commit(vec![
                Change::Delete(p.key.clone(), None),
                Change::Root(None),
                Change::DeleteRootKey(None),
                Change::DeleteAux(k.clone(), None),
                Change::DeleteAux(
                    b"billed".to_vec(),
                    freeing(key_freed, Freed::Sectioned(7, 8)),
                ),
            ]),
        ),
        put_lone(31, &p, NodeChange::ValueWritten, Bill::New),
        delete_lone(31, &p, freeing(key_freed, Freed::Plain(100))),
    ]);
    immediate.extend(every_read(30, &p.key));

    let mut last = every_read(10, &a.key);
    last.extend(every_read(20, &n.key));
    last.extend(every_read(13, &k));

    vec![
        Part::Deferred(first),
        Part::Commit,
        Part::Deferred(second),
        Part::Deferred(third),
        Part::Deferred(ordered),
        Part::Deferred(refused),
        Part::Unstaged(after_the_refusal),
        Part::NotTaken(not_taken),
        Part::Unstaged(unstaged),
        Part::Commit,
        Part::Immediate(immediate),
        Part::Unstaged(last),
    ]
}

#[test]
fn every_operation_of_the_store_costs_what_it_costs_on_rocksdb() {
    let history = history();
    let parts: Vec<&Part> = (history.iter())
        .filter(|part| !matches!(part, Part::Commit))
        .collect();
    // The two ways a plain removal and a sectioned one add up: the order
    // in which a batch is charged shows in the first.
    for arithmetic in [0, 1] {
        let _arithmetic = use_basic_sectioned_removal_addition_version(arithmetic);
        let expected = play_on_rocks(&history);
        let found = play_on_lignin(&history, |_, _| ());
        assert_eq!((found.len(), expected.len()), (parts.len(), parts.len()));
        for (n, ((found, expected), part)) in found.iter().zip(&expected).zip(&parts).enumerate() {
            let script = match part {
                Part::Deferred(script)
                | Part::Unstaged(script)
                | Part::Immediate(script)
                | Part::NotTaken(script) => script,
                Part::Commit => unreachable!("a commit is no act"),
            };
            assert_eq!(
                (found.0.len(), expected.0.len()),
                (script.len(), script.len())
            );
            for (((found, expected), step), s) in
                found.0.iter().zip(&expected.0).zip(script).zip(0..)
            {
                assert_eq!(
                    found, expected,
                    "arithmetic {arithmetic}, part {n}, step {s}: {step:?}"
                );
            }
            assert_eq!(
                found.1, expected.1,
                "arithmetic {arithmetic}, the batch of part {n}"
            );
        }

        // The history is not one of calls that all fail or all cost
        // nothing.
        let applied = |n: usize| found[n].1.clone().expect("a batch was applied");
        let first = applied(0).0;
        assert_eq!(applied(0).1, Ended::Done);
        assert!(
            first.seek_count > 12 && first.storage_cost.added_bytes > 1_000,
            "{first:?}"
        );
        let removals = applied(2).0;
        assert_eq!(applied(2).1, Ended::Done);
        assert!(removals.storage_loaded_bytes > 100, "{removals:?}");
        assert!(
            removals.storage_cost.removed_bytes.has_removal(),
            "{removals:?}"
        );
        // The two removals of the ordered batch add up to another sum in
        // the other order, under the first arithmetic.
        let (ordered, ended) = applied(3);
        assert_eq!(ended, Ended::Done);
        let mut in_the_other_order = sectioned(3, 34) + sectioned(0, 60);
        in_the_other_order += Freed::Plain(84).bytes();
        assert_eq!(
            ordered.storage_cost.removed_bytes == in_the_other_order,
            arithmetic == 1,
            "{ordered:?}"
        );
        let refused = applied(4);
        assert_eq!(refused.1, Ended::Failed("cost"));
        assert_eq!(refused.0.seek_count, 5, "{refused:?}");
        assert_eq!(refused.0.storage_loaded_bytes, 0, "{refused:?}");
        assert_eq!(refused.0.storage_cost, StorageCost::default());
        let not_taken = applied(6);
        assert_eq!(not_taken.1, Ended::Failed("storage"));
        assert!(not_taken.0.seek_count >= 4, "{not_taken:?}");
        assert_eq!(not_taken.0.storage_cost, StorageCost::default());
    }
}

fn sum_of(act: &Act) -> OperationCost {
    let (steps, applied) = act;
    let costs = steps.iter().chain(applied).map(|(cost, _)| cost.clone());
    costs.fold(OperationCost::default(), |sum, cost| sum + cost)
}

#[test]
fn the_store_reports_what_the_model_hands_out_and_nothing_else() {
    // Every cost the store reports for the history is added up, part by
    // part, and compared with what the cost model handed out meanwhile. A
    // charge worked out beside the model is in the first sum only, and a
    // charge of the model that the store dropped in the second only. The
    // test asks the model for no charge itself.
    let history = history();
    let mut total = OperationCost::default();
    ledger::take();
    play_on_lignin(&history, |part, act| {
        let reported = sum_of(act);
        assert_eq!(reported, ledger::take(), "{part:?}");
        total += reported;
    });
    assert!(total.seek_count > 120, "{total:?}");
    assert!(total.storage_loaded_bytes > 2_000, "{total:?}");
    assert!(total.storage_cost.added_bytes > 1_500, "{total:?}");
    assert!(total.storage_cost.replaced_bytes > 100, "{total:?}");
    assert!(total.storage_cost.removed_bytes.has_removal(), "{total:?}");
}

fn seek_and_load(loaded: usize) -> OperationCost {
    OperationCost {
        seek_count: 1,
        storage_loaded_bytes: loaded as u64,
        ..Default::default()
    }
}

#[test]
fn a_fetch_is_one_seek_and_the_record_length_however_it_is_served() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (1..=31).map(|n| basic(n, &[n as u8; 300])).collect();
    commit(&writer, &ROOT, &mut root, &nodes);
    let root = root.expect("the Merk has a root");
    let engine_reads = || {
        let (keyed, followed) = reads(&transaction);
        keyed + followed
    };

    // The root has two children. Its cell holds the length of their keys
    // and not the keys, so the store reads three cells to build the node,
    // and charges the one fetch Merk made: a seek and the record length.
    let store = LigninNodeStore::deferred(&writer, ROOT, None);
    let before = engine_reads();
    let read = store.get_node(&root, NO_VALUE_DEFINED_COST, version());
    let node = read.value.unwrap().expect("the root is stored");
    assert_eq!(engine_reads() - before, 3);
    let record_len = node.encode().len();
    assert!(node.link(true).is_some() && node.link(false).is_some());
    assert_eq!(read.cost, seek_and_load(record_len));

    // The same for a read that only looks at the node, which reads one
    // cell, and for a read of the fields.
    let before = engine_reads();
    let held = store.has_node(&root);
    assert!(held.value.unwrap());
    assert_eq!(engine_reads() - before, 1);
    assert_eq!(held.cost, read.cost);
    assert_eq!(store.get_node_for_read(&root).cost, read.cost);

    // A child is fetched through its parent's link, where the parent's
    // read saw it, and costs what a lookup of its key costs.
    let link = node.link(true).expect("a left child");
    let fetched = store.fetch_node(link, NO_VALUE_DEFINED_COST.as_ref(), version());
    let child = fetched.value.expect("the child is stored");
    assert_eq!(fetched.cost, seek_and_load(child.encode().len()));
    let by_key = LigninNodeStore::deferred(&writer, ROOT, None);
    let looked_up = by_key.get_node(link.key(), NO_VALUE_DEFINED_COST, version());
    assert_eq!(looked_up.cost, fetched.cost);

    // A node that has to be read again, because its location went stale
    // between the read of its cell and of a child's, is one fetch still.
    let unsteady = Unsteady {
        under: &writer,
        stale: AtomicUsize::new(5),
        missing: AtomicUsize::new(0),
    };
    let store = LigninNodeStore::new(&unsteady, ROOT, Mode::Deferred(None));
    let before = engine_reads();
    let again = store.get_node(&root, NO_VALUE_DEFINED_COST, version());
    assert_eq!(
        again.value.unwrap().expect("the root").encode(),
        node.encode()
    );
    assert!(engine_reads() - before > 3);
    assert_eq!(again.cost, read.cost);

    // A read that gives up is charged as a lookup that found nothing.
    let unsteady = Unsteady {
        under: &writer,
        stale: AtomicUsize::new(usize::MAX),
        missing: AtomicUsize::new(0),
    };
    let store = LigninNodeStore::new(&unsteady, ROOT, Mode::Deferred(None));
    let given_up = store.get_node(&root, NO_VALUE_DEFINED_COST, version());
    assert!(matches!(given_up.value, Err(Error::StorageError(_))));
    assert_eq!(given_up.cost, seek_and_load(0));

    // An immediate handle is charged nothing for the same reads.
    let store = LigninNodeStore::immediate(&writer, ROOT);
    let read = store.get_node(&root, NO_VALUE_DEFINED_COST, version());
    assert!(read.value.unwrap().is_some());
    assert_eq!(read.cost, OperationCost::default());
}

#[test]
fn a_delete_with_no_cost_is_charged_from_the_committed_record() {
    use integer_encoding::VarInt;

    let committed = plain(b"a", 40);
    let rewritten = plain(b"a", 900);
    let created = plain(b"n", 64);
    let history = vec![
        Part::Deferred(vec![put_lone(
            10,
            &committed,
            NodeChange::ValueWritten,
            Bill::New,
        )]),
        Part::Commit,
        Part::Deferred(vec![
            put_lone(10, &rewritten, NodeChange::ValueWritten, Bill::Replaced),
            put_lone(20, &created, NodeChange::ValueWritten, Bill::New),
        ]),
        Part::Deferred(vec![delete_lone(10, &rewritten, None)]),
        Part::Deferred(vec![delete_lone(20, &created, None)]),
        Part::Unstaged(vec![
            step(10, Step::Has(committed.key.clone())),
            step(20, Step::Has(created.key.clone())),
        ]),
    ];
    let found = play_on_lignin(&history, |_, _| ());
    assert_eq!(found, play_on_rocks(&history));

    // Two seeks, the committed record loaded, and the stored key and that
    // record removed, each with its varint (`cost-model.md` D2).
    let removal = |record_len: usize| {
        let stored_key_len = 32 + 1;
        let removed = stored_key_len + record_len + 1 + record_len.required_space();
        (
            OperationCost {
                seek_count: 2,
                storage_loaded_bytes: record_len as u64,
                storage_cost: StorageCost {
                    removed_bytes: StorageRemovedBytes::BasicStorageRemoval(removed as u32),
                    ..Default::default()
                },
                ..Default::default()
            },
            Ended::Done,
        )
    };
    // The transaction holds the node with a value of 900 bytes when it
    // removes it. The committed state holds it with one of 40.
    let committed_len = committed.node().encode().len();
    assert!(rewritten.node().encode().len() > committed_len + 800);
    assert_eq!(found[2].1, Some(removal(committed_len)));
    // A node the transaction created is not in the committed state: an
    // empty record is removed.
    assert_eq!(found[3].1, Some(removal(0)));
    let gone = (seek_and_load(0), Ended::Held(false));
    assert_eq!(found[4].0, vec![gone.clone(), gone]);
}

#[test]
fn the_nodes_of_a_merk_removed_with_no_cost_are_charged_as_committed() {
    // A tree whose links carry aggregates, so that the record length of a
    // node depends on its children.
    let tree_type = TreeType::CountSumTree;
    let prefix = at(7);
    let subtree = LigninSubtree::alone(prefix);
    let rocks = TempStorage::new();
    let lignin = Lignin::new();
    let path = lignin.path();
    let mut random = Random(99);
    let mut model = BTreeMap::new();
    let mut root = None;
    let mut transaction = path.begin();
    let mut writer = LigninWriter::new(transaction.scope().unwrap());
    let mut rocks_tx = rocks.start_transaction();
    let mut committed_lens = HashMap::new();

    for round in 0..6 {
        let batch = random_batch(&mut random, tree_type, &mut model, true);
        let storage_batch = StorageBatch::new();
        let context = rocks
            .get_transactional_storage_context_by_subtree_prefix(
                prefix,
                Some(&storage_batch),
                &rocks_tx,
            )
            .unwrap();
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
        let (_, mut on_rocks) = open_as(context, Opened::Layered, &root, tree_type, version());
        let (_, mut on_lignin) = open_as(store, Opened::Layered, &root, tree_type, version());
        assert_eq!(apply(&mut on_lignin, &batch), apply(&mut on_rocks, &batch));
        root = on_rocks.root_key();
        drop((on_rocks, on_lignin));
        rocks
            .commit_multi_context_batch(storage_batch, Some(&rocks_tx))
            .unwrap()
            .unwrap();
        staged.apply(&writer).unwrap().unwrap();

        // The first three batches are committed. The others stay in the
        // transaction, which then holds nodes the committed state does
        // not have, and nodes with other children than it has.
        if round == 2 {
            rocks.commit_transaction(rocks_tx).unwrap().unwrap();
            rocks_tx = rocks.start_transaction();
            writer.into_scope().merge();
            assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
            transaction = path.begin();
            writer = LigninWriter::new(transaction.scope().unwrap());
            let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
            for (key, record) in records(&reader, root.clone()) {
                committed_lens.insert(key, record.len());
            }
        }
    }

    // Every node the transaction holds is removed by its key with no
    // cost, as `Merk::clear` and the removal of a whole subtree do.
    let reader = LigninNodeStore::deferred(&writer, subtree.clone(), None);
    let held = records(&reader, root);
    let mut removals: Vec<Change> = (held.iter())
        .map(|(key, _)| Change::Delete(key.clone(), None))
        .collect();
    removals.push(Change::Root(None));
    let script = vec![(prefix, Step::Commit(removals))];
    let expected = deferred_on_rocks(&rocks, &rocks_tx, &script);
    let found = deferred_on_lignin(&writer, &script);
    assert_eq!(found, expected);

    // What was loaded is the committed record of each node that has one,
    // and not the record the transaction holds.
    let (cost, ended) = found.1.expect("the batch was applied");
    assert_eq!(ended, Ended::Done);
    let committed: usize = (held.iter())
        .filter_map(|(key, _)| committed_lens.get(key))
        .sum();
    let in_the_transaction: usize = held.iter().map(|(_, record)| record.len()).sum();
    assert_eq!(cost.seek_count as usize, 2 * held.len());
    assert_eq!(cost.storage_loaded_bytes as usize, committed);
    assert_ne!(committed, in_the_transaction);
    let created = (held.iter())
        .filter(|(key, _)| !committed_lens.contains_key(key))
        .count();
    assert!(
        created > 3 && created < held.len(),
        "{created} of {}",
        held.len()
    );
}

#[test]
fn a_removal_of_a_key_that_is_no_node_is_charged_and_changes_nothing() {
    // The repair of an indexed tree's ranking removes a row that no node of
    // the ranking's Merk links, with a cost it worked out itself, in a
    // change set that states no root (GroveDB #1063). On RocksDB such a
    // row is a record under the Merk's prefix. The store holds no record
    // that no link leads to, so it has nothing to remove.
    let a = plain(b"a", 40);
    let orphan = b"orphan".to_vec();
    let cost = freeing(Freed::Plain(39), Freed::Plain(120));
    let removal = |cost| {
        vec![step(
            10,
            Step::Commit(vec![Change::Delete(orphan.clone(), cost)]),
        )]
    };
    let mut the_tree = every_read(10, &a.key);
    the_tree.push(step(10, Step::Has(orphan.clone())));
    let charged = |seek_count, removed| OperationCost {
        seek_count,
        storage_cost: StorageCost {
            removed_bytes: StorageRemovedBytes::BasicStorageRemoval(removed),
            ..Default::default()
        },
        ..Default::default()
    };

    let rocks = TempStorage::new();
    let lignin = Lignin::new();
    let path = lignin.path();
    let first = vec![put_lone(10, &a, NodeChange::ValueWritten, Bill::New)];
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let rocks_tx = rocks.start_transaction();
    assert_eq!(
        deferred_on_lignin(&writer, &first),
        deferred_on_rocks(&rocks, &rocks_tx, &first)
    );
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    rocks.commit_transaction(rocks_tx).unwrap().unwrap();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut rocks_tx = rocks.start_transaction();

    // With the cost the repair gives, the removal is charged as any removal
    // with a cost is: one seek and the bytes the writer says it frees. It
    // is one write of the key in the transaction, and the tree is read as
    // before.
    let writes = transaction.stats().writes;
    let found = deferred_on_lignin(&writer, &removal(cost.clone()));
    assert_eq!(
        found,
        deferred_on_rocks(&rocks, &rocks_tx, &removal(cost.clone()))
    );
    assert_eq!(found.1, Some((charged(1, 159), Ended::Done)));
    assert_eq!(transaction.stats().writes - writes, 1);
    let read = unstaged_on_lignin(&writer, &the_tree);
    assert_eq!(read, unstaged_on_rocks(&rocks, &rocks_tx, &the_tree));
    assert_eq!(read.last(), Some(&(seek_and_load(0), Ended::Held(false))));

    // RocksDB is charged the same when it holds a record under the key
    // that no node links, which is what the repair removes there: a
    // removal with a cost measures nothing.
    let record = [7; 120];
    let under_the_merk = rocks
        .get_immediate_storage_context_by_subtree_prefix(at(10), &rocks_tx)
        .unwrap();
    StorageContext::put(&under_the_merk, &orphan, &record, None, None)
        .unwrap()
        .expect("the record is put");
    rocks.commit_transaction(rocks_tx).unwrap().unwrap();
    rocks_tx = rocks.start_transaction();
    let held = vec![step(10, Step::Has(orphan.clone()))];
    let is_held = |held: bool| vec![(seek_and_load(if held { 120 } else { 0 }), Ended::Held(held))];
    assert_eq!(unstaged_on_rocks(&rocks, &rocks_tx, &held), is_held(true));
    let on_rocks = deferred_on_rocks(&rocks, &rocks_tx, &removal(cost.clone()));
    assert_eq!(on_rocks, found);
    assert_eq!(unstaged_on_rocks(&rocks, &rocks_tx, &held), is_held(false));
    // Removed with no cost, such a record is measured where it is
    // committed. That is a charge the store cannot make while it holds no
    // such record, and the repair does not ask for it.
    let measured = deferred_on_rocks(&rocks, &rocks_tx, &removal(None));
    let from_the_record = OperationCost {
        storage_loaded_bytes: 120,
        ..charged(2, 38 + 120 + 1 + 1)
    };
    assert_eq!(measured.1, Some((from_the_record, Ended::Done)));
    rocks.commit_transaction(rocks_tx).unwrap().unwrap();
    rocks_tx = rocks.start_transaction();

    // With no cost and no such record, it is charged as the removal of a
    // key the committed state lacks: two seeks, nothing loaded, and the
    // stored key and an empty record removed, each with the byte of its
    // length.
    let found = deferred_on_lignin(&writer, &removal(None));
    assert_eq!(found, deferred_on_rocks(&rocks, &rocks_tx, &removal(None)));
    assert_eq!(found.1, Some((charged(2, 38 + 1 + 1), Ended::Done)));

    // An immediate handle charges the seek, and the bytes of a cost.
    let both: Script = [removal(cost), removal(None)].concat();
    let found = immediate_on_lignin(&writer, &both);
    assert_eq!(found, immediate_on_rocks(&rocks, &rocks_tx, &both));
    let taken = Ended::Committed { refused: 0 };
    let none_removed = OperationCost::with_seek_count(1);
    assert_eq!(
        found,
        [(charged(1, 159), taken.clone()), (none_removed, taken)]
    );

    // What the transaction commits is the tree it had.
    assert_eq!(
        unstaged_on_lignin(&writer, &the_tree),
        unstaged_on_rocks(&rocks, &rocks_tx, &the_tree)
    );
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    assert_eq!(lignin.root_in_directory(at(10)), Some(a.key));
}

#[test]
fn a_node_that_has_no_record_length_is_not_written() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    // A link to a child whose key is longer than a link can say has no
    // encoding, so the node has no record to measure.
    let unencodable = Link::Reference {
        hash: [3; 32],
        child_heights: (1, 0),
        key: vec![1; 256],
        aggregate_data: AggregateData::NoAggregateData,
    };
    let node = TreeNode::from_fields(
        be(2),
        b"value".to_vec(),
        [7; 32],
        Some(unencodable),
        None,
        TreeFeatureType::BasicMerkNode,
    )
    .unwrap();

    let staged = LigninBatch::new();
    for store in [
        LigninNodeStore::deferred(&writer, ROOT, Some(&staged)),
        LigninNodeStore::immediate(&writer, ROOT),
    ] {
        let written = store.write_node(&node, NodeChange::ValueWritten);
        assert_eq!(written.cost, OperationCost::default());
        assert!(matches!(written.value, Err(Error::EdError(_))));

        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(&node, NodeChange::ValueWritten);
        let put = change_set.put_node(node.key(), prepared, None);
        assert!(matches!(put, Err(Error::EdError(_))));
        let committed = store.commit(change_set);
        assert_eq!(committed.cost, OperationCost::default());
        committed.value.expect("a change set with nothing in it");
    }
    assert!(staged.is_empty());
    assert_eq!(transaction.stats().writes, 0);
}

/// What a conversion to a 128-bit sum ended in: the tree type, the root
/// hash and the root key it left, or the refusal.
type Converted = Result<(TreeType, [u8; 32], Option<Vec<u8>>), Ended>;

/// Converts `merk` as a client whose values decide nothing of what a node
/// contributes, and returns the Merk if it was converted.
fn convert_as_a_client<'db, S: NodeStore<'db>>(
    merk: Merk<S>,
    grove_version: &GroveVersion,
) -> (OperationCost, Option<Merk<S>>, Converted) {
    let node_type = merk.tree_type.inner_node_type();
    let converted = merk.convert_to_big_sum_tree(
        &|_key, _value, _widened| Ok(()),
        &|key, value| stored_cost(key, value, node_type),
        VALUE_DEFINED_COST.as_ref(),
        grove_version,
    );
    match converted.value {
        Ok(merk) => {
            let left = (merk.tree_type, merk.root_hash().unwrap(), merk.root_key());
            (converted.cost, Some(merk), Ok(left))
        }
        Err(error) => (converted.cost, None, Err(failed(error))),
    }
}

/// The same Merk on RocksDB and on LigninDB is converted to the tree type
/// with a 128-bit sum (GroveDB #1063): every node is written again with a
/// wider feature and wider aggregates in its links, and no value changes.
/// The conversion, the batch it stages and what is read afterwards cost
/// the same on both, and LigninDB writes no value. `nodes_of` is the tree
/// type whose features the nodes of the Merk carry.
fn a_conversion_costs_the_same(
    grove_version: &GroveVersion,
    (tree_type, nodes_of): (TreeType, TreeType),
    immediate: bool,
    seed: u64,
) -> bool {
    let at = |what: &str| {
        format!(
            "{what}: grove version {}, {tree_type:?}, immediate {immediate}, seed {seed}",
            grove_version.protocol_version
        )
    };
    let _arithmetic = use_basic_sectioned_removal_addition_version(
        grove_version
            .grovedb_versions
            .storage_costs
            .add_basic_storage_removal_to_sectioned_storage_removal,
    );
    let prefix = [7; 32];
    let subtree = LigninSubtree::alone(prefix);
    let opened = Opened::Layered;
    let rocks = TempStorage::new();
    let lignin = Lignin::new();
    let path = lignin.path();
    let mut random = Random(seed);
    let mut model = BTreeMap::new();
    let keys = [3, 40, 77, 110].map(key);
    let turn = seed as usize;

    if immediate {
        let rocks_tx = rocks.start_transaction();
        let context = rocks
            .get_immediate_storage_context_by_subtree_prefix(prefix, &rocks_tx)
            .unwrap();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let store = LigninNodeStore::immediate(&writer, subtree);
        let (_, mut on_rocks) = open_as(context, opened, &None, tree_type, grove_version);
        let (_, mut on_lignin) = open_as(store, opened, &None, tree_type, grove_version);
        for _ in 0..3 {
            let batch = mixed_batch(&mut random, nodes_of, &mut model);
            let expected = apply_as_a_client(&mut on_rocks, &batch, &[], grove_version);
            let found = apply_as_a_client(&mut on_lignin, &batch, &[], grove_version);
            assert_eq!(found, expected, "{}", at("a batch before the conversion"));
            assert_eq!(
                found.1,
                Ended::Done,
                "{}",
                at("a batch before the conversion")
            );
        }
        assert!(
            on_rocks.root_key().is_some(),
            "{}",
            at("the tree to convert")
        );
        let (expected_cost, on_rocks, expected) = convert_as_a_client(on_rocks, grove_version);
        let (cost, on_lignin, found) = convert_as_a_client(on_lignin, grove_version);
        assert_eq!(found, expected, "{}", at("the conversion"));
        assert_eq!(cost, expected_cost, "{}", at("the cost of the conversion"));
        let (Some(on_rocks), Some(on_lignin)) = (on_rocks, on_lignin) else {
            return false;
        };
        assert_eq!(
            read_as_a_client(&on_lignin, &keys, turn, grove_version),
            read_as_a_client(&on_rocks, &keys, turn, grove_version),
            "{}",
            at("the reads of the converted tree")
        );
        let root = on_rocks.root_key();
        assert_same_records(
            &records(&on_lignin.storage, root.clone()),
            &records(&on_rocks.storage, root),
            &at("the converted tree"),
        );
        return true;
    }

    let mut root: Option<Vec<u8>> = None;
    let mut transaction = path.begin();
    let mut writer = LigninWriter::new(transaction.scope().unwrap());
    let mut rocks_tx = rocks.start_transaction();
    // The first two batches are committed and the third is not, so the
    // conversion meets nodes of the committed state and nodes that the
    // transaction wrote.
    for round in 0..3 {
        let batch = mixed_batch(&mut random, nodes_of, &mut model);
        let storage_batch = StorageBatch::new();
        let context = rocks
            .get_transactional_storage_context_by_subtree_prefix(
                prefix,
                Some(&storage_batch),
                &rocks_tx,
            )
            .unwrap();
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
        let (_, mut on_rocks) = open_as(context, opened, &root, tree_type, grove_version);
        let (_, mut on_lignin) = open_as(store, opened, &root, tree_type, grove_version);
        let expected = apply_as_a_client(&mut on_rocks, &batch, &[], grove_version);
        let found = apply_as_a_client(&mut on_lignin, &batch, &[], grove_version);
        assert_eq!(found, expected, "{}", at("a batch before the conversion"));
        assert_eq!(
            found.1,
            Ended::Done,
            "{}",
            at("a batch before the conversion")
        );
        root = on_rocks.root_key();
        drop((on_rocks, on_lignin));
        let expected = rocks.commit_multi_context_batch(storage_batch, Some(&rocks_tx));
        let found = staged.apply(&writer);
        assert_eq!(found.cost, expected.cost, "{}", at("a batch applied"));
        (expected.value).expect("the batch is applied on RocksDB");
        found.value.expect("the batch is applied");
        if round == 1 {
            rocks.commit_transaction(rocks_tx).unwrap().unwrap();
            rocks_tx = rocks.start_transaction();
            writer.into_scope().merge();
            assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
            transaction = path.begin();
            writer = LigninWriter::new(transaction.scope().unwrap());
        }
    }

    let storage_batch = StorageBatch::new();
    let context = rocks
        .get_transactional_storage_context_by_subtree_prefix(
            prefix,
            Some(&storage_batch),
            &rocks_tx,
        )
        .unwrap();
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(&staged));
    let (_, on_rocks) = open_as(context, opened, &root, tree_type, grove_version);
    let (_, on_lignin) = open_as(store, opened, &root, tree_type, grove_version);
    let (expected_cost, on_rocks, expected) = convert_as_a_client(on_rocks, grove_version);
    let (cost, on_lignin, found) = convert_as_a_client(on_lignin, grove_version);
    assert_eq!(found, expected, "{}", at("the conversion"));
    assert_eq!(cost, expected_cost, "{}", at("the cost of the conversion"));
    drop((on_rocks, on_lignin));
    let expected = rocks.commit_multi_context_batch(storage_batch, Some(&rocks_tx));
    let read_before = reads(&transaction);
    let applied = staged.apply(&writer);
    let read_after = reads(&transaction);
    assert_eq!(
        applied.cost,
        expected.cost,
        "{}",
        at("the batch of the conversion")
    );
    (expected.value).expect("the batch of the conversion is applied on RocksDB");
    let applied = applied
        .value
        .expect("the batch of the conversion is applied");
    let Ok((wide, _, root)) = found else {
        return false;
    };

    // Every node was written, as on RocksDB, and no value was: the store
    // held the bytes of each.
    let context = rocks
        .get_transactional_storage_context_by_subtree_prefix(prefix, None, &rocks_tx)
        .unwrap();
    let store = LigninNodeStore::deferred(&writer, subtree.clone(), None);
    let stored = records(&context, root.clone());
    assert_same_records(
        &records(&store, root.clone()),
        &stored,
        &at("the converted tree"),
    );
    let nodes = stored.len();
    assert_eq!(
        (applied.values_written, applied.values_kept),
        (0, nodes),
        "{}",
        at("the values of the converted tree")
    );
    // What keeping them took: one read of each node, where the conversion
    // had seen it, with the bytes of its value. No key was looked up, and
    // none of it is in the cost.
    assert_eq!(
        (read_after.0 - read_before.0, read_after.1 - read_before.1),
        (0, nodes as u64),
        "{}",
        at("the reads that kept the values")
    );
    // The batch was one put for each node, and each grew by 8 bytes for
    // the wider feature of its own and by 8 for the wider aggregate in its
    // parent's link, and by one more where the length of its cost takes
    // another byte to say.
    let growth = expected.cost.storage_cost.added_bytes as usize;
    assert_eq!(expected.cost.seek_count as usize, nodes);
    assert!(
        nodes > 0 && (16 * nodes..=17 * nodes).contains(&growth),
        "{growth} bytes added to {nodes} nodes"
    );
    let (expected, on_rocks) = open_as(context, opened, &root, wide, grove_version);
    let (cost, on_lignin) = open_as(store, opened, &root, wide, grove_version);
    assert_eq!(cost, expected, "{}", at("the open of the converted tree"));
    assert_eq!(
        read_as_a_client(&on_lignin, &keys, turn, grove_version),
        read_as_a_client(&on_rocks, &keys, turn, grove_version),
        "{}",
        at("the reads of the converted tree")
    );
    true
}

#[test]
fn a_conversion_to_a_big_sum_tree_costs_what_it_costs_on_rocksdb() {
    // The primary of an indexed tree is a Merk with the nodes of the tree
    // it mirrors. The rows of its rankings are nodes of other Merks, which
    // GroveDB removes and puts under wider keys.
    let indexed = [
        (TreeType::ProvableSumIndexedTree, TreeType::ProvableSumTree),
        (
            TreeType::ProvableCountProvableSumIndexedTree,
            TreeType::ProvableCountProvableSumTree,
        ),
    ];
    let tree_types: Vec<_> = (TREE_TYPES.into_iter())
        .map(|tree_type| (tree_type, tree_type))
        .chain(indexed)
        .collect();
    let mut converted = Vec::new();
    for (v, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        for (n, tree_type) in tree_types.iter().enumerate() {
            let seed = 0x2545_F491 + (v * 16 + n) as u64;
            for immediate in [false, true] {
                if a_conversion_costs_the_same(grove_version, *tree_type, immediate, seed) {
                    converted.push((v + 1, tree_type.0, immediate));
                }
            }
        }
    }
    // The grove version that has the conversion converts each of the seven
    // tree types that keep a 64-bit sum, in both ways of writing. Every
    // other conversion is refused, on both engines alike.
    let narrow = [
        TreeType::SumTree,
        TreeType::CountSumTree,
        TreeType::ProvableCountSumTree,
        TreeType::ProvableSumTree,
        TreeType::ProvableCountProvableSumTree,
        TreeType::ProvableSumIndexedTree,
        TreeType::ProvableCountProvableSumIndexedTree,
    ];
    let expected: Vec<_> = (narrow.into_iter())
        .flat_map(|tree_type| [(5, tree_type, false), (5, tree_type, true)])
        .collect();
    assert_eq!(converted, expected);
}
