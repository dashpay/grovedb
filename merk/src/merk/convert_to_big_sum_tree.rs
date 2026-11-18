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
/// sum.
///
/// `ProvableCountedSummedMerkNode` has its count in the node hash and its
/// sum beside it. No feature type with a 128-bit sum does, and it takes the
/// one that has both in the hash, as its tree type does
/// ([`TreeType::big_sum_counterpart`]).
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
        TreeFeatureType::ProvableCountedSummedMerkNode(count, sum)
        | TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => Some(
            TreeFeatureType::ProvableCountedAndProvableBigSummedMerkNode(count, i128::from(sum)),
        ),
        TreeFeatureType::BasicMerkNode
        | TreeFeatureType::BigSummedMerkNode(_)
        | TreeFeatureType::CountedMerkNode(_)
        | TreeFeatureType::ProvableCountedMerkNode(_)
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
    /// ([`TreeType::big_sum_counterpart`]), and returns it with
    /// [`Merk::tree_type`] set to that type.
    ///
    /// A node records its own contribution and each child link records the
    /// child's aggregate, both at the width of the tree's sum, so every node
    /// of the tree is read and written again. Each node keeps the count and
    /// the sum it contributes, with the sum widened; keys, values, value
    /// hashes and the shape of the tree stay as they are. A tree whose sum
    /// is in its node hashes gets new node hashes and a new root hash, and
    /// so does a `ProvableCountSumTree`, whose sum enters them. A `SumTree`
    /// and a `CountSumTree` keep theirs.
    ///
    /// The values are opaque here, as they are to [`Merk::apply`]. What
    /// they mean is the caller's:
    ///
    /// - `check_node` is called with each node's key, its value and the
    ///   feature type the node is about to take, and an error from it
    ///   refuses the conversion. A caller whose values decide what a node
    ///   contributes checks here that the value agrees with the widened
    ///   feature type under the wide tree type.
    /// - `old_specialized_cost` gives the storage cost of a node as it is
    ///   stored, under the tree type the Merk has now, from its key and
    ///   value: what [`Merk::apply_with_specialized_costs`] takes. Each node
    ///   is billed against it, whether or not this handle read the node from
    ///   storage, so the cost reports the growth as added bytes.
    /// - `value_defined_cost_fn` is the one the Merk was opened with.
    ///
    /// Nothing is written until every node is checked and widened, and the
    /// whole tree is held in memory until then. The Merk is taken by value
    /// and handed back only once the write is made: after a refusal or a
    /// failure there is no handle left to go on with, and storage is as it
    /// was.
    ///
    /// Available where
    /// `operations.insert.convert_to_big_sum_tree` is enabled.
    ///
    /// The caller owns what lies outside this Merk: the element that
    /// describes the tree in its parent, and an indexed tree's secondaries.
    pub fn convert_to_big_sum_tree<V>(
        mut self,
        check_node: &impl Fn(&[u8], &[u8], TreeFeatureType) -> Result<(), Error>,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        value_defined_cost_fn: Option<&V>,
        grove_version: &GroveVersion,
    ) -> CostResult<Self, Error>
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

        {
            let source = self.source();
            cost_return_on_error!(
                &mut cost,
                self.use_tree_mut(|maybe_root| match maybe_root {
                    Some(root) => widen_node(
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
        }
        self.tree_type = wide_tree_type;

        // No key is added, removed or moved, so the root key stays and only
        // the rewritten nodes are written.
        let key_updates =
            KeyUpdates::new(BTreeSet::new(), BTreeSet::new(), LinkedList::new(), None);
        let aux: &AuxMerkBatch<Vec<u8>> = &[];
        cost_return_on_error!(
            &mut cost,
            self.commit(key_updates, aux, None, old_specialized_cost, grove_version)
        );
        Ok(self).wrap_with_cost(cost)
    }
}

/// Checks `node` and every node under it, gives each its widened feature
/// type, loads the nodes that are not in memory and marks each link as
/// modified so that the commit writes the node and computes its aggregate
/// and hash again.
///
/// A node must carry the feature type of the tree it is in, and the caller's
/// check must accept the feature type it is about to take.
fn widen_node<S: Fetch, V>(
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
            widen_node(
                &mut child,
                source,
                narrow_tree_type,
                check_node,
                value_defined_cost_fn,
                grove_version,
            )
        );
        *node.slot_mut(left) = Link::maybe_from_modified_tree(Some(child));
    }

    // The commit bills a node against its old value. A node this handle
    // wrote itself has none, or the one it replaced: what is stored is the
    // value it holds now, which the conversion keeps.
    if node.old_value.as_ref() != Some(node.value_ref()) {
        node.old_value = Some(node.value_ref().clone());
    }
    node.known_storage_cost = None;
    node.set_feature_type(widened);

    Ok(()).wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_element::Element;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{
            test_utils::TempStorage, PrefixedRocksDbImmediateStorageContext, RocksDbStorage,
        },
        Storage,
    };
    use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

    use crate::{
        element::{
            costs::ElementCostExtensions, insert::ElementInsertToStorageExtensions,
            tree_type::ElementTreeTypeExtensions,
        },
        tree::{kv::ValueDefinedCostType, AggregateData},
        tree_type::TreeType,
        CryptoHash, Error, Merk, Op, TreeFeatureType,
    };

    type Transaction<'a> = <RocksDbStorage as Storage<'a>>::Transaction;
    /// A Merk that writes as it commits, so that the cost of a call is the
    /// cost the call returns.
    type ImmediateMerk<'a> = Merk<PrefixedRocksDbImmediateStorageContext<'a>>;

    const PAIRS: [(TreeType, TreeType); 7] = [
        (TreeType::SumTree, TreeType::BigSumTree),
        (TreeType::CountSumTree, TreeType::CountBigSumTree),
        (TreeType::ProvableSumTree, TreeType::ProvableBigSumTree),
        (
            TreeType::ProvableCountSumTree,
            TreeType::ProvableCountProvableBigSumTree,
        ),
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

    fn open<'a>(
        storage: &'a TempStorage,
        tx: &'a Transaction<'a>,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> ImmediateMerk<'a> {
        Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), tx)
                .unwrap(),
            tree_type,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .unwrap()
        .expect("open")
    }

    /// Converts a Merk whose values are elements, as GroveDB does: a node's
    /// element must contribute under the wide tree type what the node
    /// contributes now.
    fn convert<'a>(
        merk: ImmediateMerk<'a>,
        grove_version: &GroveVersion,
    ) -> (Result<ImmediateMerk<'a>, Error>, OperationCost) {
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

    /// Sum items of both signs, an item and a sum tree element, in an order
    /// that takes the tree several levels deep.
    fn fill(merk: &mut ImmediateMerk<'_>, grove_version: &GroveVersion) {
        for i in 0..40u8 {
            let key = [i.wrapping_mul(37)];
            Element::new_sum_item(i64::from(i) - 10)
                .insert(merk, key, None, grove_version)
                .unwrap()
                .expect("insert a sum item");
        }
        Element::new_item(vec![1, 2, 3])
            .insert(merk, [1, 1], None, grove_version)
            .unwrap()
            .expect("insert an item");
        Element::SumTree(None, 1_000, None)
            .insert_subtree(merk, [2, 2], [7; 32], None, grove_version)
            .unwrap()
            .expect("insert a sum tree");
    }

    /// The root hash and aggregate a tree of `tree_type` has when it is
    /// written with [`fill`].
    fn built(tree_type: TreeType, grove_version: &GroveVersion) -> (CryptoHash, AggregateData) {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut merk = open(&storage, &tx, tree_type, grove_version);
        fill(&mut merk, grove_version);
        (
            merk.root_hash().unwrap(),
            merk.aggregate_data().expect("aggregate"),
        )
    }

    #[test]
    fn a_converted_merk_is_the_merk_built_wide() {
        let grove_version = GroveVersion::latest();
        for (narrow, wide) in PAIRS {
            let storage = TempStorage::new();
            let tx = storage.start_transaction();
            let mut merk = open(&storage, &tx, narrow, grove_version);
            fill(&mut merk, grove_version);
            let narrow_root = merk.root_hash().unwrap();

            let converted = convert(merk, grove_version).0.expect("convert");

            let (wide_root, wide_aggregate) = built(wide, grove_version);
            assert_eq!(converted.tree_type, wide, "{narrow}");
            assert_eq!(
                converted.aggregate_data().expect("aggregate"),
                wide_aggregate,
                "{narrow}"
            );
            assert_eq!(converted.root_hash().unwrap(), wide_root, "{narrow}");
            // A sum in the node hashes moves the root, a sum beside them
            // does not.
            let sum_is_hashed = !matches!(narrow, TreeType::SumTree | TreeType::CountSumTree);
            assert_eq!(wide_root != narrow_root, sum_is_hashed, "{narrow}");

            // What was written is what the handle holds.
            drop(converted);
            let reopened = open(&storage, &tx, wide, grove_version);
            assert_eq!(reopened.root_hash().unwrap(), wide_root, "{narrow}");
        }
    }

    #[test]
    fn an_empty_merk_only_changes_its_type() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let merk = open(&storage, &tx, TreeType::SumTree, grove_version);
        let converted = convert(merk, grove_version).0.expect("convert");
        assert_eq!(converted.tree_type, TreeType::BigSumTree);
        assert_eq!(
            converted.aggregate_data().expect("aggregate"),
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
            TreeType::ProvableCountTree,
        ] {
            let storage = TempStorage::new();
            let tx = storage.start_transaction();
            let merk = open(&storage, &tx, tree_type, grove_version);
            let result = convert(merk, grove_version).0;
            assert!(
                matches!(result, Err(Error::InvalidOperation(_))),
                "{tree_type}: {:?}",
                result.err()
            );
        }
    }

    /// Opaque values are widened by what each node records, with no reading
    /// of the value: the sum the caller supplied on the put is the sum the
    /// node keeps.
    #[test]
    fn a_node_keeps_the_contribution_it_was_stored_with() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut merk = open(&storage, &tx, TreeType::CountSumTree, grove_version);
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

        let converted = merk
            .convert_to_big_sum_tree(
                &|_, _, _| Ok(()),
                &|_, _| Ok(0),
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
            .unwrap()
            .expect("convert");

        assert_eq!(converted.tree_type, TreeType::CountBigSumTree);
        assert_eq!(
            converted.aggregate_data().expect("aggregate"),
            AggregateData::CountAndBigSum(18, 9)
        );
    }

    /// A refusal takes the handle and writes nothing: the tree read back
    /// from storage is the one that was there, and it takes further writes.
    #[test]
    fn a_refused_conversion_writes_nothing() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut merk = open(&storage, &tx, TreeType::ProvableSumTree, grove_version);
        fill(&mut merk, grove_version);
        // Under a tree with a 64-bit sum a big sum tree adds nothing, and
        // under one with a 128-bit sum it adds its sum.
        Element::BigSumTree(None, 5, None)
            .insert_subtree(&mut merk, [3, 3], [9; 32], None, grove_version)
            .unwrap()
            .expect("insert a big sum tree");
        let root = merk.root_hash().unwrap();
        let aggregate = merk.aggregate_data().expect("aggregate");

        let result = convert(merk, grove_version).0;
        assert!(
            matches!(result, Err(Error::InvalidInputError(_))),
            "got {:?}",
            result.err()
        );

        let mut reopened = open(&storage, &tx, TreeType::ProvableSumTree, grove_version);
        assert_eq!(reopened.root_hash().unwrap(), root);
        assert_eq!(reopened.aggregate_data().expect("aggregate"), aggregate);
        Element::new_sum_item(4)
            .insert(&mut reopened, [4, 4], None, grove_version)
            .unwrap()
            .expect("the tree still takes writes");
        assert_eq!(
            reopened.aggregate_data().expect("aggregate"),
            AggregateData::ProvableSum(aggregate.as_sum_i64() + 4)
        );
    }

    #[test]
    fn the_conversion_is_gated_to_the_version_that_introduces_it() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut merk = open(&storage, &tx, TreeType::CountSumTree, &GROVE_V4);
        fill(&mut merk, &GROVE_V4);
        let root = merk.root_hash().unwrap();

        let result = convert(merk, &GROVE_V4).0;
        assert!(
            matches!(result, Err(Error::VersionError(_))),
            "got {:?}",
            result.err()
        );

        let reopened = open(&storage, &tx, TreeType::CountSumTree, &GROVE_V4);
        assert_eq!(reopened.root_hash().unwrap(), root);
        assert!(matches!(
            reopened.aggregate_data().expect("aggregate"),
            AggregateData::CountAndSum(..)
        ));
    }

    /// A node is billed against what is stored, whether the handle read it
    /// from storage or wrote it itself and still holds it.
    #[test]
    fn a_handle_that_wrote_the_nodes_is_billed_as_one_that_read_them() {
        let grove_version = GroveVersion::latest();
        let write = |merk: &mut ImmediateMerk<'_>| {
            for i in 0..12u8 {
                Element::new_sum_item(i64::from(i))
                    .insert(merk, [i.wrapping_mul(29)], None, grove_version)
                    .unwrap()
                    .expect("insert a sum item");
            }
        };

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut retained = open(&storage, &tx, TreeType::SumTree, grove_version);
        write(&mut retained);
        let (result, retained_cost) = convert(retained, grove_version);
        let retained_root = result
            .expect("convert the handle that wrote the nodes")
            .root_hash()
            .unwrap();

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut writer = open(&storage, &tx, TreeType::SumTree, grove_version);
        write(&mut writer);
        drop(writer);
        let reopened = open(&storage, &tx, TreeType::SumTree, grove_version);
        let (result, reopened_cost) = convert(reopened, grove_version);
        let reopened_root = result
            .expect("convert a reopened handle")
            .root_hash()
            .unwrap();

        assert_eq!(
            retained_cost.storage_cost, reopened_cost.storage_cost,
            "the storage cost does not depend on who wrote the nodes"
        );
        // 8 bytes in each node and 8 in the link to it, and nothing billed
        // as new.
        assert_eq!(retained_cost.storage_cost.added_bytes, 12 * 16);
        assert_eq!(retained_root, reopened_root);
    }
}
