//! The storage rules and the logical state against the RocksDB storage
//! layer itself: the same calls are made on both, and every cost and every
//! result is compared.

use grovedb_costs::{
    storage_cost::{
        key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes, StorageCost,
    },
    ChildrenSizesWithIsSumTree, OperationCost,
};
use grovedb_merk::{
    tree::{AggregateData, TreeNode},
    Link, TreeFeatureType,
};
use grovedb_storage::{
    rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbTransactionContext, RocksDbStorage},
    Batch, RawIterator, Storage, StorageBatch, StorageContext,
};

use super::{
    rules::{self, CursorMove, Keyspace},
    state::{
        charge_batch, BatchOp, Cursor, Database, Prefix, Record, StoredKey, Transaction, Write,
    },
};

type Path = &'static [&'static [u8]];

const ROOT: Path = &[];
const SHALLOW: Path = &[b"contracts"];
const DEEP: Path = &[b"contracts", &[7; 32], b"documents", &[9; 32]];

fn prefix(path: Path) -> Prefix {
    RocksDbStorage::build_prefix(path.into()).value
}

/// A node as Merk holds it before it writes its record.
fn node(
    key: &[u8],
    value_len: usize,
    feature: TreeFeatureType,
    children: [Option<usize>; 2],
) -> TreeNode {
    let link = |key_len: Option<usize>| {
        key_len.map(|key_len| Link::Reference {
            hash: [3; 32],
            child_heights: (1, 2),
            key: vec![0xCD; key_len],
            aggregate_data: AggregateData::from(feature),
        })
    };
    TreeNode::from_fields(
        key.to_vec(),
        vec![0xEE; value_len],
        [5; 32],
        link(children[0]),
        link(children[1]),
        feature,
    )
    .value
}

fn link_sizes(node: &TreeNode) -> ChildrenSizesWithIsSumTree {
    Some((
        node.feature_type()
            .tree_feature_specialized_type_and_length(),
        node.child_ref_and_sum_size(true),
        node.child_ref_and_sum_size(false),
    ))
}

fn added(added_bytes: u32) -> StorageCost {
    StorageCost {
        added_bytes,
        ..Default::default()
    }
}

/// The cost a Merk gives with a new node: no key cost yet, and the value
/// cost to be checked.
fn new_node_cost(value_cost: u32) -> KeyValueStorageCost {
    KeyValueStorageCost {
        key_storage_cost: StorageCost::default(),
        value_storage_cost: added(value_cost),
        new_node: true,
        needs_value_verification: true,
        prepaid: false,
    }
}

fn removal(key: u32, value: u32) -> KeyValueStorageCost {
    let removed = |removed| StorageCost {
        removed_bytes: StorageRemovedBytes::BasicStorageRemoval(removed),
        ..Default::default()
    };
    KeyValueStorageCost {
        key_storage_cost: removed(key),
        value_storage_cost: removed(value),
        ..Default::default()
    }
}

/// One write of a batch, to be staged on both sides.
struct Staged {
    keyspace: Keyspace,
    path: Path,
    key: &'static [u8],
    value: Option<Vec<u8>>,
    sizes: fn(&[u8]) -> ChildrenSizesWithIsSumTree,
    cost: Option<KeyValueStorageCost>,
}

fn no_sizes(_: &[u8]) -> ChildrenSizesWithIsSumTree {
    None
}

fn put(
    keyspace: Keyspace,
    path: Path,
    key: &'static [u8],
    value: &[u8],
    cost: Option<KeyValueStorageCost>,
) -> Staged {
    Staged {
        keyspace,
        path,
        key,
        value: Some(value.to_vec()),
        sizes: no_sizes,
        cost,
    }
}

fn delete(
    keyspace: Keyspace,
    path: Path,
    key: &'static [u8],
    cost: Option<KeyValueStorageCost>,
) -> Staged {
    Staged {
        keyspace,
        path,
        key,
        value: None,
        sizes: no_sizes,
        cost,
    }
}

fn put_node(path: Path, node: &'static TreeNode, checked: bool) -> Staged {
    fn sizes_of(record: &[u8]) -> ChildrenSizesWithIsSumTree {
        let node = TreeNode::decode_raw(
            record,
            b"node".to_vec(),
            None::<
                fn(
                    &[u8],
                    &grovedb_version::version::GroveVersion,
                ) -> Option<grovedb_merk::tree::kv::ValueDefinedCostType>,
            >,
            grovedb_version::version::GroveVersion::latest(),
        )
        .expect("a node record");
        link_sizes(&node)
    }
    let value_cost = node.value_encoding_length_with_parent_to_child_reference();
    Staged {
        keyspace: Keyspace::Data,
        path,
        key: node.key(),
        value: Some(node.encode()),
        sizes: sizes_of,
        cost: checked.then(|| new_node_cost(value_cost)),
    }
}

/// The RocksDB storage and the model, side by side, each with one open
/// transaction.
struct Sides<'s> {
    storage: &'s TempStorage,
    real: <RocksDbStorage as Storage<'s>>::Transaction,
    db: &'s Database,
    model: Transaction,
}

impl<'s> Sides<'s> {
    fn new(storage: &'s TempStorage, db: &'s Database) -> Self {
        Sides {
            storage,
            real: storage.start_transaction(),
            db,
            model: Transaction::new(),
        }
    }

    fn commit(self) {
        self.storage
            .commit_transaction(self.real)
            .value
            .expect("the transaction commits");
        self.model.commit(self.db);
    }

    /// Applies one batch on both sides and returns whether it was taken.
    fn apply(&mut self, writes: Vec<Staged>) -> bool {
        let batch = StorageBatch::new();
        let mut ops = Vec::new();
        for write in writes {
            let opened = self.storage.get_transactional_storage_context(
                write.path.into(),
                Some(&batch),
                &self.real,
            );
            assert_eq!(
                opened.cost,
                rules::open_by_path(write.path.iter().map(|segment| segment.len()))
            );
            let context = opened.value;
            let sizes = || write.value.as_deref().and_then(write.sizes);
            let staged = match (&write.value, write.keyspace) {
                (Some(value), Keyspace::Data) => {
                    context.put(write.key, value, sizes(), write.cost.clone())
                }
                (Some(value), Keyspace::Aux) => {
                    context.put_aux(write.key, value, write.cost.clone())
                }
                (Some(value), Keyspace::Roots) => {
                    context.put_root(write.key, value, write.cost.clone())
                }
                (Some(value), Keyspace::Meta) => {
                    context.put_meta(write.key, value, write.cost.clone())
                }
                (None, Keyspace::Data) => context.delete(write.key, write.cost.clone()),
                (None, Keyspace::Aux) => context.delete_aux(write.key, write.cost.clone()),
                (None, Keyspace::Roots) => context.delete_root(write.key, write.cost.clone()),
                (None, Keyspace::Meta) => context.delete_meta(write.key, write.cost.clone()),
            };
            assert_eq!(staged.cost, OperationCost::default());
            staged.value.expect("the write is staged");

            let key = StoredKey::new(prefix(write.path), write.key);
            ops.push(match write.value {
                Some(value) => BatchOp::Put {
                    keyspace: write.keyspace,
                    key,
                    children_sizes: (write.sizes)(&value),
                    cost_info: if write.keyspace == Keyspace::Data {
                        rules::complete_new_key_cost(write.cost, write.key.len())
                    } else {
                        write.cost
                    },
                    value,
                },
                None => BatchOp::Delete {
                    keyspace: write.keyspace,
                    key,
                    cost_info: write.cost,
                },
            });
        }
        ops.sort_by_key(|op| {
            let (keyspace, key) = match op {
                BatchOp::Put { keyspace, key, .. } | BatchOp::Delete { keyspace, key, .. } => {
                    (*keyspace, key.clone())
                }
            };
            let order = match keyspace {
                Keyspace::Meta => 0,
                Keyspace::Aux => 1,
                Keyspace::Roots => 2,
                Keyspace::Data => 3,
            };
            (order, key)
        });

        let applied = self
            .storage
            .commit_multi_context_batch(batch, Some(&self.real));
        let mut model_writes = Vec::new();
        let (on_success, at_once) = charge_batch(self.db, ops, &mut model_writes);
        let taken = on_success.is_ok();
        let model_cost = match on_success {
            Ok(on_success) => {
                self.model.write(model_writes);
                at_once + rules::batch_written(on_success, true)
            }
            Err(_) => at_once,
        };
        assert_eq!(applied.cost, model_cost);
        assert_eq!(applied.value.is_ok(), taken);
        taken
    }

    fn context(&self, path: Path) -> PrefixedRocksDbTransactionContext<'_> {
        self.storage
            .get_transactional_storage_context(path.into(), None, &self.real)
            .value
    }

    /// Reads one key on both sides and returns what is there.
    fn read(&self, keyspace: Keyspace, path: Path, key: &[u8]) -> Option<Vec<u8>> {
        let context = self.context(path);
        let real = match keyspace {
            Keyspace::Data => context.get(key),
            Keyspace::Aux => context.get_aux(key),
            Keyspace::Roots => context.get_root(key),
            Keyspace::Meta => context.get_meta(key),
        };
        let record = self
            .model
            .get(self.db, keyspace, &StoredKey::new(prefix(path), key));
        assert_eq!(
            real.cost,
            rules::read(keyspace, record.as_ref().map(Record::len)),
            "{keyspace:?} {key:?}"
        );
        let found = real.value.expect("the read succeeds");
        assert_eq!(found, record.map(|record| record.bytes()));
        found
    }

    /// Makes the same moves with a cursor of each side, looking at the
    /// validity, key and value after every one, and returns the keys the
    /// cursors stood on.
    fn walk(
        &self,
        keyspace: Keyspace,
        path: Path,
        moves: &[(CursorMove, &[u8])],
    ) -> Vec<Option<Vec<u8>>> {
        let context = self.context(path);
        let mut real = match keyspace {
            Keyspace::Data => context.raw_iter(),
            Keyspace::Aux => context.raw_iter_aux(),
            Keyspace::Meta => context.raw_iter_meta(),
            Keyspace::Roots => unreachable!("the storage layer has no cursor over root keys"),
        };
        let subtree = prefix(path);
        let mut model = Cursor::new(keyspace, self.model.base(self.db));
        let mut stood_on = Vec::new();
        for (movement, key) in moves {
            let moved = match movement {
                CursorMove::SeekToFirst => real.seek_to_first(),
                CursorMove::SeekToLast => real.seek_to_last(),
                CursorMove::Seek => real.seek(key),
                CursorMove::SeekForPrev => real.seek_for_prev(key),
                CursorMove::Next => real.next(),
                CursorMove::Prev => real.prev(),
            };
            assert_eq!(
                moved.cost,
                model.move_to(subtree, *movement, key, self.model.overlay())
            );
            let (valid, valid_cost) = model.valid(subtree);
            let real_valid = real.valid();
            assert_eq!((real_valid.value, real_valid.cost), (valid, valid_cost));
            let (model_key, key_cost) = model.key(subtree);
            let real_key = real.key();
            assert_eq!((real_key.value, real_key.cost), (model_key, key_cost));
            let (model_value, value_cost) = model.value(subtree);
            let real_value = real.value();
            assert_eq!(
                (real_value.value, real_value.cost),
                (model_value, value_cost)
            );
            stood_on.push(model_key.map(<[u8]>::to_vec));
        }
        stood_on
    }
}

#[test]
fn a_subtree_handle_costs_what_the_rules_say() {
    let storage = TempStorage::new();
    let tx = storage.start_transaction();
    let long: &[u8] = &[1; 255];
    let paths: [&[&[u8]]; 7] = [
        &[],
        &[b"a"],
        &[&[2; 55]],
        &[&[2; 56]],
        &[long, long],
        &[b"a", b"", b"c"],
        &[long, long, long, long, long, long, long, long],
    ];
    for path in paths {
        let expected = || rules::open_by_path(path.iter().map(|segment| segment.len()));
        assert_eq!(RocksDbStorage::build_prefix(path.into()).cost, expected());
        assert_eq!(
            storage
                .get_transactional_storage_context(path.into(), None, &tx)
                .cost,
            expected()
        );
        assert_eq!(
            storage.get_immediate_storage_context(path.into(), &tx).cost,
            expected()
        );
    }
    let subtree = prefix(SHALLOW);
    assert_eq!(
        storage
            .get_transactional_storage_context_by_subtree_prefix(subtree, None, &tx)
            .cost,
        rules::open_by_prefix()
    );
    assert_eq!(
        storage
            .get_immediate_storage_context_by_subtree_prefix(subtree, &tx)
            .cost,
        rules::open_by_prefix()
    );
    assert_eq!(
        RocksDbStorage::secondary_prefix_for(&subtree, 1).cost,
        rules::secondary_prefix()
    );
}

#[test]
fn batches_reads_and_cursors_cost_what_the_rules_say() {
    let storage = TempStorage::new();
    let db = Database::new();
    let mut sides = Sides::new(&storage, &db);

    let leak = |node: TreeNode| -> &'static TreeNode { Box::leak(Box::new(node)) };
    let leaf = leak(node(
        b"leaf",
        1,
        TreeFeatureType::BasicMerkNode,
        [None, None],
    ));
    let summed = leak(node(
        b"summed",
        40,
        TreeFeatureType::SummedMerkNode(-70_000),
        [Some(4), None],
    ));
    let big = TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(200, 9);
    let counted = leak(node(b"counted", 300, big, [Some(32), Some(255)]));
    let unpriced = leak(node(b"unpriced", 300, big, [None, Some(1)]));
    let root_cost = KeyValueStorageCost::for_updated_root_cost(None, 4);
    let as_given = KeyValueStorageCost {
        value_storage_cost: StorageCost {
            added_bytes: 3,
            replaced_bytes: 9,
            removed_bytes: StorageRemovedBytes::BasicStorageRemoval(2),
        },
        ..Default::default()
    };

    assert!(sides.apply(vec![
        put_node(SHALLOW, leaf, true),
        put_node(SHALLOW, summed, true),
        put_node(DEEP, counted, true),
        put_node(DEEP, unpriced, false),
        put(Keyspace::Data, SHALLOW, b"flat", &[4; 200], None),
        put(
            Keyspace::Data,
            DEEP,
            b"raw copy",
            &summed.encode(),
            Some(as_given.clone())
        ),
        put(Keyspace::Aux, ROOT, b"state", &[1; 130], None),
        put(Keyspace::Aux, ROOT, b"given", b"v", Some(as_given.clone())),
        put(Keyspace::Meta, SHALLOW, b"m", b"meta", None),
        put(Keyspace::Roots, ROOT, b"r", b"leaf", None),
        put(Keyspace::Roots, SHALLOW, b"r", b"leaf", Some(root_cost)),
        put(
            Keyspace::Data,
            SHALLOW,
            b"prepaid",
            b"paid for",
            Some(KeyValueStorageCost::prepaid())
        ),
        delete(Keyspace::Data, SHALLOW, b"never there", None),
        delete(Keyspace::Aux, ROOT, b"never there", Some(removal(40, 5))),
    ]));

    for (keyspace, path, key) in [
        (Keyspace::Data, SHALLOW, b"leaf".as_slice()),
        (Keyspace::Data, SHALLOW, b"summed"),
        (Keyspace::Data, DEEP, b"counted"),
        (Keyspace::Data, DEEP, b"raw copy"),
        (Keyspace::Data, SHALLOW, b"flat"),
        (Keyspace::Aux, ROOT, b"state"),
        (Keyspace::Meta, SHALLOW, b"m"),
        (Keyspace::Roots, SHALLOW, b"r"),
    ] {
        assert!(sides.read(keyspace, path, key).is_some(), "{key:?}");
    }
    for keyspace in Keyspace::ALL {
        assert_eq!(sides.read(keyspace, DEEP, b"absent"), None);
    }

    let wrong = KeyValueStorageCost {
        needs_value_verification: true,
        ..as_given.clone()
    };
    assert!(!sides.apply(vec![
        put(Keyspace::Meta, SHALLOW, b"before", b"charged a seek", None),
        put(Keyspace::Aux, SHALLOW, b"refused", b"value", Some(wrong)),
        put(Keyspace::Data, SHALLOW, b"after", b"never reached", None),
    ]));
    assert_eq!(sides.read(Keyspace::Meta, SHALLOW, b"before"), None);

    let through = [
        (CursorMove::SeekToFirst, b"".as_slice()),
        (CursorMove::Next, b""),
        (CursorMove::Next, b""),
        (CursorMove::Next, b""),
        (CursorMove::Next, b""),
        (CursorMove::Next, b""),
        (CursorMove::SeekToLast, b""),
        (CursorMove::Prev, b""),
        (CursorMove::Seek, b"m"),
        (CursorMove::SeekForPrev, b"m"),
        (CursorMove::Seek, b"zzz"),
        (CursorMove::SeekForPrev, b"a"),
        (CursorMove::Next, b""),
    ];
    let keys = sides.walk(Keyspace::Data, SHALLOW, &through);
    assert_eq!(keys[0].as_deref(), Some(b"flat".as_slice()));
    assert_eq!(keys[4], None);
    for (keyspace, path) in [
        (Keyspace::Data, DEEP),
        (Keyspace::Data, ROOT),
        (Keyspace::Aux, ROOT),
        (Keyspace::Aux, DEEP),
        (Keyspace::Meta, SHALLOW),
    ] {
        sides.walk(keyspace, path, &through);
    }

    assert!(sides.apply(vec![
        delete(Keyspace::Data, SHALLOW, b"flat", None),
        delete(Keyspace::Data, SHALLOW, b"summed", Some(removal(39, 120))),
        delete(Keyspace::Meta, SHALLOW, b"m", None),
        delete(Keyspace::Roots, SHALLOW, b"r", None),
        delete(Keyspace::Roots, ROOT, b"r", Some(removal(0, 0))),
    ]));
    assert_eq!(sides.read(Keyspace::Data, SHALLOW, b"flat"), None);
    sides.walk(Keyspace::Data, SHALLOW, &through);
    sides.commit();

    let mut sides = Sides::new(&storage, &db);
    assert!(sides.apply(vec![
        put(Keyspace::Data, SHALLOW, b"flat", b"again", None),
        put(Keyspace::Aux, ROOT, b"state", &[2; 131], None),
    ]));
    assert!(sides.apply(vec![
        delete(Keyspace::Data, SHALLOW, b"leaf", None),
        delete(Keyspace::Data, DEEP, b"counted", None),
        delete(Keyspace::Data, DEEP, b"raw copy", None),
        delete(Keyspace::Data, SHALLOW, b"flat", None),
        delete(Keyspace::Aux, ROOT, b"state", None),
        delete(Keyspace::Aux, ROOT, b"given", None),
    ]));
    for (keyspace, path) in [(Keyspace::Data, SHALLOW), (Keyspace::Aux, ROOT)] {
        sides.walk(keyspace, path, &through);
    }
    assert_eq!(sides.read(Keyspace::Data, DEEP, b"counted"), None);
    assert!(sides.read(Keyspace::Data, DEEP, b"unpriced").is_some());
    sides.commit();
}

#[test]
fn an_immediate_handle_costs_what_the_rules_say() {
    let storage = TempStorage::new();
    let tx = storage.start_transaction();
    let context = storage
        .get_immediate_storage_context(SHALLOW.into(), &tx)
        .value;
    let db = Database::new();
    let mut model = Transaction::new();
    let key = |key: &[u8]| StoredKey::new(prefix(SHALLOW), key);

    let direct = context.put(b"direct", b"value", None, Some(new_node_cost(1)));
    assert_eq!(direct.cost, rules::write_immediate());
    direct.value.expect("the cost of a direct write is ignored");
    model.write([Write {
        keyspace: Keyspace::Data,
        key: key(b"direct"),
        record: Some(Record::new(Keyspace::Data, b"value")),
    }]);
    let read = context.get(b"direct");
    assert_eq!(read.cost, rules::read_immediate());
    assert_eq!(
        read.value.expect("the read succeeds"),
        model
            .get(&db, Keyspace::Data, &key(b"direct"))
            .map(|record| record.bytes())
    );

    let summed = node(
        b"summed",
        40,
        TreeFeatureType::SummedMerkNode(5),
        [None, Some(9)],
    );
    let value_cost = summed.value_encoding_length_with_parent_to_child_reference();
    let record = summed.encode();
    let mut batch = context.new_batch();
    let mut expected = OperationCost::default();
    let mut expect = |charge: Result<rules::WriteCharge, (_, OperationCost)>| match charge {
        Ok(charge) => charge.add_all_to(&mut expected),
        Err((_, owed)) => expected += owed,
    };

    batch
        .put(
            b"summed",
            &record,
            link_sizes(&summed),
            Some(new_node_cost(value_cost)),
        )
        .expect("a node with its cost");
    expect(rules::immediate_batch_put(
        Keyspace::Data,
        6,
        Record::new(Keyspace::Data, &record).len(),
        &link_sizes(&summed),
        rules::complete_new_key_cost(Some(new_node_cost(value_cost)), 6).as_ref(),
    ));
    batch.put_aux(b"aux", b"value", None).expect("an aux put");
    expect(rules::immediate_batch_put(Keyspace::Aux, 3, 5, &None, None));
    batch
        .put_root(b"r", b"summed", None)
        .expect("a free root put");
    expect(rules::immediate_batch_put(
        Keyspace::Roots,
        1,
        6,
        &None,
        None,
    ));
    batch
        .put_aux(b"refused", b"value", Some(new_node_cost(1)))
        .expect_err("a cost that does not match");
    expect(rules::immediate_batch_put(
        Keyspace::Aux,
        7,
        5,
        &None,
        Some(&new_node_cost(1)),
    ));
    batch.delete(b"direct", None);
    expect(Ok(rules::immediate_batch_delete(None)));
    batch.delete_aux(b"gone", Some(removal(35, 6)));
    expect(Ok(rules::immediate_batch_delete(Some(removal(35, 6)))));
    batch.delete_root(b"gone", None);
    expect(Ok(rules::immediate_batch_delete(None)));

    let committed = context.commit_batch(batch);
    assert_eq!(committed.cost, expected);
    committed.value.expect("the batch is written");
    assert_eq!(context.get(b"direct").value.expect("a read"), None);
    assert_eq!(context.get(b"summed").value.expect("a read"), Some(record));
}
