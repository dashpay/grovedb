//! The bytes of a feature and of an aggregate that the store hands to a
//! writer outside it (`feature_bytes`, `aggregate_bytes`) are the bytes a
//! cell holds of a node that the store itself wrote.

use lignin_format::tree_page::Side;

use super::*;
use crate::store::lignin::{aggregate_bytes, feature_bytes};

/// What a cell of the latest version holds under `key`: its feature, and
/// the aggregate of each link it has.
fn stored(lignin: &Lignin, key: &[u8]) -> (Vec<u8>, [Option<Vec<u8>>; 2]) {
    let snapshot = lignin.path().snapshot();
    let operation = snapshot.operation();
    let opened = operation.open(Subtree::new(ROOT.prefix)).expect("a lookup");
    let opened = opened.expect("the subtree is entered");
    let read = opened.node(key, None, |view| {
        let view = view.expect("a node of the Merk has a cell");
        let aggregate = |side| view.link(side).map(|link| link.aggregate.to_vec());
        (
            view.feature().to_vec(),
            [aggregate(Side::Left), aggregate(Side::Right)],
        )
    });
    read.expect("a read")
}

#[test]
fn the_public_bytes_of_a_feature_and_an_aggregate_are_those_of_a_stored_cell() {
    let mut kinds = 0;
    for tree_type in TREE_TYPES {
        let lignin = Lignin::new();
        let batch: Vec<(Vec<u8>, Op)> = (1..=60u64)
            .map(|n| (be(n), Op::Put(value(n, false), feature(tree_type, n))))
            .collect();
        let transaction = lignin.path().begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let staged = LigninBatch::new();
        let mut merk = open(
            LigninNodeStore::deferred(&writer, ROOT, Some(&staged)),
            tree_type,
        );
        apply(&mut merk, &batch);
        let root = merk.root_key();
        drop(merk);
        staged.apply(&writer).value.expect("the batch is applied");
        writer.into_scope().merge();
        let committed = lignin.commit(&transaction);
        assert!(matches!(committed, Outcome::Committed(_)), "{committed:?}");
        drop(transaction);

        // Each node as Merk reads it through the store, beside its cell.
        let reader = lignin.path().begin();
        let store = LigninNodeStore::read_only(&reader, ROOT);
        let (mut nodes, mut links) = (0, 0);
        let mut ahead: Vec<Vec<u8>> = root.into_iter().collect();
        while let Some(key) = ahead.pop() {
            let node = store
                .get_node(&key, NO_VALUE_DEFINED_COST, version())
                .unwrap()
                .expect("the node is read")
                .expect("a node that is linked is stored");
            let (cell_feature, cell_aggregates) = stored(&lignin, &key);
            assert_eq!(
                feature_bytes(&node.feature_type()),
                cell_feature,
                "the feature of {key:?} under {tree_type:?}"
            );
            for (left, held) in [true, false].into_iter().zip(cell_aggregates) {
                let link = node.link(left);
                assert_eq!(link.is_some(), held.is_some(), "a link of {key:?}");
                let (Some(link), Some(held)) = (link, held) else {
                    continue;
                };
                assert_eq!(
                    aggregate_bytes(&link.aggregate_data()),
                    held,
                    "the aggregate of a link of {key:?} under {tree_type:?}"
                );
                links += 1;
                ahead.push(link.key().to_vec());
            }
            nodes += 1;
        }
        assert_eq!((nodes, links), (60, 59), "every node and link was read");
        kinds += 1;
    }
    assert_eq!(kinds, 12, "every tree type a Merk has nodes of");
}
