use std::collections::{BTreeSet, LinkedList};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::Element;
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{
    element::{costs::ElementCostExtensions, tree_type::ElementTreeTypeExtensions},
    merk::KeyUpdates,
    tree::{AuxMerkBatch, Fetch, TreeNode},
    tree_type::TreeType,
    Error, Link, Merk,
};

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Rewrites this tree, which keeps a 64-bit sum, as the tree type that
    /// holds the same children with the sum in 128 bits
    /// ([`TreeType::big_sum_counterpart`]), and sets [`Merk::tree_type`] to
    /// it.
    ///
    /// A node records its own contribution and each child link records the
    /// child's aggregate, both at the width of the tree's sum, so every node
    /// of the tree is read and written again. Each node takes the feature
    /// type its element has under the wide tree type; keys, values, value
    /// hashes and the shape of the tree stay as they are. A tree whose sum
    /// is in its node hashes gets new node hashes and a new root hash. The
    /// other trees keep theirs.
    ///
    /// The whole tree is held in memory until the write. The storage cost
    /// reports each node's growth as added bytes.
    ///
    /// The caller owns what lies outside this Merk: the element that
    /// describes the tree in its parent, and an indexed tree's secondaries.
    /// After an error the Merk no longer matches its storage and must be
    /// dropped.
    pub fn convert_to_big_sum_tree(
        &mut self,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let narrow_tree_type = self.tree_type;
        let Some(wide_tree_type) = narrow_tree_type.big_sum_counterpart() else {
            return Err(Error::InvalidOperation(
                "only a tree that keeps a 64-bit sum and has a counterpart with a 128-bit sum \
                 can be converted to a big sum tree",
            ))
            .wrap_with_cost(cost);
        };

        let source = self.source();
        cost_return_on_error!(
            &mut cost,
            self.use_tree_mut(|maybe_root| match maybe_root {
                Some(root) => widen_node(root, &source, wide_tree_type, grove_version),
                None => Ok(()).wrap_with_cost(OperationCost::default()),
            })
        );
        self.tree_type = wide_tree_type;

        // No key is added, removed or moved, so the root key stays and only
        // the rewritten nodes are written.
        let key_updates =
            KeyUpdates::new(BTreeSet::new(), BTreeSet::new(), LinkedList::new(), None);
        let aux: &AuxMerkBatch<Vec<u8>> = &[];
        self.commit(
            key_updates,
            aux,
            None,
            &|key, value| {
                Element::specialized_costs_for_key_value(
                    key,
                    value,
                    narrow_tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            grove_version,
        )
        .add_cost(cost)
    }
}

/// Gives `node` and every node under it the feature type its element has
/// under `wide_tree_type`, loading the nodes that are not in memory and
/// marking each link as modified so that the commit writes the node and
/// computes its aggregate and hash again.
fn widen_node<S: Fetch>(
    node: &mut TreeNode,
    source: &S,
    wide_tree_type: TreeType,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();

    for left in [true, false] {
        if matches!(node.link(left), Some(Link::Reference { .. })) {
            cost_return_on_error!(
                &mut cost,
                node.load(
                    left,
                    source,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
            );
        }
        let Some(link) = node.slot_mut(left).take() else {
            continue;
        };
        let mut child = match link {
            Link::Loaded { tree, .. }
            | Link::Modified { tree, .. }
            | Link::Uncommitted { tree, .. } => tree,
            Link::Reference { .. } => {
                return Err(Error::CorruptedState(
                    "a child link is still a reference after it was loaded",
                ))
                .wrap_with_cost(cost);
            }
        };
        cost_return_on_error!(
            &mut cost,
            widen_node(&mut child, source, wide_tree_type, grove_version)
        );
        *node.slot_mut(left) = Link::maybe_from_modified_tree(Some(child));
    }

    let element = cost_return_on_error_no_add!(
        cost,
        Element::deserialize(node.value_as_slice(), grove_version).map_err(Error::from)
    );
    let feature_type = cost_return_on_error_no_add!(cost, element.get_feature_type(wide_tree_type));
    node.set_feature_type(feature_type);

    Ok(()).wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;

    use grovedb_element::Element;
    use grovedb_version::version::GroveVersion;

    use crate::{
        element::insert::ElementInsertToStorageExtensions, test_utils::TempMerk,
        tree::AggregateData, tree_type::TreeType, Error,
    };

    /// A Merk of `tree_type` holding sum items of both signs, an item and a
    /// sum tree element, in an order that takes the tree several levels
    /// deep.
    fn filled(tree_type: TreeType, grove_version: &GroveVersion) -> TempMerk {
        let mut merk = TempMerk::new_with_tree_type(grove_version, tree_type);
        for i in 0..40u8 {
            let key = [i.wrapping_mul(37)];
            Element::new_sum_item(i64::from(i) - 10)
                .insert(&mut merk, key, None, grove_version)
                .unwrap()
                .expect("insert a sum item");
        }
        Element::new_item(vec![1, 2, 3])
            .insert(&mut merk, [1, 1], None, grove_version)
            .unwrap()
            .expect("insert an item");
        Element::SumTree(None, 1_000, None)
            .insert_subtree(&mut merk, [2, 2], [7; 32], None, grove_version)
            .unwrap()
            .expect("insert a sum tree");
        merk
    }

    #[test]
    fn a_converted_merk_is_the_merk_built_wide() {
        let grove_version = GroveVersion::latest();
        for (narrow, wide) in [
            (TreeType::SumTree, TreeType::BigSumTree),
            (TreeType::CountSumTree, TreeType::CountBigSumTree),
            (TreeType::ProvableSumTree, TreeType::ProvableBigSumTree),
            (
                TreeType::ProvableCountProvableSumTree,
                TreeType::ProvableCountProvableBigSumTree,
            ),
            (
                TreeType::ProvableSumIndexedTree,
                TreeType::ProvableBigSumIndexedTree,
            ),
            (
                TreeType::ProvableCountProvableSumIndexedTree,
                TreeType::ProvableCountProvableBigSumIndexedTree,
            ),
        ] {
            let mut converted = filled(narrow, grove_version);
            let narrow_root = converted.root_hash().unwrap();
            converted
                .convert_to_big_sum_tree(grove_version)
                .unwrap()
                .expect("convert");
            let built_wide = filled(wide, grove_version);

            assert_eq!(converted.deref().tree_type, wide, "{narrow}");
            assert_eq!(
                converted.aggregate_data().expect("aggregate"),
                built_wide.aggregate_data().expect("aggregate"),
                "{narrow}"
            );
            assert_eq!(
                converted.root_hash().unwrap(),
                built_wide.root_hash().unwrap(),
                "{narrow}"
            );
            // A sum in the node hashes moves the root, a sum beside them
            // does not.
            let sum_is_hashed = !matches!(narrow, TreeType::SumTree | TreeType::CountSumTree);
            assert_eq!(
                converted.root_hash().unwrap() != narrow_root,
                sum_is_hashed,
                "{narrow}"
            );
        }
    }

    #[test]
    fn an_empty_merk_only_changes_its_type() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::SumTree);
        merk.convert_to_big_sum_tree(grove_version)
            .unwrap()
            .expect("convert");
        assert_eq!(merk.deref().tree_type, TreeType::BigSumTree);
        assert_eq!(
            merk.aggregate_data().expect("aggregate"),
            AggregateData::NoAggregateData
        );
    }

    #[test]
    fn a_merk_without_a_counterpart_is_refused() {
        let grove_version = GroveVersion::latest();
        for tree_type in [
            TreeType::NormalTree,
            TreeType::BigSumTree,
            TreeType::CountTree,
            TreeType::ProvableCountSumTree,
        ] {
            let mut merk = TempMerk::new_with_tree_type(grove_version, tree_type);
            let result = merk.convert_to_big_sum_tree(grove_version).unwrap();
            assert!(
                matches!(result, Err(Error::InvalidOperation(_))),
                "{tree_type}: {result:?}"
            );
            assert_eq!(merk.deref().tree_type, tree_type);
        }
    }
}
