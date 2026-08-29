//! Cost accounting for secondary-mirror re-keys.
//!
//! A mirrored row MOVE (a group's count changing re-keys its row from
//! `(old_count ‖ key)` to `(new_count ‖ key)`) is physically a delete plus
//! an insert, but bills as the update it logically is: the pair's churn is
//! rebilled out of `added_bytes` and the unattributed removal lane into
//! `replaced_bytes` at batch-commit cost assembly (the removal lane was
//! unattributable anyway — mirror rows carry no flags, so nobody was ever
//! refunded those bytes). Unconditional: the indexed-tree family has never
//! shipped in a released version, so there is no historical accounting to
//! preserve.
//!
//! The integration tests pin the intended absolute semantics — a pure
//! re-key reports NO removal and steady-state re-keys accrete no added
//! bytes — plus the negative space: a genuine drain (the group deleted)
//! still reports its removal in full. The unit tests pin the
//! reclassifier's lane handling, including that flagged (refundable)
//! sectioned buckets are never touched.

#[cfg(test)]
mod tests {
    use grovedb_costs::{
        storage_cost::{
            removal::{StorageRemovedBytes, UNKNOWN_EPOCH},
            StorageCost,
        },
        OperationCost,
    };
    use grovedb_version::version::GroveVersion;
    use intmap::IntMap;

    use crate::{
        batch::{
            reclassify_indexed_mirror_rekey_churn, QualifiedGroveDbOp, SubelementsDeletionBehavior,
        },
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    fn total_removed(cost: &OperationCost) -> u64 {
        cost.storage_cost.removed_bytes.total_removed_bytes() as u64
    }

    /// A PCIT at `[TEST_LEAF, "cidx"]` with one count-tree group `"p"`
    /// holding `count` items — the steady pre-state a re-key needs.
    fn setup_pcit_with_group(
        grove_version: &GroveVersion,
        count: u64,
    ) -> crate::tests::TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"p",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert group");
        for i in 0..count {
            db.insert(
                [TEST_LEAF, b"cidx", b"p"].as_ref(),
                &i.to_be_bytes(),
                Element::new_item(vec![]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate group");
        }
        db
    }

    /// Bump the group's count via one batch (the deep write bubbles up and
    /// re-keys the mirror row), returning the batch's cost.
    fn bump_group_count(
        db: &crate::tests::TempGroveDb,
        item: u64,
        grove_version: &GroveVersion,
    ) -> OperationCost {
        let cost_result = db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec(), b"p".to_vec()],
                item.to_be_bytes().to_vec(),
                Element::new_item(vec![]),
            )],
            None,
            None,
            grove_version,
        );
        let cost = cost_result.cost;
        cost_result.value.expect("bump batch applies");
        cost
    }

    // -----------------------------------------------------------------
    // Integration: the intended absolute semantics through apply_batch
    // -----------------------------------------------------------------

    #[test]
    fn pure_rekey_reports_no_removal_and_carries_replaced() {
        // Bumping an existing group's count moves its mirror row. The old
        // and new rows have equal charged sizes (fixed-width sort key,
        // same item key, same payload shape), the batch deletes nothing
        // else, and the row carries no flags — so the ENTIRE removal is
        // churn and must be rebilled: no removal survives, and the
        // replaced lane carries at least the row.
        let grove_version = GroveVersion::latest();
        let db = setup_pcit_with_group(grove_version, 2);
        let cost = bump_group_count(&db, 2, grove_version);

        assert_eq!(
            total_removed(&cost),
            0,
            "a pure re-key must report no storage removal"
        );
        assert!(
            cost.storage_cost.replaced_bytes > 0,
            "the moved row must land in the replaced lane"
        );
        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash");
        assert_ne!(root_hash, [0u8; 32]);
    }

    #[test]
    fn repeated_rekeys_stay_growth_free() {
        // Steady state: after the first bump, every further bump moves the
        // row again — none of that churn may accrete in the added lane, so
        // consecutive bumps report identical added bytes and zero removal.
        let grove_version = GroveVersion::latest();
        let db = setup_pcit_with_group(grove_version, 2);
        let first = bump_group_count(&db, 2, grove_version);
        let second = bump_group_count(&db, 3, grove_version);

        assert_eq!(total_removed(&first), 0);
        assert_eq!(total_removed(&second), 0);
        assert_eq!(
            first.storage_cost.added_bytes, second.storage_cost.added_bytes,
            "steady-state bumps must not accrete added bytes from the mirror"
        );
        assert!(second.storage_cost.replaced_bytes > 0);
    }

    #[test]
    fn genuine_drain_still_reports_its_removal() {
        // Deleting the group is a real drain — the mirror row goes away
        // with nothing replacing it (old row, no new row: not a moved
        // pair), so the reclassifier must leave the removal fully intact.
        let grove_version = GroveVersion::latest();
        let db = setup_pcit_with_group(grove_version, 2);

        let cost_result = db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"p".to_vec(),
                grovedb_merk::tree_type::TreeType::CountTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            None,
            None,
            grove_version,
        );
        let cost = cost_result.cost;
        cost_result.value.expect("drain batch applies");

        assert!(
            total_removed(&cost) > 0,
            "a drain's removal must survive the reclassifier untouched"
        );
    }

    // -----------------------------------------------------------------
    // Unit: the reclassifier's lane handling
    // -----------------------------------------------------------------

    fn cost_with(added: u32, replaced: u32, removed: StorageRemovedBytes) -> StorageCost {
        StorageCost {
            added_bytes: added,
            replaced_bytes: replaced,
            removed_bytes: removed,
        }
    }

    #[test]
    fn reclassifier_moves_churn_between_lanes_basic() {
        let mut cost = cost_with(500, 40, StorageRemovedBytes::BasicStorageRemoval(120));
        reclassify_indexed_mirror_rekey_churn(&mut cost, 100);
        assert_eq!(cost.added_bytes, 400);
        assert_eq!(cost.replaced_bytes, 140);
        assert_eq!(
            cost.removed_bytes,
            StorageRemovedBytes::BasicStorageRemoval(20)
        );
    }

    #[test]
    fn reclassifier_caps_by_both_lanes_and_normalizes_empty_removal() {
        // Churn larger than the lanes can cover: capped at min(added,
        // removal) = 30; the emptied removal collapses to NoStorageRemoval.
        let mut cost = cost_with(200, 0, StorageRemovedBytes::BasicStorageRemoval(30));
        reclassify_indexed_mirror_rekey_churn(&mut cost, u32::MAX);
        assert_eq!(cost.added_bytes, 170);
        assert_eq!(cost.replaced_bytes, 30);
        assert_eq!(cost.removed_bytes, StorageRemovedBytes::NoStorageRemoval);

        // Zero churn and zero removal are both no-ops.
        let mut untouched = cost_with(200, 10, StorageRemovedBytes::NoStorageRemoval);
        reclassify_indexed_mirror_rekey_churn(&mut untouched, 50);
        assert_eq!(
            untouched,
            cost_with(200, 10, StorageRemovedBytes::NoStorageRemoval)
        );
    }

    #[test]
    fn reclassifier_only_touches_the_unattributed_sectioned_bucket() {
        // A sectioned removal mixing a flagged identity's refundable bytes
        // with the unattributed bucket (where a mirror's BasicStorageRemoval
        // lands when merged): only the unattributed bucket may be drawn
        // down, and it is pruned when emptied.
        let flagged_identity = [7u8; 32];
        let mut by_identifier = std::collections::BTreeMap::new();
        let mut flagged = IntMap::new();
        flagged.insert(3u16, 400u32);
        by_identifier.insert(flagged_identity, flagged);
        let mut unattributed = IntMap::new();
        unattributed.insert(UNKNOWN_EPOCH, 80u32);
        by_identifier.insert([0u8; 32], unattributed);

        let mut cost = cost_with(
            1000,
            0,
            StorageRemovedBytes::SectionedStorageRemoval(by_identifier),
        );
        reclassify_indexed_mirror_rekey_churn(&mut cost, 300);

        // Capped by the unattributed bucket (80), never the flagged 400.
        assert_eq!(cost.added_bytes, 920);
        assert_eq!(cost.replaced_bytes, 80);
        let StorageRemovedBytes::SectionedStorageRemoval(rest) = &cost.removed_bytes else {
            panic!("flagged removal must survive as sectioned");
        };
        assert!(!rest.contains_key(&[0u8; 32]), "emptied bucket is pruned");
        assert_eq!(
            rest.get(&flagged_identity).and_then(|m| m.get(3u16)),
            Some(&400)
        );
    }
}
