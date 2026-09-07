//! Storage charges for ordinary replacements of specialized values
//! (issue #908).
//!
//! A node loaded from storage is stamped with the `value_defined_cost` of
//! the element it holds. Trees and sum items carry a fixed cost size, so
//! for them that metadata is `Some(fixed size)`. Before GROVE_V4 an ordinary
//! `Op::Put` / `Op::PutCombinedReference` (an Item or a Reference) replacing
//! such a node kept the stale metadata: the replacement was charged from the
//! predecessor's fixed size and the physical-size verification was skipped,
//! even though the full replacement bytes were written.
//!
//! From GROVE_V4 (`merk_versions.tree.put_value = 1`) the metadata is
//! cleared on every ordinary replacement, so:
//!
//! - the charge is derived from the replacement's own serialized bytes and
//!   verified on commit against the bytes actually written
//!   (`needs_value_verification` is back on);
//! - a specialized -> ordinary replacement charges exactly the same total
//!   bytes as an ordinary -> ordinary replacement of the same shape;
//! - the bytes added by a specialized -> ordinary replacement equal the
//!   bytes removed by the reverse replacement;
//! - old-value removal accounting (always derived from the stored
//!   predecessor bytes) and the ordinary -> specialized direction are
//!   unchanged across versions.
//!
//! GROVE_V3 is live, so its legacy figures are pinned here.

use grovedb_costs::{
    storage_cost::{
        removal::StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval},
        StorageCost,
    },
    OperationCost,
};
use grovedb_element::reference_path::ReferencePathType;
use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    operations::insert::InsertOptions,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error,
};

const KEY: [u8; 32] = [7u8; 32];
const TARGET_KEY: [u8; 100] = [1u8; 100];

fn big_item() -> Element {
    Element::new_item(vec![9u8; 200])
}

fn small_item() -> Element {
    Element::new_item(vec![9u8; 5])
}

fn reference() -> Element {
    Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        b"tree".to_vec(),
        TARGET_KEY.to_vec(),
    ]))
}

/// Overwrites (`InsertOptions` with tree overrides allowed) are used so the
/// same helper can drive the tree -> item cases.
fn overwrite_options() -> InsertOptions {
    InsertOptions {
        validate_insertion_does_not_override: false,
        validate_insertion_does_not_override_tree: false,
        base_root_storage_is_free: true,
        propagate_backward_references: false,
    }
}

/// Builds `[tree]` of type `parent`, seeds a reference target and `before`
/// at `KEY`, then replaces `KEY` with `after` (direct insert or batch) and
/// returns the cost of that replacement.
fn replace_cost(
    parent: Element,
    before: Element,
    after: Element,
    batch: bool,
    grove_version: &GroveVersion,
) -> Result<OperationCost, Error> {
    let db = make_empty_grovedb();
    let tx = db.start_transaction();
    db.insert(EMPTY_PATH, b"tree", parent, None, Some(&tx), grove_version)
        .unwrap()
        .expect("parent tree");
    db.insert(
        [b"tree".as_slice()].as_ref(),
        &TARGET_KEY,
        Element::new_item(vec![3u8; 4]),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("reference target");
    db.insert(
        [b"tree".as_slice()].as_ref(),
        &KEY,
        before,
        Some(overwrite_options()),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("predecessor");
    if batch {
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"tree".to_vec()],
                KEY.to_vec(),
                after,
            )],
            None,
            Some(&tx),
            grove_version,
        )
        .cost_as_result()
    } else {
        db.insert(
            [b"tree".as_slice()].as_ref(),
            &KEY,
            after,
            Some(overwrite_options()),
            Some(&tx),
            grove_version,
        )
        .cost_as_result()
    }
}

fn storage(
    parent: Element,
    before: Element,
    after: Element,
    batch: bool,
    grove_version: &GroveVersion,
) -> StorageCost {
    replace_cost(parent, before, after, batch, grove_version)
        .expect("replacement succeeds")
        .storage_cost
}

/// The four specialized -> ordinary replacements under test, each paired
/// with the ordinary -> ordinary control of the same tree shape.
fn cases() -> Vec<(&'static str, Element, Element, Element, Element)> {
    vec![
        // (name, parent, specialized predecessor, ordinary control predecessor, replacement)
        (
            "sum item -> item",
            Element::empty_sum_tree(),
            Element::new_sum_item(5),
            small_item(),
            big_item(),
        ),
        (
            "sum item -> reference",
            Element::empty_sum_tree(),
            Element::new_sum_item(5),
            small_item(),
            reference(),
        ),
        (
            "tree -> item",
            Element::empty_tree(),
            Element::empty_tree(),
            small_item(),
            big_item(),
        ),
        (
            "sum tree -> item",
            Element::empty_tree(),
            Element::empty_sum_tree(),
            small_item(),
            big_item(),
        ),
    ]
}

#[test]
fn v4_ordinary_replacement_of_specialized_value_is_charged_like_an_ordinary_one() {
    for batch in [false, true] {
        for (name, parent, specialized, ordinary, after) in cases() {
            let subject = storage(parent.clone(), specialized, after.clone(), batch, &GROVE_V4);
            let control = storage(parent, ordinary, after, batch, &GROVE_V4);
            // Same bytes end up on disk in both cases, so the same bytes are
            // paid for. Only the added/replaced split differs, because the
            // predecessors have different sizes.
            assert_eq!(
                subject.added_bytes + subject.replaced_bytes,
                control.added_bytes + control.replaced_bytes,
                "{name} (batch={batch}): total charged bytes"
            );
            assert_eq!(
                subject.removed_bytes, NoStorageRemoval,
                "{name} (batch={batch})"
            );
            assert_eq!(
                control.removed_bytes, NoStorageRemoval,
                "{name} (batch={batch})"
            );
            // The replacement is bigger than any specialized predecessor,
            // so growth must be charged as added bytes.
            assert!(
                subject.added_bytes > 0,
                "{name} (batch={batch}): growth over the fixed-size predecessor must be added"
            );
        }
    }
}

#[test]
fn v4_added_bytes_mirror_the_reverse_replacements_removed_bytes() {
    for batch in [false, true] {
        for (parent, specialized, ordinary) in [
            (
                Element::empty_sum_tree(),
                Element::new_sum_item(5),
                big_item(),
            ),
            (Element::empty_tree(), Element::empty_tree(), big_item()),
            (Element::empty_tree(), Element::empty_sum_tree(), big_item()),
        ] {
            let forward = storage(
                parent.clone(),
                specialized.clone(),
                ordinary.clone(),
                batch,
                &GROVE_V4,
            );
            let reverse = storage(parent, ordinary, specialized, batch, &GROVE_V4);
            assert_eq!(
                reverse.removed_bytes,
                BasicStorageRemoval(forward.added_bytes),
                "batch={batch}: shrinking back must free exactly what growing charged"
            );
            assert_eq!(reverse.added_bytes, 0, "batch={batch}");
        }
    }
}

#[test]
fn v4_pinned_figures() {
    for batch in [false, true] {
        assert_eq!(
            storage(
                Element::empty_sum_tree(),
                Element::new_sum_item(5),
                big_item(),
                batch,
                &GROVE_V4
            ),
            StorageCost {
                added_bytes: 193,
                replaced_bytes: 473,
                removed_bytes: NoStorageRemoval,
            },
            "sum item -> item (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_sum_tree(),
                Element::new_sum_item(5),
                reference(),
                batch,
                &GROVE_V4
            ),
            StorageCost {
                added_bytes: 101,
                replaced_bytes: 473,
                removed_bytes: NoStorageRemoval,
            },
            "sum item -> reference (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_tree(),
                Element::empty_tree(),
                big_item(),
                batch,
                &GROVE_V4
            ),
            StorageCost {
                added_bytes: 232,
                replaced_bytes: 393,
                removed_bytes: NoStorageRemoval,
            },
            "tree -> item (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_tree(),
                Element::empty_sum_tree(),
                big_item(),
                batch,
                &GROVE_V4
            ),
            StorageCost {
                added_bytes: 223,
                replaced_bytes: 402,
                removed_bytes: NoStorageRemoval,
            },
            "sum tree -> item (batch={batch})"
        );
    }
}

/// GROVE_V3 is live: its (under-)charges are consensus-locked and must not
/// move. Each specialized -> ordinary replacement is charged as if the
/// 200-byte item were the predecessor's fixed size — zero added bytes.
#[test]
fn v3_keeps_the_predecessors_fixed_cost_size() {
    for batch in [false, true] {
        for (name, parent, specialized, _ordinary, after) in cases() {
            let legacy = storage(parent, specialized, after, batch, &GROVE_V3);
            assert_eq!(legacy.added_bytes, 0, "{name} (batch={batch})");
            assert_eq!(
                legacy.removed_bytes, NoStorageRemoval,
                "{name} (batch={batch})"
            );
        }
        assert_eq!(
            storage(
                Element::empty_sum_tree(),
                Element::new_sum_item(5),
                big_item(),
                batch,
                &GROVE_V3
            )
            .replaced_bytes,
            473,
            "sum item -> item (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_sum_tree(),
                Element::new_sum_item(5),
                reference(),
                batch,
                &GROVE_V3
            )
            .replaced_bytes,
            473,
            "sum item -> reference (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_tree(),
                Element::empty_tree(),
                big_item(),
                batch,
                &GROVE_V3
            )
            .replaced_bytes,
            393,
            "tree -> item (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_tree(),
                Element::empty_sum_tree(),
                big_item(),
                batch,
                &GROVE_V3
            )
            .replaced_bytes,
            402,
            "sum tree -> item (batch={batch})"
        );
    }
}

/// Ordinary -> ordinary replacements and ordinary -> specialized
/// replacements (whose metadata is stamped from the new op on every
/// version) are byte-identical across GROVE_V3 and GROVE_V4, as is the
/// removal credit for shrinking onto a fixed-size element.
#[test]
fn unaffected_replacements_are_identical_across_versions() {
    for batch in [false, true] {
        for (name, parent, before, after) in [
            (
                "item -> item (sum tree)",
                Element::empty_sum_tree(),
                small_item(),
                big_item(),
            ),
            (
                "item -> reference (sum tree)",
                Element::empty_sum_tree(),
                small_item(),
                reference(),
            ),
            (
                "item -> item (tree)",
                Element::empty_tree(),
                small_item(),
                big_item(),
            ),
            (
                "item -> sum item",
                Element::empty_sum_tree(),
                big_item(),
                Element::new_sum_item(5),
            ),
            (
                "item -> tree",
                Element::empty_tree(),
                big_item(),
                Element::empty_tree(),
            ),
            (
                "item -> sum tree",
                Element::empty_tree(),
                big_item(),
                Element::empty_sum_tree(),
            ),
        ] {
            let v3 = replace_cost(
                parent.clone(),
                before.clone(),
                after.clone(),
                batch,
                &GROVE_V3,
            )
            .expect("v3");
            let v4 = replace_cost(parent, before, after, batch, &GROVE_V4).expect("v4");
            assert_eq!(v3, v4, "{name} (batch={batch})");
        }
        // The removal credit is derived from the stored predecessor bytes.
        assert_eq!(
            storage(
                Element::empty_sum_tree(),
                big_item(),
                Element::new_sum_item(5),
                batch,
                &GROVE_V4
            )
            .removed_bytes,
            BasicStorageRemoval(193),
            "item -> sum item (batch={batch})"
        );
        assert_eq!(
            storage(
                Element::empty_tree(),
                big_item(),
                Element::empty_tree(),
                batch,
                &GROVE_V4
            )
            .removed_bytes,
            BasicStorageRemoval(232),
            "item -> tree (batch={batch})"
        );
    }
}

#[test]
fn unknown_put_value_version_is_rejected() {
    let mut future = GROVE_V4.clone();
    future.merk_versions.tree.put_value = 2;
    for batch in [false, true] {
        let err = replace_cost(
            Element::empty_sum_tree(),
            small_item(),
            big_item(),
            batch,
            &future,
        )
        .expect_err("unknown put_value version must be refused");
        assert!(
            err.to_string().contains("put_value"),
            "batch={batch}: unexpected error {err}"
        );
    }
}
