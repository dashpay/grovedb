//! Records that no node links (MERK-09). Inside one operation a storage
//! batch keeps a put over a later delete of the same key (GM0), so a
//! change set can reach a store with a node that the root it states does
//! not reach, and RocksDB holds that node as a record under its key. The
//! same calls on a Merk on both engines give the same answers and charge
//! the same: by key, under a cursor, and when the record is written again,
//! linked into the tree or removed.

use grovedb_costs::storage_cost::{
    key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes, StorageCost,
};

use super::{
    cursor::{with_both, Act},
    *,
};

/// A key that sorts just after `key(n)`, and that no batch of the tests
/// writes a node under.
fn after(n: u64) -> Vec<u8> {
    assert_eq!(n % 3, 0, "a key of eight bytes");
    [key(n), vec![7]].concat()
}

/// Whether the answer to [`Act::Read`] says a record is held.
fn held(answer: &str) -> bool {
    match (
        answer.ends_with("held Ok(true)"),
        answer.ends_with("held Ok(false)"),
    ) {
        (true, _) => {
            assert!(answer.starts_with("get Ok(Some("), "{answer}");
            true
        }
        (_, true) => {
            assert!(answer.starts_with("get Ok(None)"), "{answer}");
            false
        }
        _ => panic!("no answer of a read: {answer}"),
    }
}

/// How many of the rows of a scan, in its answer, are of `key`.
fn rows_of(scan: &str, key: &[u8]) -> usize {
    scan.matches(&format!("({key:?}, ")).count()
}

/// Whether the answer to [`Act::Linked`] names `key`.
fn links(linked: &str, key: &[u8]) -> bool {
    linked.contains(&format!("{key:?}"))
}

#[test]
fn a_record_that_no_node_links_is_read_and_walked_as_on_rocksdb() {
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        let seed = seed as u64;
        let lone = |key: &[u8], n: u64| (key.to_vec(), value(n, true), feature(tree_type, n));
        // Two among the nodes that are committed, and one before the
        // first node of eight bytes and one after the last node, which
        // the transaction of the test writes.
        let (inside, beside) = (after(150), after(90));
        let (first, last) = (vec![0], vec![0xff, 0xff]);
        let committed = Act::Loose(vec![lone(&inside, 1), lone(&beside, 2)], Vec::new());
        with_both(tree_type, 0x0bad_5eed + seed, 5, &[committed], |both| {
            let looks = |keys: &[&Vec<u8>]| -> Vec<Act> {
                let mut looks: Vec<Act> =
                    (keys.iter()).map(|key| Act::Read((*key).clone())).collect();
                looks.extend([
                    Act::Wander(11 + seed),
                    Act::Scan(true),
                    Act::Scan(false),
                    Act::Range(key(40), key(170), true),
                    Act::Range(key(100), key(260), false),
                    Act::IsEmpty,
                    Act::IsEmptyExcept(vec![inside.clone()]),
                    Act::Linked,
                ]);
                looks
            };
            let all = [&inside, &beside, &first, &last];

            // The committed records are read by key and shown by a cursor
            // among the nodes, and the tree links neither.
            let answers = both.deferred(&looks(&all));
            let reads: Vec<bool> = answers[1..5].iter().map(|read| held(read)).collect();
            assert_eq!(reads, [true, true, false, false], "{tree_type:?}");
            let (scan, back, linked) = (&answers[6], &answers[7], &answers[12]);
            for key in [&inside, &beside] {
                assert_eq!((rows_of(scan, key), rows_of(back, key)), (1, 1));
                assert!(!links(linked, key), "{tree_type:?}: {linked}");
            }
            both.immediate(&looks(&all));

            // A handle that read one writes it again with a kind that
            // keeps its value, as chunk restore corrects a link and a
            // height. The record has no cell that keeps a value: the
            // store states the one it read, and the record is the same on
            // both engines afterwards.
            for (immediate, change) in [
                (true, NodeChange::StructureChanged),
                (true, NodeChange::HashOnly),
                (false, NodeChange::StructureChanged),
            ] {
                let kept = both.run(immediate, &[Act::Rewrite(inside.clone(), change)]);
                assert_eq!(kept[1], "Ok(())", "{tree_type:?}, {change:?}");
                let read = both.run(immediate, &[Act::Read(inside.clone())]);
                assert!(held(&read[1]), "{tree_type:?}, {change:?}");
            }
            // In one handle: the record is written with a value of its
            // own and then with that value kept, with no read between.
            let again = lone(&beside, 7);
            let answers = both.immediate(&[
                Act::WriteNode(again.0.clone(), again.1.clone(), again.2),
                Act::Rewrite(beside.clone(), NodeChange::HashOnly),
                Act::Read(beside.clone()),
            ]);
            assert!(answers[1..3].iter().all(|answer| answer == "Ok(())"));
            assert!(held(&answers[3]), "{tree_type:?}");

            // The transaction writes one of them again, removes the other
            // with no cost, which is charged from the committed record,
            // and leaves two more at the ends of the Merk.
            let written = Act::Loose(
                vec![lone(&inside, 3), lone(&first, 4), lone(&last, 5)],
                vec![(beside.clone(), None)],
            );
            assert_eq!(both.deferred(&[written])[1], "Ok(())");
            for immediate in [false, true] {
                let answers = both.run(immediate, &looks(&all));
                let reads: Vec<bool> = answers[1..5].iter().map(|read| held(read)).collect();
                assert_eq!(reads, [true, false, true, true], "{tree_type:?}");
                let (scan, linked) = (&answers[6], &answers[12]);
                assert_eq!(rows_of(scan, &beside), 0);
                for key in [&inside, &first, &last] {
                    assert_eq!(rows_of(scan, key), 1, "{tree_type:?}");
                    assert!(!links(linked, key), "{tree_type:?}");
                }
                assert_eq!(answers[10], "false");
            }

            // A node that Merk links under the key of one takes its place,
            // and goes as a node goes.
            let put = Op::Put(value(9, false), feature(tree_type, 9));
            both.deferred(&[Act::Apply(vec![(inside.clone(), put)])]);
            let answers = both.deferred(&[Act::Read(inside.clone()), Act::Linked, Act::Scan(true)]);
            assert!(held(&answers[1]) && links(&answers[2], &inside));
            assert_eq!(rows_of(&answers[3], &inside), 1);
            both.deferred(&[Act::Apply(vec![(inside.clone(), Op::Delete)])]);
            let answers = both.deferred(&[Act::Read(inside.clone()), Act::Scan(true)]);
            assert!(!held(&answers[1]));
            assert_eq!(rows_of(&answers[2], &inside), 0);

            // One is removed with the cost its writer gives.
            let freed = |bytes| StorageCost {
                removed_bytes: StorageRemovedBytes::BasicStorageRemoval(bytes),
                ..Default::default()
            };
            let cost = KeyValueStorageCost {
                key_storage_cost: freed(34),
                value_storage_cost: freed(90),
                ..Default::default()
            };
            both.deferred(&[Act::Loose(Vec::new(), vec![(last.clone(), Some(cost))])]);
            assert!(!held(&both.deferred(&[Act::Read(last.clone())])[1]));

            // A handle that writes at once: a node that is written outside
            // a change set, and that no node links, is read at once, and
            // removed by its key.
            let lone_write = lone(&after(210), 6);
            let made = Act::WriteNode(lone_write.0.clone(), lone_write.1, lone_write.2);
            let answers = both.immediate(&[
                made,
                Act::Read(after(210)),
                Act::Scan(false),
                Act::Loose(Vec::new(), vec![(after(210), None)]),
                Act::Read(after(210)),
            ]);
            assert!(held(&answers[2]) && !held(&answers[5]));
            assert_eq!(rows_of(&answers[3], &after(210)), 1);

            // A clear takes what the cursor shows, the last record with
            // the nodes.
            assert!(held(&both.deferred(&[Act::Read(first.clone())])[1]));
            both.deferred(&[Act::Clear]);
            let answers = both.deferred(&[Act::IsEmpty, Act::Read(first.clone()), Act::Scan(true)]);
            assert_eq!(answers[1], "true");
            assert!(!held(&answers[2]));
            assert_eq!(answers[3], "[]");
        });
    }
}

/// GroveDB #1063's test of `Merk::linked_keys` on this store
/// (`linked_keys_names_what_the_tree_links_not_what_storage_holds`): a
/// copy of a node is stored under a key of its own, with the links of the
/// node it was copied from. A read by key finds it, and the walk of the
/// tree does not.
#[test]
fn linked_keys_names_what_the_tree_links_not_what_the_store_holds() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let batch: Vec<(Vec<u8>, Op)> = (0..20u64)
        .map(|n| {
            (
                n.to_be_bytes().to_vec(),
                Op::Put(vec![n as u8; 60], TreeFeatureType::BasicMerkNode),
            )
        })
        .collect();
    let commit = |staged: LigninBatch| {
        (staged.apply(&writer).unwrap()).expect("the batch is applied");
    };

    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut merk = open(store, TreeType::NormalTree);
    apply(&mut merk, &batch);
    let (linked_key, _) = &batch[7];
    let stored = |merk: &Merk<LigninNodeStore<'_>>, key: &[u8]| {
        let node = merk.storage.get_node(key, NO_VALUE_DEFINED_COST, version());
        node.unwrap().expect("the record is read")
    };
    assert!(
        stored(&merk, linked_key).is_none(),
        "the batch is not in the transaction yet"
    );
    drop(merk);
    commit(staged);

    // A copy of a node under a key of its own, linked by nothing. Its
    // links name the children of the node it was copied from.
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let merk = Merk::open_base(
        store,
        TreeType::NormalTree,
        NO_VALUE_DEFINED_COST,
        version(),
    );
    let merk = merk.unwrap().expect("the Merk opens");
    let node = stored(&merk, linked_key).expect("the node is stored");
    let unlinked = TreeNode::decode_raw(
        &node.encode(),
        b"unlinked".to_vec(),
        NO_VALUE_DEFINED_COST,
        version(),
    )
    .expect("the record of a node");
    (merk.storage.write_node(&unlinked, NodeChange::ValueWritten))
        .unwrap()
        .expect("a record is stored");
    drop(merk);
    commit(staged);

    let look = |store: LigninNodeStore<'_>| {
        let merk = Merk::open_base(
            store,
            TreeType::NormalTree,
            NO_VALUE_DEFINED_COST,
            version(),
        );
        let merk = merk.unwrap().expect("the Merk opens");
        let read = merk.get(b"unlinked", true, NO_VALUE_DEFINED_COST.as_ref(), version());
        let read = read.unwrap().expect("a read by key");
        assert_eq!(read, Some(vec![7; 60]), "a read by key finds the record");
        let record = stored(&merk, b"unlinked").expect("the record is stored");
        assert_eq!(record.encode(), unlinked.encode());
        let linked = merk.linked_keys(NO_VALUE_DEFINED_COST.as_ref(), version());
        let linked = linked.unwrap().expect("the keys the tree links");
        assert_eq!(linked.len(), 20);
        assert!(!linked.contains(b"unlinked".as_slice()));
        assert!(batch.iter().all(|(key, _)| linked.contains(key)));
    };
    look(LigninNodeStore::deferred(&writer, ROOT, None));
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    let reader = path.begin();
    look(LigninNodeStore::read_only(&reader, ROOT));
}

/// A store with the seven nodes of [`seven_nodes`] committed, and beside
/// them a record under the key 100 that no node links, with `value`.
fn seven_nodes_and_a_record(value: &[u8]) -> (Lignin, TreeNode) {
    let (lignin, _) = seven_nodes();
    let feature = TreeFeatureType::BasicMerkNode;
    let record = TreeNode::new(be(100), value.to_vec(), None, feature).unwrap();
    {
        let path = lignin.path();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let store = LigninNodeStore::immediate(&writer, ROOT);
        (store.write_node(&record, NodeChange::ValueWritten).unwrap()).expect("the record is put");
        drop(store);
        writer.into_scope().merge();
        assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));
    }
    (lignin, record)
}

/// The node under 100 with `value`, hashed with another root beside it,
/// so that its value hash is not the one a node with that value alone
/// has.
fn rehashed(value: &[u8], beside: u8) -> TreeNode {
    let feature = TreeFeatureType::BasicMerkNode;
    TreeNode::new_with_combined_value_hash(be(100), value.to_vec(), [beside; 32], feature).unwrap()
}

/// No cell keeps a value for a record that no node links, so a write of
/// one that keeps its value states the value of the node it is handed:
/// what a store of whole records is handed with the same write. A handle
/// loads such a record, writes it with another value, and then writes
/// that node again with its value kept and other hashes, with no load in
/// between. The record is the node as it was last handed over, in a scope
/// that is laid out, which reports that the node has no cell, and in a
/// scope that keeps its writes by key, which reports nothing.
#[test]
fn a_kept_write_of_an_unlinked_record_states_the_node_it_is_handed() {
    for by_key in [false, true] {
        let (lignin, was) = seven_nodes_and_a_record(b"the first");
        let path = lignin.path();
        let transaction = path.begin();
        // A scope beside another keeps its writes by key.
        let beside = by_key.then(|| transaction.scope().unwrap());
        let scope = transaction.scope().unwrap();
        assert_eq!(scope.is_laid_out(), !by_key);
        let writer = LigninWriter::new(scope);
        let store = LigninNodeStore::immediate(&writer, ROOT);
        assert_eq!(node_of(&store, 100).encode(), was.encode());

        let second = rehashed(b"another value", 1);
        let again = rehashed(b"another value", 2);
        for (node, change) in [
            (&second, NodeChange::ValueWritten),
            (&again, NodeChange::HashOnly),
            (&second, NodeChange::StructureChanged),
            (&again, NodeChange::HashOnly),
        ] {
            (store.write_node(node, change).unwrap()).expect("the record is written");
            let read = LigninNodeStore::deferred(&writer, ROOT, None);
            assert_eq!(
                node_of(&read, 100).encode(),
                node.encode(),
                "by key {by_key}, {change:?}"
            );
        }
        // The nodes of the tree are as they were. A scope that keeps its
        // writes by key follows no link of a Merk it has written.
        let read = LigninNodeStore::deferred(&writer, ROOT, None);
        if !by_key {
            assert_eq!(records(&read, Some(be(4))).len(), 7);
        }
        drop((store, read));
        writer.into_scope().rollback();
        if let Some(beside) = beside {
            beside.merge();
        }
    }
}

/// The same through a change set that is staged in a batch. The handle
/// learns from the report of a batch that a node it wrote has no cell,
/// and the next change set that is handed the node with its value kept
/// states the value: for a record the handle loaded as one, and for a
/// node of the tree that a batch leaves with no link to it.
#[test]
fn a_kept_put_of_an_unlinked_record_in_a_change_set_states_its_value() {
    let (lignin, was) = seven_nodes_and_a_record(b"the first");
    let path = lignin.path();
    let feature = TreeFeatureType::BasicMerkNode;
    let rehashed = |node: &TreeNode| {
        let value = node.value_as_slice().to_vec();
        TreeNode::new_with_combined_value_hash(node.key().to_vec(), value, [7; 32], feature)
            .unwrap()
    };
    let put = |store: &LigninNodeStore<'_>, nodes: &[(&TreeNode, NodeChange)], root: bool| {
        let mut change_set = store.change_set();
        for (node, change) in nodes {
            let prepared = change_set.prepare_node(node, *change);
            change_set.put_node(node.key(), prepared, None).unwrap();
        }
        if root {
            change_set.state_root(Some(&be(4)));
        }
        (store.commit(change_set).unwrap()).expect("the change set is staged");
    };

    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let loaded = node_of(&store, 100);
    assert_eq!(loaded.encode(), was.encode());
    (store.write_node(&loaded, NodeChange::StructureChanged))
        .unwrap()
        .expect("the write is staged");
    let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));
    store.take_locations(&applied);

    // The node is handed over again with its value kept and another value
    // hash, with no load in between.
    let again = rehashed(&loaded);
    put(&store, &[(&again, NodeChange::HashOnly)], false);
    let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));
    let read = LigninNodeStore::deferred(&writer, ROOT, None);
    assert_eq!(node_of(&read, 100).encode(), again.encode());
    drop((store, read));
    writer.into_scope().merge();
    assert!(matches!(lignin.commit(&transaction), Outcome::Committed(_)));

    // A node of the tree that the handle loaded as one. A change set
    // writes the node above it with no link to it and the node itself
    // with its value kept: it takes the value of its cell, and the report
    // names no cell for it. The change set states hashes that are not
    // those of the tree it leaves, so its scope is rolled back.
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    assert_eq!(node_of(&store, 100).encode(), again.encode());
    let leaf = node_of(&store, 1);
    let mut above = node_of(&store, 2);
    above.set_link(true, None);
    let left = [
        (&above, NodeChange::StructureChanged),
        (&leaf, NodeChange::HashOnly),
    ];
    put(&store, &left, true);
    let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (0, 0));
    store.take_locations(&applied);
    let again = rehashed(&leaf);
    put(&store, &[(&again, NodeChange::HashOnly)], false);
    let applied = (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (1, 0));
    let read = LigninNodeStore::deferred(&writer, ROOT, None);
    assert_eq!(node_of(&read, 1).encode(), again.encode());
    assert_eq!(records(&read, Some(be(4))).len(), 6);
    drop((store, read));
    writer.into_scope().rollback();
}

/// A node that the transaction left out of its tree keeps its value for a
/// later write of it, also where the handle was told nothing of the node.
/// A store of whole records takes both writes, as it is handed the whole
/// node each time.
///
/// - The write that leaves the node is applied through a scope beside
///   another one, which keeps its writes by key and reports no place, and
///   is laid out when it merges. The handle then writes the node again
///   with its value kept, with no load in between.
/// - Both writes are settled before either is applied, and are applied
///   together: the second was prepared when the node was in its tree.
#[test]
fn a_kept_put_keeps_the_value_of_a_node_the_transaction_left_unlinked() {
    let feature = TreeFeatureType::BasicMerkNode;
    let rehashed = |node: &TreeNode| {
        let value = node.value_as_slice().to_vec();
        TreeNode::new_with_combined_value_hash(node.key().to_vec(), value, [7; 32], feature)
            .unwrap()
    };
    let put = |store: &LigninNodeStore<'_>, nodes: &[(&TreeNode, NodeChange)], root: bool| {
        let mut change_set = store.change_set();
        for (node, change) in nodes {
            let prepared = change_set.prepare_node(node, *change);
            change_set.put_node(node.key(), prepared, None).unwrap();
        }
        if root {
            change_set.state_root(Some(&be(4)));
        }
        (store.commit(change_set).unwrap()).expect("the change set is staged");
    };

    for together in [false, true] {
        let (lignin, _) = seven_nodes();
        let path = lignin.path();
        let transaction = path.begin();
        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(&transaction, ROOT, Some(&staged));
        // The handle loads a leaf and the node above it. One change set
        // writes the node above with no link to the leaf and the leaf with
        // its value kept, and the next the leaf again, with other hashes
        // and its value kept.
        let leaf = node_of(&store, 1);
        let mut above = node_of(&store, 2);
        above.set_link(true, None);
        let left = [
            (&above, NodeChange::StructureChanged),
            (&leaf, NodeChange::HashOnly),
        ];
        let again = rehashed(&leaf);
        match together {
            false => {
                put(&store, &left, true);
                let beside = transaction.scope().unwrap();
                let applied = (staged.apply(&transaction).unwrap()).expect("the batch is applied");
                let mut told = 0;
                applied.locations(&ROOT.prefix, |_, _| told += 1);
                assert_eq!(told, 0, "a scope beside another reports no place");
                store.take_locations(&applied);
                beside.merge();
                put(&store, &[(&again, NodeChange::HashOnly)], false);
                let applied = (staged.apply(&transaction).unwrap()).expect("the batch is applied");
                let mut places = Vec::new();
                applied.locations(&ROOT.prefix, |key, place| {
                    places.push((key.to_vec(), place))
                });
                assert_eq!(places, [(be(1), None)]);
                assert_eq!((applied.values_written, applied.values_kept), (0, 0));
            }
            true => {
                put(&store, &left, true);
                let first = (staged.settle(&transaction).unwrap()).expect("a settled batch");
                put(&store, &[(&again, NodeChange::HashOnly)], false);
                let second = (staged.settle(&transaction).unwrap()).expect("a settled batch");
                let applied = LigninSettled::apply(vec![first, second], &transaction, &[]);
                let applied = applied.unwrap().expect("both are applied");
                assert_eq!((applied.values_written, applied.values_kept), (0, 0));
                store.take_locations(&applied);
            }
        }
        let read = LigninNodeStore::deferred(&transaction, ROOT, None);
        assert_eq!(node_of(&read, 1).encode(), again.encode(), "{together}");
        assert_eq!(records(&read, Some(be(4))).len(), 6);
        // The handle that took the report of both in knows the node as a
        // record, and states its value from here on. The one that took in
        // no report with a place for it keeps the value once more.
        let last = rehashed(&again);
        put(&store, &[(&last, NodeChange::HashOnly)], false);
        let applied = (staged.apply(&transaction).unwrap()).expect("the batch is applied");
        let stated = usize::from(together);
        assert_eq!((applied.values_written, applied.values_kept), (stated, 0));
        assert_eq!(node_of(&read, 1).encode(), last.encode());
        drop((store, read));
        transaction.rollback().expect("a rollback");
    }
}
