//! A batch that creates a tree and writes into it, against the same batch
//! without the writes.
//!
//! Upward propagation replaces the insert of a filled tree with the root
//! state the level below computed, so that insert is never applied as an
//! insert. From GROVE_V5 the checks an insert meets hold for it all the
//! same (`apply_batch.filled_tree_insert_checks` 1): the element is checked
//! where it enters the batch, and against its parent where the propagated
//! op is applied. GROVE_V4 makes them only for a tree the batch leaves
//! untouched, as GroveDB 6.1.1 did, and the state it commits for a filled
//! one is pinned here.

use std::collections::HashMap;

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes::NoStorageRemoval, OperationCost};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::estimated_costs::worst_case_costs::WorstCaseLayerInformation;
use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::WorstCaseCostsType, BatchApplyOptions, KeyInfoPath,
        QualifiedGroveDbOp,
    },
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error, GroveDb,
};

const PARENT: &[u8] = b"p";
const TREE: &[u8] = b"w";
const CHILD: &[u8] = b"k";

fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

fn insert_op(segments: &[&[u8]], key: &[u8], element: Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(path(segments), key.to_vec(), element)
}

fn non_counted(element: Element) -> Element {
    Element::NonCounted(Box::new(element))
}

fn not_summed(element: Element) -> Element {
    Element::NotSummed(Box::new(element))
}

fn not_counted_or_summed(element: Element) -> Element {
    Element::NotCountedOrSummed(Box::new(element))
}

fn item() -> Element {
    Element::new_item(b"x".to_vec())
}

fn sum_item() -> Element {
    Element::new_sum_item(5)
}

/// A count and sum indexed tree with the axes given, in the order given.
fn count_sum_indexed(axes: &[u8]) -> Element {
    Element::ProvableCountProvableSumIndexedTree(
        None,
        0,
        0,
        axes.iter().map(|tag| (*tag, None)).collect(),
        None,
    )
}

fn count_axis() -> u8 {
    IndexAxis::Count.tag()
}

fn sum_axis() -> u8 {
    IndexAxis::Sum.tag()
}

/// A root key nothing is stored under.
fn claimed_root_key() -> Option<Vec<u8>> {
    Some(b"zz".to_vec())
}

/// A database holding `parent` at `[TEST_LEAF, PARENT]`.
fn db_with_parent(parent: &Element, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    db.insert(
        [TEST_LEAF].as_ref(),
        PARENT,
        parent.clone(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the parent");
    db
}

fn root_hash(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
    db.root_hash(None, grove_version).unwrap().expect("root")
}

fn stored(db: &TempGroveDb, segments: &[&[u8]], key: &[u8], v: &GroveVersion) -> Element {
    db.get_raw(segments.into(), key, None, v)
        .unwrap()
        .expect("the element is stored")
}

fn assert_verifies(db: &TempGroveDb, grove_version: &GroveVersion, name: &str) {
    let issues = db
        .verify_grovedb(None, true, true, grove_version)
        .expect("verify");
    assert!(issues.is_empty(), "{name}: {issues:?}");
}

/// The insert of `tree` at `[TEST_LEAF, PARENT, TREE]`, alone.
fn create(tree: &Element) -> Vec<QualifiedGroveDbOp> {
    vec![insert_op(&[TEST_LEAF, PARENT], TREE, tree.clone())]
}

/// The same insert, with `child` written under the tree.
fn create_and_fill(tree: &Element, child: &Element) -> Vec<QualifiedGroveDbOp> {
    vec![
        insert_op(&[TEST_LEAF, PARENT], TREE, tree.clone()),
        insert_op(&[TEST_LEAF, PARENT, TREE], CHILD, child.clone()),
    ]
}

fn apply(
    db: &TempGroveDb,
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    db.apply_batch(ops, None, None, grove_version).unwrap()
}

/// A tree whose insert is refused when the batch only creates it.
struct Case {
    name: &'static str,
    parent: Element,
    tree: Element,
    child: Element,
    /// The root GROVE_V4 commits when the batch also fills the tree.
    v4_root: &'static str,
}

fn case(
    name: &'static str,
    parent: Element,
    tree: Element,
    child: Element,
    v4_root: &'static str,
) -> Case {
    Case {
        name,
        parent,
        tree,
        child,
        v4_root,
    }
}

/// Every check an insert makes on a tree that a filled tree skips on
/// GROVE_V4, one case per way of failing it.
fn skipped_checks() -> Vec<Case> {
    let sum_indexed = Element::empty_provable_sum_indexed_tree;
    let count_indexed = Element::empty_provable_count_indexed_tree;
    vec![
        // The axes of a count and sum indexed tree are canonical.
        case(
            "axes out of order",
            Element::empty_tree(),
            count_sum_indexed(&[sum_axis(), count_axis()]),
            sum_item(),
            "ccc7902edd64d3b98fe4286e2fa988c87518d29e280b55c8eb40aa8603ba6075",
        ),
        case(
            "an axis twice",
            Element::empty_tree(),
            count_sum_indexed(&[count_axis(), count_axis()]),
            sum_item(),
            "9622ad50ebbca0a51d8353d4f8906a5639413d17c24f5d49e90718de9d994634",
        ),
        // An indexed tree is created empty.
        case(
            "a count indexed tree claiming a primary root key",
            Element::empty_tree(),
            Element::ProvableCountIndexedTree(claimed_root_key(), None, 0, None),
            item(),
            "3c0cea2bc65212dd060c28e6c62b284c080def1bd933d2a30530b1e7caace162",
        ),
        case(
            "a count indexed tree claiming a secondary root key",
            Element::empty_tree(),
            Element::ProvableCountIndexedTree(None, claimed_root_key(), 0, None),
            item(),
            "3c0cea2bc65212dd060c28e6c62b284c080def1bd933d2a30530b1e7caace162",
        ),
        case(
            "a count indexed tree claiming a count",
            Element::empty_tree(),
            Element::ProvableCountIndexedTree(None, None, 7, None),
            item(),
            "3c0cea2bc65212dd060c28e6c62b284c080def1bd933d2a30530b1e7caace162",
        ),
        case(
            "a sum indexed tree claiming a secondary root key",
            Element::empty_tree(),
            Element::ProvableSumIndexedTree(None, claimed_root_key(), 0, None),
            sum_item(),
            "e4e52b44a840a43d4870bce77677d54e8c44f291dffd964185cb85e470624ea1",
        ),
        case(
            "a sum indexed tree claiming a sum",
            Element::empty_tree(),
            Element::ProvableSumIndexedTree(None, None, 7, None),
            sum_item(),
            "e4e52b44a840a43d4870bce77677d54e8c44f291dffd964185cb85e470624ea1",
        ),
        case(
            "a count and sum indexed tree claiming an axis root key",
            Element::empty_tree(),
            Element::ProvableCountProvableSumIndexedTree(
                None,
                0,
                0,
                vec![(count_axis(), claimed_root_key())],
                None,
            ),
            sum_item(),
            "feaeca19bf238597f518680a6f62926fb5ba9ae6d0b5168f788bfda249d1f8ff",
        ),
        case(
            "a count and sum indexed tree claiming a count and a sum",
            Element::empty_tree(),
            Element::ProvableCountProvableSumIndexedTree(
                None,
                3,
                9,
                vec![(count_axis(), None)],
                None,
            ),
            sum_item(),
            "feaeca19bf238597f518680a6f62926fb5ba9ae6d0b5168f788bfda249d1f8ff",
        ),
        // A wrapped tree sits under a parent that takes its wrapper.
        case(
            "a non-counted tree under a provable count tree",
            Element::empty_provable_count_tree(),
            non_counted(Element::empty_count_sum_tree()),
            sum_item(),
            "d7c5aeced3e7fcc19a34ef98ccd1b84cf44caf314b86f1859ca6f768f1fb9631",
        ),
        case(
            "a non-counted tree under a plain tree",
            Element::empty_tree(),
            non_counted(Element::empty_tree()),
            item(),
            "3c29714d8566d3149f273b084fcd43248859ec4fa1936e1d9f3aa07609f98053",
        ),
        case(
            "a not-summed tree under a count tree",
            Element::empty_count_tree(),
            not_summed(Element::empty_sum_tree()),
            sum_item(),
            "15e47d7b4bcd9897ec6255f1d516c3df122af07de8bcc92c31df8f621a984509",
        ),
        case(
            "a not-counted-or-summed tree under a provable count sum tree",
            Element::empty_provable_count_sum_tree(),
            not_counted_or_summed(Element::empty_count_sum_tree()),
            sum_item(),
            "569295553519fe7a4f19a02313426cb288e0975dc4148910c39af280d74a6356",
        ),
        // An entry of an indexed tree satisfies that tree's child rule.
        case(
            "a count indexed entry of a sum indexed tree",
            sum_indexed(),
            count_indexed(),
            item(),
            "c054985a97b95dfc6d8d36117afdad46e51fb672697d96b7a25261e8b60d1ee2",
        ),
        case(
            "a plain tree entry of a sum indexed tree",
            sum_indexed(),
            Element::empty_tree(),
            item(),
            "0bfced5c40754f22928d666be5d70c4a524061e2b160ade276c445e4b1df9ba1",
        ),
        case(
            "a big sum tree entry of a sum indexed tree",
            sum_indexed(),
            Element::empty_big_sum_tree(),
            sum_item(),
            "ce3b819facf13d2aa91e74d1c01b0155138c65b05543471e39d95b4626006ead",
        ),
        case(
            "a not-summed entry of a sum indexed tree",
            sum_indexed(),
            not_summed(Element::empty_sum_tree()),
            sum_item(),
            "09107b25202fe7ba27c00b7e737ee3be60b5091f9beadec0bc8b6aba56053ac4",
        ),
        case(
            "a sum tree entry of a count and sum indexed tree",
            count_sum_indexed(&[count_axis(), sum_axis()]),
            Element::empty_sum_tree(),
            sum_item(),
            "5084043f3c5143ea36c78008085816109799474351a8bccb0e42740a63b12ba6",
        ),
        case(
            "a sum indexed entry of a count and sum indexed tree",
            count_sum_indexed(&[count_axis(), sum_axis()]),
            sum_indexed(),
            sum_item(),
            "1519bbdf4baa2991b991e46eefbabc88c41bdf55898d34e7ddf0703404f5fd79",
        ),
        case(
            "a non-counted entry of a count indexed tree",
            count_indexed(),
            non_counted(Element::empty_count_tree()),
            item(),
            "a088a84fda9d8351316e4765b72441e6a5ec29108b6df3c13a6abcf3beca5776",
        ),
    ]
}

/// GROVE_V4 is what Dash Platform 5.0 runs: it refuses each of these trees
/// when the batch only creates it and takes it when the batch also fills
/// it, and the state it then commits stays what it is.
#[test]
fn grove_v4_takes_a_filled_tree_it_refuses_empty() {
    let v4 = &GROVE_V4;
    let mut roots = Vec::new();
    for case in skipped_checks() {
        let name = case.name;
        let db = db_with_parent(&case.parent, v4);
        let before = root_hash(&db, v4);
        apply(&db, create(&case.tree), v4)
            .expect_err(&format!("{name}: the insert alone is refused"));
        assert_eq!(root_hash(&db, v4), before, "{name}");

        apply(&db, create_and_fill(&case.tree, &case.child), v4)
            .unwrap_or_else(|e| panic!("{name}: the filled tree is taken, got {e:?}"));
        roots.push((name, hex::encode(root_hash(&db, v4)), case.v4_root));
    }
    for (name, root, pinned) in roots {
        assert_eq!(root, pinned, "{name}: the root GROVE_V4 commits");
    }
}

/// On GROVE_V4 the axes of a filled tree are committed in the order given.
#[test]
fn grove_v4_commits_the_axes_of_a_filled_tree_as_given() {
    let v4 = &GROVE_V4;
    let db = db_with_parent(&Element::empty_tree(), v4);
    let tree = count_sum_indexed(&[sum_axis(), count_axis()]);
    apply(&db, create_and_fill(&tree, &sum_item()), v4).expect("the filled tree is taken");
    let Element::ProvableCountProvableSumIndexedTree(_, count, sum, axes, _) =
        stored(&db, &[TEST_LEAF, PARENT], TREE, v4)
    else {
        panic!("expected a count and sum indexed tree");
    };
    assert_eq!((count, sum), (1, 5));
    assert_eq!(
        axes.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
        vec![sum_axis(), count_axis()]
    );
}

// ---- from here on the tests name the slot GROVE_V5 adds ----

/// Each way of writing an element a batch has.
fn op_kinds() -> Vec<(
    &'static str,
    fn(Vec<Vec<u8>>, Vec<u8>, Element) -> QualifiedGroveDbOp,
)> {
    vec![
        (
            "insert or replace",
            QualifiedGroveDbOp::insert_or_replace_op,
        ),
        (
            "insert if not exists",
            QualifiedGroveDbOp::insert_if_not_exists_op,
        ),
        (
            "insert if not exists or skip",
            QualifiedGroveDbOp::insert_if_not_exists_or_skip_op,
        ),
        (
            "insert known to not already exist",
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op,
        ),
        ("replace", QualifiedGroveDbOp::replace_op),
        ("patch", |path, key, element| {
            QualifiedGroveDbOp::patch_op(path, key, element, 0)
        }),
    ]
}

/// The latest version refuses each of these trees when the batch also fills
/// it, with the error the insert alone gives, and writes nothing.
#[test]
fn a_filled_tree_is_refused_as_the_insert_alone_is() {
    let v = GroveVersion::latest();
    for case in skipped_checks() {
        for (kind, op) in op_kinds() {
            let name = format!("{}, {kind}", case.name);
            let tree_op = || op(path(&[TEST_LEAF, PARENT]), TREE.to_vec(), case.tree.clone());
            let db = db_with_parent(&case.parent, v);
            let before = root_hash(&db, v);

            let alone =
                apply(&db, vec![tree_op()], v).expect_err(&format!("{name}: the insert alone"));
            let filled = apply(
                &db,
                vec![
                    tree_op(),
                    insert_op(&[TEST_LEAF, PARENT, TREE], CHILD, case.child.clone()),
                ],
                v,
            )
            .expect_err(&format!("{name}: the filled tree"));
            assert_eq!(format!("{filled:?}"), format!("{alone:?}"), "{name}");

            assert_eq!(root_hash(&db, v), before, "{name}: nothing is written");
            assert!(
                db.get_raw([TEST_LEAF, PARENT].as_ref().into(), TREE, None, v)
                    .unwrap()
                    .is_err(),
                "{name}: the tree is not stored"
            );
            assert_verifies(&db, v, &name);
        }
    }
}

/// The error does not depend on the version: the latest one gives the error
/// GROVE_V4 gives for the insert alone.
#[test]
fn the_refusal_is_the_one_grove_v4_gives_for_the_insert_alone() {
    let latest = GroveVersion::latest();
    for case in skipped_checks() {
        let on_v4 = apply(
            &db_with_parent(&case.parent, &GROVE_V4),
            create(&case.tree),
            &GROVE_V4,
        )
        .expect_err(case.name);
        let filled = apply(
            &db_with_parent(&case.parent, latest),
            create_and_fill(&case.tree, &case.child),
            latest,
        )
        .expect_err(case.name);
        assert_eq!(format!("{filled:?}"), format!("{on_v4:?}"), "{}", case.name);
    }
}

/// The tree is refused wherever it sits above what the batch writes, not
/// only directly above it.
#[test]
fn a_tree_filled_two_levels_down_is_refused() {
    let v = GroveVersion::latest();
    let tree = non_counted(Element::empty_count_sum_tree());
    let ops = |tree: &Element| {
        vec![
            insert_op(&[TEST_LEAF, PARENT], TREE, tree.clone()),
            insert_op(
                &[TEST_LEAF, PARENT, TREE],
                b"inner",
                Element::empty_count_sum_tree(),
            ),
            insert_op(&[TEST_LEAF, PARENT, TREE, b"inner"], CHILD, sum_item()),
        ]
    };

    let on_v4 = db_with_parent(&Element::empty_provable_count_tree(), &GROVE_V4);
    apply(&on_v4, ops(&tree), &GROVE_V4).expect("GROVE_V4 takes it");

    let db = db_with_parent(&Element::empty_provable_count_tree(), v);
    let before = root_hash(&db, v);
    let alone = apply(&db, create(&tree), v).expect_err("the insert alone");
    let filled = apply(&db, ops(&tree), v).expect_err("the filled tree");
    assert_eq!(format!("{filled:?}"), format!("{alone:?}"));
    assert_eq!(root_hash(&db, v), before);
}

/// Axes a count and sum indexed tree cannot have at all were refused on
/// GROVE_V4 too when the batch filled the tree, later and with another
/// error. The latest version gives the error of the insert alone.
#[test]
fn axes_no_tree_can_have_are_refused_with_the_error_of_the_insert_alone() {
    let latest = GroveVersion::latest();
    for (name, axes) in [
        ("an unknown axis", vec![count_axis(), 9]),
        ("no axis", vec![]),
    ] {
        let tree = count_sum_indexed(&axes);
        for v in [&GROVE_V4, latest] {
            let db = db_with_parent(&Element::empty_tree(), v);
            let before = root_hash(&db, v);
            let alone = apply(&db, create(&tree), v).expect_err(name);
            let filled = apply(&db, create_and_fill(&tree, &sum_item()), v).expect_err(name);
            assert_eq!(root_hash(&db, v), before, "{name}");
            let same_error = format!("{filled:?}") == format!("{alone:?}");
            assert_eq!(
                same_error,
                v.protocol_version == latest.protocol_version,
                "{name}, v{}: {filled:?}",
                v.protocol_version
            );
        }
    }
}

/// What a batch cannot fill at all is refused on both versions: a non-Merk
/// tree takes no keyed child, and an indexed tree is not written wrapped
/// from the state the batch computed. The checks an insert of them meets
/// therefore have no filled tree to hold for.
#[test]
fn a_tree_no_batch_can_fill_is_refused_on_both_versions() {
    let non_merk = [
        ("mmr tree", Element::empty_mmr_tree()),
        (
            "bulk append tree",
            Element::empty_bulk_append_tree(4).expect("chunk power"),
        ),
        ("dense tree", Element::empty_dense_tree(4)),
        (
            "commitment tree",
            Element::empty_commitment_tree(4).expect("chunk power"),
        ),
        (
            "private document store",
            Element::empty_private_document_store(32, 4).expect("config"),
        ),
    ];
    for v in [&GROVE_V4, GroveVersion::latest()] {
        for (name, tree) in &non_merk {
            let db = db_with_parent(&Element::empty_tree(), v);
            let before = root_hash(&db, v);
            let refused = apply(&db, create_and_fill(tree, &item()), v);
            assert!(
                matches!(
                    refused,
                    Err(Error::InvalidBatchOperation(message))
                        if message.contains("cannot hold keyed child elements")
                ),
                "{name}, v{}",
                v.protocol_version
            );
            assert_eq!(root_hash(&db, v), before, "{name}");
            apply(&db, create(tree), v)
                .unwrap_or_else(|e| panic!("{name}: the insert alone is taken, got {e:?}"));
        }

        // A wrapped indexed tree, under a parent that takes the wrapper.
        let wrapped = non_counted(Element::empty_provable_count_indexed_tree());
        let db = db_with_parent(&Element::empty_count_tree(), v);
        let before = root_hash(&db, v);
        let refused = apply(&db, create_and_fill(&wrapped, &item()), v);
        assert!(
            matches!(
                refused,
                Err(Error::InvalidBatchOperation(message))
                    if message.contains("indexed-tree elements cannot be wrapped")
            ),
            "v{}",
            v.protocol_version
        );
        assert_eq!(root_hash(&db, v), before);
    }
}

/// A private document store that fails the checks made on the element is
/// refused for them from the latest version, where they come first.
#[test]
fn a_private_document_store_is_checked_before_its_level_runs() {
    let latest = GroveVersion::latest();
    for (name, store) in [
        (
            "a store claiming entries",
            Element::PrivateDocumentStore(3, 32, 4, None),
        ),
        (
            "a store with no entry size",
            Element::PrivateDocumentStore(0, 0, 4, None),
        ),
    ] {
        let db = db_with_parent(&Element::empty_tree(), latest);
        let before = root_hash(&db, latest);
        let alone = apply(&db, create(&store), latest).expect_err(name);
        let filled = apply(&db, create_and_fill(&store, &item()), latest).expect_err(name);
        assert_eq!(format!("{filled:?}"), format!("{alone:?}"), "{name}");
        assert_eq!(root_hash(&db, latest), before, "{name}");

        // GROVE_V4 refuses the filled store as well, for what it holds.
        let db = db_with_parent(&Element::empty_tree(), &GROVE_V4);
        let refused = apply(&db, create_and_fill(&store, &item()), &GROVE_V4);
        assert!(
            matches!(
                refused,
                Err(Error::InvalidBatchOperation(message))
                    if message.contains("cannot hold keyed child elements")
            ),
            "{name}"
        );
    }
}

/// What a filled tree claims for itself and the batch computes anyway is
/// not checked on either version: the stored tree carries what the batch
/// put in it. A Merk tree's root key and aggregate are not checked by an
/// insert at all, and the rule that an entry of an indexed tree without a
/// root key claims no aggregate guards a value nothing derives.
#[test]
fn what_the_batch_computes_replaces_what_the_tree_claimed() {
    for v in [&GROVE_V4, GroveVersion::latest()] {
        // A sum tree claiming a root key and a sum.
        let db = db_with_parent(&Element::empty_tree(), v);
        apply(
            &db,
            create_and_fill(&Element::SumTree(claimed_root_key(), 9, None), &sum_item()),
            v,
        )
        .expect("a sum tree");
        assert_eq!(
            stored(&db, &[TEST_LEAF, PARENT], TREE, v),
            Element::SumTree(Some(CHILD.to_vec()), 5, None)
        );
        assert_verifies(&db, v, "a sum tree");

        // An entry of a sum indexed tree claiming a sum and no root key:
        // refused alone, taken filled, and ranked by its contents.
        let entry = Element::SumTree(None, 9, None);
        let db = db_with_parent(&Element::empty_provable_sum_indexed_tree(), v);
        let alone = apply(&db, create(&entry), v);
        assert!(
            matches!(
                alone,
                Err(Error::InvalidBatchOperation(message))
                    if message.contains("may not claim a non-zero aggregate")
            ),
            "v{}",
            v.protocol_version
        );
        apply(&db, create_and_fill(&entry, &sum_item()), v).expect("an entry");
        assert_eq!(
            stored(&db, &[TEST_LEAF, PARENT], TREE, v),
            Element::SumTree(Some(CHILD.to_vec()), 5, None)
        );
        let Element::ProvableSumIndexedTree(_, _, sum, _) = stored(&db, &[TEST_LEAF], PARENT, v)
        else {
            panic!("expected a sum indexed tree");
        };
        assert_eq!(sum, 5);
        assert_verifies(&db, v, "an entry");
    }
}

/// The checks on what the key already holds are not part of the slot: a
/// filled tree is written without reading its key on every version, so the
/// options and the conditional insert that refuse an insert over a stored
/// tree do not refuse it.
#[test]
fn a_filled_tree_is_written_over_a_stored_one_on_both_versions() {
    type TreeOp = fn(Vec<Vec<u8>>, Vec<u8>, Element) -> QualifiedGroveDbOp;
    let cases: [(&str, BatchApplyOptions, TreeOp); 3] = [
        (
            "validate_insertion_does_not_override",
            BatchApplyOptions {
                validate_insertion_does_not_override: true,
                ..Default::default()
            },
            QualifiedGroveDbOp::insert_or_replace_op,
        ),
        (
            "validate_insertion_does_not_override_tree",
            BatchApplyOptions {
                validate_insertion_does_not_override_tree: true,
                ..Default::default()
            },
            QualifiedGroveDbOp::insert_or_replace_op,
        ),
        (
            "insert if not exists",
            BatchApplyOptions::default(),
            QualifiedGroveDbOp::insert_if_not_exists_op,
        ),
    ];
    for v in [&GROVE_V4, GroveVersion::latest()] {
        for (name, options, op) in &cases {
            let name = format!("{name}, v{}", v.protocol_version);
            let tree_op = || op(path(&[TEST_LEAF]), PARENT.to_vec(), Element::empty_tree());
            let db = db_with_parent(&Element::empty_tree(), v);
            db.apply_batch(vec![tree_op()], Some(options.clone()), None, v)
                .unwrap()
                .expect_err(&format!("{name}: the insert alone"));
            db.apply_batch(
                vec![tree_op(), insert_op(&[TEST_LEAF, PARENT], CHILD, item())],
                Some(options.clone()),
                None,
                v,
            )
            .unwrap()
            .unwrap_or_else(|e| panic!("{name}: the filled tree is taken, got {e:?}"));
        }
    }
}

/// A tree an insert takes, with what the batch writes into it.
fn legitimate_trees() -> Vec<(&'static str, Element, Element, Element)> {
    let sum_indexed = Element::empty_provable_sum_indexed_tree;
    let count_indexed = Element::empty_provable_count_indexed_tree;
    let canonical = || count_sum_indexed(&[count_axis(), sum_axis()]);
    vec![
        (
            "a plain tree",
            Element::empty_tree(),
            Element::empty_tree(),
            item(),
        ),
        (
            "a sum tree",
            Element::empty_tree(),
            Element::empty_sum_tree(),
            sum_item(),
        ),
        (
            "a big sum tree",
            Element::empty_tree(),
            Element::empty_big_sum_tree(),
            sum_item(),
        ),
        (
            "a count tree",
            Element::empty_sum_tree(),
            Element::empty_count_tree(),
            item(),
        ),
        (
            "a count sum tree",
            Element::empty_count_sum_tree(),
            Element::empty_count_sum_tree(),
            sum_item(),
        ),
        (
            "a provable count tree",
            Element::empty_provable_count_tree(),
            Element::empty_provable_count_tree(),
            item(),
        ),
        (
            "a provable count sum tree",
            Element::empty_provable_count_sum_tree(),
            Element::empty_provable_count_sum_tree(),
            sum_item(),
        ),
        (
            "a provable sum tree",
            Element::empty_provable_sum_tree(),
            Element::empty_provable_sum_tree(),
            sum_item(),
        ),
        (
            "a provable count provable sum tree",
            Element::empty_provable_count_provable_sum_tree(),
            Element::empty_provable_count_provable_sum_tree(),
            sum_item(),
        ),
        (
            "a count big sum tree",
            Element::empty_count_big_sum_tree(),
            Element::empty_count_big_sum_tree(),
            sum_item(),
        ),
        (
            "a provable big sum tree",
            Element::empty_provable_big_sum_tree(),
            Element::empty_provable_big_sum_tree(),
            sum_item(),
        ),
        (
            "a provable count provable big sum tree",
            Element::empty_provable_count_provable_big_sum_tree(),
            Element::empty_provable_count_provable_big_sum_tree(),
            sum_item(),
        ),
        (
            "a non-counted count big sum tree under a count tree",
            Element::empty_count_tree(),
            non_counted(Element::empty_count_big_sum_tree()),
            sum_item(),
        ),
        (
            "a big sum tree entry of a count indexed tree",
            Element::empty_provable_count_indexed_tree(),
            Element::empty_big_sum_tree(),
            sum_item(),
        ),
        (
            "a non-counted tree under a count tree",
            Element::empty_count_tree(),
            non_counted(Element::empty_count_tree()),
            item(),
        ),
        (
            "a non-counted tree under a count sum tree",
            Element::empty_count_sum_tree(),
            non_counted(Element::empty_sum_tree()),
            sum_item(),
        ),
        (
            "a not-summed tree under a sum tree",
            Element::empty_sum_tree(),
            not_summed(Element::empty_sum_tree()),
            sum_item(),
        ),
        (
            "a not-summed tree under a provable count sum tree",
            Element::empty_provable_count_sum_tree(),
            not_summed(Element::empty_sum_tree()),
            sum_item(),
        ),
        (
            "a not-counted-or-summed tree under a count sum tree",
            Element::empty_count_sum_tree(),
            not_counted_or_summed(Element::empty_count_sum_tree()),
            sum_item(),
        ),
        (
            "a count indexed tree",
            Element::empty_tree(),
            count_indexed(),
            item(),
        ),
        (
            "a sum indexed tree",
            Element::empty_tree(),
            sum_indexed(),
            sum_item(),
        ),
        (
            "a count and sum indexed tree",
            Element::empty_tree(),
            canonical(),
            sum_item(),
        ),
        (
            "a count and sum indexed tree with one axis",
            Element::empty_tree(),
            count_sum_indexed(&[sum_axis()]),
            sum_item(),
        ),
        (
            "a plain tree entry of a count indexed tree",
            count_indexed(),
            Element::empty_tree(),
            item(),
        ),
        (
            "a count indexed entry of a count indexed tree",
            count_indexed(),
            count_indexed(),
            item(),
        ),
        (
            "a sum tree entry of a sum indexed tree",
            sum_indexed(),
            Element::empty_sum_tree(),
            sum_item(),
        ),
        (
            "a sum indexed entry of a sum indexed tree",
            sum_indexed(),
            sum_indexed(),
            sum_item(),
        ),
        (
            "a count sum tree entry of a count and sum indexed tree",
            canonical(),
            Element::empty_count_sum_tree(),
            sum_item(),
        ),
        (
            "a count and sum indexed entry of a count and sum indexed tree",
            canonical(),
            canonical(),
            sum_item(),
        ),
    ]
}

/// The latest version with the slot back at 0, to compare against.
fn latest_without_the_checks() -> GroveVersion {
    let mut version = GroveVersion::latest().clone();
    version
        .grovedb_versions
        .apply_batch
        .filled_tree_insert_checks = 0;
    version
}

fn apply_with_cost(
    db: &TempGroveDb,
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
    name: &str,
) -> OperationCost {
    let result = db.apply_batch(ops, None, None, grove_version);
    result
        .value
        .unwrap_or_else(|e| panic!("{name}: the batch applies, got {e:?}"));
    result.cost
}

/// A tree an insert takes is still created and filled by one batch, which
/// commits what creating it first and filling it after commits.
#[test]
fn a_tree_an_insert_takes_is_created_and_filled_as_before() {
    let v = GroveVersion::latest();
    for (name, parent, tree, child) in legitimate_trees() {
        let one_batch = db_with_parent(&parent, v);
        apply(&one_batch, create_and_fill(&tree, &child), v)
            .unwrap_or_else(|e| panic!("{name}: created and filled, got {e:?}"));

        let two_batches = db_with_parent(&parent, v);
        apply(&two_batches, create(&tree), v)
            .unwrap_or_else(|e| panic!("{name}: created, got {e:?}"));
        apply(
            &two_batches,
            vec![insert_op(&[TEST_LEAF, PARENT, TREE], CHILD, child.clone())],
            v,
        )
        .unwrap_or_else(|e| panic!("{name}: filled, got {e:?}"));

        assert_eq!(
            root_hash(&one_batch, v),
            root_hash(&two_batches, v),
            "{name}"
        );
        assert_eq!(
            stored(&one_batch, &[TEST_LEAF, PARENT], TREE, v),
            stored(&two_batches, &[TEST_LEAF, PARENT], TREE, v),
            "{name}"
        );
        assert_verifies(&one_batch, v, name);
    }
}

/// The slot changes neither the state nor the cost of a batch both of its
/// values accept: the creation alone, and the creation with the fill.
#[test]
fn the_checks_change_no_cost_and_no_root_of_an_accepted_batch() {
    let with_checks = GroveVersion::latest();
    let without_checks = latest_without_the_checks();
    for (name, parent, tree, child) in legitimate_trees() {
        for (shape, ops) in [
            ("created", create(&tree)),
            ("created and filled", create_and_fill(&tree, &child)),
        ] {
            let name = format!("{name}, {shape}");
            let checked = db_with_parent(&parent, with_checks);
            let checked_cost = apply_with_cost(&checked, ops.clone(), with_checks, &name);
            let unchecked = db_with_parent(&parent, &without_checks);
            let unchecked_cost = apply_with_cost(&unchecked, ops, &without_checks, &name);
            assert_eq!(checked_cost, unchecked_cost, "{name}: cost");
            assert_eq!(
                root_hash(&checked, with_checks),
                root_hash(&unchecked, &without_checks),
                "{name}: root"
            );
        }
    }
}

/// With the slot at 0 the latest version takes what GROVE_V4 takes: the
/// slot alone carries the refusals.
#[test]
fn the_slot_alone_carries_the_refusals() {
    let without_checks = latest_without_the_checks();
    for case in skipped_checks() {
        let db = db_with_parent(&case.parent, &without_checks);
        apply(
            &db,
            create_and_fill(&case.tree, &case.child),
            &without_checks,
        )
        .unwrap_or_else(|e| panic!("{}: taken with the slot at 0, got {e:?}", case.name));
    }
}

/// A partial batch pauses with the filled tree's propagated insert still to
/// apply. The continuation applies it under the same rule.
#[test]
fn a_paused_batch_holds_the_filled_tree_to_its_parent_when_it_continues() {
    let tree = non_counted(Element::empty_tree());
    let ops = || {
        vec![
            insert_op(&[], TREE, tree.clone()),
            insert_op(&[TREE], CHILD, item()),
        ]
    };
    let partial = |db: &TempGroveDb, v: &GroveVersion| {
        db.apply_partial_batch(ops(), None, |_cost, _left_over| Ok(vec![]), None, v)
            .unwrap()
    };

    let on_v4 = make_test_grovedb(&GROVE_V4);
    partial(&on_v4, &GROVE_V4).expect("GROVE_V4 takes it");

    let v = GroveVersion::latest();
    let db = make_test_grovedb(v);
    let before = root_hash(&db, v);
    let alone = apply(&db, vec![insert_op(&[], TREE, tree.clone())], v).expect_err("alone");
    let filled = partial(&db, v).expect_err("the filled tree");
    assert_eq!(format!("{filled:?}"), format!("{alone:?}"));
    assert_eq!(root_hash(&db, v), before);
    assert_verifies(&db, v, "a paused batch");
}

/// An add-on operation that lands on a tree the paused batch wrote into is
/// merged with that tree's computed root state, and what it writes is held
/// to the parent's rule for its wrapper.
#[test]
fn an_add_on_over_a_filled_tree_is_held_to_its_parent() {
    let wrapped = non_counted(Element::empty_tree());
    let partial = |db: &TempGroveDb, add_on: Element, v: &GroveVersion| {
        db.apply_partial_batch(
            vec![insert_op(&[TEST_LEAF], CHILD, item())],
            None,
            move |_cost, _left_over| Ok(vec![insert_op(&[], TEST_LEAF, add_on.clone())]),
            None,
            v,
        )
        .unwrap()
    };

    let on_v4 = make_test_grovedb(&GROVE_V4);
    partial(&on_v4, wrapped.clone(), &GROVE_V4).expect("GROVE_V4 takes it");
    assert_eq!(
        stored(&on_v4, &[], TEST_LEAF, &GROVE_V4),
        non_counted(Element::new_tree(Some(CHILD.to_vec())))
    );

    let v = GroveVersion::latest();
    let db = make_test_grovedb(v);
    let before = root_hash(&db, v);
    let alone = apply(&db, vec![insert_op(&[], b"other", wrapped.clone())], v).expect_err("alone");
    let merged = partial(&db, wrapped, v).expect_err("the add-on");
    assert_eq!(format!("{merged:?}"), format!("{alone:?}"));
    assert_eq!(root_hash(&db, v), before);

    // An add-on the parent takes is merged as before.
    partial(&db, Element::empty_tree_with_flags(Some(vec![7])), v)
        .expect("an add-on the parent takes");
    assert_eq!(
        stored(&db, &[], TEST_LEAF, v),
        Element::new_tree_with_flags(Some(CHILD.to_vec()), Some(vec![7]))
    );
    assert_verifies(&db, v, "an add-on");
}

/// An add-on operation enters the batch where every operation does, so a
/// tree the add-on operations create and fill is checked there.
#[test]
fn a_tree_the_add_on_creates_and_fills_is_checked() {
    let tree = count_sum_indexed(&[sum_axis(), count_axis()]);
    let partial = |db: &TempGroveDb, v: &GroveVersion| {
        let add_on = vec![
            insert_op(&[TEST_LEAF], TREE, tree.clone()),
            insert_op(&[TEST_LEAF, TREE], CHILD, sum_item()),
        ];
        db.apply_partial_batch(
            vec![insert_op(&[TEST_LEAF], b"other", item())],
            None,
            move |_cost, _left_over| Ok(add_on.clone()),
            None,
            v,
        )
        .unwrap()
    };

    let on_v4 = make_test_grovedb(&GROVE_V4);
    partial(&on_v4, &GROVE_V4).expect("GROVE_V4 takes it");

    let v = GroveVersion::latest();
    let db = make_test_grovedb(v);
    let before = root_hash(&db, v);
    let alone = apply(&db, vec![insert_op(&[TEST_LEAF], TREE, tree.clone())], v)
        .expect_err("the insert alone");
    let refused = partial(&db, v).expect_err("the add-on");
    assert_eq!(format!("{refused:?}"), format!("{alone:?}"));
    assert_eq!(root_hash(&db, v), before);
}

/// The element is checked where the batch is built, and a cost estimate
/// builds the batch the same way: from the latest version it is refused for
/// a tree the apply refuses. The estimate of a batch the apply takes is the
/// same with and without the checks.
#[test]
fn a_cost_estimate_is_refused_for_a_tree_the_apply_refuses() {
    let estimate = |tree: &Element, grove_version: &GroveVersion| {
        let paths = [
            vec![],
            path(&[TEST_LEAF]),
            path(&[TEST_LEAF, PARENT]),
            path(&[TEST_LEAF, PARENT, TREE]),
        ]
        .into_iter()
        .map(|path| {
            (
                KeyInfoPath::from_known_owned_path(path),
                WorstCaseLayerInformation::MaxElementsNumber(8),
            )
        })
        .collect::<HashMap<_, _>>();
        GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            create_and_fill(tree, &sum_item()),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
    };
    let latest = GroveVersion::latest();
    let without_checks = latest_without_the_checks();

    let unsorted = count_sum_indexed(&[sum_axis(), count_axis()]);
    estimate(&unsorted, &GROVE_V4).expect("GROVE_V4 estimates it");
    let alone = apply(
        &db_with_parent(&Element::empty_tree(), latest),
        create(&unsorted),
        latest,
    )
    .expect_err("the insert alone");
    let refused = estimate(&unsorted, latest).expect_err("the estimate");
    assert_eq!(format!("{refused:?}"), format!("{alone:?}"));

    let canonical = count_sum_indexed(&[count_axis(), sum_axis()]);
    assert_eq!(
        estimate(&canonical, latest).expect("the estimate"),
        estimate(&canonical, &without_checks).expect("the estimate")
    );
}

/// With the consistency check disabled the last operation at a key wins, so
/// an element under an earlier one was never applied. It is checked on
/// entry all the same from the latest version.
#[test]
fn an_element_a_later_operation_replaces_is_checked_on_entry() {
    let options = || {
        Some(BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        })
    };
    let unsorted = count_sum_indexed(&[sum_axis(), count_axis()]);
    let ops = || {
        vec![
            insert_op(&[TEST_LEAF, PARENT], TREE, unsorted.clone()),
            insert_op(&[TEST_LEAF, PARENT], TREE, item()),
        ]
    };

    let on_v4 = db_with_parent(&Element::empty_tree(), &GROVE_V4);
    on_v4
        .apply_batch(ops(), options(), None, &GROVE_V4)
        .unwrap()
        .expect("GROVE_V4 applies the last operation");
    assert_eq!(
        stored(&on_v4, &[TEST_LEAF, PARENT], TREE, &GROVE_V4),
        item()
    );

    let v = GroveVersion::latest();
    let db = db_with_parent(&Element::empty_tree(), v);
    let before = root_hash(&db, v);
    let alone = apply(&db, create(&unsorted), v).expect_err("the insert alone");
    let refused = db
        .apply_batch(ops(), options(), None, v)
        .unwrap()
        .expect_err("the batch");
    assert_eq!(format!("{refused:?}"), format!("{alone:?}"));
    assert_eq!(root_hash(&db, v), before);
}

/// A private document store holds no keyed element: every operation at its
/// level is refused before any is applied, a propagated insert included, so
/// that rule of the insert has no filled tree to hold for.
#[test]
fn a_tree_under_a_private_document_store_is_refused_on_both_versions() {
    for v in [&GROVE_V4, GroveVersion::latest()] {
        let store = Element::empty_private_document_store(32, 4).expect("config");
        let db = db_with_parent(&store, v);
        let before = root_hash(&db, v);
        let alone = apply(&db, create(&Element::empty_tree()), v).expect_err("the insert alone");
        let filled = apply(&db, create_and_fill(&Element::empty_tree(), &item()), v)
            .expect_err("the filled tree");
        assert_eq!(format!("{filled:?}"), format!("{alone:?}"));
        assert_eq!(root_hash(&db, v), before);
    }
}

/// The trees with a 128-bit sum exist only from the latest version, so they
/// have no GROVE_V4 outcome to pin. They are held to the same rule by the
/// same slot, as a parent, as an entry and as the tree created: none of the
/// checks names a tree type of its own.
fn wide_sum_cases() -> Vec<(&'static str, Element, Element, Element)> {
    vec![
        // An entry of a 64-bit indexed tree that carries a 128-bit sum.
        (
            "a count big sum tree entry of a sum indexed tree",
            Element::empty_provable_sum_indexed_tree(),
            Element::empty_count_big_sum_tree(),
            sum_item(),
        ),
        // A wrapped tree with a 128-bit sum under a parent that commits
        // its count.
        (
            "a non-counted count big sum tree under a provable count tree",
            Element::empty_provable_count_tree(),
            non_counted(Element::empty_count_big_sum_tree()),
            sum_item(),
        ),
        (
            "a non-counted provable big sum tree under a provable count tree",
            Element::empty_provable_count_tree(),
            non_counted(Element::empty_provable_big_sum_tree()),
            sum_item(),
        ),
        (
            "a non-counted provable count provable big sum tree under a provable count tree",
            Element::empty_provable_count_tree(),
            non_counted(Element::empty_provable_count_provable_big_sum_tree()),
            sum_item(),
        ),
        (
            "a not-summed provable big sum tree under a count tree",
            Element::empty_count_tree(),
            not_summed(Element::empty_provable_big_sum_tree()),
            sum_item(),
        ),
        // A wrapped 64-bit tree under a parent with a 128-bit sum.
        (
            "a not-counted-or-summed tree under a provable big sum tree",
            Element::empty_provable_big_sum_tree(),
            not_counted_or_summed(Element::empty_sum_tree()),
            sum_item(),
        ),
    ]
}

/// Each of them is refused when the batch also fills the tree, with the
/// error the insert alone gives, and nothing is written. With the slot at
/// 0 the filled tree is taken: the slot carries the refusal.
#[test]
fn a_filled_tree_with_a_128_bit_sum_around_it_is_refused_as_the_insert_alone_is() {
    let v = GroveVersion::latest();
    let without_checks = latest_without_the_checks();
    for (name, parent, tree, child) in wide_sum_cases() {
        let db = db_with_parent(&parent, v);
        let before = root_hash(&db, v);
        let alone = apply(&db, create(&tree), v).expect_err(&format!("{name}: alone"));
        let filled =
            apply(&db, create_and_fill(&tree, &child), v).expect_err(&format!("{name}: filled"));
        assert_eq!(format!("{filled:?}"), format!("{alone:?}"), "{name}");
        assert_eq!(root_hash(&db, v), before, "{name}: nothing is written");
        assert_verifies(&db, v, name);

        let unchecked = db_with_parent(&parent, &without_checks);
        apply(&unchecked, create(&tree), &without_checks)
            .expect_err(&format!("{name}: alone, with the slot at 0"));
        apply(&unchecked, create_and_fill(&tree, &child), &without_checks)
            .unwrap_or_else(|e| panic!("{name}: taken with the slot at 0, got {e:?}"));
    }
}
