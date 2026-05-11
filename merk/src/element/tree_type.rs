use grovedb_element::{Element, ElementFlags};

use crate::{
    Error, MaybeTree, TreeFeatureType,
    TreeFeatureType::{
        BasicMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode, SummedMerkNode,
    },
    TreeType,
};

/// Extension trait for determining tree type information from elements.
pub trait ElementTreeTypeExtensions {
    /// Check if the element is a tree and return the root_tree info and tree
    /// type
    fn root_key_and_tree_type_owned(self) -> Option<(Option<Vec<u8>>, TreeType)>;

    /// Check if the element is a tree and return the root_tree info and the
    /// tree type
    fn root_key_and_tree_type(&self) -> Option<(&Option<Vec<u8>>, TreeType)>;

    /// Check if the element is a tree and return the flags and the tree type
    fn tree_flags_and_type(&self) -> Option<(&Option<ElementFlags>, TreeType)>;

    /// Check if the element is a tree and return the tree type
    fn tree_type(&self) -> Option<TreeType>;

    /// Check if the element is a tree and return the aggregate of elements in
    /// the tree
    fn tree_feature_type(&self) -> Option<TreeFeatureType>;

    /// Check if the element is a tree and return the tree type
    fn maybe_tree_type(&self) -> MaybeTree;

    /// Get the tree feature type
    fn get_feature_type(&self, parent_tree_type: TreeType) -> Result<TreeFeatureType, Error>;
}
impl ElementTreeTypeExtensions for Element {
    /// Check if the element is a tree and return the root_tree info and tree
    /// type. Looks through `NonCounted`.
    fn root_key_and_tree_type_owned(self) -> Option<(Option<Vec<u8>>, TreeType)> {
        match self {
            Element::Tree(root_key, _) => Some((root_key, TreeType::NormalTree)),
            Element::SumTree(root_key, ..) => Some((root_key, TreeType::SumTree)),
            Element::BigSumTree(root_key, ..) => Some((root_key, TreeType::BigSumTree)),
            Element::CountTree(root_key, ..) => Some((root_key, TreeType::CountTree)),
            Element::CountSumTree(root_key, ..) => Some((root_key, TreeType::CountSumTree)),
            Element::ProvableCountTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountTree))
            }
            Element::ProvableCountSumTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountSumTree))
            }
            Element::CommitmentTree(_, chunk_power, _) => {
                Some((None, TreeType::CommitmentTree(chunk_power)))
            }
            Element::MmrTree(..) => Some((None, TreeType::MmrTree)),
            Element::BulkAppendTree(_, chunk_power, _) => {
                Some((None, TreeType::BulkAppendTree(chunk_power)))
            }
            Element::DenseAppendOnlyFixedSizeTree(_, height, _) => {
                Some((None, TreeType::DenseAppendOnlyFixedSizeTree(height)))
            }
            Element::NonCounted(inner) | Element::NotSummed(inner) => {
                inner.root_key_and_tree_type_owned()
            }
            _ => None,
        }
    }

    /// Check if the element is a tree and return the root_tree info and the
    /// tree type. Looks through `NonCounted`.
    fn root_key_and_tree_type(&self) -> Option<(&Option<Vec<u8>>, TreeType)> {
        // We use a const None to return a stable reference for non-Merk tree types.
        const NONE_ROOT_KEY: Option<Vec<u8>> = None;
        match self {
            Element::Tree(root_key, _) => Some((root_key, TreeType::NormalTree)),
            Element::SumTree(root_key, ..) => Some((root_key, TreeType::SumTree)),
            Element::BigSumTree(root_key, ..) => Some((root_key, TreeType::BigSumTree)),
            Element::CountTree(root_key, ..) => Some((root_key, TreeType::CountTree)),
            Element::CountSumTree(root_key, ..) => Some((root_key, TreeType::CountSumTree)),
            Element::ProvableCountTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountTree))
            }
            Element::ProvableCountSumTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountSumTree))
            }
            Element::CommitmentTree(_, chunk_power, _) => {
                Some((&NONE_ROOT_KEY, TreeType::CommitmentTree(*chunk_power)))
            }
            Element::MmrTree(..) => Some((&NONE_ROOT_KEY, TreeType::MmrTree)),
            Element::BulkAppendTree(_, chunk_power, _) => {
                Some((&NONE_ROOT_KEY, TreeType::BulkAppendTree(*chunk_power)))
            }
            Element::DenseAppendOnlyFixedSizeTree(_, height, _) => Some((
                &NONE_ROOT_KEY,
                TreeType::DenseAppendOnlyFixedSizeTree(*height),
            )),
            Element::NonCounted(inner) | Element::NotSummed(inner) => {
                inner.root_key_and_tree_type()
            }
            _ => None,
        }
    }

    /// Check if the element is a tree and return the flags and the tree type.
    /// Looks through `NonCounted`.
    fn tree_flags_and_type(&self) -> Option<(&Option<ElementFlags>, TreeType)> {
        match self {
            Element::Tree(_, flags) => Some((flags, TreeType::NormalTree)),
            Element::SumTree(_, _, flags) => Some((flags, TreeType::SumTree)),
            Element::BigSumTree(_, _, flags) => Some((flags, TreeType::BigSumTree)),
            Element::CountTree(_, _, flags) => Some((flags, TreeType::CountTree)),
            Element::CountSumTree(.., flags) => Some((flags, TreeType::CountSumTree)),
            Element::ProvableCountTree(_, _, flags) => Some((flags, TreeType::ProvableCountTree)),
            Element::ProvableCountSumTree(.., flags) => {
                Some((flags, TreeType::ProvableCountSumTree))
            }
            Element::CommitmentTree(_, chunk_power, flags) => {
                Some((flags, TreeType::CommitmentTree(*chunk_power)))
            }
            Element::MmrTree(.., flags) => Some((flags, TreeType::MmrTree)),
            Element::BulkAppendTree(_, chunk_power, flags) => {
                Some((flags, TreeType::BulkAppendTree(*chunk_power)))
            }
            Element::DenseAppendOnlyFixedSizeTree(_, height, flags) => {
                Some((flags, TreeType::DenseAppendOnlyFixedSizeTree(*height)))
            }
            Element::NonCounted(inner) | Element::NotSummed(inner) => inner.tree_flags_and_type(),
            _ => None,
        }
    }

    /// Check if the element is a tree and return the tree type. Looks through
    /// `NonCounted`.
    fn tree_type(&self) -> Option<TreeType> {
        match self {
            Element::Tree(..) => Some(TreeType::NormalTree),
            Element::SumTree(..) => Some(TreeType::SumTree),
            Element::BigSumTree(..) => Some(TreeType::BigSumTree),
            Element::CountTree(..) => Some(TreeType::CountTree),
            Element::CountSumTree(..) => Some(TreeType::CountSumTree),
            Element::ProvableCountTree(..) => Some(TreeType::ProvableCountTree),
            Element::ProvableCountSumTree(..) => Some(TreeType::ProvableCountSumTree),
            Element::CommitmentTree(_, chunk_power, _) => {
                Some(TreeType::CommitmentTree(*chunk_power))
            }
            Element::MmrTree(..) => Some(TreeType::MmrTree),
            Element::BulkAppendTree(_, chunk_power, _) => {
                Some(TreeType::BulkAppendTree(*chunk_power))
            }
            Element::DenseAppendOnlyFixedSizeTree(_, height, _) => {
                Some(TreeType::DenseAppendOnlyFixedSizeTree(*height))
            }
            Element::NonCounted(inner) | Element::NotSummed(inner) => inner.tree_type(),
            _ => None,
        }
    }

    /// Check if the element is a tree and return the aggregate of elements in
    /// the tree. Looks through `NonCounted`.
    fn tree_feature_type(&self) -> Option<TreeFeatureType> {
        match self {
            Element::Tree(..) => Some(BasicMerkNode),
            Element::SumTree(_, value, _) => Some(SummedMerkNode(*value)),
            Element::BigSumTree(_, value, _) => Some(BigSummedMerkNode(*value)),
            Element::CountTree(_, value, _) => Some(CountedMerkNode(*value)),
            Element::CountSumTree(_, count, sum, _) => Some(CountedSummedMerkNode(*count, *sum)),
            Element::ProvableCountTree(_, value, _) => {
                Some(TreeFeatureType::ProvableCountedMerkNode(*value))
            }
            Element::ProvableCountSumTree(_, count, sum, _) => {
                Some(TreeFeatureType::ProvableCountedSummedMerkNode(*count, *sum))
            }
            Element::CommitmentTree(..) => Some(BasicMerkNode),
            Element::MmrTree(..) => Some(BasicMerkNode),
            Element::BulkAppendTree(..) => Some(BasicMerkNode),
            Element::DenseAppendOnlyFixedSizeTree(..) => Some(BasicMerkNode),
            Element::NonCounted(inner) | Element::NotSummed(inner) => inner.tree_feature_type(),
            _ => None,
        }
    }

    /// Check if the element is a tree and return the tree type. Looks through
    /// `NonCounted`.
    fn maybe_tree_type(&self) -> MaybeTree {
        match self {
            Element::Tree(..) => MaybeTree::Tree(TreeType::NormalTree),
            Element::SumTree(..) => MaybeTree::Tree(TreeType::SumTree),
            Element::BigSumTree(..) => MaybeTree::Tree(TreeType::BigSumTree),
            Element::CountTree(..) => MaybeTree::Tree(TreeType::CountTree),
            Element::CountSumTree(..) => MaybeTree::Tree(TreeType::CountSumTree),
            Element::ProvableCountTree(..) => MaybeTree::Tree(TreeType::ProvableCountTree),
            Element::ProvableCountSumTree(..) => MaybeTree::Tree(TreeType::ProvableCountSumTree),
            Element::CommitmentTree(_, chunk_power, _) => {
                MaybeTree::Tree(TreeType::CommitmentTree(*chunk_power))
            }
            Element::MmrTree(..) => MaybeTree::Tree(TreeType::MmrTree),
            Element::BulkAppendTree(_, chunk_power, _) => {
                MaybeTree::Tree(TreeType::BulkAppendTree(*chunk_power))
            }
            Element::DenseAppendOnlyFixedSizeTree(_, height, _) => {
                MaybeTree::Tree(TreeType::DenseAppendOnlyFixedSizeTree(*height))
            }
            Element::NonCounted(inner) | Element::NotSummed(inner) => inner.maybe_tree_type(),
            _ => MaybeTree::NotTree,
        }
    }

    /// Get the tree feature type.
    ///
    /// `count_value_or_default` and `count_sum_value_or_default` already
    /// return 0 (resp. (0, inner_sum)) for `Element::NonCounted`, and
    /// `sum_value_or_default` / `big_sum_value_or_default` /
    /// `count_sum_value_or_default` already return 0 (resp. (inner_count, 0))
    /// for `Element::NotSummed`. So the existing dispatch produces the right
    /// feature type for either wrapper without an explicit branch here.
    fn get_feature_type(&self, parent_tree_type: TreeType) -> Result<TreeFeatureType, Error> {
        match parent_tree_type {
            TreeType::NormalTree => Ok(BasicMerkNode),
            TreeType::CommitmentTree(_) => Ok(BasicMerkNode),
            TreeType::SumTree => Ok(SummedMerkNode(self.sum_value_or_default())),
            TreeType::BigSumTree => Ok(BigSummedMerkNode(self.big_sum_value_or_default())),
            TreeType::CountTree => Ok(CountedMerkNode(self.count_value_or_default())),
            TreeType::CountSumTree => {
                let v = self.count_sum_value_or_default();
                Ok(CountedSummedMerkNode(v.0, v.1))
            }
            TreeType::ProvableCountTree => Ok(TreeFeatureType::ProvableCountedMerkNode(
                self.count_value_or_default(),
            )),
            TreeType::ProvableCountSumTree => {
                let v = self.count_sum_value_or_default();
                Ok(TreeFeatureType::ProvableCountedSummedMerkNode(v.0, v.1))
            }
            TreeType::MmrTree => Ok(BasicMerkNode),
            TreeType::BulkAppendTree(_) => Ok(BasicMerkNode),
            TreeType::DenseAppendOnlyFixedSizeTree(_) => Ok(BasicMerkNode),
            // Phase 1: ProvableSumTree aggregates the same i64 sum as a
            // plain SumTree but uses the new `ProvableSummedMerkNode`
            // feature type. Phase 2 will diverge the hash.
            TreeType::ProvableSumTree => Ok(TreeFeatureType::ProvableSummedMerkNode(
                self.sum_value_or_default(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        element::costs::ElementCostExtensions,
        tree::kv::ValueDefinedCostType::SpecializedValueDefinedCost,
    };

    #[test]
    fn item_with_sum_item_helpers_cover_all_behaviors() {
        let grove_version = GroveVersion::latest();
        let flags = Some(vec![1, 2, 3]);
        let element = Element::ItemWithSumItem(b"payload".to_vec(), 42, flags.clone());

        assert!(element.is_any_item());
        assert!(element.has_basic_item());
        assert!(element.is_sum_item());
        assert!(element.is_item_with_sum_item());
        assert_eq!(element.sum_value_or_default(), 42);
        assert_eq!(element.count_sum_value_or_default(), (1, 42));
        assert_eq!(element.big_sum_value_or_default(), 42);
        assert_eq!(element.as_item_bytes().unwrap(), b"payload");
        assert_eq!(
            element.clone().into_item_bytes().unwrap(),
            b"payload".to_vec()
        );
        assert_eq!(element.as_sum_item_value().unwrap(), 42);
        assert_eq!(element.clone().into_sum_item_value().unwrap(), 42);
        assert_eq!(element.get_flags(), &flags);

        let serialized = element.serialize(grove_version).expect("serialize element");
        let deserialized =
            Element::deserialize(&serialized, grove_version).expect("deserialize element");
        assert_eq!(deserialized, element);

        let explicit_cost = element.value_defined_cost(grove_version).unwrap();
        let derived_cost =
            Element::value_defined_cost_for_serialized_value(&serialized, grove_version)
                .expect("cost for serialized element");
        match (explicit_cost, derived_cost) {
            (SpecializedValueDefinedCost(explicit), SpecializedValueDefinedCost(derived)) => {
                assert!(explicit > 0);
                assert_eq!(explicit, derived);
            }
            _ => panic!("unexpected cost type"),
        }
    }

    #[test]
    fn tree_type_extensions_look_through_not_summed() {
        // All ElementTreeTypeExtensions methods must delegate through
        // NotSummed to the inner sum-tree variant, mirroring NonCounted.
        let inner_root = Some(b"r".to_vec());
        let cases: [(Element, TreeType); 4] = [
            (
                Element::SumTree(inner_root.clone(), 100, None),
                TreeType::SumTree,
            ),
            (
                Element::BigSumTree(inner_root.clone(), 100, None),
                TreeType::BigSumTree,
            ),
            (
                Element::CountSumTree(inner_root.clone(), 7, 100, None),
                TreeType::CountSumTree,
            ),
            (
                Element::ProvableCountSumTree(inner_root.clone(), 7, 100, None),
                TreeType::ProvableCountSumTree,
            ),
        ];

        for (inner, expected_tree_type) in cases {
            let wrapped = Element::new_not_summed(inner.clone()).expect("wrap ok");

            // tree_type() / maybe_tree_type() / root_key_and_tree_type{,_owned}
            // all return the inner's tree type.
            assert_eq!(wrapped.tree_type(), Some(expected_tree_type));
            assert_eq!(
                wrapped.maybe_tree_type(),
                MaybeTree::Tree(expected_tree_type)
            );
            let (rk, tt) = wrapped.root_key_and_tree_type().expect("Some");
            assert_eq!(*rk, inner_root);
            assert_eq!(tt, expected_tree_type);
            let (rk, tt) = wrapped
                .clone()
                .root_key_and_tree_type_owned()
                .expect("Some");
            assert_eq!(rk, inner_root);
            assert_eq!(tt, expected_tree_type);

            // tree_flags_and_type returns the inner's flags (None) and type.
            let (flags, tt) = wrapped.tree_flags_and_type().expect("Some");
            assert!(flags.is_none());
            assert_eq!(tt, expected_tree_type);

            // tree_feature_type returns the inner's feature type unchanged
            // (it is the per-element-type discriminant, not the parent
            // aggregation — that's `get_feature_type` below).
            assert!(wrapped.tree_feature_type().is_some());
        }
    }

    #[test]
    fn get_feature_type_zeros_sum_for_not_summed_in_sum_parents() {
        // Every sum-bearing parent type must zero out the wrapped sum
        // through `get_feature_type`. Counts (in CountSumTree /
        // ProvableCountSumTree) still propagate.
        let inner = Element::SumTree(None, 100, None);
        let ns = Element::new_not_summed(inner).expect("wrap ok");

        assert_eq!(
            ns.get_feature_type(TreeType::SumTree).unwrap(),
            SummedMerkNode(0)
        );
        assert_eq!(
            ns.get_feature_type(TreeType::BigSumTree).unwrap(),
            BigSummedMerkNode(0)
        );

        // CountSumTree parent: sum=0, count=1 (the wrapped tree counts as
        // one element).
        assert_eq!(
            ns.get_feature_type(TreeType::CountSumTree).unwrap(),
            CountedSummedMerkNode(1, 0)
        );

        // ProvableCountSumTree: same as above, just provable variant.
        match ns.get_feature_type(TreeType::ProvableCountSumTree).unwrap() {
            TreeFeatureType::ProvableCountedSummedMerkNode(c, s) => {
                assert_eq!((c, s), (1, 0));
            }
            other => panic!("expected ProvableCountedSummedMerkNode, got {:?}", other),
        }
    }
}
