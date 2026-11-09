//! A batch that hands an element to the owner its new flags name, either way
//! a writer can ask for:
//!
//! - `StorageFlags::update_element_flags_with_written_owner`: the bytes are
//!   accounted as for any other update, and the ones that stay stored go with
//!   the element.
//! - `StorageFlags::update_element_flags_replacing_on_owner_change`: the old
//!   owner is refunded for every byte and every byte of the new element is
//!   added.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;

use grovedb::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    reference_path::ReferencePathType,
    Element, ElementFlags, Error, GroveDb,
};
use grovedb_costs::{
    storage_cost::{
        removal::StorageRemovedBytes::{self, NoStorageRemoval, SectionedStorageRemoval},
        StorageCost,
    },
    OperationCost,
};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_version::version::GroveVersion;
use tempfile::TempDir;

const OLD_OWNER: [u8; 32] = [1; 32];
const NEW_OWNER: [u8; 32] = [2; 32];
const KEY: &[u8] = b"key1";

fn flags_update(
    cost: &StorageCost,
    old_flags: Option<ElementFlags>,
    new_flags: &mut ElementFlags,
) -> Result<bool, Error> {
    StorageFlags::update_element_flags_with_written_owner(cost, old_flags, new_flags)
        .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
}

fn split_removal_bytes(
    flags: &mut ElementFlags,
    removed_key_bytes: u32,
    removed_value_bytes: u32,
) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
    StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
        .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
}

fn apply(
    db: &GroveDb,
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> Result<OperationCost, Error> {
    db.apply_batch_with_element_flags_update(
        ops,
        None,
        flags_update,
        split_removal_bytes,
        None,
        grove_version,
    )
    .cost_as_result()
}

fn item(len: usize, byte: u8, flags: &StorageFlags) -> Element {
    Element::new_item_with_flags(vec![byte; len], flags.to_some_element_flags())
}

fn write_op(element: Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(vec![b"tree".to_vec()], KEY.to_vec(), element)
}

/// A grove holding `old` at `[tree]/key1`, and an empty `[index]/value`.
fn grove_with(old: Element, grove_version: &GroveVersion) -> (TempDir, GroveDb) {
    let dir = TempDir::new().expect("expected a temporary directory");
    let db = GroveDb::open(dir.path()).expect("expected to open the grove");
    let insert = |path: Vec<Vec<u8>>, key: &[u8], element: Element| {
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                path,
                key.to_vec(),
                element,
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to set up the grove");
    };
    insert(vec![], b"tree", Element::empty_tree());
    insert(vec![], b"index", Element::empty_tree());
    insert(vec![b"index".to_vec()], b"value", Element::empty_tree());
    insert(vec![b"tree".to_vec()], KEY, old);
    (dir, db)
}

fn issues(db: &GroveDb, grove_version: &GroveVersion) -> usize {
    db.visualize_verify_grovedb(None, true, false, grove_version)
        .expect("expected to verify the grove")
        .len()
}

fn stored_flags(db: &GroveDb, grove_version: &GroveVersion) -> StorageFlags {
    let element = db
        .get([b"tree".as_slice()].as_ref(), KEY, None, grove_version)
        .unwrap()
        .expect("expected the item");
    StorageFlags::from_element_flags_ref(element.get_flags().as_ref().expect("expected flags"))
        .expect("expected flags that decode")
        .expect("expected flags")
}

/// The epochs and bytes `removed` refunds, for one identity.
fn refunds(removed: &StorageRemovedBytes) -> BTreeMap<[u8; 32], BTreeMap<u16, u32>> {
    match removed {
        NoStorageRemoval => BTreeMap::new(),
        SectionedStorageRemoval(by_identifier) => by_identifier
            .iter()
            .map(|(identifier, epochs)| {
                (
                    *identifier,
                    epochs
                        .iter()
                        .map(|(epoch, bytes)| (epoch, *bytes))
                        .collect(),
                )
            })
            .collect(),
        other => panic!("expected sectioned removal, got {other:?}"),
    }
}

/// A's item is rewritten in epoch 2 by someone who pays for the write. The
/// flags it writes name either A, who keeps the item, or B, who takes it. The
/// figures do not depend on which: what changes is who is refunded later.
#[test]
fn should_account_the_same_bytes_whoever_the_new_flags_name_as_owner() {
    let grove_version = GroveVersion::latest();
    let single_epoch = StorageFlags::SingleEpochOwned(0, OLD_OWNER);
    let two_epochs = StorageFlags::MultiEpochOwned(0, BTreeMap::from([(1, 40)]), OLD_OWNER);
    // (label, old flags, length before, length after, added bytes, the bytes
    // refunded to the old owner by epoch, the epochs left in the flags)
    let cases = [
        (
            "30 bytes larger",
            single_epoch.clone(),
            20,
            50,
            34,
            BTreeMap::new(),
            Some(BTreeMap::from([(2, 34)])),
        ),
        (
            "30 bytes smaller",
            single_epoch.clone(),
            60,
            30,
            0,
            BTreeMap::from([(0, 30)]),
            None,
        ),
        (
            "the same size",
            single_epoch,
            20,
            20,
            0,
            BTreeMap::new(),
            None,
        ),
        (
            "30 bytes smaller, across epochs",
            two_epochs,
            60,
            30,
            0,
            BTreeMap::from([(1, 30)]),
            Some(BTreeMap::from([(1, 10)])),
        ),
    ];
    for (label, old, before, after, added_bytes, refunded, epochs) in cases {
        for owner in [OLD_OWNER, NEW_OWNER] {
            let (_dir, db) = grove_with(item(before, 7, &old), grove_version);
            let written = StorageFlags::SingleEpochOwned(2, owner);
            let cost = apply(&db, vec![write_op(item(after, 8, &written))], grove_version)
                .unwrap_or_else(|e| panic!("{label}: expected to apply: {e}"));

            assert_eq!(cost.storage_cost.added_bytes, added_bytes, "{label}");
            let expected_refunds = if refunded.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([(OLD_OWNER, refunded.clone())])
            };
            assert_eq!(
                refunds(&cost.storage_cost.removed_bytes),
                expected_refunds,
                "{label}: bytes the update frees go to the old owner"
            );

            let flags = stored_flags(&db, grove_version);
            assert_eq!(flags.owner_id(), Some(&owner), "{label}");
            assert_eq!(flags.epoch_index_map(), epochs.as_ref(), "{label}");
            assert_eq!(issues(&db, grove_version), 0, "{label}");

            let deleted = apply(
                &db,
                vec![QualifiedGroveDbOp::delete_op(
                    vec![b"tree".to_vec()],
                    KEY.to_vec(),
                )],
                grove_version,
            )
            .unwrap_or_else(|e| panic!("{label}: expected to delete: {e}"));
            assert_eq!(
                refunds(&deleted.storage_cost.removed_bytes)
                    .into_keys()
                    .collect::<Vec<_>>(),
                vec![owner],
                "{label}: the bytes still stored go to the owner afterwards"
            );
        }
    }
}

/// A's item is handed to B by a write that answers
/// `AccountAsReplacement`: A gets back everything deleting the item would
/// refund, B pays for every byte of what is written, and the flags are what
/// the writer wrote, in the writer's epoch only.
#[test]
fn should_refund_the_old_owner_in_full_and_charge_the_new_owner_for_everything() {
    let grove_version = GroveVersion::latest();
    let old = StorageFlags::MultiEpochOwned(0, BTreeMap::from([(1, 40)]), OLD_OWNER);
    let written = StorageFlags::SingleEpochOwned(2, NEW_OWNER);
    let delete = || QualifiedGroveDbOp::delete_op(vec![b"tree".to_vec()], KEY.to_vec());
    for (before, after) in [(60, 90), (60, 30), (60, 60)] {
        let label = format!("{before} to {after} bytes");
        let (_dir, db) = grove_with(item(before, 7, &old), grove_version);
        let deletion = apply(&db, vec![delete()], grove_version)
            .unwrap_or_else(|e| panic!("{label}: expected to delete: {e}"));
        let everything_stored = refunds(&deletion.storage_cost.removed_bytes);
        assert_eq!(
            everything_stored.keys().collect::<Vec<_>>(),
            vec![&OLD_OWNER],
            "{label}"
        );

        let (_dir, db) = grove_with(item(before, 7, &old), grove_version);
        let new_element = item(after, 8, &written);
        let cost = db
            .apply_batch_with_element_flags_update(
                vec![write_op(new_element.clone())],
                Some(BatchApplyOptions {
                    allow_replacement_accounting: true,
                    ..Default::default()
                }),
                |cost, old_flags, new_flags| {
                    StorageFlags::update_element_flags_replacing_on_owner_change(
                        cost, old_flags, new_flags,
                    )
                    .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
                },
                split_removal_bytes,
                None,
                grove_version,
            )
            .cost_as_result()
            .unwrap_or_else(|e| panic!("{label}: expected to apply: {e}"));

        assert_eq!(
            refunds(&cost.storage_cost.removed_bytes),
            everything_stored,
            "{label}: the old owner is refunded for every byte"
        );
        assert_eq!(stored_flags(&db, grove_version), written, "{label}");
        assert_eq!(issues(&db, grove_version), 0, "{label}");

        let new_deletion = apply(&db, vec![delete()], grove_version)
            .unwrap_or_else(|e| panic!("{label}: expected to delete: {e}"));
        let new_refunds = refunds(&new_deletion.storage_cost.removed_bytes);
        assert_eq!(
            new_refunds.keys().collect::<Vec<_>>(),
            vec![&NEW_OWNER],
            "{label}"
        );
        assert_eq!(
            new_refunds[&NEW_OWNER].values().sum::<u32>(),
            cost.storage_cost.added_bytes,
            "{label}: every byte of the new element was added"
        );
    }
}

/// An unowned item gaining an owner grows its flags, so the update is
/// measured again, whatever it first looked like. It settles and the commit
/// matches what it stores.
#[test]
fn should_give_an_unowned_item_the_owner_the_new_flags_name() {
    let grove_version = GroveVersion::latest();
    let old = StorageFlags::SingleEpoch(0);
    let written = StorageFlags::SingleEpochOwned(2, NEW_OWNER);
    for (before, after) in [(20, 50), (60, 59), (60, 30), (60, 10), (20, 20)] {
        let (_dir, db) = grove_with(item(before, 7, &old), grove_version);
        apply(&db, vec![write_op(item(after, 8, &written))], grove_version)
            .unwrap_or_else(|e| panic!("{before} to {after} bytes: expected to apply: {e}"));
        assert_eq!(
            stored_flags(&db, grove_version).owner_id(),
            Some(&NEW_OWNER),
            "{before} to {after} bytes"
        );
        assert_eq!(issues(&db, grove_version), 0, "{before} to {after} bytes");
    }
}

/// A reference written in the same batch commits to the flags the update
/// stores, new owner included. The deeper reference resolves before its
/// target is written, so it goes through the prediction.
#[test]
fn should_commit_a_same_batch_reference_to_the_owner_the_new_flags_name() {
    let grove_version = GroveVersion::latest();
    let old = StorageFlags::SingleEpochOwned(0, OLD_OWNER);
    let written = StorageFlags::SingleEpochOwned(2, NEW_OWNER);
    for (before, after) in [(20, 50), (60, 30), (20, 20)] {
        let (_dir, db) = grove_with(item(before, 7, &old), grove_version);
        let reference = QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"index".to_vec(), b"value".to_vec()],
            b"reference".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"tree".to_vec(),
                KEY.to_vec(),
            ])),
        );
        apply(
            &db,
            vec![write_op(item(after, 8, &written)), reference],
            grove_version,
        )
        .unwrap_or_else(|e| panic!("{before} to {after} bytes: expected to apply: {e}"));
        assert_eq!(
            stored_flags(&db, grove_version).owner_id(),
            Some(&NEW_OWNER),
            "{before} to {after} bytes"
        );
        assert_eq!(issues(&db, grove_version), 0, "{before} to {after} bytes");
    }
}
