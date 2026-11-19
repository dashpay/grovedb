// With no engine in the build, the tests that compare the in-memory store
// with another are left out, and the helpers only they call are unused.
#![cfg_attr(not(merk_test_engine), allow(dead_code))]

use std::collections::BTreeMap;

use grovedb_version::version::GroveVersion;

use super::{mem::MemNodeStore, NodeChange, NodeCursor, NodeStore};
#[cfg(merk_test_engine)]
use crate::test_utils::TempMerk;
use crate::{
    merk::{chunks::ChunkProducer, restore::Restorer},
    proofs::Query,
    tree::{kv::ValueDefinedCostType, TreeNode},
    KVIterator, Merk, Op, TreeFeatureType, TreeType,
};

const NO_VALUE_DEFINED_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> =
    None;

fn assert_send<T: Send>() {}

fn merk_is_send_when_its_node_store_is<S: Send>() {
    assert_send::<Merk<S>>();
}

fn open(store: &MemNodeStore, tree_type: TreeType) -> Merk<MemNodeStore> {
    Merk::open_base(
        store.clone(),
        tree_type,
        NO_VALUE_DEFINED_COST,
        GroveVersion::latest(),
    )
    .unwrap()
    .expect("the store opens")
}

fn feature(tree_type: TreeType, n: u64) -> TreeFeatureType {
    let sum = (n % 41) as i64 - 20;
    match tree_type {
        TreeType::NormalTree => TreeFeatureType::BasicMerkNode,
        TreeType::SumTree => TreeFeatureType::SummedMerkNode(sum),
        TreeType::CountSumTree => TreeFeatureType::CountedSummedMerkNode(1, sum),
        TreeType::ProvableCountTree => TreeFeatureType::ProvableCountedMerkNode(1),
        other => panic!("no feature type chosen for {other:?}"),
    }
}

fn put(tree_type: TreeType, key: u64, value: u64) -> (Vec<u8>, Op) {
    let mut bytes = vec![123];
    bytes.extend(value.to_be_bytes());
    bytes.resize(9 + (value % 90) as usize, value as u8);
    (
        key.to_be_bytes().to_vec(),
        Op::Put(bytes, feature(tree_type, value)),
    )
}

fn delete(key: u64) -> (Vec<u8>, Op) {
    (key.to_be_bytes().to_vec(), Op::Delete)
}

fn apply<'db, S: NodeStore<'db>>(merk: &mut Merk<S>, batch: &[(Vec<u8>, Op)]) {
    merk.apply::<_, Vec<u8>>(batch, &[], None, GroveVersion::latest())
        .unwrap()
        .expect("the batch applies");
}

fn records<'db, S: NodeStore<'db>>(store: &S) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut cursor = store.cursor();
    cursor.seek_to_first().unwrap();
    let mut records = vec![];
    while cursor.valid().unwrap() {
        records.push((
            cursor.key().unwrap().unwrap().to_vec(),
            cursor.value().unwrap().unwrap().to_vec(),
        ));
        cursor.next().unwrap();
    }
    records
}

fn scan<'db, S: NodeStore<'db>>(store: &S, left_to_right: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut query = Query::new_with_direction(left_to_right);
    query.insert_all();
    let mut rows = KVIterator::new(store.cursor(), &query).unwrap();
    let mut found = vec![];
    while let Some(row) = rows.next_kv().unwrap() {
        found.push(row);
    }
    found
}

/// Names the first key whose record differs, with both nodes decoded.
fn assert_same_records(found: &[(Vec<u8>, Vec<u8>)], expected: &[(Vec<u8>, Vec<u8>)]) {
    use ed::Decode;

    use crate::tree::TreeNodeInner;

    let found_keys: Vec<_> = found.iter().map(|(key, _)| key).collect();
    let expected_keys: Vec<_> = expected.iter().map(|(key, _)| key).collect();
    assert_eq!(found_keys, expected_keys, "the stores hold the same keys");
    for ((key, found), (_, expected)) in found.iter().zip(expected) {
        assert_eq!(
            TreeNodeInner::decode(found.as_slice()).unwrap(),
            TreeNodeInner::decode(expected.as_slice()).unwrap(),
            "the node under {key:?}"
        );
        assert_eq!(found, expected, "the record under {key:?}");
    }
}

fn count_changes(seen: &mut [u32; 4], store: &MemNodeStore) {
    for (_, change) in store.take_log() {
        seen[match change {
            Some(NodeChange::ValueWritten) => 0,
            Some(NodeChange::StructureChanged) => 1,
            Some(NodeChange::HashOnly) => 2,
            None => 3,
        }] += 1;
    }
}

struct Random(u64);

impl Random {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// One batch over a small key space: new keys, rewritten values and
/// removals of keys the model holds, sorted by key.
fn random_batch(
    random: &mut Random,
    tree_type: TreeType,
    model: &mut BTreeMap<u64, u64>,
) -> Vec<(Vec<u8>, Op)> {
    let mut batch = BTreeMap::new();
    for _ in 0..1 + random.next(12) {
        let key = random.next(400);
        if model.contains_key(&key) && random.next(3) == 0 {
            model.remove(&key);
            batch.insert(key, delete(key));
        } else {
            let value = random.next(u64::MAX);
            model.insert(key, value);
            batch.insert(key, put(tree_type, key, value));
        }
    }
    batch.into_values().collect()
}

/// Runs one random history on the in-memory store and on RocksDB. The
/// in-memory store keeps of each written node only what its change kind
/// names, so equal records mean every kind told the truth.
#[cfg(merk_test_engine)]
fn same_tree_on_both_stores(tree_type: TreeType, seed: u64, reopen_every: usize) -> [u32; 4] {
    let grove_version = GroveVersion::latest();
    let store = MemNodeStore::new();
    let mut merk = open(&store, tree_type);
    let mut reference = TempMerk::new_with_tree_type(grove_version, tree_type);
    let mut random = Random(seed);
    let mut model = BTreeMap::new();
    let mut seen = [0; 4];

    for round in 0..120 {
        let batch = random_batch(&mut random, tree_type, &mut model);
        apply(&mut merk, &batch);
        apply(&mut reference, &batch);
        reference.commit(grove_version);
        if round % reopen_every == reopen_every - 1 {
            merk = open(&store, tree_type);
        }

        count_changes(&mut seen, &store);
        assert_eq!(merk.root_hash().unwrap(), reference.root_hash().unwrap());
        assert_eq!(
            merk.is_empty_tree().unwrap(),
            reference.is_empty_tree().unwrap()
        );
        if round % 10 == 9 {
            assert_same_records(&store.records(), &records(&reference.storage));
        }
    }

    assert_same_records(&store.records(), &records(&reference.storage));
    assert_same_records(&records(&store), &records(&reference.storage));
    assert_eq!(store.root_key().unwrap().unwrap(), merk.root_key());
    for left_to_right in [true, false] {
        assert_eq!(
            scan(&store, left_to_right),
            scan(&reference.storage, left_to_right)
        );
    }
    for key in model.keys() {
        let key = key.to_be_bytes();
        for allow_cache in [true, false] {
            assert_eq!(
                merk.get(&key, allow_cache, NO_VALUE_DEFINED_COST, grove_version)
                    .unwrap()
                    .unwrap(),
                reference
                    .get(&key, allow_cache, NO_VALUE_DEFINED_COST, grove_version)
                    .unwrap()
                    .unwrap()
            );
        }
        let mut query = Query::new();
        query.insert_key(key.to_vec());
        assert_eq!(
            merk.prove(query.clone(), None, grove_version)
                .unwrap()
                .unwrap()
                .proof,
            reference
                .prove(query, None, grove_version)
                .unwrap()
                .unwrap()
                .proof
        );
    }
    seen
}

#[test]
fn merk_and_its_node_store_handle_are_send() {
    merk_is_send_when_its_node_store_is::<MemNodeStore>();
    assert_send::<TreeNode>();
    assert_send::<MemNodeStore>();
    assert_send::<<MemNodeStore as NodeStore<'static>>::ChangeSet>();
    assert_send::<<MemNodeStore as NodeStore<'static>>::Cursor>();
}

#[test]
fn a_merk_can_be_written_from_one_thread_after_another() {
    let tree_type = TreeType::NormalTree;
    let first: Vec<_> = (0..40).map(|n| put(tree_type, n, n)).collect();
    let second: Vec<_> = (20..60).map(|n| put(tree_type, n, n + 1000)).collect();

    let store = MemNodeStore::new();
    let mut merk = open(&store, tree_type);
    apply(&mut merk, &first);
    let merk = std::thread::spawn({
        let second = second.clone();
        move || {
            apply(&mut merk, &second);
            merk
        }
    })
    .join()
    .unwrap();

    let mut reference = open(&MemNodeStore::new(), tree_type);
    apply(&mut reference, &first);
    apply(&mut reference, &second);
    assert_eq!(merk.root_hash().unwrap(), reference.root_hash().unwrap());
    assert_eq!(merk.storage.records(), reference.storage.records());
}

/// How often the Merk on the in-memory store is dropped and opened again:
/// after every commit, so each node is loaded fresh; after every fifth, so
/// loaded nodes are committed and then changed again; and never, so every
/// node is one this Merk created.
const REOPEN_EVERY: [usize; 3] = [1, 5, usize::MAX];

#[cfg(merk_test_engine)]
#[test]
fn a_merk_gives_the_same_tree_on_a_store_that_keeps_only_what_changed() {
    let mut seen_by_reopening = [[0; 4]; 3];
    for (seed, tree_type) in [
        TreeType::NormalTree,
        TreeType::SumTree,
        TreeType::CountSumTree,
        TreeType::ProvableCountTree,
    ]
    .into_iter()
    .enumerate()
    {
        for (seen, reopen_every) in seen_by_reopening.iter_mut().zip(REOPEN_EVERY) {
            let counts =
                same_tree_on_both_stores(tree_type, 0x9E37_79B9 + seed as u64, reopen_every);
            for (total, count) in seen.iter_mut().zip(counts) {
                *total += count;
            }
        }
    }

    let [every_commit, every_fifth, never] = seen_by_reopening;
    for seen in [every_commit, every_fifth] {
        assert!(
            seen.iter().all(|count| *count > 100),
            "every kind of change is written: {seen:?}"
        );
    }
    assert_eq!(
        never[1..3],
        [0, 0],
        "a node this Merk created has no stored state to keep"
    );
}

/// Puts and removes elements the way GroveDB does, through the `Element`
/// functions, on the in-memory store and on RocksDB: items, sum items,
/// references and subtrees, whose node value hash is given and changes
/// while their bytes stay.
#[cfg(feature = "rocksdb_storage")]
fn same_elements_on_both_stores(tree_type: TreeType, seed: u64, reopen_every: usize) -> [u32; 4] {
    use grovedb_element::{reference_path::ReferencePathType, Element};
    use grovedb_path::SubtreePath;
    use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};

    use crate::element::{
        costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions,
        insert::ElementInsertToStorageExtensions,
    };

    fn step<'db, S: NodeStore<'db>>(
        merk: &mut Merk<S>,
        tree_type: TreeType,
        key: u64,
        remove: bool,
        payload: u64,
    ) {
        let grove_version = GroveVersion::latest();
        let hash = *blake3::hash(&payload.to_be_bytes()).as_bytes();
        let key_bytes = key.to_be_bytes();
        let is_tree = key % 4 == 2;
        if remove {
            Element::delete(merk, key_bytes, None, is_tree, tree_type, grove_version)
                .unwrap()
                .expect("the element is removed");
            return;
        }
        match key % 4 {
            0 => Element::new_item(vec![payload as u8; 1 + (payload % 70) as usize]).insert(
                merk,
                key_bytes,
                None,
                grove_version,
            ),
            1 => Element::new_reference(ReferencePathType::AbsolutePathReference(vec![payload
                .to_be_bytes()
                .to_vec()]))
            .insert_reference(merk, key_bytes, hash, None, grove_version),
            2 => Element::empty_tree().insert_subtree(merk, key_bytes, hash, None, grove_version),
            _ if tree_type == TreeType::SumTree => Element::new_sum_item(payload as i64 % 1000)
                .insert(merk, key_bytes, None, grove_version),
            _ => Element::new_item_with_flags(vec![7; 9], Some(vec![payload as u8; 3])).insert(
                merk,
                key_bytes,
                None,
                grove_version,
            ),
        }
        .unwrap()
        .expect("the element is put");
    }

    let grove_version = GroveVersion::latest();
    let element_cost = Some(&Element::value_defined_cost_for_serialized_value);
    let storage = TempStorage::new();
    let tx = storage.start_transaction();
    let store = MemNodeStore::new();
    let open_mem = || {
        Merk::open_base(store.clone(), tree_type, element_cost, grove_version)
            .unwrap()
            .expect("the store opens")
    };
    let mut merk = open_mem();
    let mut random = Random(seed);
    let mut held = std::collections::BTreeSet::new();
    let mut seen = [0; 4];

    for round in 0..150 {
        let batch = StorageBatch::new();
        let mut reference = Merk::open_base(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), &tx)
                .unwrap(),
            tree_type,
            element_cost,
            grove_version,
        )
        .unwrap()
        .expect("the reference opens");

        for _ in 0..1 + random.next(4) {
            let key = random.next(120);
            let remove = held.contains(&key) && random.next(4) == 0;
            let payload = random.next(u64::MAX);
            if remove {
                held.remove(&key);
            } else {
                held.insert(key);
            }
            step(&mut merk, tree_type, key, remove, payload);
            step(&mut reference, tree_type, key, remove, payload);
            batch.next_operation();
        }

        let reference_root = reference.root_hash().unwrap();
        drop(reference);
        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("the reference commits");
        assert_eq!(merk.root_hash().unwrap(), reference_root);
        if round % reopen_every == reopen_every - 1 {
            merk = open_mem();
        }
        count_changes(&mut seen, &store);
    }

    let reference = storage
        .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
        .unwrap();
    assert_same_records(&store.records(), &records(&reference));
    seen
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn elements_give_the_same_tree_on_a_store_that_keeps_only_what_changed() {
    for (seed, tree_type) in [TreeType::NormalTree, TreeType::SumTree]
        .into_iter()
        .enumerate()
    {
        for reopen_every in REOPEN_EVERY {
            let seen =
                same_elements_on_both_stores(tree_type, 0xC2B2_AE35 + seed as u64, reopen_every);
            if reopen_every != usize::MAX {
                assert!(
                    seen.iter().all(|count| *count > 20),
                    "every kind of change is written: {seen:?}"
                );
            }
        }
    }
}

#[test]
fn a_commit_names_what_changed_in_each_node() {
    let tree_type = TreeType::NormalTree;
    let key = |n: u64| n.to_be_bytes().to_vec();
    let log = |store: &MemNodeStore| -> BTreeMap<Vec<u8>, Option<NodeChange>> {
        store.take_log().into_iter().collect()
    };

    let store = MemNodeStore::new();
    let seven: Vec<_> = (1..=7).map(|n| put(tree_type, n, n)).collect();
    apply(&mut open(&store, tree_type), &seven);
    assert_eq!(
        log(&store),
        (1..=7)
            .map(|n| (key(n), Some(NodeChange::ValueWritten)))
            .collect()
    );
    assert_eq!(store.root_key().unwrap().unwrap(), Some(key(4)));

    apply(&mut open(&store, tree_type), &[put(tree_type, 1, 100)]);
    assert_eq!(
        log(&store),
        BTreeMap::from([
            (key(1), Some(NodeChange::ValueWritten)),
            (key(2), Some(NodeChange::HashOnly)),
            (key(4), Some(NodeChange::HashOnly)),
        ])
    );

    apply(&mut open(&store, tree_type), &[put(tree_type, 8, 8)]);
    assert_eq!(
        log(&store),
        BTreeMap::from([
            (key(8), Some(NodeChange::ValueWritten)),
            (key(7), Some(NodeChange::StructureChanged)),
            (key(6), Some(NodeChange::HashOnly)),
            (key(4), Some(NodeChange::HashOnly)),
        ])
    );

    apply(&mut open(&store, tree_type), &[delete(8)]);
    assert_eq!(
        log(&store),
        BTreeMap::from([
            (key(8), None),
            (key(7), Some(NodeChange::StructureChanged)),
            (key(6), Some(NodeChange::HashOnly)),
            (key(4), Some(NodeChange::HashOnly)),
        ])
    );

    apply(&mut open(&store, tree_type), &[]);
    assert_eq!(
        log(&store),
        BTreeMap::from([(key(4), Some(NodeChange::HashOnly))]),
        "a commit that changes nothing still writes the root node"
    );

    let mut merk = open(&store, tree_type);
    apply(&mut merk, &[put(tree_type, 8, 8), put(tree_type, 9, 9)]);
    assert_eq!(
        log(&store),
        BTreeMap::from([
            (key(8), Some(NodeChange::ValueWritten)),
            (key(9), Some(NodeChange::ValueWritten)),
            (key(7), Some(NodeChange::StructureChanged)),
            (key(6), Some(NodeChange::StructureChanged)),
            (key(4), Some(NodeChange::HashOnly)),
        ])
    );
    assert_eq!(merk.height(), Some(4));
}

#[test]
fn a_node_this_merk_inserted_is_written_whole_until_it_is_loaded_again() {
    let tree_type = TreeType::NormalTree;
    let store = MemNodeStore::new();
    let mut merk = open(&store, tree_type);
    apply(
        &mut merk,
        &(1..=7).map(|n| put(tree_type, n, n)).collect::<Vec<_>>(),
    );
    store.take_log();

    apply(&mut merk, &[put(tree_type, 1, 100)]);
    assert!(store
        .take_log()
        .into_iter()
        .all(|(_, change)| change == Some(NodeChange::ValueWritten)));
}

#[test]
fn chunk_restore_writes_through_the_node_store() {
    let grove_version = GroveVersion::latest();
    let tree_type = TreeType::NormalTree;
    let mut source = open(&MemNodeStore::new(), tree_type);
    apply(
        &mut source,
        &(0..1000).map(|n| put(tree_type, n, n)).collect::<Vec<_>>(),
    );

    let restored_store = MemNodeStore::new();
    let mut producer = ChunkProducer::new(&source).expect("the source produces chunks");
    let mut restorer = Restorer::new(
        open(&restored_store, tree_type),
        source.root_hash().unwrap(),
        None,
    );
    let mut next = Some(vec![]);
    while let Some(chunk_id) = next {
        let (chunk, following) = producer.chunk(&chunk_id, grove_version).unwrap();
        restorer
            .process_chunk(&chunk_id, chunk, grove_version)
            .expect("the chunk restores");
        next = following;
    }
    let restored = restorer.finalize(grove_version).expect("restore finalizes");

    assert_eq!(restored.root_hash().unwrap(), source.root_hash().unwrap());
    assert_eq!(restored_store.records(), source.storage.records());
    let kinds: Vec<_> = restored_store
        .take_log()
        .into_iter()
        .map(|(_, change)| change)
        .collect();
    for kind in [
        NodeChange::ValueWritten,
        NodeChange::StructureChanged,
        NodeChange::HashOnly,
    ] {
        assert!(kinds.contains(&Some(kind)), "restore writes {kind:?}");
    }
}

#[test]
fn a_merk_keeps_its_side_records_in_its_node_store() {
    let grove_version = GroveVersion::latest();
    let store = MemNodeStore::new();
    let mut merk = open(&store, TreeType::NormalTree);

    merk.apply::<_, Vec<u8>>(
        &[put(TreeType::NormalTree, 1, 1)],
        &[(
            b"aux".to_vec(),
            Op::Put(b"a".to_vec(), TreeFeatureType::BasicMerkNode),
            None,
        )],
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert_eq!(merk.get_aux(b"aux").unwrap().unwrap(), Some(b"a".to_vec()));
    merk.apply::<Vec<u8>, _>(
        &[],
        &[(b"aux".to_vec(), Op::Delete, None)],
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert_eq!(merk.get_aux(b"aux").unwrap().unwrap(), None);

    merk.put_meta(b"meta".to_vec(), b"m".to_vec())
        .unwrap()
        .unwrap();
    assert_eq!(
        open(&store, TreeType::NormalTree)
            .get_meta(b"meta".to_vec())
            .unwrap()
            .unwrap(),
        Some(b"m".as_slice())
    );
    merk.delete_meta(b"meta").unwrap().unwrap();
    assert_eq!(
        open(&store, TreeType::NormalTree)
            .get_meta(b"meta".to_vec())
            .unwrap()
            .unwrap(),
        None
    );

    merk.set_base_root_key(None).unwrap().unwrap();
    assert_eq!(store.root_key().unwrap().unwrap(), None);
    assert!(open(&store, TreeType::NormalTree).root_key().is_none());
    merk.set_base_root_key(Some(1u64.to_be_bytes().to_vec()))
        .unwrap()
        .unwrap();
    assert!(open(&store, TreeType::NormalTree).root_key().is_some());

    merk.clear().unwrap().unwrap();
    assert!(store.records().is_empty());
    assert!(merk.is_empty_tree().unwrap());
}

#[test]
fn a_cursor_seeks_and_steps_in_both_directions() {
    let tree_type = TreeType::NormalTree;
    let store = MemNodeStore::new();
    apply(
        &mut open(&store, tree_type),
        &[2, 4, 6].map(|n| put(tree_type, n, n)),
    );
    let key = |n: u64| n.to_be_bytes().to_vec();
    let at = |cursor: &<MemNodeStore as NodeStore<'static>>::Cursor| {
        cursor.key().unwrap().map(<[u8]>::to_vec)
    };

    assert!(store.has_node(&key(4)).unwrap().unwrap());
    assert!(!store.has_node(&key(3)).unwrap().unwrap());

    let mut cursor = store.cursor();
    assert!(!cursor.valid().unwrap());
    assert_eq!(cursor.value().unwrap(), None);

    cursor.seek(key(3)).unwrap();
    assert_eq!(at(&cursor), Some(key(4)));
    cursor.seek_for_prev(key(3)).unwrap();
    assert_eq!(at(&cursor), Some(key(2)));
    cursor.prev().unwrap();
    assert_eq!(at(&cursor), None);
    cursor.seek_for_prev(key(1)).unwrap();
    assert_eq!(at(&cursor), None);

    cursor.seek_to_last().unwrap();
    assert_eq!(at(&cursor), Some(key(6)));
    cursor.next().unwrap();
    assert_eq!(at(&cursor), None);
    cursor.seek(key(7)).unwrap();
    assert_eq!(at(&cursor), None);
    assert!(cursor.status().is_ok());
}
