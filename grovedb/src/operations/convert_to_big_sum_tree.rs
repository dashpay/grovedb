//! Converting a tree that keeps a 64-bit sum into its counterpart with a
//! 128-bit sum, in place.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, reconstruct::ElementReconstructExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
    merk::NodeType,
    Error as MerkError,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageBatch};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::{
        indexed_tree::{validate_indexed_child_for_variant, SecondaryRowWrites},
        insert::check_wide_sum_tree_creation,
    },
    util::{check_v1_slot_enabled, validate_path_segment_lengths, TxRef, MAX_KEY_LENGTH},
    Element, Error, GroveDb, TransactionArg,
};

impl GroveDb {
    /// Converts the tree at `path`/`key`, which keeps a 64-bit sum, into the
    /// tree that holds the same children with the sum in 128 bits, in place:
    ///
    /// | tree                                  | becomes                                  |
    /// |---------------------------------------|------------------------------------------|
    /// | `SumTree`                             | `BigSumTree`                             |
    /// | `CountSumTree`                        | `CountBigSumTree`                        |
    /// | `ProvableSumTree`                     | `ProvableBigSumTree`                     |
    /// | `ProvableCountProvableSumTree`        | `ProvableCountProvableBigSumTree`        |
    /// | `ProvableSumIndexedTree`              | `ProvableBigSumIndexedTree`              |
    /// | `ProvableCountProvableSumIndexedTree` | `ProvableCountProvableBigSumIndexedTree` |
    ///
    /// The tree keeps its path, its children, its flags, its count and its
    /// sum, and the wrapper it is in (`NonCounted`, `NotSummed`,
    /// `NotCountedOrSummed`). An indexed tree's sum axis becomes the big sum
    /// axis. From then on the tree accepts what its new type accepts, and
    /// its sum can pass the `i64` range.
    ///
    /// Only the tree named is converted. A tree inside it keeps its type,
    /// and goes on adding its own 64-bit sum to the converted tree's.
    ///
    /// # Refusals
    ///
    /// - The element is not one of the six trees above. A
    ///   `ProvableCountSumTree` has no counterpart with a 128-bit sum.
    /// - The tree adds its sum to a parent that keeps a 64-bit sum. A tree
    ///   with a 128-bit sum adds nothing to such a parent, so the parent
    ///   would stop following it: convert the parent first. A tree wrapped in
    ///   `NotSummed` or `NotCountedOrSummed` adds nothing either way and is
    ///   converted.
    /// - The tree holds a child that would add a different count or sum to
    ///   it once it is converted. That is a tree with a 128-bit sum inside
    ///   the tree: it adds nothing to a 64-bit sum and its whole sum to a
    ///   128-bit one, so the tree's sum would change. Every child must add
    ///   what it adds today.
    /// - An indexed tree holds an entry its counterpart does not accept, or
    ///   its axes are not canonical. A tree written as the supported paths
    ///   write it has neither.
    /// - An indexed tree holds a key longer than 239 bytes and has no average
    ///   axis. The big sum axis ranks keys of at most 239 bytes, like the
    ///   average axis, where the sum axis ranks keys of up to 247.
    ///
    /// # Cost
    ///
    /// Every node of the tree records sums at the tree's width, so every
    /// node is read and written again, and the whole tree is held in memory
    /// until the write: one visit per child. The storage cost reports the
    /// growth as added bytes: 8 in each child's node and 8 in the link to
    /// it, one more where that takes the node's length past a size its
    /// length prefix can hold, and 7 for the tree's own element.
    ///
    /// An indexed tree costs more. Its sum secondary is ordered by the 8-byte
    /// encoding of each entry's sum and its big sum secondary by the 16-byte
    /// one, so each of its rows is removed and written again (8 bytes longer
    /// in the key), and so is each row of its average secondary, which
    /// carries the entry's sum under a 64-bit tree only. The rows of an axis
    /// are sorted and applied as one change to its secondary: `O(n log n)`
    /// work for each of those axes, with their own storage cost beside the
    /// primary's. The count rows stay as they are.
    ///
    /// A tree whose sum is in its node hashes gets a new root hash. The
    /// other two keep theirs, and only the element that describes the tree
    /// changes.
    ///
    /// Everything is written at the end, in one batch: a call that is
    /// refused or fails leaves the tree as it was.
    pub fn convert_to_big_sum_tree<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(
            cost,
            check_v1_slot_enabled(
                "convert_to_big_sum_tree",
                grove_version
                    .grovedb_versions
                    .operations
                    .insert
                    .convert_to_big_sum_tree,
            )
        );

        let path: SubtreePath<B> = path.into();
        cost_return_on_error_no_add!(cost, validate_path_segment_lengths(&path));
        if key.len() > MAX_KEY_LENGTH {
            return Err(Error::InvalidInput("key length must be at most 255 bytes"))
                .wrap_with_cost(cost);
        }

        let tx = TxRef::new(&self.db, transaction);
        let batch = StorageBatch::new();

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );
        let parent_tree_type = parent_merk.tree_type;

        let narrow_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, key, true, grove_version).map_err(Error::MerkError)
        );
        let Some(wide_element) = narrow_element.big_sum_counterpart() else {
            return Err(Error::InvalidInput(
                "only a tree that keeps a 64-bit sum and has a counterpart with a 128-bit sum \
                 can be converted to a big sum tree (SumTree, CountSumTree, ProvableSumTree, \
                 ProvableCountProvableSumTree, ProvableSumIndexedTree or \
                 ProvableCountProvableSumIndexedTree)",
            ))
            .wrap_with_cost(cost);
        };

        // The converted tree must be one this version can create, with the
        // axes its type requires, and one its parent accepts.
        cost_return_on_error_no_add!(
            cost,
            check_wide_sum_tree_creation(&wide_element, "convert to", grove_version)
        );
        // A tree written before its axes were checked on every path can hold
        // them out of order or twice: neither side of the conversion may.
        let axes_are_canonical = match (narrow_element.underlying(), wide_element.underlying()) {
            (
                Element::ProvableCountProvableSumIndexedTree(.., narrow_axes, _),
                Element::ProvableCountProvableBigSumIndexedTree(.., wide_axes, _),
            ) => {
                Element::validate_pcpsit_axes(narrow_axes).is_ok()
                    && Element::validate_pcpbsit_axes(wide_axes).is_ok()
            }
            _ => true,
        };
        if !axes_are_canonical {
            return Err(Error::InvalidInput(
                "the indexed tree's axes are not canonical (1..=3 entries, sorted ascending by \
                 tag, no duplicates): it cannot be converted",
            ))
            .wrap_with_cost(cost);
        }
        cost_return_on_error_no_add!(
            cost,
            wide_element
                .validate_insertable_into(parent_tree_type)
                .map_err(Error::MerkError)
        );
        if parent_tree_type.is_indexed_primary() {
            cost_return_on_error_no_add!(
                cost,
                validate_indexed_child_for_variant(&wide_element, parent_tree_type, grove_version)
            );
        }

        // A tree with a 128-bit sum adds nothing to a parent that sums in 64
        // bits. Judged by the parent's type, not by what the two elements
        // add today, so that an empty tree is refused as a filled one is.
        let parent_sums_in_64_bits = matches!(
            parent_tree_type.inner_node_type(),
            NodeType::SumNode
                | NodeType::CountSumNode
                | NodeType::ProvableCountSumNode
                | NodeType::ProvableSumNode
                | NodeType::ProvableCountProvableSumNode
        );
        let left_out_of_the_parent_sum =
            narrow_element.is_not_summed() || narrow_element.is_not_counted_or_summed();
        if parent_sums_in_64_bits && !left_out_of_the_parent_sum {
            return Err(Error::InvalidInput(
                "the tree adds its sum to a parent that keeps a 64-bit sum, and would add \
                 nothing to it with a 128-bit sum: convert the parent first",
            ))
            .wrap_with_cost(cost);
        }
        // Past that refusal the two elements add the same count and sum to
        // the parent.
        let narrow_feature_type = cost_return_on_error_no_add!(
            cost,
            narrow_element
                .get_feature_type(parent_tree_type)
                .map_err(Error::MerkError)
        );
        let wide_feature_type = cost_return_on_error_no_add!(
            cost,
            wide_element
                .get_feature_type(parent_tree_type)
                .map_err(Error::MerkError)
        );
        if narrow_feature_type != wide_feature_type {
            return Err(Error::CorruptedCodeExecution(
                "a tree and its counterpart with a 128-bit sum add different aggregates to \
                 their parent",
            ))
            .wrap_with_cost(cost);
        }

        let tree_path = path.derive_owned_with_child(key);
        let tree_path_ref: SubtreePath<B> = (&tree_path).into();
        let mut tree_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                tree_path_ref.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );
        let narrow_tree_type = tree_merk.tree_type;
        let wide_tree_type = cost_return_on_error_no_add!(
            cost,
            narrow_tree_type
                .big_sum_counterpart()
                .ok_or(Error::CorruptedCodeExecution(
                    "a tree element has a counterpart with a 128-bit sum and its tree type \
                     has none"
                ))
        );
        cost_return_on_error_into!(
            &mut cost,
            tree_merk.convert_to_big_sum_tree(
                // Each child must be one the converted tree accepts, adding
                // to it what it adds to the tree today.
                &|_key, value, widened_feature_type| {
                    let child = Element::deserialize(value, grove_version)?;
                    if child.get_feature_type(wide_tree_type)? != widened_feature_type {
                        return Err(MerkError::InvalidInputError(
                            "the tree holds a child that would add a different count or sum \
                             to it with a 128-bit sum, as a tree with a 128-bit sum does: the \
                             conversion would change the tree's sum",
                        ));
                    }
                    if wide_tree_type.is_indexed_primary() {
                        match validate_indexed_child_for_variant(
                            &child,
                            wide_tree_type,
                            grove_version,
                        ) {
                            Ok(()) => {}
                            Err(Error::InvalidInput(message)) => {
                                return Err(MerkError::InvalidInputError(message));
                            }
                            Err(e) => return Err(MerkError::CorruptedData(e.to_string())),
                        }
                    }
                    Ok(())
                },
                &|key, value| {
                    Element::specialized_costs_for_key_value(
                        key,
                        value,
                        narrow_tree_type.inner_node_type(),
                        grove_version,
                    )
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                },
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
        );

        // What the parent must hold once the tree is converted: the element
        // with its new root state, and the hashes it is committed with.
        let (wide_element, root_hash, indexed_second_hash) = if tree_merk
            .tree_type
            .is_indexed_primary()
        {
            // The rows of the sum axis live where the big sum axis keeps
            // its rows, under shorter keys: the rebuild removes each as a
            // row the tree no longer has and writes the row that replaces
            // it, all the rows of an axis in one change, so that only the
            // sums of the finished secondary are computed.
            let (wide_element, primary_root_hash, second_hash) = cost_return_on_error!(
                &mut cost,
                self.rebuild_indexed_tree_secondaries(
                    tree_path_ref,
                    &tree_merk,
                    &wide_element,
                    |key_len, max_item_key_len| {
                        Error::NotSupported(format!(
                            "the indexed tree holds a key of {key_len} bytes, and its \
                                 counterpart with a 128-bit sum ranks keys of at most \
                                 {max_item_key_len} bytes"
                        ))
                    },
                    SecondaryRowWrites::Together,
                    tx.as_ref(),
                    &batch,
                    grove_version,
                )
            );
            (wide_element, primary_root_hash, Some(second_hash))
        } else {
            let (root_hash, root_key, aggregate_data) =
                cost_return_on_error_into!(&mut cost, tree_merk.root_hash_key_and_aggregate_data());
            let wide_element = cost_return_on_error_no_add!(
                cost,
                wide_element
                    .reconstruct_with_root_key(root_key, aggregate_data)
                    .ok_or(Error::CorruptedCodeExecution(
                        "a tree element could not be rebuilt with its root key"
                    ))
            );
            (wide_element, root_hash, None)
        };
        drop(tree_merk);

        // Under an indexed primary the tree's row binds the element's value
        // hash, which the write below changes: the propagation refreshes it.
        let old_indexed_state = cost_return_on_error!(
            &mut cost,
            GroveDb::capture_indexed_entry_state(&parent_merk, key, &narrow_element, grove_version)
        );
        match indexed_second_hash {
            Some(second_hash) => cost_return_on_error_into!(
                &mut cost,
                wide_element.insert_count_indexed_subtree(
                    &mut parent_merk,
                    key,
                    root_hash,
                    second_hash,
                    None,
                    grove_version,
                )
            ),
            None => cost_return_on_error_into!(
                &mut cost,
                wide_element.insert_subtree(&mut parent_merk, key, root_hash, None, grove_version)
            ),
        }

        let mut merk_cache = HashMap::new();
        merk_cache.insert(path.clone(), parent_merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction_refreshing_indexed_row(
                merk_cache,
                path,
                key,
                old_indexed_state,
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }
}
