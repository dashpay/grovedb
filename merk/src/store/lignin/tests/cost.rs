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

use std::collections::HashMap;

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
use grovedb_storage::{rocksdb_storage::RocksDbStorage, Error as StorageError};
use grovedb_version::version::GROVE_VERSIONS;

use super::*;
use crate::{proofs::Query, store::NodeWriteCost};

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

fn old_specialized_cost(value: &[u8]) -> Result<u32, Error> {
    Ok(match value.first() {
        Some(b'S') => SPECIALIZED,
        Some(b'T') => LAYERED,
        _ => value.len() as u32,
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

// ---------------------------------------------------------------------
// A Merk on both stores
// ---------------------------------------------------------------------

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
    done(merk.apply_with_costs_just_in_time_value_update(
        batch,
        aux,
        None,
        &|_key, value| old_specialized_cost(value),
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

/// Every read of a Merk the comparison makes, each with what it cost.
fn read_as_a_client<'db, S: NodeStore<'db>>(
    merk: &Merk<S>,
    keys: &[Vec<u8>],
    grove_version: &GroveVersion,
) -> Vec<(OperationCost, String)> {
    let mut reads = Vec::new();
    let mut read = |cost: OperationCost, found: String| reads.push((cost, found));
    for (n, key) in keys.iter().enumerate() {
        let cached = n.is_multiple_of(2);
        let found = merk.get(key, cached, VALUE_DEFINED_COST, grove_version);
        read(found.cost, format!("{:?}", found.value));
        let found = merk.exists(key, VALUE_DEFINED_COST, grove_version);
        read(found.cost, format!("{:?}", found.value));
        let found = merk.exists_by_traversing_tree(key, VALUE_DEFINED_COST, grove_version);
        read(found.cost, format!("{:?}", found.value));
        let found = merk.get_feature_type(key, !cached, VALUE_DEFINED_COST, grove_version);
        read(found.cost, format!("{:?}", found.value));
        let found = merk.get_hash(key, cached, VALUE_DEFINED_COST, grove_version);
        read(found.cost, format!("{:?}", found.value));
        let found = merk.get_value_and_value_hash(key, !cached, VALUE_DEFINED_COST, grove_version);
        read(found.cost, format!("{:?}", found.value));
    }
    let mut query = Query::new();
    match keys {
        [first, .., last] if first <= last => {
            query.insert_range_inclusive(first.clone()..=last.clone())
        }
        _ => query.insert_all(),
    }
    let proved = merk.prove(query, Some(7), grove_version);
    let proof = proved.value.map(|proved| proved.proof);
    read(proved.cost, format!("{proof:?}"));
    let hash = merk.root_hash();
    read(hash.cost, format!("{:?}", hash.value));
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
            let keys: Vec<_> = (0..4).map(|_| key(random.below(120))).collect();
            assert_eq!(
                read_as_a_client(&on_lignin, &keys, grove_version),
                read_as_a_client(&on_rocks, &keys, grove_version),
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
        let keys: Vec<_> = (0..4).map(|_| key(random.below(120))).collect();
        assert_eq!(
            read_as_a_client(&on_lignin, &keys, grove_version),
            read_as_a_client(&on_rocks, &keys, grove_version),
            "{}",
            at(round, "the reads")
        );
    }
}

#[test]
fn a_merk_on_lignindb_costs_what_it_costs_on_rocksdb_on_every_grove_version() {
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
            a_merk_costs_the_same(grove_version, tree_type, opened, false, seed, 18);
            a_merk_costs_the_same(grove_version, tree_type, opened, true, seed, 12);
        }
    }
}

// ---------------------------------------------------------------------
// Each operation of the seam on both engines
// ---------------------------------------------------------------------

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

/// What a put of `node` is to pay for its value, by the rule the storage
/// layer checks a writer's cost with (`cost-model.md` K2).
fn paid_value_len(node: &TreeNode) -> u32 {
    use integer_encoding::VarInt;

    let stored_key_len = (32 + node.key().len()) as u32;
    let sizes = (
        node.feature_type()
            .tree_feature_specialized_type_and_length(),
        node.child_ref_and_sum_size(true),
        node.child_ref_and_sum_size(false),
    );
    let mut cost = OperationCost::default();
    cost.add_key_value_storage_costs(
        stored_key_len,
        node.encoding_length() as u32,
        Some(sizes),
        None,
    )
    .expect("a put with no cost of the writer's");
    cost.storage_cost.added_bytes - stored_key_len - stored_key_len.required_space() as u32
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
    let ended = match applied.value {
        Ok(()) => Ended::Done,
        Err(StorageError::CostError(_)) => Ended::Failed("cost"),
        Err(_) => Ended::Failed("storage"),
    };
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
            Part::Unstaged(script) => {
                let open = |prefix| {
                    rocks
                        .get_transactional_storage_context_by_subtree_prefix(
                            prefix,
                            None,
                            &transaction,
                        )
                        .unwrap()
                };
                (run_script(open, script), None)
            }
            Part::Immediate(script) => {
                let open = |prefix| {
                    rocks
                        .get_immediate_storage_context_by_subtree_prefix(prefix, &transaction)
                        .unwrap()
                };
                (run_script(open, script), None)
            }
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
            Part::Unstaged(script) => {
                let open =
                    |prefix| LigninNodeStore::deferred(&writer, LigninSubtree::alone(prefix), None);
                (run_script(open, script), None)
            }
            Part::Immediate(script) => {
                let open =
                    |prefix| LigninNodeStore::immediate(&writer, LigninSubtree::alone(prefix));
                (run_script(open, script), None)
            }
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

    // A put whose cost does not match its record refuses the batch. What
    // came before it in the order of a batch stays charged, and what
    // comes after it is not reached.
    let refused = vec![
        step(22, Step::PutMeta(b"before".to_vec(), vec![1; 30])),
        step(22, Step::Commit(vec![Change::DeleteAux(k.clone(), None)])),
        put_lone(21, &plain(b"early", 9), NodeChange::ValueWritten, Bill::New),
        put_lone(
            22,
            &plain(b"q", 50),
            NodeChange::ValueWritten,
            Bill::Mismatched,
        ),
        put_lone(23, &plain(b"late", 9), NodeChange::ValueWritten, Bill::New),
    ];
    let after_the_refusal = vec![
        step(21, Step::Has(b"early".to_vec())),
        step(23, Step::Has(b"late".to_vec())),
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
        assert_eq!(found.len(), parts.len());
        for (n, ((found, expected), part)) in found.iter().zip(&expected).zip(&parts).enumerate() {
            let script = match part {
                Part::Deferred(script)
                | Part::Unstaged(script)
                | Part::Immediate(script)
                | Part::NotTaken(script) => script,
                Part::Commit => unreachable!("a commit is no act"),
            };
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
        let refused = applied(3);
        assert_eq!(refused.1, Ended::Failed("cost"));
        assert!(refused.0.seek_count >= 4, "{refused:?}");
        assert_eq!(refused.0.storage_cost, StorageCost::default());
        let not_taken = applied(5);
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
    // test calls no function of the model itself.
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

    // A cost from beside the model does not balance.
    let store = Lignin::new();
    let path = store.path();
    let transaction = path.begin();
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    let read = reader.has_node(b"k");
    assert!(!read.value.expect("the read"));
    assert_eq!(read.cost, ledger::issued());
    let beside = read.cost + OperationCost::with_seek_count(1);
    assert_ne!(beside, ledger::take());
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
    let created = held.len()
        - held
            .iter()
            .filter(|(key, _)| committed_lens.contains_key(key))
            .count();
    assert!(
        created > 3 && created < held.len(),
        "{created} of {}",
        held.len()
    );
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
