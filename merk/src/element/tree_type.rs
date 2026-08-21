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
            Element::ProvableSumTree(root_key, ..) => Some((root_key, TreeType::ProvableSumTree)),
            Element::ProvableCountProvableSumTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountProvableSumTree))
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
            // For indexed trees, return the primary root key and the tree
            // type. Secondary root keys (or the axes TLV for PCPSIT) are
            // part of the element bytes but are not surfaced through this
            // single-root-key API — callers needing them must read the
            // element bytes directly.
            Element::ProvableSumIndexedTree(primary_root_key, ..) => {
                Some((primary_root_key, TreeType::ProvableSumIndexedTree))
            }
            Element::ProvableCountIndexedTree(primary_root_key, ..) => {
                Some((primary_root_key, TreeType::ProvableCountIndexedTree))
            }
            Element::ProvableCountProvableSumIndexedTree(primary_root_key, ..) => Some((
                primary_root_key,
                TreeType::ProvableCountProvableSumIndexedTree,
            )),
            Element::PrivateDocumentStore(_, _, chunk_power, _) => {
                Some((None, TreeType::PrivateDocumentStore(chunk_power)))
            }
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.root_key_and_tree_type_owned(),
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
            Element::ProvableSumTree(root_key, ..) => Some((root_key, TreeType::ProvableSumTree)),
            Element::ProvableCountProvableSumTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountProvableSumTree))
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
            Element::ProvableSumIndexedTree(primary_root_key, ..) => {
                Some((primary_root_key, TreeType::ProvableSumIndexedTree))
            }
            Element::ProvableCountIndexedTree(primary_root_key, ..) => {
                Some((primary_root_key, TreeType::ProvableCountIndexedTree))
            }
            Element::ProvableCountProvableSumIndexedTree(primary_root_key, ..) => Some((
                primary_root_key,
                TreeType::ProvableCountProvableSumIndexedTree,
            )),
            Element::PrivateDocumentStore(_, _, chunk_power, _) => {
                Some((&NONE_ROOT_KEY, TreeType::PrivateDocumentStore(*chunk_power)))
            }
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.root_key_and_tree_type(),
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
            Element::ProvableSumTree(_, _, flags) => Some((flags, TreeType::ProvableSumTree)),
            Element::ProvableCountProvableSumTree(_, _, _, flags) => {
                Some((flags, TreeType::ProvableCountProvableSumTree))
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
            Element::ProvableSumIndexedTree(.., flags) => {
                Some((flags, TreeType::ProvableSumIndexedTree))
            }
            Element::ProvableCountIndexedTree(.., flags) => {
                Some((flags, TreeType::ProvableCountIndexedTree))
            }
            // PCPSIT's flags are the trailing field after `axes`.
            Element::ProvableCountProvableSumIndexedTree(_, _, _, _, flags) => {
                Some((flags, TreeType::ProvableCountProvableSumIndexedTree))
            }
            Element::PrivateDocumentStore(_, _, chunk_power, flags) => {
                Some((flags, TreeType::PrivateDocumentStore(*chunk_power)))
            }
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.tree_flags_and_type(),
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
            Element::ProvableSumTree(..) => Some(TreeType::ProvableSumTree),
            Element::ProvableCountProvableSumTree(..) => {
                Some(TreeType::ProvableCountProvableSumTree)
            }
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
            Element::ProvableSumIndexedTree(..) => Some(TreeType::ProvableSumIndexedTree),
            Element::ProvableCountIndexedTree(..) => Some(TreeType::ProvableCountIndexedTree),
            Element::ProvableCountProvableSumIndexedTree(..) => {
                Some(TreeType::ProvableCountProvableSumIndexedTree)
            }
            Element::PrivateDocumentStore(_, _, chunk_power, _) => {
                Some(TreeType::PrivateDocumentStore(*chunk_power))
            }
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.tree_type(),
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
            Element::ProvableSumTree(_, value, _) => {
                Some(TreeFeatureType::ProvableSummedMerkNode(*value))
            }
            Element::ProvableCountProvableSumTree(_, count, sum, _) => Some(
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(*count, *sum),
            ),
            Element::CommitmentTree(..) => Some(BasicMerkNode),
            Element::MmrTree(..) => Some(BasicMerkNode),
            Element::BulkAppendTree(..) => Some(BasicMerkNode),
            Element::DenseAppendOnlyFixedSizeTree(..) => Some(BasicMerkNode),
            // ProvableSumIndexedTree's primary uses ProvableSummedMerkNode.
            Element::ProvableSumIndexedTree(.., sum_value, _) => {
                Some(TreeFeatureType::ProvableSummedMerkNode(*sum_value))
            }
            // ProvableCountIndexedTree's primary uses ProvableCountedMerkNode.
            Element::ProvableCountIndexedTree(.., count_value, _) => {
                Some(TreeFeatureType::ProvableCountedMerkNode(*count_value))
            }
            // ProvableCountProvableSumIndexedTree's primary uses
            // ProvableCountedAndProvableSummedMerkNode (both axes baked
            // into the node hash).
            Element::ProvableCountProvableSumIndexedTree(_, count_value, sum_value, _, _) => Some(
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(*count_value, *sum_value),
            ),
            Element::PrivateDocumentStore(..) => Some(BasicMerkNode),
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.tree_feature_type(),
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
            Element::ProvableSumTree(..) => MaybeTree::Tree(TreeType::ProvableSumTree),
            Element::ProvableCountProvableSumTree(..) => {
                MaybeTree::Tree(TreeType::ProvableCountProvableSumTree)
            }
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
            Element::ProvableSumIndexedTree(..) => {
                MaybeTree::Tree(TreeType::ProvableSumIndexedTree)
            }
            Element::ProvableCountIndexedTree(..) => {
                MaybeTree::Tree(TreeType::ProvableCountIndexedTree)
            }
            Element::ProvableCountProvableSumIndexedTree(..) => {
                MaybeTree::Tree(TreeType::ProvableCountProvableSumIndexedTree)
            }
            Element::PrivateDocumentStore(_, _, chunk_power, _) => {
                MaybeTree::Tree(TreeType::PrivateDocumentStore(*chunk_power))
            }
            Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) => inner.maybe_tree_type(),
            _ => MaybeTree::NotTree,
        }
    }

    /// Get the tree feature type.
    ///
    /// `count_value_or_default` and `count_sum_value_or_default` already
    /// return 0 (resp. (0, inner_sum)) for `Element::NonCounted`,
    /// `sum_value_or_default` / `big_sum_value_or_default` /
    /// `count_sum_value_or_default` already return 0 (resp. (inner_count, 0))
    /// for `Element::NotSummed`, and all four helpers return 0 (resp.
    /// (0, 0)) for `Element::NotCountedOrSummed`. So the existing dispatch
    /// produces the right feature type for every wrapper without an
    /// explicit branch here.
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
            TreeType::PrivateDocumentStore(_) => Ok(BasicMerkNode),
            // ProvableSumTree aggregates an i64 sum (same arithmetic
            // shape as plain SumTree) but carries it via
            // `ProvableSummedMerkNode` so the sum is baked into every
            // node's hash via `node_hash_with_sum` — making sum
            // tampering catchable through proof verification.
            TreeType::ProvableSumTree => Ok(TreeFeatureType::ProvableSummedMerkNode(
                self.sum_value_or_default(),
            )),
            // ProvableCountProvableSumTree aggregates BOTH a u64 count
            // AND an i64 sum, carried via
            // `ProvableCountedAndProvableSummedMerkNode`. Both aggregates
            // are baked into every node's hash via
            // `node_hash_with_count_and_sum`, enabling both
            // `AggregateCountOnRange` and `AggregateSumOnRange` proofs.
            TreeType::ProvableCountProvableSumTree => {
                let v = self.count_sum_value_or_default();
                Ok(TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    v.0, v.1,
                ))
            }
            // ProvableSumIndexedTree's primary aggregates like ProvableSumTree.
            TreeType::ProvableSumIndexedTree => Ok(TreeFeatureType::ProvableSummedMerkNode(
                self.sum_value_or_default(),
            )),
            // ProvableCountIndexedTree's primary aggregates like ProvableCountTree.
            TreeType::ProvableCountIndexedTree => Ok(TreeFeatureType::ProvableCountedMerkNode(
                self.count_value_or_default(),
            )),
            // ProvableCountProvableSumIndexedTree's primary aggregates like
            // ProvableCountProvableSumTree.
            TreeType::ProvableCountProvableSumIndexedTree => {
                let v = self.count_sum_value_or_default();
                Ok(TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    v.0, v.1,
                ))
            }
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
    fn tree_type_extensions_look_through_not_counted_or_summed() {
        // Every ElementTreeTypeExtensions method must delegate through
        // NotCountedOrSummed to the inner sum-tree variant.
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
            let wrapped = Element::new_not_counted_or_summed(inner.clone()).expect("wrap ok");

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

            let (flags, tt) = wrapped.tree_flags_and_type().expect("Some");
            assert!(flags.is_none());
            assert_eq!(tt, expected_tree_type);

            assert!(wrapped.tree_feature_type().is_some());
        }
    }

    #[test]
    fn get_feature_type_zeros_both_axes_for_not_counted_or_summed() {
        // NotCountedOrSummed must zero out BOTH count and sum in
        // count-and-sum-bearing parents.
        let inner = Element::CountSumTree(None, 7, 100, None);
        let ncos = Element::new_not_counted_or_summed(inner).expect("wrap ok");

        // CountSumTree parent: count=0, sum=0.
        assert_eq!(
            ncos.get_feature_type(TreeType::CountSumTree).unwrap(),
            CountedSummedMerkNode(0, 0)
        );

        // ProvableCountSumTree parent: same.
        match ncos
            .get_feature_type(TreeType::ProvableCountSumTree)
            .unwrap()
        {
            TreeFeatureType::ProvableCountedSummedMerkNode(c, s) => {
                assert_eq!((c, s), (0, 0));
            }
            other => panic!("expected ProvableCountedSummedMerkNode, got {:?}", other),
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

        // Sum-bearing parent: ProvableSumTree must also zero out the wrapped
        // sum so the wrapper semantics stay consistent across the family.
        // The sum-bearing branch uses the `ProvableSummedMerkNode(0)`
        // feature type.
        match ns.get_feature_type(TreeType::ProvableSumTree).unwrap() {
            TreeFeatureType::ProvableSummedMerkNode(s) => assert_eq!(s, 0),
            other => panic!("expected ProvableSummedMerkNode(0), got {:?}", other),
        }
    }

    // -------------------------------------------------------------------
    // Per-variant extension-method coverage: each tree-bearing variant
    // has match arms in `root_key_and_tree_type`, `tree_flags_and_type`,
    // `tree_type`, `tree_feature_type`, and `maybe_tree_type`. The
    // existing tests above don't drive every variant directly; the
    // following do.
    // -------------------------------------------------------------------

    fn assert_provable_sum_tree_arms(e: &Element) {
        let (rk, tt) = e.root_key_and_tree_type().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::ProvableSumTree);

        assert_eq!(e.tree_type(), Some(TreeType::ProvableSumTree));
        assert_eq!(
            e.maybe_tree_type(),
            MaybeTree::Tree(TreeType::ProvableSumTree)
        );
        let (flags, tt) = e.tree_flags_and_type().expect("Some");
        assert_eq!(flags, e.get_flags());
        assert_eq!(tt, TreeType::ProvableSumTree);

        match e.tree_feature_type() {
            Some(TreeFeatureType::ProvableSummedMerkNode(_)) => {}
            other => panic!("expected ProvableSummedMerkNode, got {:?}", other),
        }
    }

    #[test]
    fn provable_sum_tree_extension_arms_direct() {
        // Directly drive every per-variant arm for ProvableSumTree without
        // wrappers, covering the lines that the wrapper-delegation test
        // can't reach.
        let e = Element::ProvableSumTree(None, 42, Some(vec![9, 8]));
        assert_provable_sum_tree_arms(&e);
    }

    #[test]
    fn commitment_tree_extension_arms_direct() {
        // CommitmentTree carries a chunk_power that flows through every
        // helper. Drive the per-variant arms directly.
        let chunk_power = 4u8;
        let e = Element::CommitmentTree(0, chunk_power, Some(vec![1]));

        let (rk, tt) = e.root_key_and_tree_type().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::CommitmentTree(chunk_power));

        assert_eq!(e.tree_type(), Some(TreeType::CommitmentTree(chunk_power)));
        assert_eq!(
            e.maybe_tree_type(),
            MaybeTree::Tree(TreeType::CommitmentTree(chunk_power))
        );

        let (flags, tt) = e.tree_flags_and_type().expect("Some");
        assert!(flags.is_some());
        assert_eq!(tt, TreeType::CommitmentTree(chunk_power));
        assert_eq!(e.tree_feature_type(), Some(BasicMerkNode));
    }

    #[test]
    fn private_document_store_extension_arms_direct() {
        // PrivateDocumentStore carries {entry_size, chunk_power}; only the
        // chunk_power flows into the TreeType. Drive every dispatch arm.
        let entry_size = 64u32;
        let chunk_power = 4u8;
        let e = Element::PrivateDocumentStore(3, entry_size, chunk_power, Some(vec![1]));

        let (rk, tt) = e.root_key_and_tree_type().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::PrivateDocumentStore(chunk_power));

        let (rk, tt) = e.clone().root_key_and_tree_type_owned().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::PrivateDocumentStore(chunk_power));

        assert_eq!(
            e.tree_type(),
            Some(TreeType::PrivateDocumentStore(chunk_power))
        );
        assert_eq!(
            e.maybe_tree_type(),
            MaybeTree::Tree(TreeType::PrivateDocumentStore(chunk_power))
        );

        let (flags, tt) = e.tree_flags_and_type().expect("Some");
        assert!(flags.is_some());
        assert_eq!(tt, TreeType::PrivateDocumentStore(chunk_power));
        assert_eq!(e.tree_feature_type(), Some(BasicMerkNode));

        // Children of a PDS parent (unreachable in practice — inserts are
        // rejected) still resolve to BasicMerkNode for exhaustiveness.
        assert_eq!(
            Element::new_item(b"x".to_vec())
                .get_feature_type(TreeType::PrivateDocumentStore(chunk_power))
                .expect("feature type"),
            BasicMerkNode
        );
    }

    #[test]
    fn bulk_append_tree_extension_arms_direct() {
        let chunk_power = 8u8;
        let e = Element::BulkAppendTree(0, chunk_power, None);

        let (rk, tt) = e.root_key_and_tree_type().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::BulkAppendTree(chunk_power));

        assert_eq!(e.tree_type(), Some(TreeType::BulkAppendTree(chunk_power)));
        assert_eq!(
            e.maybe_tree_type(),
            MaybeTree::Tree(TreeType::BulkAppendTree(chunk_power))
        );
        let (flags, tt) = e.tree_flags_and_type().expect("Some");
        assert!(flags.is_none());
        assert_eq!(tt, TreeType::BulkAppendTree(chunk_power));
        assert_eq!(e.tree_feature_type(), Some(BasicMerkNode));
    }

    #[test]
    fn dense_append_only_tree_extension_arms_direct() {
        let height = 5u8;
        let e = Element::DenseAppendOnlyFixedSizeTree(0, height, Some(vec![]));

        let (rk, tt) = e.root_key_and_tree_type().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::DenseAppendOnlyFixedSizeTree(height));

        assert_eq!(
            e.tree_type(),
            Some(TreeType::DenseAppendOnlyFixedSizeTree(height))
        );
        assert_eq!(
            e.maybe_tree_type(),
            MaybeTree::Tree(TreeType::DenseAppendOnlyFixedSizeTree(height))
        );

        let (flags, tt) = e.tree_flags_and_type().expect("Some");
        assert!(flags.is_some());
        assert_eq!(tt, TreeType::DenseAppendOnlyFixedSizeTree(height));
        assert_eq!(e.tree_feature_type(), Some(BasicMerkNode));
    }

    #[test]
    fn mmr_tree_extension_arms_direct() {
        let e = Element::MmrTree(0, None);
        let (rk, tt) = e.root_key_and_tree_type().expect("Some");
        assert!(rk.is_none());
        assert_eq!(tt, TreeType::MmrTree);

        assert_eq!(e.tree_type(), Some(TreeType::MmrTree));
        assert_eq!(e.maybe_tree_type(), MaybeTree::Tree(TreeType::MmrTree));
        assert_eq!(e.tree_feature_type(), Some(BasicMerkNode));
    }

    #[test]
    fn provable_sum_tree_through_not_summed_wrapper() {
        // Drive the look-through arm for ProvableSumTree specifically.
        let inner = Element::ProvableSumTree(None, 99, None);
        let ns = Element::new_not_summed(inner).expect("wrap ok");
        assert_provable_sum_tree_arms(&ns);
    }

    // =====================================================================
    // Coverage: indexed-tree arms in ElementTreeTypeExtensions methods
    // (mirrors the old cidx coverage block but for the new PSIT variant).
    // =====================================================================

    #[test]
    fn tree_type_extensions_cover_provable_sum_indexed_tree_arms() {
        // Build a ProvableSumIndexedTree (PSIT) and verify every trait
        // method returns the expected PSIT-shaped value.
        let primary_root = Some(b"primary_root".to_vec());
        let secondary_root = Some(b"secondary_root".to_vec());
        let flags = Some(vec![9, 9]);
        let sum_value: i64 = -42;

        let psit = Element::ProvableSumIndexedTree(
            primary_root.clone(),
            secondary_root.clone(),
            sum_value,
            flags.clone(),
        );

        // tree_type()
        assert_eq!(psit.tree_type(), Some(TreeType::ProvableSumIndexedTree));

        // maybe_tree_type()
        assert_eq!(
            psit.maybe_tree_type(),
            MaybeTree::Tree(TreeType::ProvableSumIndexedTree)
        );

        // root_key_and_tree_type() — borrowed primary_root_key
        let (rk, tt) = psit.root_key_and_tree_type().expect("Some");
        assert_eq!(*rk, primary_root);
        assert_eq!(tt, TreeType::ProvableSumIndexedTree);

        // root_key_and_tree_type_owned() — owned primary_root_key
        let (rk_owned, tt) = psit.clone().root_key_and_tree_type_owned().expect("Some");
        assert_eq!(rk_owned, primary_root);
        assert_eq!(tt, TreeType::ProvableSumIndexedTree);

        // tree_flags_and_type()
        let (f, tt) = psit.tree_flags_and_type().expect("Some");
        assert_eq!(*f, flags);
        assert_eq!(tt, TreeType::ProvableSumIndexedTree);

        // tree_feature_type() — ProvableSummedMerkNode with the PSIT's
        // sum_value.
        match psit.tree_feature_type().expect("Some") {
            TreeFeatureType::ProvableSummedMerkNode(s) => assert_eq!(s, sum_value),
            other => panic!("expected ProvableSummedMerkNode, got {:?}", other),
        }
    }

    #[test]
    fn tree_type_extensions_cover_provable_count_indexed_tree_arms() {
        // Mirror of the previous test for ProvableCountIndexedTree.
        let primary_root = Some(b"primary_root".to_vec());
        let secondary_root = Some(b"secondary_root".to_vec());
        let flags = Some(vec![3, 3]);
        let count_value: u64 = 99;

        let pcidx = Element::ProvableCountIndexedTree(
            primary_root.clone(),
            secondary_root.clone(),
            count_value,
            flags.clone(),
        );

        assert_eq!(pcidx.tree_type(), Some(TreeType::ProvableCountIndexedTree));
        assert_eq!(
            pcidx.maybe_tree_type(),
            MaybeTree::Tree(TreeType::ProvableCountIndexedTree)
        );

        let (rk, tt) = pcidx.root_key_and_tree_type().expect("Some");
        assert_eq!(*rk, primary_root);
        assert_eq!(tt, TreeType::ProvableCountIndexedTree);

        let (rk_owned, tt) = pcidx.clone().root_key_and_tree_type_owned().expect("Some");
        assert_eq!(rk_owned, primary_root);
        assert_eq!(tt, TreeType::ProvableCountIndexedTree);

        let (f, tt) = pcidx.tree_flags_and_type().expect("Some");
        assert_eq!(*f, flags);
        assert_eq!(tt, TreeType::ProvableCountIndexedTree);

        match pcidx.tree_feature_type().expect("Some") {
            TreeFeatureType::ProvableCountedMerkNode(c) => assert_eq!(c, count_value),
            other => panic!("expected ProvableCountedMerkNode, got {:?}", other),
        }
    }

    #[test]
    fn tree_type_extensions_look_through_non_counted_wrapping_indexed_tree() {
        // NonCounted-wrapped indexed tree must delegate every trait method
        // to the inner indexed tree. Uses PCIT (count-indexed) as the
        // representative case.
        let primary_root = Some(b"primary_root".to_vec());
        let secondary_root = Some(b"secondary_root".to_vec());
        let count_value: u64 = 17;

        let inner = Element::ProvableCountIndexedTree(
            primary_root.clone(),
            secondary_root.clone(),
            count_value,
            None,
        );
        let wrapped = Element::new_non_counted(inner).expect("wrap");

        assert_eq!(
            wrapped.tree_type(),
            Some(TreeType::ProvableCountIndexedTree)
        );
        assert_eq!(
            wrapped.maybe_tree_type(),
            MaybeTree::Tree(TreeType::ProvableCountIndexedTree)
        );
        let (rk, tt) = wrapped.root_key_and_tree_type().expect("Some");
        assert_eq!(*rk, primary_root);
        assert_eq!(tt, TreeType::ProvableCountIndexedTree);
        // tree_feature_type delegates through the wrapper.
        assert!(wrapped.tree_feature_type().is_some());
    }

    #[test]
    fn tree_type_extensions_cover_provable_count_provable_sum_indexed_tree_arms() {
        // Build a PCPSIT and verify the extension methods return the
        // PCPSIT-shaped value across the board.
        let primary_root = Some(b"primary_root".to_vec());
        let flags = Some(vec![7, 7]);
        let count_value: u64 = 11;
        let sum_value: i64 = 23;
        let axes = vec![(0u8, None), (1u8, Some(b"sec".to_vec()))];

        let pcpsit = Element::ProvableCountProvableSumIndexedTree(
            primary_root.clone(),
            count_value,
            sum_value,
            axes.clone(),
            flags.clone(),
        );

        assert_eq!(
            pcpsit.tree_type(),
            Some(TreeType::ProvableCountProvableSumIndexedTree)
        );
        assert_eq!(
            pcpsit.maybe_tree_type(),
            MaybeTree::Tree(TreeType::ProvableCountProvableSumIndexedTree)
        );

        let (rk, tt) = pcpsit.root_key_and_tree_type().expect("Some");
        assert_eq!(*rk, primary_root);
        assert_eq!(tt, TreeType::ProvableCountProvableSumIndexedTree);

        let (rk_owned, tt) = pcpsit.clone().root_key_and_tree_type_owned().expect("Some");
        assert_eq!(rk_owned, primary_root);
        assert_eq!(tt, TreeType::ProvableCountProvableSumIndexedTree);

        let (f, tt) = pcpsit.tree_flags_and_type().expect("Some");
        assert_eq!(*f, flags);
        assert_eq!(tt, TreeType::ProvableCountProvableSumIndexedTree);

        match pcpsit.tree_feature_type().expect("Some") {
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(c, s) => {
                assert_eq!(c, count_value);
                assert_eq!(s, sum_value);
            }
            other => panic!(
                "expected ProvableCountedAndProvableSummedMerkNode, got {:?}",
                other
            ),
        }
    }
}
