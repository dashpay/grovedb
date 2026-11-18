use super::*;
use crate::{
    storage_cost::{
        removal::{
            with_basic_sectioned_removal_addition_version, StorageRemovalPerEpochByIdentifier,
            StorageRemovedBytes,
            StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval, SectionedStorageRemoval},
            UNKNOWN_EPOCH,
        },
        StorageCost,
    },
    TreeCostType,
};

const MODEL: StorageCostModel = StorageCostModel::V0;

fn storage(added_bytes: u32, replaced_bytes: u32, removed: u32) -> StorageCost {
    StorageCost {
        added_bytes,
        replaced_bytes,
        removed_bytes: if removed == 0 {
            NoStorageRemoval
        } else {
            BasicStorageRemoval(removed)
        },
    }
}

fn info(key: StorageCost, value: StorageCost, new_node: bool, verify: bool) -> KeyValueStorageCost {
    KeyValueStorageCost {
        key_storage_cost: key,
        value_storage_cost: value,
        new_node,
        needs_value_verification: verify,
        prepaid: false,
    }
}

fn seeks(seek_count: u32) -> OperationCost {
    OperationCost::with_seek_count(seek_count)
}

fn loaded(storage_loaded_bytes: u64) -> OperationCost {
    OperationCost::with_storage_loaded_bytes(storage_loaded_bytes)
}

fn hashes(hash_node_calls: u32) -> OperationCost {
    OperationCost::with_hash_node_calls(hash_node_calls)
}

fn stored(storage_cost: StorageCost) -> OperationCost {
    OperationCost {
        storage_cost,
        ..Default::default()
    }
}

fn sectioned(owner: u8, sections: &[(u16, u32)]) -> StorageRemovedBytes {
    let mut by_owner = StorageRemovalPerEpochByIdentifier::default();
    let epochs = by_owner.entry([owner; 32]).or_default();
    for (epoch, bytes) in sections {
        epochs.insert(*epoch, *bytes);
    }
    SectionedStorageRemoval(by_owner)
}

fn removal(removed_bytes: StorageRemovedBytes) -> KeyValueStorageCost {
    KeyValueStorageCost {
        key_storage_cost: StorageCost {
            removed_bytes,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn a_stored_key_is_the_prefix_and_the_key() {
    assert_eq!(PREFIX_LEN, 32);
    assert_eq!(MODEL.stored_key_len(0), 32);
    assert_eq!(MODEL.stored_key_len(4), 36);
    assert_eq!(MODEL.stored_key_len(255), 287);
}

#[test]
fn the_record_length_of_a_node_is_a_sum_of_its_sizes() {
    assert_eq!(MODEL.node_record_len(1, 0, None, None), 68);
    assert_eq!(
        MODEL.node_record_len(10, 3, Some((4 + 36, 0)), Some((32 + 36, 17))),
        (1 + 40) + (1 + 68 + 17) + (1 + 3) + 64 + 10
    );
    assert_eq!(
        MODEL.node_record_len(0, 0, None, Some((36, 0))),
        1 + (1 + 36) + 1 + 64
    );
}

fn cost_types_with_their_class() -> [(Option<TreeCostType>, u32); 5] {
    [
        (None, 0),
        (Some(TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes), 8),
        (
            Some(TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes),
            16,
        ),
        (Some(TreeCostType::TreeFeatureUses16Bytes), 16),
        (
            Some(TreeCostType::TreeFeatureUsesVarIntCostAs8BytesAnd16Bytes),
            24,
        ),
    ]
}

#[test]
fn the_links_of_a_node_cancel_out_of_what_its_value_is_charged() {
    let links = [None, Some((36, 0)), Some((4 + 36, 3)), Some((255 + 36, 26))];
    for payload_len in [1, 10, 26] {
        for left in links {
            for right in links {
                for value_len in [0usize, 1, 60, 300] {
                    for (cost_type, class) in cost_types_with_their_class() {
                        let feature = cost_type.map(|cost_type| (cost_type, payload_len));
                        let feature_len = feature.as_ref().map_or(0, |(_, len)| *len);
                        let record_len = MODEL.node_record_len(value_len, feature_len, left, right);
                        let mut charge = MODEL.batch();
                        charge
                            .put(
                                Keyspace::Data,
                                36,
                                record_len,
                                Some((feature, left, right)),
                                None,
                            )
                            .expect("a put with no cost is not checked");
                        let body = 65 + class + value_len as u32;
                        let body_varint = if body < 128 { 1 } else { 2 };
                        assert_eq!(
                            charge.finish(true),
                            OperationCost {
                                seek_count: 1,
                                storage_cost: storage(
                                    37 + body + body_varint + 36 + 4 + class,
                                    0,
                                    0
                                ),
                                ..Default::default()
                            },
                            "{class} {feature_len} {left:?} {right:?} {value_len}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_prefix_is_charged_by_the_blocks_of_its_message() {
    assert_eq!(MODEL.open_by_path([]), OperationCost::default());
    assert_eq!(MODEL.open_by_path([0]), hashes(1));
    assert_eq!(MODEL.open_by_path([1]), hashes(1));
    assert_eq!(MODEL.open_by_path([64 - 8 - 1]), hashes(1));
    assert_eq!(MODEL.open_by_path([64 - 8]), hashes(2));
    assert_eq!(MODEL.open_by_path([3, 4]), hashes(1));
    assert_eq!(MODEL.open_by_path([255, 255]), hashes(9));
    assert_eq!(MODEL.open_by_path([255; 8]), hashes(33));
    assert_eq!(MODEL.open_by_path([255; 9]), hashes(37));
    assert_eq!(MODEL.open_by_prefix(), OperationCost::default());
    assert_eq!(MODEL.secondary_prefix(), hashes(1));
}

#[test]
fn a_read_is_a_seek_and_the_record() {
    assert_eq!(
        MODEL.read(Some(70)),
        OperationCost {
            seek_count: 1,
            storage_loaded_bytes: 70,
            ..Default::default()
        }
    );
    assert_eq!(MODEL.read(Some(0)), seeks(1));
    assert_eq!(MODEL.read(None), seeks(1));
    assert_eq!(MODEL.read_immediate(), OperationCost::default());
}

#[test]
fn what_is_charged_elsewhere_or_not_at_all_charges_nothing() {
    assert_eq!(MODEL.staged_write(), OperationCost::default());
    assert_eq!(MODEL.refused_write(), OperationCost::default());
    assert_eq!(MODEL.write_immediate(), OperationCost::default());
    assert_eq!(MODEL.transaction_commit(), OperationCost::default());
}

#[test]
fn a_new_data_record_gets_its_stored_key_added() {
    let update = info(storage(0, 0, 0), storage(0, 5, 0), false, false);
    assert_eq!(
        MODEL.complete_new_key_cost(Some(update.clone()), 36),
        Some(update)
    );
    let new = info(storage(0, 0, 0), storage(5, 0, 0), true, false);
    let completed = MODEL
        .complete_new_key_cost(Some(new.clone()), 36)
        .expect("a cost");
    assert_eq!(completed.key_storage_cost, storage(37, 0, 0));
    assert_eq!(completed.value_storage_cost, new.value_storage_cost);
    let completed = MODEL
        .complete_new_key_cost(Some(new.clone()), 232)
        .expect("a cost");
    assert_eq!(completed.key_storage_cost, storage(232 + 2, 0, 0));
    let partly_given = info(storage(3, 1, 0), storage(5, 0, 0), true, false);
    let completed = MODEL
        .complete_new_key_cost(Some(partly_given), 36)
        .expect("a cost");
    assert_eq!(completed.key_storage_cost, storage(40, 1, 0));
    assert_eq!(MODEL.complete_new_key_cost(None, 36), None);
    assert_eq!(
        MODEL.complete_new_key_cost(Some(KeyValueStorageCost::prepaid()), 36),
        Some(KeyValueStorageCost::prepaid())
    );
}

#[test]
fn a_put_with_no_cost_adds_its_key_and_value() {
    for keyspace in [Keyspace::Data, Keyspace::Aux, Keyspace::Meta] {
        let mut charge = MODEL.batch();
        charge.put(keyspace, 35, 10, None, None).expect("charged");
        let (at_once, on_success) = charge.clone().into_parts();
        assert_eq!(at_once, seeks(1));
        assert_eq!(on_success, stored(storage(35 + 1 + 10 + 1, 0, 0)));
        assert_eq!(
            charge.finish(true),
            OperationCost {
                seek_count: 1,
                storage_cost: storage(47, 0, 0),
                ..Default::default()
            }
        );
    }
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Data, 36, 128, None, None)
        .expect("charged");
    assert_eq!(charge.into_parts().1, stored(storage(37 + 128 + 2, 0, 0)));
}

#[test]
fn the_sizes_of_a_node_count_in_the_data_keyspace_only() {
    let leaf = || Some((None, None, None));
    let mut node = MODEL.batch();
    node.put(Keyspace::Data, 33, 68, leaf(), None)
        .expect("charged");
    assert_eq!(
        node.into_parts().1,
        stored(storage(34 + 66 + 1 + 33 + 4, 0, 0))
    );
    for keyspace in [Keyspace::Aux, Keyspace::Meta] {
        let mut flat = MODEL.batch();
        flat.put(keyspace, 33, 68, leaf(), None).expect("charged");
        assert_eq!(flat.into_parts().1, stored(storage(34 + 68 + 1, 0, 0)));
    }
}

#[test]
fn a_put_takes_the_writers_cost_after_checking_it() {
    let key = storage(36, 0, 0);
    let given = info(key.clone(), storage(4, 7, 2), true, false);
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Data, 35, 500, None, Some(given))
        .expect("a value cost taken as given");
    assert_eq!(charge.into_parts().1, stored(storage(40, 7, 2)));

    let verified = info(key.clone(), storage(4, 7, 0), true, true);
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Data, 35, 10, None, Some(verified))
        .expect("a value cost that matches");
    assert_eq!(charge.into_parts().1, stored(storage(40, 7, 0)));

    let update = info(storage(0, 0, 0), storage(4, 7, 0), false, true);
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Data, 35, 10, None, Some(update))
        .expect("the key of an update is not checked");
    assert_eq!(charge.into_parts().1, stored(storage(4, 7, 0)));

    let wrong_value = info(key, storage(4, 8, 0), true, true);
    let mut charge = MODEL.batch();
    let error = charge
        .put(Keyspace::Data, 35, 10, None, Some(wrong_value))
        .expect_err("a value cost that does not match");
    assert!(matches!(
        error,
        Error::StorageCostMismatch {
            actual_total_bytes: 11,
            ..
        }
    ));
    assert_eq!(
        charge.clone().into_parts(),
        (seeks(1), stored(storage(0, 0, 0)))
    );
    assert_eq!(charge.finish(false), seeks(1));

    let wrong_key = info(storage(35, 0, 0), storage(4, 7, 0), true, true);
    let mut charge = MODEL.batch();
    let error = charge
        .put(Keyspace::Data, 35, 10, None, Some(wrong_key))
        .expect_err("a key cost that does not match");
    assert!(matches!(
        error,
        Error::StorageCostMismatch {
            actual_total_bytes: 36,
            ..
        }
    ));
    assert_eq!(charge.finish(false), seeks(1));
}

#[test]
fn a_root_key_put_is_charged_only_with_a_cost() {
    let mut free = MODEL.batch();
    free.put(Keyspace::Roots, 33, 5, None, None)
        .expect("charged");
    assert_eq!(free.finish(true), seeks(1));

    let given = KeyValueStorageCost::for_updated_root_cost(None, 5);
    let mut charged = MODEL.batch();
    charged
        .put(Keyspace::Roots, 33, 5, None, Some(given))
        .expect("charged");
    assert_eq!(
        charged.finish(true),
        OperationCost {
            seek_count: 1,
            storage_cost: storage(34 + 6, 0, 0),
            ..Default::default()
        }
    );

    let wrong = info(storage(1, 0, 0), storage(6, 0, 0), true, true);
    let mut refused = MODEL.batch();
    refused
        .put(Keyspace::Roots, 33, 5, None, Some(wrong))
        .expect_err("a key cost that does not match");
    assert_eq!(refused.finish(false), seeks(1));
}

#[test]
fn a_prepaid_put_is_free() {
    for keyspace in [
        Keyspace::Data,
        Keyspace::Aux,
        Keyspace::Roots,
        Keyspace::Meta,
    ] {
        let mut charge = MODEL.batch();
        charge
            .put(
                keyspace,
                35,
                10,
                Some((None, None, None)),
                Some(KeyValueStorageCost::prepaid()),
            )
            .expect("charged");
        assert_eq!(charge.finish(true), OperationCost::default());

        let mut immediate = MODEL.immediate_batch();
        immediate
            .put(keyspace, 35, 10, None, Some(KeyValueStorageCost::prepaid()))
            .expect("charged");
        assert_eq!(immediate.finish(), OperationCost::default());
    }
    let mut with_bytes = KeyValueStorageCost::prepaid();
    with_bytes.value_storage_cost.replaced_bytes = 9;
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Aux, 35, 10, None, Some(with_bytes))
        .expect("charged");
    assert_eq!(charge.finish(true), stored(storage(0, 9, 0)));
}

#[test]
fn a_delete_is_charged_from_its_cost_or_the_committed_record() {
    let given = info(storage(0, 0, 37), storage(0, 0, 80), false, false);
    let mut charge = MODEL.batch();
    charge.delete(given);
    assert_eq!(charge.into_parts(), (seeks(1), stored(storage(0, 0, 117))));

    let mut charge = MODEL.batch();
    charge.delete(KeyValueStorageCost::default());
    assert_eq!(charge.finish(true), seeks(1));

    let mut committed = MODEL.batch();
    committed
        .delete_uncosted(36, Ok::<_, ()>(Some(200)))
        .expect("read");
    assert_eq!(
        committed.into_parts(),
        (
            OperationCost {
                seek_count: 2,
                storage_loaded_bytes: 200,
                ..Default::default()
            },
            stored(storage(0, 0, 36 + 200 + 1 + 2))
        )
    );

    let mut absent = MODEL.batch();
    absent.delete_uncosted(36, Ok::<_, ()>(None)).expect("read");
    assert_eq!(
        absent.into_parts(),
        (seeks(2), stored(storage(0, 0, 36 + 1 + 1)))
    );

    let mut empty = MODEL.batch();
    empty
        .delete_uncosted(36, Ok::<_, ()>(Some(0)))
        .expect("read");
    assert_eq!(
        empty.into_parts(),
        (seeks(2), stored(storage(0, 0, 36 + 1 + 1)))
    );
}

#[test]
fn a_committed_read_that_fails_fails_the_batch_and_keeps_its_seeks() {
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Meta, 33, 4, None, None)
        .expect("charged");
    assert_eq!(
        charge.delete_uncosted(36, Err::<Option<usize>, _>("unreadable")),
        Err("unreadable")
    );
    assert_eq!(
        charge.clone().into_parts(),
        (seeks(3), stored(storage(34 + 5, 0, 0)))
    );
    assert_eq!(charge.finish(false), seeks(3));
}

#[test]
fn the_storage_cost_of_a_batch_is_owed_once_its_writes_are_taken() {
    let on_success = OperationCost::with_storage_written_bytes(9);
    assert_eq!(MODEL.batch_written(on_success.clone(), true), on_success);
    assert_eq!(
        MODEL.batch_written(on_success, false),
        OperationCost::default()
    );

    let fill = || {
        let mut charge = MODEL.batch();
        charge
            .put(Keyspace::Aux, 35, 10, None, None)
            .expect("charged");
        charge
            .delete_uncosted(36, Ok::<_, ()>(Some(5)))
            .expect("read");
        charge.delete(removal(BasicStorageRemoval(4)));
        charge
    };
    assert_eq!(
        fill().finish(true),
        OperationCost {
            seek_count: 4,
            storage_loaded_bytes: 5,
            storage_cost: storage(47, 0, 36 + 5 + 1 + 1 + 4),
            ..Default::default()
        }
    );
    assert_eq!(
        fill().finish(false),
        OperationCost {
            seek_count: 4,
            storage_loaded_bytes: 5,
            ..Default::default()
        }
    );
    let (at_once, on_success) = fill().into_parts();
    let mut joined = MODEL.batch_written(on_success, true);
    joined += at_once;
    assert_eq!(joined, fill().finish(true));
}

#[test]
fn a_batch_is_charged_meta_first_and_data_last() {
    assert_eq!(
        MODEL.batch_order(),
        [
            Keyspace::Meta,
            Keyspace::Aux,
            Keyspace::Roots,
            Keyspace::Data
        ]
    );
}

#[test]
fn the_order_of_a_batch_shows_in_its_removed_bytes() {
    let plain_then_sectioned = || {
        let mut charge = MODEL.batch();
        charge.delete_uncosted(33, Ok::<_, ()>(None)).expect("read");
        charge.delete(removal(sectioned(0, &[(3, 50)])));
        charge.finish(true).storage_cost.removed_bytes
    };
    let sectioned_then_plain = || {
        let mut charge = MODEL.batch();
        charge.delete(removal(sectioned(0, &[(3, 50)])));
        charge.delete_uncosted(33, Ok::<_, ()>(None)).expect("read");
        charge.finish(true).storage_cost.removed_bytes
    };
    assert_eq!(
        plain_then_sectioned(),
        SectionedStorageRemoval(Default::default())
    );
    assert_eq!(
        sectioned_then_plain(),
        sectioned(0, &[(3, 50), (UNKNOWN_EPOCH, 35)])
    );
    with_basic_sectioned_removal_addition_version(1, || {
        assert_eq!(
            plain_then_sectioned(),
            sectioned(0, &[(3, 50), (UNKNOWN_EPOCH, 35)])
        );
        assert_eq!(plain_then_sectioned(), sectioned_then_plain());
    });
}

#[test]
fn the_key_of_a_put_is_charged_before_its_value() {
    let given = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            removed_bytes: BasicStorageRemoval(5),
            ..Default::default()
        },
        value_storage_cost: StorageCost {
            removed_bytes: sectioned(0, &[(3, 50)]),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut charge = MODEL.batch();
    charge
        .put(Keyspace::Aux, 35, 10, None, Some(given.clone()))
        .expect("a cost taken as given");
    assert_eq!(
        charge.finish(true).storage_cost.removed_bytes,
        SectionedStorageRemoval(Default::default())
    );

    let mut immediate = MODEL.immediate_batch();
    immediate
        .put(Keyspace::Aux, 35, 10, None, Some(given))
        .expect("a cost taken as given");
    assert_eq!(
        immediate.finish().storage_cost.removed_bytes,
        SectionedStorageRemoval(Default::default())
    );
}

#[test]
fn a_charge_meets_the_running_cost_as_the_parts_were_added_before_the_model() {
    let forms = || {
        [
            NoStorageRemoval,
            BasicStorageRemoval(5),
            sectioned(0, &[(0, 7)]),
            sectioned(9, &[(0, 11)]),
        ]
    };
    let removed = |removed_bytes: &StorageRemovedBytes| StorageCost {
        added_bytes: 1,
        replaced_bytes: 2,
        removed_bytes: removed_bytes.clone(),
    };
    for arithmetic in [0, 1] {
        with_basic_sectioned_removal_addition_version(arithmetic, || {
            for first in forms() {
                for key in forms() {
                    for value in forms() {
                        let case = format!("{arithmetic}: {first:?} + {key:?} + {value:?}");
                        let given = info(removed(&key), removed(&value), false, false);

                        let mut before = OperationCost::default();
                        before.storage_cost.removed_bytes += first.clone();
                        before
                            .add_key_value_storage_costs(35, 10, None, Some(given.clone()))
                            .expect("a cost taken as given");
                        before.storage_cost.removed_bytes += given.clone().combined_removed_bytes();

                        let mut charge = MODEL.batch();
                        charge.delete(removal(first.clone()));
                        charge
                            .put(Keyspace::Aux, 35, 10, None, Some(given.clone()))
                            .expect("a cost taken as given");
                        charge.delete(given.clone());
                        assert_eq!(
                            charge.finish(true).storage_cost,
                            before.storage_cost,
                            "{case}"
                        );

                        let mut immediate = MODEL.immediate_batch();
                        immediate.delete(Some(removal(first.clone())));
                        immediate
                            .put(Keyspace::Aux, 35, 10, None, Some(given.clone()))
                            .expect("a cost taken as given");
                        immediate.delete(Some(given));
                        assert_eq!(
                            immediate.finish().storage_cost,
                            before.storage_cost,
                            "{case}"
                        );
                    }
                }
            }
        });
    }
}

#[test]
fn the_batch_of_an_immediate_handle_is_charged_as_it_is_filled() {
    let mut charge = MODEL.immediate_batch();
    charge
        .put(Keyspace::Aux, 35, 10, None, None)
        .expect("charged");
    assert_eq!(
        charge.clone().finish(),
        OperationCost {
            seek_count: 1,
            storage_cost: storage(47, 0, 0),
            ..Default::default()
        }
    );
    charge
        .put(Keyspace::Roots, 33, 5, None, None)
        .expect("a free root key");
    charge
        .put(
            Keyspace::Data,
            33,
            68,
            Some((None, None, None)),
            Some(info(storage(34, 0, 0), storage(104, 0, 0), true, true)),
        )
        .expect("a node with its cost");
    let wrong = info(storage(0, 0, 0), storage(1, 0, 0), false, true);
    charge
        .put(Keyspace::Aux, 35, 10, None, Some(wrong))
        .expect_err("a cost that does not match");
    charge.delete(None);
    charge.delete(Some(info(storage(0, 0, 3), storage(0, 0, 4), false, false)));
    assert_eq!(
        charge.finish(),
        OperationCost {
            seek_count: 6,
            storage_cost: storage(47 + 34 + 104, 0, 7),
            ..Default::default()
        }
    );
}

#[test]
fn a_cursor_charges_moves_keys_and_values() {
    assert_eq!(MODEL.cursor_move(), seeks(1));
    assert_eq!(MODEL.cursor_key(Some(37)), loaded(37));
    assert_eq!(MODEL.cursor_key(Some(32)), loaded(32));
    assert_eq!(MODEL.cursor_key(None), loaded(288));
    assert_eq!(MODEL.cursor_value(Some((37, 100))), loaded(137));
    assert_eq!(MODEL.cursor_value(Some((37, 0))), loaded(37));
    assert_eq!(MODEL.cursor_value(None), loaded(288));
}

#[test]
fn clearing_a_keyspace_is_charged_as_the_walk_over_its_records() {
    assert_eq!(
        MODEL.clear_walk([]),
        OperationCost {
            seek_count: 1,
            storage_loaded_bytes: 288,
            ..Default::default()
        }
    );
    assert_eq!(
        MODEL.clear_walk([33, 40, 32]),
        OperationCost {
            seek_count: 4,
            storage_loaded_bytes: 2 * (33 + 40 + 32) + 288,
            ..Default::default()
        }
    );
}

#[test]
fn the_model_and_its_charges_can_be_shown_and_compared() {
    assert_eq!(format!("{MODEL:?}"), "V0");
    assert_eq!(format!("{:?}", Keyspace::Roots), "Roots");
    let charge = MODEL.batch();
    assert_eq!(charge, charge.clone());
    assert!(format!("{charge:?}").starts_with("BatchCharge"));
    let immediate = MODEL.immediate_batch();
    assert_eq!(immediate, immediate.clone());
    assert!(format!("{immediate:?}").starts_with("ImmediateBatchCharge"));
}
