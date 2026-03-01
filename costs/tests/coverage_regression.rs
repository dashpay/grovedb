use std::collections::BTreeMap;

use grovedb_costs::{
    context::{CostContext, CostResult, CostsExt},
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_into,
    cost_return_on_error_into_default, cost_return_on_error_into_no_add,
    cost_return_on_error_no_add,
    error::Error,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{Identifier, StorageRemovedBytes, StorageRemovedBytes::*},
        transition::OperationStorageTransitionType,
        StorageCost,
    },
    OperationCost, TreeCostType,
};
use integer_encoding::VarInt;
use intmap::IntMap;

fn sectioned(identifier: Identifier, epoch: u16, bytes: u32) -> StorageRemovedBytes {
    let mut per_identifier = BTreeMap::new();
    let mut per_epoch = IntMap::new();
    per_epoch.insert(epoch, bytes);
    per_identifier.insert(identifier, per_epoch);
    SectionedStorageRemoval(per_identifier)
}

#[test]
fn operation_cost_helpers_and_adds_are_exercised() {
    let default = OperationCost::default();
    assert!(default.is_nothing());

    assert_eq!(OperationCost::with_seek_count(7).seek_count, 7);
    assert_eq!(
        OperationCost::with_storage_written_bytes(8)
            .storage_cost
            .added_bytes,
        8
    );
    assert_eq!(
        OperationCost::with_storage_loaded_bytes(9).storage_loaded_bytes,
        9
    );
    assert_eq!(
        OperationCost::with_storage_freed_bytes(10)
            .storage_cost
            .removed_bytes,
        BasicStorageRemoval(10)
    );
    assert_eq!(OperationCost::with_hash_node_calls(11).hash_node_calls, 11);

    let left = OperationCost {
        seek_count: 1,
        storage_cost: StorageCost {
            added_bytes: 2,
            replaced_bytes: 3,
            removed_bytes: BasicStorageRemoval(4),
        },
        storage_loaded_bytes: 5,
        hash_node_calls: 6,
        sinsemilla_hash_calls: 7,
    };
    let right = OperationCost {
        seek_count: 10,
        storage_cost: StorageCost {
            added_bytes: 20,
            replaced_bytes: 30,
            removed_bytes: BasicStorageRemoval(40),
        },
        storage_loaded_bytes: 50,
        hash_node_calls: 60,
        sinsemilla_hash_calls: 70,
    };

    let added = left.clone() + right.clone();
    assert_eq!(added.seek_count, 11);
    assert_eq!(added.storage_cost.added_bytes, 22);
    assert_eq!(added.storage_cost.replaced_bytes, 33);
    assert_eq!(added.storage_cost.removed_bytes, BasicStorageRemoval(44));
    assert_eq!(added.storage_loaded_bytes, 55);
    assert_eq!(added.hash_node_calls, 66);
    assert_eq!(added.sinsemilla_hash_calls, 77);

    let mut assigned = left.clone();
    assigned += right.clone();
    assert_eq!(assigned, added);

    assert!(!assigned.worse_or_eq_than(&left));
    assert!(!left.worse_or_eq_than(&assigned));
}

#[test]
fn add_key_value_storage_costs_without_explicit_info_uses_lengths() {
    let mut op_cost = OperationCost::default();
    let key_len = 33;
    let value_len = 75;

    op_cost
        .add_key_value_storage_costs(key_len, value_len, None, None)
        .expect("must succeed");

    let expected_key_paid = key_len + key_len.required_space() as u32;
    let expected_value_paid = value_len + value_len.required_space() as u32;
    assert_eq!(
        op_cost.storage_cost.added_bytes,
        expected_key_paid + expected_value_paid
    );
    assert_eq!(op_cost.storage_cost.replaced_bytes, 0);
}

#[test]
fn add_key_value_storage_costs_handles_tree_cost_variants() {
    for tree_cost_type in [
        TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
        TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
        TreeCostType::TreeFeatureUses16Bytes,
    ] {
        let mut op_cost = OperationCost::default();
        let key_len = 5;
        let value_len = 100;

        let children_sizes = Some((Some((tree_cost_type, 20)), Some((6, 7)), Some((8, 9))));

        op_cost
            .add_key_value_storage_costs(key_len, value_len, children_sizes, None)
            .expect("must succeed");

        // Ensure branch was applied: key + value costs are both added.
        assert!(op_cost.storage_cost.added_bytes > key_len);
    }
}

#[test]
fn add_key_value_storage_costs_respects_verification_flags_and_errors() {
    let key_len = 9;
    let paid_key_len = key_len + key_len.required_space() as u32;

    let mut with_mismatch = OperationCost::default();
    let info_mismatch = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            added_bytes: paid_key_len + 1,
            replaced_bytes: 0,
            removed_bytes: NoStorageRemoval,
        },
        value_storage_cost: StorageCost {
            added_bytes: 3,
            replaced_bytes: 4,
            removed_bytes: NoStorageRemoval,
        },
        new_node: true,
        needs_value_verification: false,
    };

    let err = with_mismatch
        .add_key_value_storage_costs(key_len, 200, None, Some(info_mismatch))
        .expect_err("must return mismatch error");
    match err {
        Error::StorageCostMismatch {
            expected,
            actual_total_bytes,
        } => {
            assert_eq!(expected.added_bytes, paid_key_len + 1);
            assert_eq!(actual_total_bytes, paid_key_len);
        }
    }

    let mut update_node = OperationCost::default();
    let info_no_key_verify = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            added_bytes: 999,
            replaced_bytes: 0,
            removed_bytes: NoStorageRemoval,
        },
        value_storage_cost: StorageCost {
            added_bytes: 5,
            replaced_bytes: 6,
            removed_bytes: NoStorageRemoval,
        },
        new_node: false,
        needs_value_verification: false,
    };

    update_node
        .add_key_value_storage_costs(key_len, 1, None, Some(info_no_key_verify))
        .expect("update path should skip key verification");
    assert_eq!(update_node.storage_cost.added_bytes, 1004);
    assert_eq!(update_node.storage_cost.replaced_bytes, 6);

    let mut needs_value_verification = OperationCost::default();
    let info_value_verify = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            added_bytes: paid_key_len,
            replaced_bytes: 0,
            removed_bytes: NoStorageRemoval,
        },
        value_storage_cost: StorageCost {
            added_bytes: 0,
            replaced_bytes: 0,
            removed_bytes: NoStorageRemoval,
        },
        new_node: true,
        needs_value_verification: true,
    };

    let err = needs_value_verification
        .add_key_value_storage_costs(key_len, 3, None, Some(info_value_verify))
        .expect_err("value verification mismatch must error");
    assert!(matches!(err, Error::StorageCostMismatch { .. }));
}

#[test]
fn cost_context_helpers_and_result_helpers_are_exercised() {
    let mut accumulated = OperationCost::default();
    let value = CostContext {
        value: 41_u32,
        cost: OperationCost::with_seek_count(2),
    }
    .unwrap_add_cost(&mut accumulated);
    assert_eq!(value, 41);
    assert_eq!(accumulated.seek_count, 2);

    let wrapped = 10_u32.wrap_with_cost(OperationCost::with_storage_loaded_bytes(5));
    assert_eq!(*wrapped.value(), 10);
    assert_eq!(wrapped.cost().storage_loaded_bytes, 5);

    let unwrapped = wrapped.unwrap();
    assert_eq!(unwrapped, 10);

    let nested = CostContext {
        value: CostContext {
            value: "ok",
            cost: OperationCost::with_hash_node_calls(3),
        },
        cost: OperationCost::with_seek_count(4),
    };
    let flattened = nested.flatten();
    assert_eq!(flattened.value, "ok");
    assert_eq!(flattened.cost.seek_count, 4);
    assert_eq!(flattened.cost.hash_node_calls, 3);

    let ok_cost: CostResult<u32, ()> = CostContext {
        value: Ok(1),
        cost: OperationCost::with_seek_count(6),
    };
    assert_eq!(
        ok_cost.cost_as_result().expect("ok"),
        OperationCost::with_seek_count(6)
    );

    let err_cost: CostResult<u32, &str> = CostContext {
        value: Err("e"),
        cost: OperationCost::with_seek_count(9),
    };
    assert_eq!(err_cost.cost_as_result(), Err("e"));

    let mut touched = false;
    let for_ok_result: CostResult<u32, ()> = CostContext {
        value: Ok(7),
        cost: OperationCost::default(),
    }
    .for_ok(|x| {
        touched = *x == 7;
    });
    assert!(touched);
    assert_eq!(for_ok_result.value, Ok(7));

    touched = false;
    let for_err_result: CostResult<u32, &str> = CostContext {
        value: Err("err"),
        cost: OperationCost::default(),
    }
    .for_ok(|_| {
        touched = true;
    });
    assert!(!touched);
    assert_eq!(for_err_result.value, Err("err"));
}

fn macro_target_ok() -> CostResult<u32, &'static str> {
    let mut cost = OperationCost::with_seek_count(1);
    let value = cost_return_on_error!(
        &mut cost,
        CostContext {
            value: Ok(11_u32),
            cost: OperationCost::with_storage_loaded_bytes(12),
        }
    );
    Ok(value + 1).wrap_with_cost(cost)
}

fn macro_target_err() -> CostResult<u32, &'static str> {
    let mut cost = OperationCost::with_seek_count(2);
    let _ = cost_return_on_error!(
        &mut cost,
        CostContext {
            value: Err("boom"),
            cost: OperationCost::with_storage_loaded_bytes(22),
        }
    );
    Ok(0).wrap_with_cost(cost)
}

fn macro_target_into_err() -> CostResult<u32, String> {
    let mut cost = OperationCost::with_seek_count(3);
    let _ = cost_return_on_error_into!(
        &mut cost,
        CostContext {
            value: Err("boom"),
            cost: OperationCost::with_storage_loaded_bytes(23),
        }
    );
    Ok(0).wrap_with_cost(cost)
}

fn macro_target_no_add_err() -> CostResult<u32, &'static str> {
    let cost = OperationCost::with_seek_count(4);
    let _ = cost_return_on_error_no_add!(cost, Err::<u32, _>("boom"));
    Ok(0).wrap_with_cost(cost)
}

fn macro_target_into_no_add_err() -> CostResult<u32, String> {
    let cost = OperationCost::with_seek_count(5);
    let _ = cost_return_on_error_into_no_add!(cost, Err::<u32, _>("boom"));
    Ok(0).wrap_with_cost(cost)
}

fn macro_target_default_err() -> CostResult<u32, &'static str> {
    let _ = cost_return_on_error_default!(Err::<u32, _>("boom"));
    Ok(0).wrap_with_cost(OperationCost::with_seek_count(99))
}

fn macro_target_into_default_err() -> CostResult<u32, String> {
    let _ = cost_return_on_error_into_default!(Err::<u32, _>("boom"));
    Ok(0).wrap_with_cost(OperationCost::with_seek_count(99))
}

#[test]
fn context_macros_cover_ok_and_err_paths() {
    let ok = macro_target_ok();
    assert_eq!(ok.value, Ok(12));
    assert_eq!(ok.cost.seek_count, 1);
    assert_eq!(ok.cost.storage_loaded_bytes, 12);

    let err = macro_target_err();
    assert_eq!(err.value, Err("boom"));
    assert_eq!(err.cost.seek_count, 2);
    assert_eq!(err.cost.storage_loaded_bytes, 22);

    let err_into = macro_target_into_err();
    assert_eq!(err_into.value, Err("boom".to_owned()));
    assert_eq!(err_into.cost.seek_count, 3);
    assert_eq!(err_into.cost.storage_loaded_bytes, 23);

    let no_add = macro_target_no_add_err();
    assert_eq!(no_add.value, Err("boom"));
    assert_eq!(no_add.cost.seek_count, 4);

    let into_no_add = macro_target_into_no_add_err();
    assert_eq!(into_no_add.value, Err("boom".to_owned()));
    assert_eq!(into_no_add.cost.seek_count, 5);

    let default_err = macro_target_default_err();
    assert_eq!(default_err.value, Err("boom"));
    assert_eq!(default_err.cost, OperationCost::default());

    let into_default_err = macro_target_into_default_err();
    assert_eq!(into_default_err.value, Err("boom".to_owned()));
    assert_eq!(into_default_err.cost, OperationCost::default());
}

#[test]
fn storage_cost_verify_and_transition_paths() {
    let cost = StorageCost {
        added_bytes: 3,
        replaced_bytes: 4,
        removed_bytes: NoStorageRemoval,
    };
    assert!(cost.verify(7).is_ok());
    assert!(matches!(
        cost.verify(6),
        Err(Error::StorageCostMismatch { .. })
    ));

    assert!(cost.verify_key_storage_cost(1, false).is_ok());
    assert!(cost.verify_key_storage_cost(7, true).is_ok());
    assert!(cost.verify_key_storage_cost(8, true).is_err());

    assert!(StorageCost::default().worse_or_eq_than(&StorageCost::default()));
    assert!(!StorageCost {
        added_bytes: 0,
        replaced_bytes: 0,
        removed_bytes: BasicStorageRemoval(1),
    }
    .worse_or_eq_than(&StorageCost::default()));

    assert!(!StorageCost::default().has_storage_change());
    assert!(!StorageCost {
        added_bytes: 0,
        replaced_bytes: 1,
        removed_bytes: NoStorageRemoval,
    }
    .has_storage_change());
    assert!(StorageCost {
        added_bytes: 1,
        replaced_bytes: 0,
        removed_bytes: NoStorageRemoval,
    }
    .has_storage_change());
    assert!(StorageCost {
        added_bytes: 0,
        replaced_bytes: 0,
        removed_bytes: BasicStorageRemoval(1),
    }
    .has_storage_change());

    let mut sum = StorageCost::default();
    sum += StorageCost {
        added_bytes: 1,
        replaced_bytes: 2,
        removed_bytes: BasicStorageRemoval(3),
    };
    let sum2 = sum.clone()
        + StorageCost {
            added_bytes: 4,
            replaced_bytes: 5,
            removed_bytes: BasicStorageRemoval(6),
        };
    assert_eq!(sum2.added_bytes, 5);
    assert_eq!(sum2.replaced_bytes, 7);
    assert_eq!(sum2.removed_bytes, BasicStorageRemoval(9));

    let insert = StorageCost {
        added_bytes: 1,
        replaced_bytes: 0,
        removed_bytes: NoStorageRemoval,
    };
    assert!(matches!(
        insert.transition_type(),
        OperationStorageTransitionType::OperationInsertNew
    ));

    let update_bigger = StorageCost {
        added_bytes: 1,
        replaced_bytes: 1,
        removed_bytes: NoStorageRemoval,
    };
    assert!(matches!(
        update_bigger.transition_type(),
        OperationStorageTransitionType::OperationUpdateBiggerSize
    ));

    let replace = StorageCost {
        added_bytes: 1,
        replaced_bytes: 0,
        removed_bytes: BasicStorageRemoval(1),
    };
    assert!(matches!(
        replace.transition_type(),
        OperationStorageTransitionType::OperationReplace
    ));

    let update_smaller = StorageCost {
        added_bytes: 0,
        replaced_bytes: 1,
        removed_bytes: BasicStorageRemoval(1),
    };
    assert!(matches!(
        update_smaller.transition_type(),
        OperationStorageTransitionType::OperationUpdateSmallerSize
    ));

    let delete = StorageCost {
        added_bytes: 0,
        replaced_bytes: 0,
        removed_bytes: BasicStorageRemoval(1),
    };
    assert!(matches!(
        delete.transition_type(),
        OperationStorageTransitionType::OperationDelete
    ));

    let update_same = StorageCost {
        added_bytes: 0,
        replaced_bytes: 1,
        removed_bytes: NoStorageRemoval,
    };
    assert!(matches!(
        update_same.transition_type(),
        OperationStorageTransitionType::OperationUpdateSameSize
    ));

    assert!(matches!(
        StorageCost::default().transition_type(),
        OperationStorageTransitionType::OperationNone
    ));
}

#[test]
fn key_value_storage_cost_paths_are_exercised() {
    let insert = KeyValueStorageCost::for_updated_root_cost(None, 20);
    assert!(insert.new_node);
    assert_eq!(insert.key_storage_cost.added_bytes, 34);
    assert_eq!(insert.value_storage_cost.added_bytes, 21);

    let less = KeyValueStorageCost::for_updated_root_cost(Some(20), 10);
    assert!(!less.new_node);
    assert_eq!(less.key_storage_cost.replaced_bytes, 34);
    assert_eq!(less.value_storage_cost.added_bytes, 0);
    assert_eq!(less.value_storage_cost.replaced_bytes, 11);
    assert_eq!(
        less.value_storage_cost.removed_bytes,
        BasicStorageRemoval(10)
    );

    let equal = KeyValueStorageCost::for_updated_root_cost(Some(10), 10);
    assert_eq!(equal.value_storage_cost.replaced_bytes, 11);
    assert_eq!(equal.value_storage_cost.removed_bytes, NoStorageRemoval);

    let greater = KeyValueStorageCost::for_updated_root_cost(Some(10), 20);
    assert_eq!(greater.value_storage_cost.added_bytes, 10);
    assert_eq!(greater.value_storage_cost.replaced_bytes, 11);

    let combined = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            added_bytes: 0,
            replaced_bytes: 0,
            removed_bytes: BasicStorageRemoval(5),
        },
        value_storage_cost: StorageCost {
            added_bytes: 0,
            replaced_bytes: 0,
            removed_bytes: BasicStorageRemoval(7),
        },
        new_node: true,
        needs_value_verification: true,
    }
    .combined_removed_bytes();
    assert_eq!(combined, BasicStorageRemoval(12));

    let mut a = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            added_bytes: 1,
            replaced_bytes: 2,
            removed_bytes: BasicStorageRemoval(3),
        },
        value_storage_cost: StorageCost {
            added_bytes: 4,
            replaced_bytes: 5,
            removed_bytes: BasicStorageRemoval(6),
        },
        new_node: true,
        needs_value_verification: true,
    };
    let b = KeyValueStorageCost {
        key_storage_cost: StorageCost {
            added_bytes: 10,
            replaced_bytes: 20,
            removed_bytes: BasicStorageRemoval(30),
        },
        value_storage_cost: StorageCost {
            added_bytes: 40,
            replaced_bytes: 50,
            removed_bytes: BasicStorageRemoval(60),
        },
        new_node: false,
        needs_value_verification: false,
    };

    let sum = a.clone() + b.clone();
    assert_eq!(sum.key_storage_cost.added_bytes, 11);
    assert_eq!(sum.value_storage_cost.replaced_bytes, 55);
    assert!(!sum.new_node);
    assert!(!sum.needs_value_verification);

    a += b;
    assert!(a == sum);
}

#[test]
fn storage_removed_bytes_add_and_add_assign_paths_are_exercised() {
    let identifier_a: Identifier = [1; 32];
    let identifier_b: Identifier = [2; 32];

    assert_eq!(NoStorageRemoval + NoStorageRemoval, NoStorageRemoval);
    assert_eq!(
        NoStorageRemoval + BasicStorageRemoval(5),
        BasicStorageRemoval(5)
    );

    let no_plus_sectioned = NoStorageRemoval + sectioned(identifier_a, 7, 10);
    assert!(matches!(no_plus_sectioned, SectionedStorageRemoval(_)));

    assert_eq!(
        BasicStorageRemoval(5) + NoStorageRemoval,
        BasicStorageRemoval(5)
    );
    assert_eq!(
        BasicStorageRemoval(5) + BasicStorageRemoval(6),
        BasicStorageRemoval(11)
    );

    let basic_plus_sectioned_missing_default =
        BasicStorageRemoval(7) + sectioned(identifier_a, 8, 9);
    assert!(basic_plus_sectioned_missing_default.has_removal());

    let basic_plus_sectioned_with_default = BasicStorageRemoval(4)
        + sectioned(
            Identifier::default(),
            grovedb_costs::storage_cost::removal::UNKNOWN_EPOCH,
            5,
        );
    assert!(matches!(
        basic_plus_sectioned_with_default,
        SectionedStorageRemoval(_)
    ));

    let sectioned_plus_no = sectioned(identifier_a, 1, 2) + NoStorageRemoval;
    assert!(sectioned_plus_no.has_removal());

    let sectioned_plus_basic_missing_default =
        sectioned(identifier_a, 3, 4) + BasicStorageRemoval(5);
    assert!(sectioned_plus_basic_missing_default.has_removal());

    let sectioned_plus_basic_with_default = sectioned(
        Identifier::default(),
        grovedb_costs::storage_cost::removal::UNKNOWN_EPOCH,
        6,
    ) + BasicStorageRemoval(7);
    assert!(matches!(
        sectioned_plus_basic_with_default,
        SectionedStorageRemoval(_)
    ));

    let sectioned_plus_sectioned = sectioned(identifier_a, 1, 10) + sectioned(identifier_a, 1, 20);
    assert_eq!(sectioned_plus_sectioned.total_removed_bytes(), 30);

    let sectioned_merge_disjoint = sectioned(identifier_a, 1, 10) + sectioned(identifier_b, 2, 20);
    assert_eq!(sectioned_merge_disjoint.total_removed_bytes(), 30);

    let mut add_assign = NoStorageRemoval;
    add_assign += BasicStorageRemoval(1);
    assert_eq!(add_assign, BasicStorageRemoval(1));

    let mut basic_assign = BasicStorageRemoval(2);
    basic_assign += NoStorageRemoval;
    assert_eq!(basic_assign, BasicStorageRemoval(2));

    basic_assign += BasicStorageRemoval(3);
    assert_eq!(basic_assign, BasicStorageRemoval(5));

    let mut basic_assign_with_sectioned = BasicStorageRemoval(4);
    basic_assign_with_sectioned += sectioned(identifier_a, 9, 10);
    assert!(matches!(
        basic_assign_with_sectioned,
        SectionedStorageRemoval(_)
    ));

    let mut sectioned_assign = sectioned(identifier_a, 10, 1);
    sectioned_assign += NoStorageRemoval;
    assert!(sectioned_assign.has_removal());

    sectioned_assign += BasicStorageRemoval(2);
    assert!(sectioned_assign.has_removal());

    sectioned_assign += sectioned(identifier_a, 10, 3);
    assert!(sectioned_assign.has_removal());

    assert!(NoStorageRemoval < BasicStorageRemoval(1));
    assert_eq!(
        BasicStorageRemoval(4).partial_cmp(&BasicStorageRemoval(4)),
        Some(std::cmp::Ordering::Equal)
    );

    assert!(!NoStorageRemoval.has_removal());
    assert!(!BasicStorageRemoval(0).has_removal());
    assert!(BasicStorageRemoval(1).has_removal());

    let mut zero_sectioned_map = BTreeMap::new();
    let mut zero_epochs = IntMap::new();
    zero_epochs.insert(1, 0);
    zero_sectioned_map.insert(identifier_a, zero_epochs);
    assert!(!SectionedStorageRemoval(zero_sectioned_map).has_removal());

    assert_eq!(NoStorageRemoval.total_removed_bytes(), 0);
    assert_eq!(BasicStorageRemoval(7).total_removed_bytes(), 7);
    assert_eq!(sectioned(identifier_b, 2, 11).total_removed_bytes(), 11);
}

#[test]
fn error_display_includes_expected_data() {
    let err = Error::StorageCostMismatch {
        expected: StorageCost {
            added_bytes: 8,
            replaced_bytes: 9,
            removed_bytes: NoStorageRemoval,
        },
        actual_total_bytes: 10,
    };

    let display = err.to_string();
    assert!(display.contains("added: 8"));
    assert!(display.contains("replaced: 9"));
    assert!(display.contains("actual:10"));
}
