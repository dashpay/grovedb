use std::collections::{BTreeSet, LinkedList};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_storage::StorageContext;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    merk::KeyUpdates,
    tree::{kv::ValueDefinedCostType, AuxMerkBatch, Fetch, TreeNode},
    tree_type::TreeType,
    Error, Link, Merk, TreeFeatureType,
};

/// The feature type of a node that keeps a 64-bit sum, with the same count
/// and the sum in 128 bits. `None` for a feature type that keeps no 64-bit
/// sum, and for `ProvableCountedSummedMerkNode`, which has no counterpart.
fn widened_feature_type(feature_type: TreeFeatureType) -> Option<TreeFeatureType> {
    match feature_type {
        TreeFeatureType::SummedMerkNode(sum) => {
            Some(TreeFeatureType::BigSummedMerkNode(i128::from(sum)))
        }
        TreeFeatureType::CountedSummedMerkNode(count, sum) => Some(
            TreeFeatureType::CountedBigSummedMerkNode(count, i128::from(sum)),
        ),
        TreeFeatureType::ProvableSummedMerkNode(sum) => {
            Some(TreeFeatureType::ProvableBigSummedMerkNode(i128::from(sum)))
        }
        TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => Some(
            TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(count, i128::from(sum)),
        ),
        TreeFeatureType::BasicMerkNode
        | TreeFeatureType::BigSummedMerkNode(_)
        | TreeFeatureType::CountedMerkNode(_)
        | TreeFeatureType::ProvableCountedMerkNode(_)
        | TreeFeatureType::ProvableCountedSummedMerkNode(..)
        | TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(..)
        | TreeFeatureType::CountedBigSummedMerkNode(..)
        | TreeFeatureType::ProvableBigSummedMerkNode(_) => None,
    }
}

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Rewrites this tree, which keeps a 64-bit sum, as the tree type that
    /// holds the same nodes with the sum in 128 bits
    /// ([`TreeType::big_sum_counterpart`]), and sets [`Merk::tree_type`] to
    /// it.
    ///
    /// A node records its own contribution and each child link records the
    /// child's aggregate, both at the width of the tree's sum, so every node
    /// of the tree is read and written again. Each node keeps the count and
    /// the sum it contributes, with the sum widened; keys, values, value
    /// hashes and the shape of the tree stay as they are. A tree whose sum
    /// is in its node hashes gets new node hashes and a new root hash. The
    /// other trees keep theirs.
    ///
    /// The values are opaque here, as they are to [`Merk::apply`]. What
    /// they mean is the caller's:
    ///
    /// - `check_node` is called with each node's key, its value and the
    ///   feature type the node is about to take, before anything changes,
    ///   and an error from it refuses the conversion. A caller whose values
    ///   decide what a node contributes checks here that the value agrees
    ///   with the widened feature type under the wide tree type.
    /// - `old_specialized_cost` gives the storage cost of a node as it is
    ///   stored, under the tree type the Merk has now, from its key and
    ///   value: what [`Merk::apply_with_specialized_costs`] takes. Each node
    ///   is billed against it, whether or not this handle read the node from
    ///   storage, so the cost reports the growth as added bytes.
    /// - `value_defined_cost_fn` is the one the Merk was opened with.
    ///
    /// The whole tree is loaded and checked before any node changes: a
    /// refusal or a failed read leaves the Merk as it was. The tree is held
    /// in memory until the write.
    ///
    /// Available where
    /// `operations.insert.convert_to_big_sum_tree` is enabled.
    ///
    /// The caller owns what lies outside this Merk: the element that
    /// describes the tree in its parent, and an indexed tree's secondaries.
    pub fn convert_to_big_sum_tree<V>(
        &mut self,
        check_node: &impl Fn(&[u8], &[u8], TreeFeatureType) -> Result<(), Error>,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        value_defined_cost_fn: Option<&V>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        V: Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
    {
        let mut cost = OperationCost::default();

        let slot = grove_version
            .grovedb_versions
            .operations
            .insert
            .convert_to_big_sum_tree;
        if slot != 1 {
            return Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "convert_to_big_sum_tree".to_string(),
                    known_versions: vec![1],
                    received: slot,
                },
            ))
            .wrap_with_cost(cost);
        }

        let narrow_tree_type = self.tree_type;
        let Some(wide_tree_type) = narrow_tree_type.big_sum_counterpart() else {
            return Err(Error::InvalidOperation(
                "only a tree that keeps a 64-bit sum and has a counterpart with a 128-bit sum \
                 can be converted to a big sum tree",
            ))
            .wrap_with_cost(cost);
        };

        // Load and check everything first, so that a refusal changes nothing.
        let source = self.source();
        cost_return_on_error!(
            &mut cost,
            self.use_tree_mut(|maybe_root| match maybe_root {
                Some(root) => load_and_check(
                    root,
                    &source,
                    narrow_tree_type,
                    check_node,
                    value_defined_cost_fn,
                    grove_version,
                ),
                None => Ok(()).wrap_with_cost(OperationCost::default()),
            })
        );

        cost_return_on_error_no_add!(
            cost,
            self.use_tree_mut(|maybe_root| match maybe_root {
                Some(root) => widen_loaded(root),
                None => Ok(()),
            })
        );
        self.tree_type = wide_tree_type;

        // No key is added, removed or moved, so the root key stays and only
        // the rewritten nodes are written.
        let key_updates =
            KeyUpdates::new(BTreeSet::new(), BTreeSet::new(), LinkedList::new(), None);
        let aux: &AuxMerkBatch<Vec<u8>> = &[];
        self.commit(key_updates, aux, None, old_specialized_cost, grove_version)
            .add_cost(cost)
    }
}

/// Loads `node`'s whole subtree into memory and checks each node: it must
/// carry the feature type of the tree it is in, and the caller's check must
/// accept the feature type it is about to take. Changes nothing but which
/// nodes are in memory.
fn load_and_check<S: Fetch, V>(
    node: &mut TreeNode,
    source: &S,
    narrow_tree_type: TreeType,
    check_node: &impl Fn(&[u8], &[u8], TreeFeatureType) -> Result<(), Error>,
    value_defined_cost_fn: Option<&V>,
    grove_version: &GroveVersion,
) -> CostResult<(), Error>
where
    V: Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
{
    let mut cost = OperationCost::default();

    let feature_type = node.feature_type();
    let widened = match widened_feature_type(feature_type) {
        Some(widened) if feature_type.node_type() == narrow_tree_type.inner_node_type() => widened,
        _ => {
            return Err(Error::CorruptedState(
                "a node does not carry the feature type of the tree it is in",
            ))
            .wrap_with_cost(cost);
        }
    };
    cost_return_on_error_no_add!(cost, check_node(node.key(), node.value_as_slice(), widened));

    for left in [true, false] {
        if matches!(node.link(left), Some(Link::Reference { .. })) {
            cost_return_on_error!(
                &mut cost,
                node.load(left, source, value_defined_cost_fn, grove_version)
            );
        }
        if let Some(child) = node.child_mut(left) {
            cost_return_on_error!(
                &mut cost,
                load_and_check(
                    child,
                    source,
                    narrow_tree_type,
                    check_node,
                    value_defined_cost_fn,
                    grove_version,
                )
            );
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Gives `node` and every node under it its widened feature type, and marks
/// each link as modified so that the commit writes the node and computes its
/// aggregate and hash again. The subtree is in memory and checked
/// ([`load_and_check`]), so nothing here reads storage.
fn widen_loaded(node: &mut TreeNode) -> Result<(), Error> {
    for left in [true, false] {
        let Some(link) = node.slot_mut(left).take() else {
            continue;
        };
        match link {
            Link::Loaded { tree, .. }
            | Link::Modified { tree, .. }
            | Link::Uncommitted { tree, .. } => {
                let mut child = tree;
                let widened = widen_loaded(&mut child);
                *node.slot_mut(left) = Link::maybe_from_modified_tree(Some(child));
                widened?;
            }
            reference @ Link::Reference { .. } => {
                *node.slot_mut(left) = Some(reference);
                return Err(Error::CorruptedState(
                    "a child link is a reference after its subtree was loaded",
                ));
            }
        }
    }

    let widened = widened_feature_type(node.feature_type()).ok_or(Error::CorruptedState(
        "a node lost the feature type it was checked with",
    ))?;
    // The commit bills a node against its old value. A node this handle
    // wrote itself has none, or the one it replaced: what is stored is the
    // value it holds now, which the conversion keeps.
    if node.old_value.as_ref() != Some(node.value_ref()) {
        node.old_value = Some(node.value_ref().clone());
    }
    node.known_storage_cost = None;
    node.set_feature_type(widened);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;

    use grovedb_costs::OperationCost;
    use grovedb_element::Element;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{
            test_utils::TempStorage, PrefixedRocksDbImmediateStorageContext, RocksDbStorage,
        },
        Storage, StorageContext,
    };
    use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

    use crate::{
        element::{
            costs::ElementCostExtensions, insert::ElementInsertToStorageExtensions,
            tree_type::ElementTreeTypeExtensions,
        },
        test_utils::TempMerk,
        tree::{kv::ValueDefinedCostType, AggregateData},
        tree_type::TreeType,
        Error, Merk, Op, TreeFeatureType,
    };

    const PAIRS: [(TreeType, TreeType); 6] = [
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
    ];

    /// Converts a Merk whose values are elements, as GroveDB does: a node's
    /// element must contribute under the wide tree type what the node
    /// contributes now.
    fn convert<'db, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        grove_version: &GroveVersion,
    ) -> (Result<(), Error>, OperationCost) {
        let narrow = merk.tree_type;
        let wide = narrow.big_sum_counterpart();
        let converted = merk.convert_to_big_sum_tree(
            &|_key, value, widened| {
                let element = Element::deserialize(value, grove_version)?;
                let wide = wide.ok_or(Error::InvalidOperation("no counterpart"))?;
                if element.get_feature_type(wide)? == widened {
                    Ok(())
                } else {
                    Err(Error::InvalidInputError(
                        "the element contributes otherwise",
                    ))
                }
            },
            &|key, value| {
                Element::specialized_costs_for_key_value(
                    key,
                    value,
                    narrow.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        );
        (converted.value, converted.cost)
    }

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
        for (narrow, wide) in PAIRS {
            let mut converted = filled(narrow, grove_version);
            let narrow_root = converted.root_hash().unwrap();
            convert(&mut converted, grove_version).0.expect("convert");
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
        convert(&mut merk, grove_version).0.expect("convert");
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
            let result = convert(&mut merk, grove_version).0;
            assert!(
                matches!(result, Err(Error::InvalidOperation(_))),
                "{tree_type}: {result:?}"
            );
            assert_eq!(merk.deref().tree_type, tree_type);
        }
    }

    /// Opaque values are widened by what each node records, with no reading
    /// of the value: the sum the caller supplied on the put is the sum the
    /// node keeps.
    #[test]
    fn a_node_keeps_the_contribution_it_was_stored_with() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountSumTree);
        let batch: Vec<(Vec<u8>, Op)> = (0u8..9)
            .map(|i| {
                (
                    vec![i],
                    Op::Put(
                        vec![0xff; 5],
                        TreeFeatureType::CountedSummedMerkNode(2, i64::from(i) - 3),
                    ),
                )
            })
            .collect();
        merk.apply::<_, Vec<_>>(&batch, &[], None, grove_version)
            .unwrap()
            .expect("apply");
        assert_eq!(
            merk.aggregate_data().expect("aggregate"),
            AggregateData::CountAndSum(18, 9)
        );

        merk.convert_to_big_sum_tree(
            &|_, _, _| Ok(()),
            &|_, _| Ok(0),
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .expect("convert");

        assert_eq!(merk.deref().tree_type, TreeType::CountBigSumTree);
        assert_eq!(
            merk.aggregate_data().expect("aggregate"),
            AggregateData::CountAndBigSum(18, 9)
        );
    }

    /// A refusal by the caller's check comes before any node changes, so the
    /// Merk is the one it was and takes further writes.
    #[test]
    fn a_refused_conversion_leaves_the_merk_as_it_was() {
        let grove_version = GroveVersion::latest();
        let mut merk = filled(TreeType::ProvableSumTree, grove_version);
        // Under a tree with a 64-bit sum a big sum tree adds nothing, and
        // under one with a 128-bit sum it adds its sum.
        Element::BigSumTree(None, 5, None)
            .insert_subtree(&mut merk, [3, 3], [9; 32], None, grove_version)
            .unwrap()
            .expect("insert a big sum tree");
        let root = merk.root_hash().unwrap();
        let aggregate = merk.aggregate_data().expect("aggregate");

        let result = convert(&mut merk, grove_version).0;
        assert!(
            matches!(result, Err(Error::InvalidInputError(_))),
            "got {result:?}"
        );
        assert_eq!(merk.deref().tree_type, TreeType::ProvableSumTree);
        assert_eq!(merk.root_hash().unwrap(), root);
        assert_eq!(merk.aggregate_data().expect("aggregate"), aggregate);

        Element::new_sum_item(4)
            .insert(&mut merk, [4, 4], None, grove_version)
            .unwrap()
            .expect("the Merk still takes writes");
        assert_eq!(
            merk.aggregate_data().expect("aggregate"),
            AggregateData::ProvableSum(aggregate.as_sum_i64() + 4)
        );
    }

    #[test]
    fn the_conversion_is_gated_to_the_version_that_introduces_it() {
        let mut merk = filled(TreeType::CountSumTree, &GROVE_V4);
        let root = merk.root_hash().unwrap();

        let result = convert(&mut merk, &GROVE_V4).0;
        assert!(
            matches!(result, Err(Error::VersionError(_))),
            "got {result:?}"
        );
        assert_eq!(merk.deref().tree_type, TreeType::CountSumTree);
        assert_eq!(merk.root_hash().unwrap(), root);
        assert!(matches!(
            merk.aggregate_data().expect("aggregate"),
            AggregateData::CountAndSum(..)
        ));
    }

    /// A node is billed against what is stored, whether the handle read it
    /// from storage or wrote it itself and still holds it.
    #[test]
    fn a_handle_that_wrote_the_nodes_is_billed_as_one_that_read_them() {
        let grove_version = GroveVersion::latest();
        let open =
            |storage: &'static TempStorage,
             tx: &'static <RocksDbStorage as Storage<'static>>::Transaction| {
                Merk::open_base(
                    storage
                        .get_immediate_storage_context(SubtreePath::empty(), tx)
                        .unwrap(),
                    TreeType::SumTree,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .unwrap()
                .expect("open")
            };
        let fill = |merk: &mut Merk<PrefixedRocksDbImmediateStorageContext<'static>>| {
            for i in 0..12u8 {
                Element::new_sum_item(i64::from(i))
                    .insert(merk, [i.wrapping_mul(29)], None, grove_version)
                    .unwrap()
                    .expect("insert a sum item");
            }
        };
        // An immediate context writes as the Merk commits, so the cost of
        // the conversion is the cost its call returns.
        let leak = || {
            let storage: &'static TempStorage = Box::leak(Box::new(TempStorage::new()));
            let tx: &'static _ = Box::leak(Box::new(storage.start_transaction()));
            (storage, tx)
        };

        let (storage, tx) = leak();
        let mut retained = open(storage, tx);
        fill(&mut retained);
        let (result, retained_cost) = convert(&mut retained, grove_version);
        result.expect("convert the handle that wrote the nodes");

        let (storage, tx) = leak();
        let mut writer = open(storage, tx);
        fill(&mut writer);
        drop(writer);
        let mut reopened = open(storage, tx);
        let (result, reopened_cost) = convert(&mut reopened, grove_version);
        result.expect("convert a reopened handle");

        assert_eq!(
            retained_cost.storage_cost, reopened_cost.storage_cost,
            "the storage cost does not depend on who wrote the nodes"
        );
        // 8 bytes in each node and 8 in the link to it, and nothing billed
        // as new.
        assert_eq!(retained_cost.storage_cost.added_bytes, 12 * 16);
        assert_eq!(retained.root_hash().unwrap(), reopened.root_hash().unwrap());
    }
}
