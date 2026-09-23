//! Referrers of a flagged backward-references item stay bound when a batch
//! apply's just-in-time flags update rewrites the item's flags.
//!
//! Drive applies batches with `StorageFlags::update_element_flags` as the
//! flags callback. On every cross-epoch update of a flagged value it
//! rewrites the new flags: a same-size write takes back the old flags, a
//! larger one merges into a multi-epoch map, a smaller one combines the
//! removed bytes. Referrers commit to the LOGICAL hash of their terminal
//! item, flags included, so the batch planner must commit them to the
//! bytes that finally land, not to the bytes the caller supplied.
//! Otherwise `verify_grovedb` reports every referrer and honest proofs of
//! them fail to verify.

use grovedb_costs::storage_cost::removal::{
    StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval,
};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::ClearOptions,
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    BackwardReferences, BackwardsReferences, Element, Error, GroveDb, PathQuery, Query,
};

fn epoch_flags(epoch: u16) -> Option<Vec<u8>> {
    Some(StorageFlags::SingleEpoch(epoch).to_element_flags())
}

fn flagged_item(value: &[u8], epoch: u16) -> Element {
    Element::ItemWithBackwardsReferences(
        value.to_vec(),
        BackwardReferences::with_max_incoming(4),
        epoch_flags(epoch),
    )
}

fn sibling_bidi(target: &[u8]) -> Element {
    Element::new_bidirectional_reference(ReferencePathType::SiblingReference(target.to_vec()))
}

fn stored_storage_flags(
    db: &TempGroveDb,
    path: &[&[u8]],
    key: &[u8],
    grove_version: &GroveVersion,
) -> Option<StorageFlags> {
    let stored = db
        .get_raw(path.into(), key, None, grove_version)
        .unwrap()
        .expect("element present");
    stored
        .get_flags()
        .as_ref()
        .and_then(|flags| StorageFlags::from_element_flags_ref(flags).unwrap())
}

/// Apply `ops` the way Drive does: epoch-based storage flags merged on
/// every update of a flagged value.
fn apply_with_epoch_flags(
    db: &TempGroveDb,
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    db.apply_batch_with_element_flags_update(
        ops,
        None,
        |cost, old_flags, new_flags| {
            StorageFlags::update_element_flags(cost, old_flags, new_flags)
                .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
        },
        |flags, removed_key_bytes, removed_value_bytes| {
            StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
                .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
        },
        None,
        grove_version,
    )
    .unwrap()
}

/// Every node verifies, and an honest proof of each referrer verifies
/// against the live root and returns the item it resolves to.
fn assert_referrers_bound(
    db: &TempGroveDb,
    path: &[&[u8]],
    referrers: &[&[u8]],
    expected_value: &[u8],
    grove_version: &GroveVersion,
) {
    let issues = db.verify_grovedb(None, true, true, grove_version).unwrap();
    assert!(
        issues.is_empty(),
        "verify_grovedb reported {} stale node(s): {:?}",
        issues.len(),
        issues.keys().collect::<Vec<_>>()
    );
    let live_root = db.root_hash(None, grove_version).unwrap().unwrap();
    for referrer in referrers {
        let resolved = db
            .get(path, referrer, None, grove_version)
            .unwrap()
            .expect("the referrer resolves");
        assert_eq!(resolved.as_item_bytes().unwrap(), expected_value);

        let mut query = Query::new();
        query.insert_key(referrer.to_vec());
        let path_query =
            PathQuery::new_unsized(path.iter().map(|segment| segment.to_vec()).collect(), query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (root, results) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .unwrap_or_else(|e| {
                panic!(
                    "an honest proof of {} must verify: {e:?}",
                    String::from_utf8_lossy(referrer)
                )
            });
        assert_eq!(root, live_root, "the proof must commit to the live root");
        assert_eq!(results.len(), 1);
        let element = results[0]
            .2
            .as_ref()
            .expect("the referrer is proved present");
        assert_eq!(element.as_item_bytes().unwrap(), expected_value);
    }
}

/// `TEST_LEAF` holding `value` (a flagged item written in epoch 1) and the
/// chain `value <- r1 <- r2`.
fn db_with_flagged_chain(grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"value",
        flagged_item(b"hello", 1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    for (key, target) in [(b"r1", b"value".as_slice()), (b"r2", b"r1".as_slice())] {
        db.insert(
            &[TEST_LEAF],
            key,
            sibling_bidi(target),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
    assert_referrers_bound(&db, &[TEST_LEAF], &[b"r1", b"r2"], b"hello", grove_version);
    db
}

fn replace_value_op(value: &[u8], epoch: u16) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        flagged_item(value, epoch),
    )
}

/// A same-size, a larger and a smaller epoch-3 update of an epoch-1 item:
/// each stores flags other than the supplied `SingleEpoch(3)`, and the
/// direct referrer and the upstream chain member both follow.
#[test]
fn cross_epoch_update_keeps_referrer_chain_bound() {
    let grove_version = GroveVersion::latest();
    let cases: [(&str, &[u8], fn(&StorageFlags) -> bool); 3] = [
        ("same size", b"world", |flags| {
            *flags == StorageFlags::SingleEpoch(1)
        }),
        (
            "larger",
            b"hello world, larger",
            |flags| matches!(flags, StorageFlags::MultiEpoch(1, map) if map.contains_key(&3)),
        ),
        ("smaller", b"hi", |flags| {
            *flags == StorageFlags::SingleEpoch(1)
        }),
    ];
    for (label, value, expected_flags) in cases {
        let db = db_with_flagged_chain(grove_version);
        apply_with_epoch_flags(&db, vec![replace_value_op(value, 3)], grove_version)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));

        let stored = stored_storage_flags(&db, &[TEST_LEAF], b"value", grove_version)
            .expect("the item keeps storage flags");
        assert!(
            expected_flags(&stored),
            "{label}: the callback must have rewritten the flags, got {stored:?}"
        );
        assert_referrers_bound(&db, &[TEST_LEAF], &[b"r1", b"r2"], value, grove_version);
    }
}

/// The stored and the supplied flags keep differing under Drive's callback,
/// so no later write "heals" a stale referrer: every write has to commit its
/// referrers to the landed bytes. Three successive cross-epoch updates, each
/// checked.
#[test]
fn successive_cross_epoch_updates_keep_referrers_bound() {
    let grove_version = GroveVersion::latest();
    let db = db_with_flagged_chain(grove_version);
    for (epoch, value) in [
        (3, b"hello, epoch three".as_slice()),
        (5, b"hello, epoch five, larger still".as_slice()),
        (6, b"six".as_slice()),
    ] {
        apply_with_epoch_flags(&db, vec![replace_value_op(value, epoch)], grove_version)
            .unwrap_or_else(|e| panic!("epoch {epoch}: {e:?}"));
        let stored = stored_storage_flags(&db, &[TEST_LEAF], b"value", grove_version)
            .expect("the item keeps storage flags");
        assert_ne!(
            stored,
            StorageFlags::SingleEpoch(epoch),
            "epoch {epoch}: the stored flags differ from the supplied ones"
        );
        assert_eq!(*stored.base_epoch(), 1, "the base epoch never moves");
        assert_referrers_bound(&db, &[TEST_LEAF], &[b"r1", b"r2"], value, grove_version);
    }
}

/// Pass 2: a reference inserted in the same batch that updates its target
/// resolves the target from the batch's staged state. Its end hash, and
/// the hashes of references stacked on it in the same batch, must be the
/// landed ones too — including when the new registration is what makes the
/// target larger (the flags the callback derives depend on the full size).
#[test]
fn same_batch_referrers_of_an_updated_item_commit_to_the_landed_bytes() {
    let grove_version = GroveVersion::latest();

    // The target already has referrers.
    let db = db_with_flagged_chain(grove_version);
    apply_with_epoch_flags(
        &db,
        vec![
            replace_value_op(b"hello again", 3),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"n1".to_vec(),
                sibling_bidi(b"value"),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"n2".to_vec(),
                sibling_bidi(b"n1"),
            ),
        ],
        grove_version,
    )
    .unwrap();
    assert_referrers_bound(
        &db,
        &[TEST_LEAF],
        &[b"r1", b"r2", b"n1", b"n2"],
        b"hello again",
        grove_version,
    );

    // The target has no referrer yet: the batch's own registration is the
    // only one, and a same-size value update still grows the element.
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"value",
        flagged_item(b"hello", 1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    apply_with_epoch_flags(
        &db,
        vec![
            replace_value_op(b"world", 3),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"n1".to_vec(),
                sibling_bidi(b"value"),
            ),
        ],
        grove_version,
    )
    .unwrap();
    assert!(matches!(
        stored_storage_flags(&db, &[TEST_LEAF], b"value", grove_version),
        Some(StorageFlags::MultiEpoch(1, _))
    ));
    assert_referrers_bound(&db, &[TEST_LEAF], &[b"n1"], b"world", grove_version);
}

/// Replacing a flagged bidirectional reference with a flagged item keeps
/// the reference's referrer and moves it onto the item: the referrer must
/// commit to the item as it lands.
#[test]
fn item_replacing_a_flagged_reference_binds_the_carried_referrer() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"value",
        flagged_item(b"hello", 1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"mid",
        Element::new_bidirectional_reference_with_options(
            ReferencePathType::SiblingReference(b"value".to_vec()),
            None,
            false,
            epoch_flags(1),
        ),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"top",
        sibling_bidi(b"mid"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    apply_with_epoch_flags(
        &db,
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"mid".to_vec(),
            flagged_item(b"now an item, and larger", 3),
        )],
        grove_version,
    )
    .unwrap();
    assert!(matches!(
        stored_storage_flags(&db, &[TEST_LEAF], b"mid", grove_version),
        Some(StorageFlags::MultiEpoch(1, _))
    ));
    assert_referrers_bound(
        &db,
        &[TEST_LEAF],
        &[b"top"],
        b"now an item, and larger",
        grove_version,
    );
}

/// A plain reference inserted in the same batch hashes its target from the
/// batch's pending op. For a target carrying referrers, the prediction must
/// measure the full stored element (referrer list included), as Merk does.
#[test]
fn same_batch_plain_reference_to_an_updated_referenced_item_is_bound() {
    let grove_version = GroveVersion::latest();
    for value in [b"hello world, larger".as_slice(), b"world", b"hi"] {
        let db = db_with_flagged_chain(grove_version);
        apply_with_epoch_flags(
            &db,
            vec![
                replace_value_op(value, 3),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"plain".to_vec(),
                    Element::new_reference(ReferencePathType::SiblingReference(b"value".to_vec())),
                ),
            ],
            grove_version,
        )
        .unwrap();
        assert_referrers_bound(
            &db,
            &[TEST_LEAF],
            &[b"r1", b"r2", b"plain"],
            value,
            grove_version,
        );
    }
}

/// A family sum item is not a fixed-cost sum item: Merk gives it no
/// value-defined cost, so its measured size follows the encoded sum and the
/// referrer list, and the callback merges epochs when it grows and combines
/// removed bytes when it shrinks. Both the bidirectional referrer and a
/// plain reference written in the same batch must commit to the rewritten
/// flags.
#[test]
fn sum_item_in_a_sum_tree_keeps_referrers_bound() {
    let grove_version = GroveVersion::latest();
    let path: &[&[u8]] = &[TEST_LEAF, b"sums"];
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"sums",
        Element::empty_sum_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    let sum_item = |sum: i64, epoch: u16| {
        Element::SumItemWithBackwardsReferences(
            sum,
            BackwardReferences::with_max_incoming(4),
            epoch_flags(epoch),
        )
    };
    db.insert(path, b"value", sum_item(5, 1), None, None, grove_version)
        .unwrap()
        .unwrap();
    db.insert(
        path,
        b"r1",
        sibling_bidi(b"value"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // A plain reference is not maintained when its target changes, so each
    // round's batch rewrites it alongside the target.
    let plain: &[u8] = b"plain";
    for (sum, epoch) in [(5_000_000_000i64, 3u16), (-7, 4)] {
        apply_with_epoch_flags(
            &db,
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    path.iter().map(|segment| segment.to_vec()).collect(),
                    b"value".to_vec(),
                    sum_item(sum, epoch),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    path.iter().map(|segment| segment.to_vec()).collect(),
                    plain.to_vec(),
                    Element::new_reference(ReferencePathType::SiblingReference(b"value".to_vec())),
                ),
            ],
            grove_version,
        )
        .unwrap_or_else(|e| panic!("sum {sum}: {e:?}"));
        let stored = stored_storage_flags(&db, path, b"value", grove_version)
            .expect("the item keeps storage flags");
        assert_ne!(
            stored,
            StorageFlags::SingleEpoch(epoch),
            "sum {sum}: the callback rewrites the supplied flags"
        );
        assert_eq!(*stored.base_epoch(), 1);
        let issues = db.verify_grovedb(None, true, true, grove_version).unwrap();
        assert!(issues.is_empty(), "sum {sum}: {issues:?}");
        let live_root = db.root_hash(None, grove_version).unwrap().unwrap();
        for referrer in [b"r1".as_slice(), plain] {
            let resolved = db
                .get(path, referrer, None, grove_version)
                .unwrap()
                .unwrap();
            assert_eq!(resolved.sum_value_or_default(), sum);

            let mut query = Query::new();
            query.insert_key(referrer.to_vec());
            let path_query = PathQuery::new_unsized(
                path.iter().map(|segment| segment.to_vec()).collect(),
                query,
            );
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .unwrap();
            let (root, _) = GroveDb::verify_query(&proof, &path_query, grove_version)
                .unwrap_or_else(|e| panic!("sum {sum}: honest proof must verify: {e:?}"));
            assert_eq!(root, live_root);
        }
    }
}

/// Any deterministic callback is honored, not just Drive's: one that
/// appends a byte to the flags of every update.
#[test]
fn mutating_callback_on_a_referenced_item_keeps_referrers_bound() {
    let grove_version = GroveVersion::latest();
    let db = db_with_flagged_chain(grove_version);
    db.apply_batch_with_element_flags_update(
        vec![replace_value_op(b"updated", 1)],
        None,
        |_cost, _old_flags, new_flags| {
            new_flags.push(7);
            Ok(true)
        },
        |_flags, _removed_key_bytes, _removed_value_bytes| {
            Ok((
                StorageRemovedBytes::NoStorageRemoval,
                StorageRemovedBytes::NoStorageRemoval,
            ))
        },
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    let stored = db
        .get_raw([TEST_LEAF].as_ref().into(), b"value", None, grove_version)
        .unwrap()
        .unwrap();
    let mut expected_flags = epoch_flags(1).unwrap();
    expected_flags.push(7);
    assert_eq!(stored.get_flags(), &Some(expected_flags));
    assert_referrers_bound(
        &db,
        &[TEST_LEAF],
        &[b"r1", b"r2"],
        b"updated",
        grove_version,
    );
}

/// A batch that only registers a new referrer still rewrites the target,
/// and a callback may change the flags of that write too. Re-propagating the
/// landed hash also drops a dangling referrer entry from the target, which
/// changes its size and so its landed flags once more: the settling pass
/// must go round again until the committed hash holds.
#[test]
fn settling_resettles_after_dropping_a_dangling_referrer_entry() {
    let grove_version = GroveVersion::latest();
    let db = db_with_flagged_chain(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"origins",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"origins"],
        b"origin",
        Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"value".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    // Clearing without bookkeeping leaves `origin`'s entry on `value`.
    db.clear_subtree(
        &[TEST_LEAF, b"origins"],
        Some(ClearOptions {
            backwards_references: BackwardsReferences::DontCheck,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap();
    // Public reads strip the referrer list; it lives on the stored element.
    let registered = |db: &TempGroveDb| {
        use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(
                SubtreePath::from(&[TEST_LEAF]),
                &tx,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        Element::get(&merk, b"value", true, grove_version)
            .unwrap()
            .unwrap()
            .backward_references()
            .map(|refs| refs.len())
            .unwrap()
    };
    assert_eq!(registered(&db), 2, "r1 and the dangling origin");

    // Stamps the measured storage change into the flags: the landed bytes
    // follow every size change of the target.
    db.apply_batch_with_element_flags_update(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"n1".to_vec(),
            sibling_bidi(b"value"),
        )],
        None,
        |cost, _old_flags, new_flags| {
            let removed = match cost.removed_bytes {
                BasicStorageRemoval(bytes) => bytes,
                _ => 0,
            };
            let mut stamp = cost.added_bytes.to_be_bytes().to_vec();
            stamp.extend(removed.to_be_bytes());
            stamp.extend(cost.replaced_bytes.to_be_bytes());
            *new_flags = stamp;
            Ok(true)
        },
        |_flags, removed_key_bytes, removed_value_bytes| {
            Ok((
                BasicStorageRemoval(removed_key_bytes),
                BasicStorageRemoval(removed_value_bytes),
            ))
        },
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        registered(&db),
        2,
        "the dangling entry is dropped, n1 added"
    );
    assert_ne!(
        db.get_raw([TEST_LEAF].as_ref().into(), b"value", None, grove_version)
            .unwrap()
            .unwrap()
            .get_flags(),
        &epoch_flags(1),
        "the registration write's flags were rewritten"
    );
    assert_referrers_bound(
        &db,
        &[TEST_LEAF],
        &[b"r1", b"r2", b"n1"],
        b"hello",
        grove_version,
    );
}

/// A failing callback fails the batch cleanly, whether the planner or the
/// apply consults it first, and leaves the grove untouched.
#[test]
fn failing_callback_rejects_the_batch() {
    let grove_version = GroveVersion::latest();
    let db = db_with_flagged_chain(grove_version);
    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();
    let result = db
        .apply_batch_with_element_flags_update(
            vec![replace_value_op(b"updated", 3)],
            None,
            |_cost, _old_flags, _new_flags| {
                Err(Error::JustInTimeElementFlagsClientError(
                    "refused".to_owned(),
                ))
            },
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((
                    StorageRemovedBytes::NoStorageRemoval,
                    StorageRemovedBytes::NoStorageRemoval,
                ))
            },
            None,
            grove_version,
        )
        .unwrap();
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        db.root_hash(None, grove_version).unwrap().unwrap(),
        root_before
    );
    assert_referrers_bound(&db, &[TEST_LEAF], &[b"r1", b"r2"], b"hello", grove_version);
}
