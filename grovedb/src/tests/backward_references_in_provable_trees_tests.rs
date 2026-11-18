//! Backward-references items as entries of the provable aggregate trees
//! and of the indexed trees.
//!
//! A backward-references item commits to its serialization with the
//! referrer list emptied, like the plain item it extends, so it proves in
//! the value-carrying node its host uses for items (`KVCount`, `KVSum`,
//! `KVCountSum`, `KVCountBigSum`, `KVBigSum`) and the host needs no rule of
//! its own.
//! These tests insert such an item into every host, directly and by batch,
//! prove and verify it, and check that registering a referrer on it moves
//! no hash of the host while the reference itself is committed.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{element::get::ElementFetchFromStorageExtensions, CryptoHash};
    use grovedb_path::SubtreePath;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, GroveDb, PathQuery, Query,
    };

    const HOST: &[u8] = b"host";
    const ENTRY: &[u8] = b"entry";
    const REFERRER: &[u8] = b"referrer";

    /// The trees under test, named without their shared `Provable` prefix
    /// and `Tree` suffix.
    #[derive(Clone, Copy, Debug)]
    enum Host {
        Count,
        CountSum,
        Sum,
        CountProvableSum,
        CountProvableBigSum,
        BigSum,
        CountIndexed,
        SumIndexed,
        CountProvableSumIndexed,
    }

    impl Host {
        const ALL: [Host; 9] = [
            Host::Count,
            Host::CountSum,
            Host::Sum,
            Host::CountProvableSum,
            Host::CountProvableBigSum,
            Host::BigSum,
            Host::CountIndexed,
            Host::SumIndexed,
            Host::CountProvableSumIndexed,
        ];

        fn empty_element(self) -> Element {
            match self {
                Host::Count => Element::empty_provable_count_tree(),
                Host::CountSum => Element::empty_provable_count_sum_tree(),
                Host::Sum => Element::empty_provable_sum_tree(),
                Host::CountProvableSum => Element::empty_provable_count_provable_sum_tree(),
                Host::CountProvableBigSum => Element::empty_provable_count_provable_big_sum_tree(),
                Host::BigSum => Element::empty_provable_big_sum_tree(),
                Host::CountIndexed => Element::empty_provable_count_indexed_tree(),
                Host::SumIndexed => Element::empty_provable_sum_indexed_tree(),
                Host::CountProvableSumIndexed => {
                    Element::empty_provable_count_provable_sum_indexed_tree(vec![
                        (IndexAxis::Count.tag(), None),
                        (IndexAxis::Sum.tag(), None),
                        (IndexAxis::Avg.tag(), None),
                    ])
                    .expect("canonical axes")
                }
            }
        }

        fn is_indexed(self) -> bool {
            matches!(
                self,
                Host::CountIndexed | Host::SumIndexed | Host::CountProvableSumIndexed
            )
        }

        /// The backward-references items this host accepts. The two
        /// count-only hosts are not sum trees, so they take the plain
        /// item; the others take the sum-carrying variants. The
        /// count-and-sum indexed tree asks its children for a count and a
        /// sum and names the plain `SumItem` as its only bare sum item, so
        /// it takes the item-with-sum variant alone.
        fn entries(self) -> Vec<Element> {
            let item_with_sum = Element::new_item_with_sum_item_allowing_bidirectional_references(
                b"payload".to_vec(),
                7,
            );
            match self {
                Host::Count | Host::CountIndexed => {
                    vec![Element::new_item_allowing_bidirectional_references(
                        b"payload".to_vec(),
                    )]
                }
                Host::CountProvableSumIndexed => vec![item_with_sum],
                _ => vec![
                    Element::new_sum_item_allowing_bidirectional_references(7),
                    item_with_sum,
                ],
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Write {
        Direct,
        Batch,
    }

    const WRITES: [Write; 2] = [Write::Direct, Write::Batch];

    fn host_path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), HOST.to_vec()]
    }

    /// A database holding an empty `host` at `[TEST_LEAF, HOST]`.
    fn db_with_host(host: Host, gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        db.insert(&[TEST_LEAF], HOST, host.empty_element(), None, None, gv)
            .unwrap()
            .expect("insert the host tree");
        db
    }

    fn insert_entry(db: &GroveDb, host: Host, write: Write, entry: Element, gv: &GroveVersion) {
        let path = [TEST_LEAF, HOST];
        let result = match (write, host) {
            (Write::Batch, _) => db
                .apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        host_path(),
                        ENTRY.to_vec(),
                        entry,
                    )],
                    None,
                    None,
                    gv,
                )
                .unwrap(),
            (Write::Direct, Host::CountIndexed) => db
                .insert_into_count_indexed_tree(&path, ENTRY, entry, None, gv)
                .unwrap(),
            (Write::Direct, Host::SumIndexed) => db
                .insert_into_provable_sum_indexed_tree(&path, ENTRY, entry, None, gv)
                .unwrap(),
            (Write::Direct, Host::CountProvableSumIndexed) => db
                .insert_into_provable_count_provable_sum_indexed_tree(&path, ENTRY, entry, None, gv)
                .unwrap(),
            (Write::Direct, _) => db.insert(&path, ENTRY, entry, None, None, gv).unwrap(),
        };
        result.unwrap_or_else(|e| {
            panic!("Provable{host:?}Tree must accept the entry by {write:?}: {e}")
        });
    }

    /// Register a bidirectional reference to the entry from another
    /// subtree, so that nothing under `TEST_LEAF` gains a node.
    ///
    /// A registration rewrites its target, and a write below an indexed
    /// tree is only supported in a batch (the direct path does not
    /// propagate through an indexed tree), so for an indexed host the
    /// reference always goes in by batch.
    fn insert_referrer(db: &GroveDb, host: Host, write: Write, label: &str, gv: &GroveVersion) {
        let write = if host.is_indexed() {
            Write::Batch
        } else {
            write
        };
        let reference =
            Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                HOST.to_vec(),
                ENTRY.to_vec(),
            ]));
        match write {
            Write::Direct => db
                .insert(&[ANOTHER_TEST_LEAF], REFERRER, reference, None, None, gv)
                .unwrap(),
            Write::Batch => db
                .apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![ANOTHER_TEST_LEAF.to_vec()],
                        REFERRER.to_vec(),
                        reference,
                    )],
                    None,
                    None,
                    gv,
                )
                .unwrap(),
        }
        .unwrap_or_else(|e| panic!("{label}: register a referrer: {e}"));
    }

    fn key_query(path: Vec<Vec<u8>>, key: &[u8]) -> PathQuery {
        let mut query = Query::new();
        query.insert_key(key.to_vec());
        PathQuery::new_unsized(path, query)
    }

    /// Prove `key` at `path` and verify it against the live root,
    /// returning the single proved element.
    fn prove_and_verify(
        db: &GroveDb,
        path: Vec<Vec<u8>>,
        key: &[u8],
        label: &str,
        gv: &GroveVersion,
    ) -> Element {
        let path_query = key_query(path, key);
        let proof = db
            .prove_query(&path_query, None, gv)
            .unwrap()
            .unwrap_or_else(|e| panic!("{label}: prove: {e}"));
        let (root, mut rows) = GroveDb::verify_query(&proof, &path_query, gv)
            .unwrap_or_else(|e| panic!("{label}: verify: {e}"));
        assert_eq!(
            root,
            db.root_hash(None, gv).unwrap().unwrap(),
            "{label}: the proof verifies against the live root"
        );
        assert_eq!(rows.len(), 1, "{label}: one row");
        rows.pop()
            .and_then(|(_, _, element)| element)
            .unwrap_or_else(|| panic!("{label}: the row carries an element"))
    }

    /// The Merk root hash of the tree at `path`.
    fn merk_root_hash(db: &GroveDb, path: &[&[u8]], gv: &GroveVersion) -> CryptoHash {
        let tx = db.start_transaction();
        db.open_transactional_merk_at_path(SubtreePath::from(path), &tx, None, gv)
            .unwrap()
            .unwrap()
            .root_hash()
            .unwrap()
    }

    /// The value hash and the stored (unstripped) element of the node at
    /// `path` / `key`.
    fn stored_node(
        db: &GroveDb,
        path: &[&[u8]],
        key: &[u8],
        gv: &GroveVersion,
    ) -> (CryptoHash, Element) {
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(SubtreePath::from(path), &tx, None, gv)
            .unwrap()
            .unwrap();
        let hash = Element::get_value_hash(&merk, key, true, gv)
            .unwrap()
            .unwrap()
            .expect("the node exists");
        let element = Element::get(&merk, key, true, gv).unwrap().unwrap();
        (hash, element)
    }

    fn assert_grove_verifies(db: &GroveDb, label: &str, gv: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, gv)
            .unwrap_or_else(|e| panic!("{label}: verify_grovedb: {e}"));
        assert!(issues.is_empty(), "{label}: verify_grovedb: {issues:?}");
    }

    /// Every provable aggregate tree and every indexed tree accepts a
    /// backward-references item, by direct insert and by batch, and proves
    /// it: the verifier returns the item under the live root.
    #[test]
    fn every_provable_host_accepts_and_proves_a_backward_references_item() {
        let gv = GroveVersion::latest();
        for host in Host::ALL {
            for write in WRITES {
                for entry in host.entries() {
                    let label = format!("Provable{host:?}Tree/{write:?}/{}", entry.type_str());
                    let db = db_with_host(host, gv);
                    insert_entry(&db, host, write, entry.clone(), gv);

                    assert_eq!(
                        prove_and_verify(&db, host_path(), ENTRY, &label, gv),
                        entry,
                        "{label}: the proved entry"
                    );
                    assert_grove_verifies(&db, &label, gv);
                }
            }
        }
    }

    /// Registering a referrer on an entry of a provable or indexed tree
    /// rewrites the entry's stored bytes and nothing else under the host:
    /// the entry's value hash, the host's Merk root hash, the host's own
    /// node (which for an indexed tree also commits its secondary
    /// indexes) and the subtree above it all keep their hashes. The
    /// reference is committed where it lives, so the database root moves,
    /// and both the entry and the reference still prove.
    #[test]
    fn registering_a_referrer_moves_no_hash_of_the_host() {
        let gv = GroveVersion::latest();
        for host in Host::ALL {
            for write in WRITES {
                for entry in host.entries() {
                    let label = format!("Provable{host:?}Tree/{write:?}/{}", entry.type_str());
                    let db = db_with_host(host, gv);
                    insert_entry(&db, host, write, entry.clone(), gv);

                    let (entry_hash_before, stored_before) =
                        stored_node(&db, &[TEST_LEAF, HOST], ENTRY, gv);
                    assert!(
                        stored_before.backward_references().unwrap().is_empty(),
                        "{label}: no referrer yet"
                    );
                    let host_root_before = merk_root_hash(&db, &[TEST_LEAF, HOST], gv);
                    let (host_node_before, _) = stored_node(&db, &[TEST_LEAF], HOST, gv);
                    let (leaf_node_before, _) = stored_node(&db, &[], TEST_LEAF, gv);
                    let db_root_before = db.root_hash(None, gv).unwrap().unwrap();

                    insert_referrer(&db, host, write, &label, gv);

                    // The registration is stored on the entry.
                    let (entry_hash_after, stored_after) =
                        stored_node(&db, &[TEST_LEAF, HOST], ENTRY, gv);
                    assert_eq!(
                        stored_after.backward_references().unwrap().len(),
                        1,
                        "{label}: the referrer is registered on the entry"
                    );
                    assert_ne!(
                        stored_after.serialize(gv).unwrap(),
                        stored_before.serialize(gv).unwrap(),
                        "{label}: the entry's stored bytes changed"
                    );

                    // No hash of the host moved.
                    assert_eq!(
                        entry_hash_after, entry_hash_before,
                        "{label}: the entry's value hash"
                    );
                    assert_eq!(
                        merk_root_hash(&db, &[TEST_LEAF, HOST], gv),
                        host_root_before,
                        "{label}: the host's Merk root hash"
                    );
                    assert_eq!(
                        stored_node(&db, &[TEST_LEAF], HOST, gv).0,
                        host_node_before,
                        "{label}: the host's own node"
                    );
                    assert_eq!(
                        stored_node(&db, &[], TEST_LEAF, gv).0,
                        leaf_node_before,
                        "{label}: the subtree holding the host"
                    );

                    // The reference itself is committed: the database root
                    // moved, and the reference proves and resolves to the
                    // entry.
                    assert_ne!(
                        db.root_hash(None, gv).unwrap().unwrap(),
                        db_root_before,
                        "{label}: the reference is committed under the database root"
                    );
                    assert_eq!(
                        prove_and_verify(
                            &db,
                            vec![ANOTHER_TEST_LEAF.to_vec()],
                            REFERRER,
                            &label,
                            gv
                        ),
                        entry,
                        "{label}: the proved reference resolves to the entry"
                    );

                    // The entry still proves, without its referrer list.
                    assert_eq!(
                        prove_and_verify(&db, host_path(), ENTRY, &label, gv),
                        entry,
                        "{label}: the proved entry after registration"
                    );
                    assert_grove_verifies(&db, &label, gv);
                }
            }
        }
    }

    /// A bidirectional reference is itself an entry of every provable
    /// aggregate tree, by direct insert and by batch: it proves under the
    /// live root and resolves to its target, which registers it.
    #[test]
    fn a_bidirectional_reference_lives_in_every_provable_aggregate_tree() {
        let gv = GroveVersion::latest();
        let target = Element::new_item_allowing_bidirectional_references(b"target".to_vec());
        for host in Host::ALL.into_iter().filter(|host| !host.is_indexed()) {
            for write in WRITES {
                let label = format!("Provable{host:?}Tree/{write:?}");
                let db = db_with_host(host, gv);
                db.insert(
                    &[ANOTHER_TEST_LEAF],
                    b"target",
                    target.clone(),
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("insert the target");

                let reference =
                    Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(
                        vec![ANOTHER_TEST_LEAF.to_vec(), b"target".to_vec()],
                    ));
                match write {
                    Write::Direct => db
                        .insert(&[TEST_LEAF, HOST], REFERRER, reference, None, None, gv)
                        .unwrap(),
                    Write::Batch => db
                        .apply_batch(
                            vec![QualifiedGroveDbOp::insert_or_replace_op(
                                host_path(),
                                REFERRER.to_vec(),
                                reference,
                            )],
                            None,
                            None,
                            gv,
                        )
                        .unwrap(),
                }
                .unwrap_or_else(|e| panic!("{label}: the host must accept the reference: {e}"));

                assert_eq!(
                    prove_and_verify(&db, host_path(), REFERRER, &label, gv),
                    target,
                    "{label}: the proved reference resolves to its target"
                );
                let (_, stored_target) = stored_node(&db, &[ANOTHER_TEST_LEAF], b"target", gv);
                assert_eq!(
                    stored_target.backward_references().unwrap().len(),
                    1,
                    "{label}: the target registers the reference"
                );
                assert_grove_verifies(&db, &label, gv);
            }
        }
    }
}
