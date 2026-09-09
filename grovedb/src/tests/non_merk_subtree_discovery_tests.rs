//! Issue #890: recursive discovery must include non-Merk descendants for
//! cleanup without interpreting their records as Merk nodes.

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::tree_type::TreeType;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use crate::{
    batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
    operations::delete::DeleteOptions,
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element, Error, TransactionArg,
};

#[derive(Clone, Copy, Debug)]
enum Family {
    Mmr,
    Bulk,
    Dense,
    Commitment,
    PrivateDocumentStore,
}

impl Family {
    fn empty_element(self) -> Element {
        match self {
            Self::Mmr => Element::empty_mmr_tree(),
            Self::Bulk => Element::empty_bulk_append_tree(2).expect("valid chunk power"),
            Self::Dense => Element::empty_dense_tree(3),
            Self::Commitment => Element::empty_commitment_tree(2).expect("valid chunk power"),
            Self::PrivateDocumentStore => {
                Element::empty_private_document_store(48, 2).expect("valid configuration")
            }
        }
    }

    fn populate(self, db: &TempGroveDb, gv: &GroveVersion) {
        // Five entries exercise an MMR merge, a full dense/bulk chunk and
        // a partially filled buffer, as well as commitment-tree metadata.
        for i in 0..5u8 {
            let path = &[b"outer".as_slice(), b"mid".as_slice()];
            match self {
                Self::Mmr => {
                    db.mmr_tree_append(path, b"payload", vec![i], None, gv)
                        .unwrap()
                        .expect("MMR append");
                }
                Self::Bulk => {
                    db.bulk_append(path, b"payload", vec![i], None, gv)
                        .unwrap()
                        .expect("bulk append");
                }
                Self::Dense => {
                    db.dense_tree_insert(path, b"payload", vec![i], None, gv)
                        .unwrap()
                        .expect("dense insert");
                }
                Self::Commitment => {
                    use grovedb_commitment_tree::{
                        DashMemo, NoteBytesData, TransmittedNoteCiphertext,
                    };
                    let mut cmx = [0; 32];
                    cmx[0] = i;
                    let ciphertext = TransmittedNoteCiphertext::<DashMemo>::from_parts(
                        [i; 32],
                        NoteBytesData([i; 104]),
                        [i; 80],
                    );
                    db.commitment_tree_insert(
                        path, b"payload", cmx, [i; 32], [i; 32], ciphertext, None, gv,
                    )
                    .unwrap()
                    .expect("commitment insert");
                }
                Self::PrivateDocumentStore => {
                    db.private_document_store_insert(path, b"payload", vec![i; 48], None, gv)
                        .unwrap()
                        .expect("private document insert");
                }
            }
        }
    }
}

fn namespace_non_empty(db: &TempGroveDb, prefix: [u8; 32], tx: TransactionArg) -> bool {
    let local_tx = db.start_transaction();
    let storage = db
        .db
        .get_transactional_storage_context_by_subtree_prefix(prefix, None, tx.unwrap_or(&local_tx))
        .unwrap();
    let mut iter = storage.raw_iter();
    iter.seek_to_first().unwrap();
    iter.valid().unwrap()
}

fn prefix(path: &[&[u8]]) -> [u8; 32] {
    RocksDbStorage::build_prefix(SubtreePath::from(path)).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum DeleteRoute {
    Direct,
    FullBatch,
    PartialBatch,
}

fn assert_discovery_and_cleanup(family: Family, wrapped: bool) {
    let gv = GroveVersion::latest();
    for populated in [false, true] {
        for route in [
            DeleteRoute::Direct,
            DeleteRoute::FullBatch,
            DeleteRoute::PartialBatch,
        ] {
            let db = make_empty_grovedb();
            db.insert(
                EMPTY_PATH,
                b"survivor",
                Element::empty_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert sibling outside deleted subtree");
            db.insert(
                &[b"survivor".as_slice()],
                b"item",
                Element::new_item(vec![42]),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("populate survivor");
            let expected_root = db.root_hash(None, gv).unwrap().expect("survivor root");

            let mut axes: Vec<_> = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
                .into_iter()
                .map(|axis| (axis.tag(), None))
                .collect();
            axes.sort_by_key(|(tag, _)| *tag);
            for (path, key, element) in [
                (&[] as &[&[u8]], b"outer".as_slice(), Element::empty_tree()),
                (
                    &[b"outer".as_slice()],
                    b"mid".as_slice(),
                    Element::empty_count_tree(),
                ),
                (
                    &[b"outer".as_slice()],
                    b"ordinary".as_slice(),
                    Element::empty_tree(),
                ),
                (
                    &[b"outer".as_slice()],
                    b"indexed".as_slice(),
                    Element::empty_provable_count_provable_sum_indexed_tree(axes)
                        .expect("valid axes"),
                ),
            ] {
                db.insert(path, key, element, None, None, gv)
                    .unwrap()
                    .expect("insert Merk tree");
            }
            let element = family.empty_element();
            let element = if wrapped {
                Element::new_non_counted(element).expect("valid wrapper")
            } else {
                element
            };
            db.insert(
                &[b"outer".as_slice(), b"mid".as_slice()],
                b"payload",
                element,
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert non-Merk tree");
            db.insert(
                &[b"outer".as_slice(), b"ordinary".as_slice()],
                b"item",
                Element::new_item(vec![7]),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("populate ordinary sibling");
            db.insert_into_provable_count_provable_sum_indexed_tree(
                &[b"outer".as_slice(), b"indexed".as_slice()],
                b"item",
                Element::new_item_with_sum_item(vec![8], 8),
                None,
                gv,
            )
            .unwrap()
            .expect("populate all indexed axes");
            if populated {
                family.populate(&db, gv);
            }

            let paths: Vec<Vec<Vec<u8>>> = [
                vec![b"outer".as_slice()],
                vec![b"outer", b"indexed"],
                vec![b"outer", b"mid"],
                vec![b"outer", b"mid", b"payload"],
                vec![b"outer", b"ordinary"],
            ]
            .into_iter()
            .map(|path| path.into_iter().map(<[u8]>::to_vec).collect())
            .collect();
            let mut discovered = db
                .find_subtrees(&[b"outer".as_slice()].as_slice().into(), None, gv)
                .unwrap()
                .expect("discover mixed storage families");
            discovered.sort();
            assert_eq!(discovered, paths, "{family:?}, populated={populated}");

            let primary_prefixes: Vec<_> = paths
                .iter()
                .map(|path| {
                    RocksDbStorage::build_prefix(SubtreePath::from(path.as_slice())).unwrap()
                })
                .collect();
            let payload_prefix = prefix(&[b"outer", b"mid", b"payload"]);
            assert_eq!(namespace_non_empty(&db, payload_prefix, None), populated);
            let indexed_prefix = prefix(&[b"outer", b"indexed"]);
            let secondary_prefixes: Vec<_> = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
                .into_iter()
                .map(|axis| {
                    RocksDbStorage::secondary_prefix_for(&indexed_prefix, axis.tag()).unwrap()
                })
                .collect();
            for &secondary in &secondary_prefixes {
                assert!(
                    namespace_non_empty(&db, secondary, None),
                    "axis must be populated before cleanup"
                );
            }

            let tx = db.start_transaction();
            let ops = vec![QualifiedGroveDbOp::delete_tree_op(
                vec![],
                b"outer".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )];
            match route {
                DeleteRoute::Direct => db.delete(
                    EMPTY_PATH,
                    b"outer",
                    Some(DeleteOptions {
                        allow_deleting_non_empty_trees: true,
                        deleting_non_empty_trees_returns_error: false,
                        ..Default::default()
                    }),
                    Some(&tx),
                    gv,
                ),
                DeleteRoute::FullBatch => db.apply_batch(ops, None, Some(&tx), gv),
                DeleteRoute::PartialBatch => {
                    db.apply_partial_batch(ops, None, |_, _| Ok(vec![]), Some(&tx), gv)
                }
            }
            .unwrap()
            .expect("delete ancestor of non-Merk tree");

            for &owned_prefix in primary_prefixes.iter().chain(&secondary_prefixes) {
                assert!(
                    !namespace_non_empty(&db, owned_prefix, Some(&tx)),
                    "{family:?} {route:?}: owned namespace survived"
                );
            }
            assert!(
                namespace_non_empty(&db, prefix(&[b"outer"]), None),
                "deletion must remain transaction-local"
            );
            db.commit_transaction(tx).unwrap().expect("commit deletion");
            for &owned_prefix in primary_prefixes.iter().chain(&secondary_prefixes) {
                assert!(
                    !namespace_non_empty(&db, owned_prefix, None),
                    "committed cleanup must persist"
                );
            }
            assert_eq!(
                db.root_hash(None, gv).unwrap().expect("root after delete"),
                expected_root
            );
            assert_eq!(
                db.get(&[b"survivor".as_slice()], b"item", None, gv)
                    .unwrap()
                    .expect("retained item"),
                Element::new_item(vec![42])
            );
        }
    }
}

#[test]
fn discovers_and_deletes_nested_mmr() {
    assert_discovery_and_cleanup(Family::Mmr, false);
}

#[test]
fn discovers_and_deletes_nested_bulk() {
    assert_discovery_and_cleanup(Family::Bulk, false);
}

#[test]
fn discovers_and_deletes_nested_dense() {
    assert_discovery_and_cleanup(Family::Dense, false);
}

#[test]
fn discovers_and_deletes_nested_commitment() {
    assert_discovery_and_cleanup(Family::Commitment, false);
}

#[test]
fn discovers_and_deletes_nested_private_document_store() {
    assert_discovery_and_cleanup(Family::PrivateDocumentStore, false);
}

#[test]
fn discovers_and_deletes_wrapped_non_merk_descendant() {
    assert_discovery_and_cleanup(Family::Mmr, true);
}

#[test]
fn legacy_versions_preserve_populated_mmr_discovery_failure() {
    for gv in GROVE_VERSIONS.iter().filter(|gv| gv.protocol_version <= 3) {
        let db = make_empty_grovedb();
        db.insert(EMPTY_PATH, b"outer", Element::empty_tree(), None, None, gv)
            .unwrap()
            .expect("insert outer");
        db.insert(
            &[b"outer".as_slice()],
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert MMR");
        db.mmr_tree_append(&[b"outer".as_slice()], b"mmr", vec![1], None, gv)
            .unwrap()
            .expect("append MMR");
        assert!(db
            .find_subtrees(&[b"outer".as_slice()].as_slice().into(), None, gv)
            .unwrap()
            .is_err());
    }
}

#[test]
fn unknown_discovery_version_fails_closed() {
    let db = make_empty_grovedb();
    let mut gv = GroveVersion::latest().clone();
    gv.grovedb_versions
        .operations
        .non_merk_tree
        .subtree_discovery = 2;
    let result = db.find_subtrees(&EMPTY_PATH, None, &gv);
    assert!(matches!(result.value, Err(Error::VersionError(_))));
    assert_eq!(result.cost, Default::default());
}
