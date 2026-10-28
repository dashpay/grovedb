use grovedb_version::version::GroveVersion;

use grovedb::{query_result_type::QueryResultType, Element, GroveDb, PathQuery, Query, SizedQuery};

fn history_key(revision: u64) -> Vec<u8> {
    [
        ((revision / 2) * 1000).to_be_bytes(),
        revision.to_be_bytes(),
    ]
    .concat()
}

#[test]
fn branch_count_offset_history_page_and_exact_keys_share_one_proof() {
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let mut leaf = Query::new_range_full();
    leaf.limit = Some(1);
    leaf.offset = Some(6);
    let rows = round_trip(&db, &composed(leaf));
    let history_rows: Vec<_> = rows.iter().filter(|(_, key, _)| key.len() == 16).collect();
    assert_eq!(history_rows.len(), 1);
    assert_eq!(
        history_rows[0].1,
        history_key(7),
        "revision positions skip only inside history"
    );
    assert_eq!(
        rows.len(),
        4,
        "both exact siblings and the history tree accompany the page"
    );
}

fn fixture(n: u64, target: Option<Element>, present: [bool; 2]) -> (tempfile::TempDir, GroveDb) {
    let v = GroveVersion::latest();
    let dir = tempfile::TempDir::new().unwrap();
    let db = GroveDb::open(dir.path()).unwrap();
    db.insert(
        &[] as &[&[u8]],
        b"type",
        Element::empty_tree(),
        None,
        None,
        v,
    )
    .unwrap()
    .unwrap();
    for child in [0u8, 1, 2] {
        let branch = if child == 2
            && target
                .as_ref()
                .is_some_and(|e| matches!(e, Element::NonCounted(_)))
        {
            Element::empty_count_tree()
        } else {
            Element::empty_tree()
        };
        db.insert(&[b"type".as_slice()], &[child], branch, None, None, v)
            .unwrap()
            .unwrap();
    }
    for child in [0u8, 1] {
        if present[child as usize] {
            db.insert(
                &[b"type".as_slice(), &[child]],
                b"doc",
                Element::new_item(vec![child]),
                None,
                None,
                v,
            )
            .unwrap()
            .unwrap();
        }
    }
    if let Some(target) = target {
        db.insert(&[b"type".as_slice(), &[2]], b"doc", target, None, None, v)
            .unwrap()
            .unwrap();
        for revision in 1..=n {
            db.insert(
                &[b"type".as_slice(), &[2], b"doc"],
                &history_key(revision),
                Element::new_item(vec![revision as u8]),
                None,
                None,
                v,
            )
            .unwrap()
            .unwrap();
        }
    }
    (dir, db)
}

fn composed(leaf: Query) -> PathQuery {
    let mut history = Query::new_single_key(b"doc".to_vec());
    history.add_parent_tree_on_subquery = true;
    history.set_subquery(leaf);
    let mut root = Query::new();
    for child in [0u8, 1] {
        root.insert_key(vec![child]);
        root.add_conditional_subquery(
            grovedb::QueryItem::Key(vec![child]),
            None,
            Some(Query::new_single_key(b"doc".to_vec())),
        );
    }
    root.insert_key(vec![2]);
    root.add_conditional_subquery(grovedb::QueryItem::Key(vec![2]), None, Some(history));
    PathQuery::new(vec![b"type".to_vec()], SizedQuery::new(root, None, None))
}

type RawRow = (Vec<Vec<u8>>, Vec<u8>, Vec<u8>);

fn round_trip(db: &GroveDb, query: &PathQuery) -> Vec<RawRow> {
    let v = GroveVersion::latest();
    let proof = db.prove_query(query, None, v).unwrap().unwrap();
    let (hash, rows) = GroveDb::verify_query_raw(&proof, query, v).unwrap();
    assert_eq!(hash, db.root_hash(None, v).unwrap().unwrap());
    let (trusted, _) = db
        .query_raw(
            query,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            v,
        )
        .unwrap()
        .unwrap();
    let trusted: Vec<_> = trusted
        .to_path_key_elements()
        .into_iter()
        .map(|(p, k, e)| (p, k, e.serialize(v).unwrap()))
        .collect();
    let verified: Vec<_> = rows.into_iter().map(|r| (r.path, r.key, r.value)).collect();
    assert_eq!(trusted, verified);
    verified
}

#[test]
fn local_reference_pages_match_ordinary_raw_typed_and_verified_reads() {
    assert_reference_page_parity(false);
}

#[test]
fn local_summed_reference_pages_match_ordinary_raw_typed_and_verified_reads() {
    assert_reference_page_parity(true);
}

fn assert_reference_page_parity(with_sum: bool) {
    assert_reference_target_page_parity(with_sum, None, true, GroveVersion::latest());
}

fn empty_reference_targets() -> [Element; 4] {
    [
        Element::empty_tree(),
        Element::empty_provable_count_indexed_tree(),
        Element::empty_provable_sum_indexed_tree(),
        Element::empty_provable_count_provable_sum_indexed_tree(vec![
            (0, None),
            (1, None),
            (2, None),
        ])
        .unwrap(),
    ]
}

#[test]
fn local_empty_tree_reference_pages_preserve_raw_and_verified_projections() {
    for target in empty_reference_targets() {
        for with_sum in [false, true] {
            assert_reference_target_page_parity(
                with_sum,
                Some(target.clone()),
                true,
                GroveVersion::latest(),
            );
        }
    }
}

#[test]
fn global_empty_tree_reference_pages_preserve_raw_and_verified_projections() {
    for v in [
        &grovedb_version::version::v4::GROVE_V4,
        GroveVersion::latest(),
    ] {
        for target in empty_reference_targets() {
            for with_sum in [false, true] {
                assert_reference_target_page_parity(with_sum, Some(target.clone()), false, v);
            }
        }
    }
}

fn assert_reference_target_page_parity(
    with_sum: bool,
    tree_target: Option<Element>,
    local_page: bool,
    v: &GroveVersion,
) {
    use grovedb::reference_path::ReferencePathType::{AbsolutePathReference, SiblingReference};

    let path = vec![b"type".to_vec(), vec![2], b"doc".to_vec()];
    let storage_path: Vec<_> = path.iter().map(Vec::as_slice).collect();
    for host in [
        Element::empty_provable_count_tree(),
        Element::empty_provable_count_sum_tree(),
        Element::empty_provable_count_provable_sum_tree(),
    ] {
        let (_dir, db) = fixture(0, Some(host), [false, false]);
        for index in 0..6u8 {
            let target_key = vec![b'z', index];
            db.insert(
                storage_path.as_slice(),
                &target_key,
                tree_target
                    .clone()
                    .unwrap_or_else(|| Element::new_item(vec![index, 42])),
                None,
                None,
                v,
            )
            .unwrap()
            .unwrap();
            let reference = SiblingReference(target_key);
            let element = if with_sum {
                Element::new_reference_with_sum_item(reference, i64::from(index) + 7)
            } else {
                Element::new_reference(reference)
            };
            // Current writes refuse tree terminals. Legacy direct writes can
            // persist them, and later readers must retain their commitments.
            let insert_version = if tree_target.is_some() {
                let root = db.root_hash(None, v).unwrap().unwrap();
                let refused = db
                    .insert(
                        storage_path.as_slice(),
                        &[b'r', index],
                        element.clone(),
                        None,
                        None,
                        v,
                    )
                    .unwrap();
                assert!(matches!(
                    refused,
                    Err(grovedb::Error::InvalidInput(
                        "references can not point to trees"
                    ))
                ));
                assert_eq!(
                    db.get_raw_optional(storage_path.as_slice().into(), &[b'r', index], None, v)
                        .unwrap()
                        .unwrap(),
                    None
                );
                assert_eq!(db.root_hash(None, v).unwrap().unwrap(), root);
                &grovedb_version::version::v3::GROVE_V3
            } else {
                v
            };
            db.insert(
                storage_path.as_slice(),
                &[b'r', index],
                element,
                None,
                None,
                insert_version,
            )
            .unwrap()
            .unwrap();
        }
        let root = db.root_hash(None, v).unwrap().unwrap();
        for left_to_right in [true, false] {
            for offset in [0, 2u16] {
                if !local_page && offset == 0 {
                    continue;
                }
                let mut indices: Vec<_> = (0..6u8).collect();
                if !left_to_right {
                    indices.reverse();
                }
                let selected: Vec<_> = indices
                    .into_iter()
                    .skip(usize::from(offset))
                    .take(2)
                    .collect();
                let expected_raw: Vec<_> = selected
                    .iter()
                    .map(|&index| {
                        let mut target = path.clone();
                        target.push(vec![b'z', index]);
                        let reference = AbsolutePathReference(target);
                        let element = if with_sum {
                            Element::new_reference_with_sum_item(reference, i64::from(index) + 7)
                        } else {
                            Element::new_reference(reference)
                        };
                        (path.clone(), vec![b'r', index], element)
                    })
                    .collect();
                let expected_typed: Vec<_> = selected
                    .iter()
                    .map(|&index| {
                        (
                            path.clone(),
                            vec![b'r', index],
                            tree_target
                                .clone()
                                .unwrap_or_else(|| Element::new_item(vec![index, 42])),
                        )
                    })
                    .collect();
                let mut local = Query::new_with_direction(left_to_right);
                local.insert_range(b"r".to_vec()..b"s".to_vec());
                let page = if local_page {
                    local.offset = Some(offset);
                    local.limit = Some(2);
                    PathQuery::new_unsized(path.clone(), local)
                } else {
                    PathQuery::new(path.clone(), SizedQuery::new(local, Some(2), Some(offset)))
                };
                let mut ordinary = Query::new_with_direction(left_to_right);
                ordinary.insert_range_inclusive(
                    vec![b'r', *selected.iter().min().unwrap()]
                        ..=vec![b'r', *selected.iter().max().unwrap()],
                );
                for (query, is_page) in [
                    (page, true),
                    (PathQuery::new_unsized(path.clone(), ordinary), false),
                ] {
                    let (raw, skipped) = db
                        .query_raw(
                            &query,
                            true,
                            true,
                            true,
                            QueryResultType::QueryPathKeyElementTrioResultType,
                            None,
                            v,
                        )
                        .unwrap()
                        .unwrap();
                    assert_eq!(skipped, if !local_page && is_page { offset } else { 0 });
                    assert_eq!(raw.to_path_key_elements(), expected_raw);
                    let typed = db
                        .query(
                            &query,
                            true,
                            true,
                            true,
                            QueryResultType::QueryPathKeyElementTrioResultType,
                            None,
                            v,
                        )
                        .unwrap();
                    if tree_target.is_some() {
                        assert!(matches!(
                            typed,
                            Err(grovedb::Error::InvalidQuery(
                                "the reference must result in an item"
                            ))
                        ));
                        if !is_page {
                            continue;
                        }
                    } else {
                        assert_eq!(typed.unwrap().0.to_path_key_elements(), expected_typed);
                    }
                    let proof = db.prove_query(&query, None, v).unwrap().unwrap();
                    let (verified_root, raw) =
                        GroveDb::verify_query_raw(&proof, &query, v).unwrap();
                    assert_eq!(verified_root, root);
                    assert_eq!(
                        raw.into_iter()
                            .map(|row| (row.path, row.key, row.value))
                            .collect::<Vec<_>>(),
                        expected_typed
                            .iter()
                            .map(|(p, k, e)| (p.clone(), k.clone(), e.serialize(v).unwrap()))
                            .collect::<Vec<_>>()
                    );
                    let (verified_root, typed) = GroveDb::verify_query(&proof, &query, v).unwrap();
                    assert_eq!(verified_root, root);
                    assert_eq!(
                        typed,
                        expected_typed
                            .iter()
                            .map(|(p, k, e)| (p.clone(), k.clone(), Some(e.clone())))
                            .collect::<Vec<_>>()
                    );
                    if tree_target.is_some() {
                        let altered = mutate_layer(&proof, &storage_path, |layer| {
                            use grovedb::operations::proof::ProofBytes;
                            use grovedb_merk::proofs::{encode_into, Decoder, Node, Op};

                            let ProofBytes::Merk(bytes) = &mut layer.merk_proof else {
                                panic!("Merk proof required")
                            };
                            let mut ops: Vec<_> = Decoder::new(bytes).map(Result::unwrap).collect();
                            let value = ops
                                .iter_mut()
                                .find_map(|op| match op {
                                    Op::Push(
                                        Node::KVRefValueHashCount(_, value, ..)
                                        | Node::KVRefValueHashCountSum(_, value, ..),
                                    )
                                    | Op::PushInverted(
                                        Node::KVRefValueHashCount(_, value, ..)
                                        | Node::KVRefValueHashCountSum(_, value, ..),
                                    ) => Some(value),
                                    _ => None,
                                })
                                .expect("page must contain a resolved reference");
                            let replacement = Element::empty_tree_with_flags(Some(vec![99]))
                                .serialize(v)
                                .unwrap();
                            assert_ne!(*value, replacement);
                            *value = replacement;
                            bytes.clear();
                            encode_into(ops.iter(), bytes);
                        });
                        reject_authenticated(&db, &altered, &query);
                        if let Ok((hash, _)) = GroveDb::verify_query(&altered, &query, v) {
                            assert_ne!(
                                hash, root,
                                "changed reference target must not authenticate"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn history_time_cursor_revision_and_start_pages_authenticate_sibling_absence() {
    for target in [
        Element::empty_provable_count_tree(),
        Element::empty_provable_count_sum_tree(),
        Element::empty_provable_count_provable_sum_tree(),
    ] {
        for present in [[true, true], [false, true], [true, false], [false, false]] {
            let (_dir, db) = fixture(12, Some(target.clone()), present);
            for (kind, expected) in [
                (0, vec![6, 7, 8]),
                (1, vec![7, 8, 9]),
                (2, vec![7]),
                (3, vec![7, 8, 9]),
            ] {
                let mut leaf = Query::new();
                match kind {
                    0 => leaf.insert_range_from(3000u64.to_be_bytes().to_vec()..),
                    1 => leaf.insert_range_after(history_key(6)..),
                    _ => leaf.insert_all(),
                }
                leaf.limit = Some(if kind == 2 { 1 } else { 3 });
                if kind >= 2 {
                    leaf.offset = Some(6);
                }
                let rows = round_trip(&db, &composed(leaf));
                let keys: Vec<_> = rows
                    .iter()
                    .filter(|(_, k, _)| k.len() == 16)
                    .map(|(_, k, _)| k.clone())
                    .collect();
                assert_eq!(
                    keys,
                    expected.into_iter().map(history_key).collect::<Vec<_>>()
                );
                for child in [0u8, 1] {
                    assert_eq!(
                        rows.iter()
                            .any(|(p, k, _)| p == &vec![b"type".to_vec(), vec![child]]
                                && k == b"doc"),
                        present[child as usize]
                    );
                }
                assert_eq!(
                    rows.len(),
                    keys.len() + 1 + present.into_iter().filter(|p| *p).count()
                );
            }
        }
    }
}

#[test]
fn local_offset_boundaries_empty_missing_and_reverse_are_consistent() {
    for count in [0, 12] {
        let (_dir, db) = fixture(
            count,
            Some(Element::empty_provable_count_tree()),
            [false, false],
        );
        for offset in [0, 6, 12, u16::MAX] {
            for direction in [false, true] {
                let mut leaf = Query::new_range_full();
                leaf.offset = Some(offset);
                leaf.limit = Some(3);
                leaf.left_to_right = direction;
                let rows = round_trip(&db, &composed(leaf));
                assert_eq!(
                    rows.iter()
                        .filter(|(path, key, _)| path == &vec![b"type".to_vec(), vec![2]]
                            && key == b"doc")
                        .count(),
                    1,
                    "the history count element is returned even for an empty page"
                );
                let mut revisions: Vec<_> = (1..=count).collect();
                if !direction {
                    revisions.reverse();
                }
                let expected: Vec<_> = revisions
                    .into_iter()
                    .skip(offset as usize)
                    .take(3)
                    .map(history_key)
                    .collect();
                assert_eq!(
                    rows.iter()
                        .filter(|(_, k, _)| k.len() == 16)
                        .map(|(_, k, _)| k.clone())
                        .collect::<Vec<_>>(),
                    expected
                );
            }
        }
    }
    let (_dir, db) = fixture(0, None, [false, false]);
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(1);
    leaf.limit = Some(1);
    assert!(round_trip(&db, &composed(leaf)).is_empty());
}

#[test]
fn local_offsets_refuse_ordinary_and_non_tree_hosts_even_when_empty() {
    let v = GroveVersion::latest();
    for target in [Element::empty_tree(), Element::new_item(vec![1])] {
        let (_dir, db) = fixture(0, Some(target), [true, true]);
        for offset in [0, 1] {
            let mut leaf = Query::new_range_full();
            leaf.offset = Some(offset);
            leaf.limit = Some(1);
            let query = composed(leaf);
            assert!(
                db.prove_query(&query, None, v).unwrap().is_err(),
                "invalid host must fail proving"
            );
            assert!(
                db.query_raw(
                    &query,
                    true,
                    true,
                    true,
                    QueryResultType::QueryPathKeyElementTrioResultType,
                    None,
                    v
                )
                .unwrap()
                .is_err(),
                "invalid host must fail trusted reading"
            );
        }
    }
}

#[test]
fn synthesized_path_targets_the_count_host_and_preserves_parent_budgets() {
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(6);
    leaf.limit = Some(1);
    let mut root = Query::new_single_key(vec![2]);
    root.set_subquery_path(vec![b"doc".to_vec()]);
    root.set_subquery(leaf);
    let q = PathQuery::new(vec![b"type".to_vec()], SizedQuery::new(root, None, None));
    let rows = round_trip(&db, &q);
    assert_eq!(rows[0].1, history_key(7));
}

#[test]
fn exhausted_local_page_charges_global_traversal_budget_but_not_instance_rows() {
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(u16::MAX);
    leaf.limit = Some(1);
    let mut q = composed(leaf);
    q.query.query.left_to_right = false;
    q.query.limit = Some(2);
    q.query.query.limit = Some(1);
    let rows = round_trip(&db, &q);
    assert!(rows
        .iter()
        .any(|(p, k, _)| p == &vec![b"type".to_vec(), vec![1]] && k == b"doc"));
}

#[test]
fn merge_lifts_only_exclusive_count_offsets_and_old_versions_refuse_them() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let page = PathQuery::new(
        vec![b"type".to_vec(), vec![2], b"doc".to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(2), Some(6)),
    );
    let pointer = PathQuery::new(
        vec![b"type".to_vec(), vec![0]],
        SizedQuery::new(Query::new_single_key(b"doc".to_vec()), None, None),
    );
    let merged = PathQuery::merge(vec![&page, &pointer], v).unwrap();
    let rows = round_trip(&db, &merged);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].1, history_key(7));
    for old in &grovedb_version::version::GROVE_VERSIONS[..4] {
        assert!(PathQuery::merge(vec![&page, &pointer], old).is_err());
        assert!(db.prove_query(&merged, None, old).unwrap().is_err());
        assert!(db
            .query_raw(
                &merged,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                old
            )
            .unwrap()
            .is_err());
    }
    let root = PathQuery::new(
        vec![b"type".to_vec()],
        SizedQuery::new(Query::new_range_full(), None, Some(1)),
    );
    assert!(PathQuery::merge(vec![&root, &pointer], v).is_err());
    let overlap = PathQuery::new(
        page.path.clone(),
        SizedQuery::new(Query::new_range_full(), None, None),
    );
    assert!(PathQuery::merge(vec![&page, &overlap], v).is_err());
}

fn decode_proof(bytes: &[u8]) -> grovedb::operations::proof::GroveDBProof {
    bincode::decode_from_slice(
        bytes,
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit(),
    )
    .unwrap()
    .0
}

fn encode_proof(proof: grovedb::operations::proof::GroveDBProof) -> Vec<u8> {
    bincode::encode_to_vec(
        proof,
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit(),
    )
    .unwrap()
}

fn mutate_layer(
    proof: &[u8],
    path: &[&[u8]],
    mutate: impl FnOnce(&mut grovedb::operations::proof::LayerProof),
) -> Vec<u8> {
    let grovedb::operations::proof::GroveDBProof::V1(mut proof) = decode_proof(proof) else {
        panic!("V1 proof required")
    };
    let mut layer = &mut proof.root_layer;
    for key in path {
        layer = layer.lower_layers.get_mut(*key).unwrap();
    }
    mutate(layer);
    encode_proof(grovedb::operations::proof::GroveDBProof::V1(proof))
}

fn reject_authenticated(db: &GroveDb, proof: &[u8], query: &PathQuery) {
    if let Ok((hash, _)) = GroveDb::verify_query_raw(proof, query, GroveVersion::latest()) {
        assert_ne!(
            hash,
            db.root_hash(None, GroveVersion::latest()).unwrap().unwrap(),
            "altered answer must not authenticate against the trusted root"
        );
    }
}

#[test]
fn count_pages_reject_direct_empty_tree_substitutions() {
    use grovedb::operations::proof::ProofBytes;
    use grovedb_merk::{
        proofs::{encode_into, Decoder, Node, Op},
        tree::{value_hash, TreeFeatureType},
    };

    let v = GroveVersion::latest();
    for host in [
        Element::empty_provable_count_tree(),
        Element::empty_provable_count_sum_tree(),
        Element::empty_provable_count_provable_sum_tree(),
    ] {
        let (_dir, db) = fixture(4, Some(host), [false, false]);
        for local in [true, false] {
            for direction in [true, false] {
                let mut range = Query::new_with_direction(direction);
                range.insert_all();
                let page = if local {
                    range.offset = Some(1);
                    range.limit = Some(1);
                    SizedQuery::new(range, None, None)
                } else {
                    SizedQuery::new(range, Some(1), Some(1))
                };
                let path = vec![b"type".to_vec(), vec![2], b"doc".to_vec()];
                let query = PathQuery::new(path, page);
                let proof = db.prove_query(&query, None, v).unwrap().unwrap();
                let (hash, rows) = GroveDb::verify_query_raw(&proof, &query, v).unwrap();
                assert_eq!(hash, db.root_hash(None, v).unwrap().unwrap());
                assert_eq!(rows.len(), 1);
                for target in empty_reference_targets() {
                    let altered = mutate_layer(&proof, &[b"type", &[2], b"doc"], |layer| {
                        let ProofBytes::Merk(bytes) = &mut layer.merk_proof else {
                            panic!("Merk proof required")
                        };
                        let mut ops: Vec<_> = Decoder::new(bytes).map(Result::unwrap).collect();
                        let node = ops
                            .iter_mut()
                            .find_map(|op| match op {
                                Op::Push(node @ (Node::KVCount(..) | Node::KVCountSum(..)))
                                | Op::PushInverted(
                                    node @ (Node::KVCount(..) | Node::KVCountSum(..)),
                                ) => Some(node),
                                _ => None,
                            })
                            .expect("page must contain a direct item");
                        let (key, value, feature) = match node {
                            Node::KVCount(key, value, count) => (
                                key.clone(),
                                value.clone(),
                                TreeFeatureType::ProvableCountedMerkNode(*count),
                            ),
                            Node::KVCountSum(key, value, count, sum) => (
                                key.clone(),
                                value.clone(),
                                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                                    *count, *sum,
                                ),
                            ),
                            _ => unreachable!(),
                        };
                        *node = Node::KVValueHashFeatureType(
                            key,
                            target.serialize(v).unwrap(),
                            value_hash(&value).unwrap(),
                            feature,
                        );
                        bytes.clear();
                        encode_into(ops.iter(), bytes);
                    });
                    for error in [
                        GroveDb::verify_query_raw(&altered, &query, v).unwrap_err(),
                        GroveDb::verify_query(&altered, &query, v).unwrap_err(),
                    ] {
                        assert!(
                            matches!(error, grovedb::Error::InvalidProof(_, ref message)
                            if message.contains("empty-tree return")),
                            "unexpected refusal: {error:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn count_pages_reject_changed_counts_missing_extra_and_reordered_rows() {
    use grovedb::operations::proof::ProofBytes;
    use grovedb_merk::proofs::{encode_into, Decoder, Node, Op};
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(40, Some(Element::empty_provable_count_tree()), [true, true]);
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(17);
    leaf.limit = Some(3);
    let query = composed(leaf);
    let proof = db.prove_query(&query, None, v).unwrap().unwrap();
    for mutation in 0..5 {
        let altered = mutate_layer(&proof, &[b"type", &[2], b"doc"], |layer| {
            let ProofBytes::Merk(bytes) = &mut layer.merk_proof else {
                panic!("Merk")
            };
            let mut ops: Vec<_> = Decoder::new(bytes).map(Result::unwrap).collect();
            let mut changed = false;
            for index in 0..ops.len() {
                match &mut ops[index] {
                    Op::Push(Node::HashWithCount(_, _, _, count)) if mutation == 0 => {
                        *count += 1;
                        changed = true;
                        break;
                    }
                    Op::Push(Node::KVCount(_, value, _)) if mutation == 1 => {
                        *value = Element::new_item(vec![99]).serialize(v).unwrap();
                        changed = true;
                        break;
                    }
                    Op::Push(Node::KVCount(..)) if mutation == 2 => {
                        ops.remove(index);
                        changed = true;
                        break;
                    }
                    Op::Push(Node::KVCount(..)) if mutation == 3 => {
                        ops.insert(index, ops[index].clone());
                        changed = true;
                        break;
                    }
                    _ => (),
                }
            }
            if mutation == 4 {
                ops.reverse();
                changed = true;
            }
            assert!(changed, "fixture must exercise mutation {mutation}");
            bytes.clear();
            encode_into(ops.iter(), bytes);
        });
        reject_authenticated(&db, &altered, &query);
    }
    let mut shifted = Query::new_range_full();
    shifted.offset = Some(16);
    shifted.limit = Some(3);
    reject_authenticated(&db, &proof, &composed(shifted));
    let mut extra = Query::new_range_full();
    extra.offset = Some(17);
    extra.limit = Some(4);
    reject_authenticated(&db, &proof, &composed(extra));
    let mut reversed = Query::new_range_full();
    reversed.offset = Some(17);
    reversed.limit = Some(3);
    reversed.left_to_right = false;
    reject_authenticated(&db, &proof, &composed(reversed));
    let mut shifted_range = Query::new();
    shifted_range.insert_range_from(history_key(20)..);
    shifted_range.offset = Some(17);
    shifted_range.limit = Some(3);
    reject_authenticated(&db, &proof, &composed(shifted_range));
    let nested = mutate_layer(&proof, &[b"type", &[2], b"doc"], |layer| {
        layer
            .lower_layers
            .insert(b"unexpected".to_vec(), layer.clone());
    });
    reject_authenticated(&db, &nested, &query);
    assert!(GroveDb::verify_query_with_absence_proof(&proof, &query, v).is_err());
}

#[test]
fn exact_siblings_cannot_be_removed_fabricated_or_replaced_by_pagination_layers() {
    use grovedb::operations::proof::ProofBytes;
    use grovedb_merk::{
        proofs::{encode_into, Decoder, Node, Op},
        tree::value_hash,
    };
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(
        12,
        Some(Element::empty_provable_count_tree()),
        [true, false],
    );
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(6);
    leaf.limit = Some(1);
    db.insert(
        &[b"type".as_slice(), &[1]],
        b"other",
        Element::new_item(vec![9]),
        None,
        None,
        v,
    )
    .unwrap()
    .unwrap();
    let query = composed(leaf);
    let proof = db.prove_query(&query, None, v).unwrap().unwrap();
    let hidden = mutate_layer(&proof, &[b"type", &[0]], |layer| {
        let ProofBytes::Merk(bytes) = &mut layer.merk_proof else {
            panic!("Merk")
        };
        let ops: Vec<_> = Decoder::new(bytes)
            .map(|op| match op.unwrap() {
                Op::Push(Node::KV(key, value)) => {
                    Op::Push(Node::KVDigest(key, value_hash(&value).unwrap()))
                }
                other => other,
            })
            .collect();
        bytes.clear();
        encode_into(ops.iter(), bytes);
    });
    reject_authenticated(&db, &hidden, &query);
    let fabricated = mutate_layer(&proof, &[b"type", &[1]], |layer| {
        let mut bytes = Vec::new();
        encode_into(
            [Op::Push(Node::KV(
                b"doc".to_vec(),
                Element::new_item(vec![1]).serialize(v).unwrap(),
            ))]
            .iter(),
            &mut bytes,
        );
        layer.merk_proof = ProofBytes::Merk(bytes);
    });
    reject_authenticated(&db, &fabricated, &query);
    let missing = mutate_layer(&proof, &[b"type"], |layer| {
        layer.lower_layers.remove([0].as_slice());
    });
    reject_authenticated(&db, &missing, &query);
    let mixed = mutate_layer(&proof, &[b"type"], |layer| {
        let history = layer.lower_layers.get([2].as_slice()).unwrap().clone();
        layer.lower_layers.insert(vec![0], history);
    });
    reject_authenticated(&db, &mixed, &query);
}

#[test]
fn independent_document_instances_reset_their_offsets_and_caps() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    db.insert(
        &[b"type".as_slice(), &[2]],
        b"other",
        Element::empty_provable_count_tree(),
        None,
        None,
        v,
    )
    .unwrap()
    .unwrap();
    for r in 1..=12 {
        db.insert(
            &[b"type".as_slice(), &[2], b"other"],
            &history_key(r),
            Element::new_item(vec![r as u8]),
            None,
            None,
            v,
        )
        .unwrap()
        .unwrap();
    }
    let mut docs = Query::new();
    for (key, offset, limit) in [(b"doc".as_slice(), 6, 1), (b"other".as_slice(), 2, 2)] {
        let mut leaf = Query::new_range_full();
        leaf.offset = Some(offset);
        leaf.limit = Some(limit);
        docs.insert_key(key.to_vec());
        docs.add_conditional_subquery(grovedb::QueryItem::Key(key.to_vec()), None, Some(leaf));
    }
    let query = PathQuery::new(
        vec![b"type".to_vec(), vec![2]],
        SizedQuery::new(docs, None, None),
    );
    let proof = db.prove_query(&query, None, v).unwrap().unwrap();
    let rows = round_trip(&db, &query);
    assert_eq!(
        rows.iter().map(|(_, k, _)| k.clone()).collect::<Vec<_>>(),
        [7, 3, 4].map(history_key)
    );
    let mut shifted = query.clone();
    let branches = shifted
        .query
        .query
        .conditional_subquery_branches
        .as_mut()
        .unwrap();
    branches
        .get_mut(&grovedb::QueryItem::Key(b"doc".to_vec()))
        .unwrap()
        .subquery
        .as_mut()
        .unwrap()
        .offset = Some(2);
    branches
        .get_mut(&grovedb::QueryItem::Key(b"other".to_vec()))
        .unwrap()
        .subquery
        .as_mut()
        .unwrap()
        .offset = Some(6);
    reject_authenticated(&db, &proof, &shifted);
    let mut capped = query.clone();
    capped.query.query.limit = Some(2);
    assert_eq!(round_trip(&db, &capped).len(), 2);
}

#[test]
fn empty_local_pages_consume_one_global_slot_in_all_three_paths() {
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(u16::MAX);
    leaf.limit = Some(1);
    for cap in [1, 2] {
        let mut q = composed(leaf.clone());
        q.query.query.left_to_right = false;
        q.query.limit = Some(cap);
        let rows = round_trip(&db, &q);
        assert_eq!(
            rows.iter()
                .filter(|(p, k, _)| p == &vec![b"type".to_vec(), vec![1]] && k == b"doc")
                .count(),
            usize::from(cap == 2)
        );
    }
}

#[test]
fn local_root_offsets_and_aggregate_projections_fail_before_reading_proof_bytes() {
    let v = GroveVersion::latest();
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(0);
    let root = PathQuery::new(vec![], SizedQuery::new(leaf, None, None));
    assert!(GroveDb::verify_query_raw(&[], &root, v)
        .unwrap_err()
        .to_string()
        .contains("root"));
    let mut aggregate = Query::new_aggregate_count_on_range(grovedb::QueryItem::RangeFull(..));
    aggregate.offset = Some(0);
    let aggregate = SizedQuery::new(aggregate, None, None);
    assert!(aggregate.validate_leaf_aggregate_count_on_range().is_err());
}

#[test]
fn v5_parent_echoes_cover_path_only_descents_without_consuming_row_limits() {
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let mut query = Query::new_single_key(vec![0]);
    query.set_subquery_path(vec![b"doc".to_vec()]);
    query.add_parent_tree_on_subquery = true;
    query.limit = Some(1);
    let q = PathQuery::new(
        vec![b"type".to_vec()],
        SizedQuery::new(query, Some(1), None),
    );
    let rows = round_trip(&db, &q);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, vec![b"type".to_vec()]);
    assert_eq!(rows[0].1, vec![0]);
}

#[test]
fn wrapped_count_hosts_keep_their_parent_bytes_and_serve_local_pages() {
    for count in [0, 12] {
        let (_dir, db) = fixture(
            count,
            Some(Element::NonCounted(Box::new(
                Element::empty_provable_count_tree(),
            ))),
            [true, true],
        );
        let mut leaf = Query::new_range_full();
        leaf.offset = Some(6);
        leaf.limit = Some(1);
        let rows = round_trip(&db, &composed(leaf));
        assert_eq!(
            rows.iter().filter(|(_, k, _)| k.len() == 16).count(),
            usize::from(count > 0)
        );
    }
}

#[test]
fn parent_echo_and_lifted_offsets_validate_their_serving_selectors() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(12, Some(Element::empty_provable_count_tree()), [true, true]);
    let echo = composed(Query::new_range_full());
    let proof = db.prove_query(&echo, None, v).unwrap().unwrap();
    for (capability, engine) in [(2, 2), (0, 2), (1, 1), (1, 3)] {
        let mut doctored = v.clone();
        doctored
            .grovedb_versions
            .path_query_methods
            .per_instance_query_offsets = capability;
        doctored.grovedb_versions.element.path_query_push = engine;
        assert!(
            GroveDb::verify_query_raw(&proof, &echo, &doctored).is_err(),
            "parent echo selector must fail closed: {capability}/{engine}"
        );
        assert!(db
            .query_raw(
                &echo,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                &doctored
            )
            .unwrap()
            .is_err());
        let page = PathQuery::new(
            vec![b"type".to_vec(), vec![2], b"doc".to_vec()],
            SizedQuery::new(Query::new_range_full(), Some(1), Some(6)),
        );
        let pointer = PathQuery::new(
            vec![b"type".to_vec(), vec![0]],
            SizedQuery::new(Query::new_single_key(b"doc".to_vec()), None, None),
        );
        assert!(
            PathQuery::merge(vec![&page, &pointer], &doctored).is_err(),
            "lifting must validate the newly authored offset"
        );
    }
}

#[test]
fn platform_consumer_merges_history_carrier_with_unbudgeted_exact_reads() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(
        12,
        Some(Element::empty_provable_count_tree()),
        [true, false],
    );
    let mut page = Query::new_range_full();
    page.limit = Some(2);
    page.offset = Some(6);
    let mut history = Query::new_single_key(b"doc".to_vec());
    history.add_parent_tree_on_subquery = true;
    history.set_subquery(page);
    let history = PathQuery::new_unsized(vec![b"type".to_vec(), vec![2]], history);
    let pointer = PathQuery::new_unsized(
        vec![b"type".to_vec(), vec![0]],
        Query::new_single_key(b"doc".to_vec()),
    );
    let lifecycle = PathQuery::new_unsized(
        vec![b"type".to_vec(), vec![1]],
        Query::new_single_key(b"doc".to_vec()),
    );
    let merged = PathQuery::merge(vec![&history, &pointer, &lifecycle], v).unwrap();
    let rows = round_trip(&db, &merged);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        db.query_item_value(&merged, true, true, true, None, v)
            .unwrap()
            .unwrap(),
        (vec![vec![0], vec![7], vec![8]], 0),
        "typed Platform reads return the exact sibling and page values",
    );
    assert_eq!(
        rows.iter()
            .filter(|(_, key, _)| key.len() == 16)
            .map(|(_, key, _)| key.clone())
            .collect::<Vec<_>>(),
        [7, 8].map(history_key)
    );
}

fn page_behind_intermediate_path() -> PathQuery {
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(1);
    leaf.limit = Some(1);
    let mut root = Query::new_single_key(vec![2]);
    root.set_subquery_path(vec![b"doc".to_vec(), b"nested".to_vec()]);
    root.set_subquery(leaf);
    PathQuery::new_unsized(vec![b"type".to_vec()], root)
}

#[test]
fn non_tree_intermediates_cannot_replace_a_requested_descendant_page() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(0, Some(Element::new_item(vec![9])), [true, true]);
    let q = page_behind_intermediate_path();
    let exact = PathQuery::new_unsized(
        vec![b"type".to_vec(), vec![2]],
        Query::new_single_key(b"doc".to_vec()),
    );
    let proof = db.prove_query(&exact, None, v).unwrap().unwrap();
    assert!(
        GroveDb::verify_query_raw(&proof, &q, v).is_err(),
        "an authentic Item cannot stand in for its requested descendant page"
    );
    assert!(db.prove_query(&q, None, v).unwrap().is_err());
    assert!(db
        .query_raw(
            &q,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            v
        )
        .unwrap()
        .is_err());
    let direct = PathQuery::new(
        vec![
            b"type".to_vec(),
            vec![2],
            b"doc".to_vec(),
            b"nested".to_vec(),
        ],
        q.query.clone(),
    );
    assert!(
        db.prove_query(&direct, None, v).unwrap().is_err(),
        "path prefixes above the query's own node also require trees"
    );
}

#[test]
fn missing_intermediates_prove_absence_and_unrelated_items_remain_readable() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(0, None, [true, true]);
    let q = page_behind_intermediate_path();
    let proof = db.prove_query(&q, None, v).unwrap().unwrap();
    let (hash, rows) = GroveDb::verify_query_raw(&proof, &q, v).unwrap();
    assert_eq!(hash, db.root_hash(None, v).unwrap().unwrap());
    assert!(rows.is_empty());
    let (rows, _) = db
        .query_raw(
            &q,
            true,
            true,
            false,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            v,
        )
        .unwrap()
        .unwrap();
    assert!(rows.is_empty());
    let sibling = PathQuery::new_unsized(
        vec![b"type".to_vec(), vec![0]],
        Query::new_single_key(b"doc".to_vec()),
    );
    assert_eq!(round_trip(&db, &sibling).len(), 1);
}

#[test]
fn non_merk_position_rows_cannot_replace_a_requested_descendant_page() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(0, Some(Element::empty_mmr_tree()), [true, true]);
    db.mmr_tree_append(&[b"type".as_slice(), &[2]], b"doc", vec![42], None, v)
        .unwrap()
        .unwrap();
    let mut exact = Query::new_single_key(b"doc".to_vec());
    exact.set_subquery(Query::new_single_key(0u64.to_be_bytes().to_vec()));
    let exact = PathQuery::new_unsized(vec![b"type".to_vec(), vec![2]], exact);
    let proof = db.prove_query(&exact, None, v).unwrap().unwrap();
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(1);
    leaf.limit = Some(1);
    let mut positions = Query::new_single_key(0u64.to_be_bytes().to_vec());
    positions.set_subquery(leaf);
    let mut query = Query::new_single_key(b"doc".to_vec());
    query.set_subquery(positions);
    let query = PathQuery::new_unsized(vec![b"type".to_vec(), vec![2]], query);
    assert!(
        GroveDb::verify_query_raw(&proof, &query, v).is_err(),
        "a non-Merk position result cannot satisfy descendant pagination"
    );
    assert!(db.prove_query(&query, None, v).unwrap().is_err());
    assert!(db
        .query_raw(
            &query,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            v
        )
        .unwrap()
        .is_err());
}

fn parent_echo_query(shape: u8) -> PathQuery {
    let mut query = Query::new_range_full();
    query.add_parent_tree_on_subquery = true;
    match shape {
        0 => query.set_subquery(Query::new_range_full()),
        1 => query.add_conditional_subquery(
            grovedb::QueryItem::Key(b"doc".to_vec()),
            None,
            Some(Query::new_range_full()),
        ),
        _ => query.set_subquery_path(vec![history_key(1)]),
    }
    PathQuery::new(
        vec![b"type".to_vec(), vec![2]],
        SizedQuery::new(query, Some(1), None),
    )
}

#[test]
fn typed_query_ignores_parent_echoes_under_v4_and_v5() {
    for host in [Element::empty_tree(), Element::empty_provable_count_tree()] {
        let (_dir, db) = fixture(1, Some(host), [true, true]);
        for v in [
            &grovedb_version::version::v4::GROVE_V4,
            GroveVersion::latest(),
        ] {
            for shape in 0..3 {
                for result_type in [
                    QueryResultType::QueryElementResultType,
                    QueryResultType::QueryKeyElementPairResultType,
                    QueryResultType::QueryPathKeyElementTrioResultType,
                ] {
                    let (rows, skipped) = db
                        .query(
                            &parent_echo_query(shape),
                            true,
                            true,
                            true,
                            result_type,
                            None,
                            v,
                        )
                        .unwrap()
                        .unwrap();
                    assert_eq!(rows.to_elements(), vec![Element::new_item(vec![1])]);
                    assert_eq!(skipped, 0);
                }
            }
        }
    }
}

#[test]
fn typed_item_values_ignore_parent_echoes_under_v4_and_v5() {
    use grovedb::operations::QueryItemOrSumReturnType;
    for host in [Element::empty_tree(), Element::empty_provable_count_tree()] {
        for n in [0, 1] {
            let (_dir, db) = fixture(n, Some(host.clone()), [true, true]);
            for v in [
                &grovedb_version::version::v4::GROVE_V4,
                GroveVersion::latest(),
            ] {
                for shape in 0..3 {
                    let query = parent_echo_query(shape);
                    let expected = if n == 0 { vec![] } else { vec![vec![1]] };
                    let (values, skipped) = db
                        .query_item_value(&query, true, true, true, None, v)
                        .unwrap()
                        .unwrap();
                    assert_eq!(values, expected);
                    assert_eq!(skipped, 0);
                    let (values, _) = db
                        .query_item_value_or_sum(&query, true, true, true, None, v)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        values,
                        expected
                            .into_iter()
                            .map(QueryItemOrSumReturnType::ItemData)
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}

#[test]
fn typed_sums_ignore_parent_echoes_under_v4_and_v5() {
    let (_dir, db) = fixture(0, Some(Element::empty_sum_tree()), [true, true]);
    db.insert(
        &[b"type".as_slice(), &[2], b"doc"],
        &history_key(1),
        Element::new_sum_item(42),
        None,
        None,
        GroveVersion::latest(),
    )
    .unwrap()
    .unwrap();
    for v in [
        &grovedb_version::version::v4::GROVE_V4,
        GroveVersion::latest(),
    ] {
        for shape in 0..3 {
            assert_eq!(
                db.query_sums(&parent_echo_query(shape), true, true, true, None, v)
                    .unwrap()
                    .unwrap(),
                (vec![42], 0)
            );
        }
    }
}

#[test]
#[allow(deprecated)]
fn typed_encoded_many_ignores_parent_echoes_under_v4_and_v5() {
    let (_dir, db) = fixture(0, Some(Element::empty_tree()), [true, true]);
    db.insert(
        &[b"type".as_slice(), &[2], b"doc"],
        &history_key(1),
        Element::new_reference(
            grovedb::reference_path::ReferencePathType::AbsolutePathReference(vec![
                b"type".to_vec(),
                vec![0],
                b"doc".to_vec(),
            ]),
        ),
        None,
        None,
        GroveVersion::latest(),
    )
    .unwrap()
    .unwrap();
    for v in [
        &grovedb_version::version::v4::GROVE_V4,
        GroveVersion::latest(),
    ] {
        for shape in 0..3 {
            let query = parent_echo_query(shape);
            assert_eq!(
                db.query_encoded_many(&[&query], true, true, true, None, v)
                    .unwrap()
                    .unwrap(),
                vec![vec![0]]
            );
            let raw = db
                .query_many_raw(
                    &[&query],
                    true,
                    true,
                    true,
                    QueryResultType::QueryElementResultType,
                    None,
                    v,
                )
                .unwrap()
                .unwrap();
            assert_eq!(raw.len(), if v.protocol_version == 5 { 2 } else { 1 });
        }
    }
}

#[test]
fn missing_direct_local_page_matches_ordinary_ranges_and_authenticated_absence() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(0, None, [true, true]);
    for offset in [None, Some(1)] {
        let mut page = Query::new_range_full();
        page.offset = offset;
        page.limit = Some(1);
        let query = PathQuery::new_unsized(vec![b"type".to_vec(), vec![2], b"doc".to_vec()], page);
        let proof = db.prove_query(&query, None, v).unwrap().unwrap();
        let (root, verified) = GroveDb::verify_query_raw(&proof, &query, v).unwrap();
        assert_eq!(root, db.root_hash(None, v).unwrap().unwrap());
        assert!(verified.is_empty());
        for strict in [false, true] {
            let (rows, skipped) = db
                .query_raw(
                    &query,
                    true,
                    true,
                    strict,
                    QueryResultType::QueryPathKeyElementTrioResultType,
                    None,
                    v,
                )
                .unwrap()
                .unwrap();
            assert!(
                rows.is_empty(),
                "direct missing ranges have no rows with strict={strict}"
            );
            assert_eq!(skipped, 0);
        }
    }
}

#[test]
fn merged_global_zero_offsets_do_not_require_count_tree_hosts() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(2, Some(Element::empty_tree()), [true, true]);
    let page = PathQuery::new(
        vec![b"type".to_vec(), vec![2], b"doc".to_vec()],
        SizedQuery::new(Query::new_range_full(), None, Some(0)),
    );
    let sibling = PathQuery::new_unsized(
        vec![b"type".to_vec(), vec![0]],
        Query::new_single_key(b"doc".to_vec()),
    );
    let single = PathQuery::merge(vec![&page], v).unwrap();
    assert_eq!(single.query.offset, Some(0));
    assert!(!single.query.query.has_instance_offset_anywhere());
    for inputs in [vec![&page, &sibling], vec![&sibling, &page]] {
        let merged = PathQuery::merge(inputs, v).unwrap();
        assert!(
            !merged.query.query.has_instance_offset_anywhere(),
            "a global zero skip must not become a local count operation"
        );
        assert_eq!(round_trip(&db, &merged).len(), 3);
    }
    let root = PathQuery::new(
        vec![b"type".to_vec()],
        SizedQuery::new(Query::new_single_key(vec![0]), None, Some(0)),
    );
    let merged = PathQuery::merge(vec![&root, &page], v).unwrap();
    assert!(!merged.query.query.has_instance_offset_anywhere());
    assert_eq!(round_trip(&db, &merged).len(), 3);
}

#[test]
fn missing_subquery_path_page_charges_one_shared_slot_before_its_sibling() {
    let v = GroveVersion::latest();
    let (_dir, db) = fixture(0, None, [true, true]);
    for cap in [1, 2] {
        let mut page = Query::new_range_full();
        page.offset = Some(1);
        page.limit = Some(1);
        let mut root = Query::new_with_direction(false);
        root.insert_key(vec![2]);
        root.add_conditional_subquery(
            grovedb::QueryItem::Key(vec![2]),
            Some(vec![b"doc".to_vec()]),
            Some(page),
        );
        root.insert_key(vec![1]);
        root.add_conditional_subquery(
            grovedb::QueryItem::Key(vec![1]),
            None,
            Some(Query::new_single_key(b"doc".to_vec())),
        );
        let query = PathQuery::new(
            vec![b"type".to_vec()],
            SizedQuery::new(root, Some(cap), None),
        );
        let verified = round_trip(&db, &query);
        assert_eq!(
            verified.len(),
            usize::from(cap == 2),
            "the missing page consumes one global slot before the sibling"
        );
        if cap == 2 {
            assert_eq!(verified[0].0, vec![b"type".to_vec(), vec![1]]);
            assert_eq!(verified[0].1, b"doc");
        }
        let (relaxed, _) = db
            .query_raw(
                &query,
                true,
                true,
                false,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                v,
            )
            .unwrap()
            .unwrap();
        let relaxed: Vec<_> = relaxed
            .to_path_key_elements()
            .into_iter()
            .map(|(p, k, e)| (p, k, e.serialize(v).unwrap()))
            .collect();
        assert_eq!(relaxed, verified);
    }
}
