//! `average_case_finalize` **v0** (`GROVE_V6`), the first. See the module
//! docs in [`super`][`mod@super`].
//!
//! For every Merk that hosts a marked element: its open, the replacement of
//! each marked element as a batch estimates `ReplaceTreeRootKey`, with the
//! byte of its wrapper where it has one and the hash calls of the roots it
//! names, and what the finalize does in the Merk besides
//! (`add_average_case_merk_finalize`).

use std::collections::{HashMap, HashSet};

use grovedb_costs::{storage_cost::StorageCost, CostResult, CostsExt, OperationCost};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    estimated_costs::average_case_costs::{
        add_average_case_merk_finalize, EstimatedLayerInformation, EstimatedLayerSizes,
    },
    tree_type::{CostSize, TreeType},
};
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{
    batch::KeyInfoPath,
    estimated_costs::{
        add_in_full, hash_blocks, hash_calls, indexed_tree_hash_calls, marked_count, marked_hosts,
        missing_layer, serialized_length_len, MOST_AXES_OF_AN_INDEXED_TREE,
    },
    operations::indexed_tree::axis_sort_key_len,
    Error, GroveDb,
};

/// The keys of a layer at their longest, whatever the layer holds. A kind
/// that a layer of several kinds gives no weight holds no element there.
fn layer_key_size(sizes: &EstimatedLayerSizes) -> u32 {
    match sizes {
        EstimatedLayerSizes::AllSubtrees(key, ..)
        | EstimatedLayerSizes::AllItems(key, ..)
        | EstimatedLayerSizes::AllReference(key, ..)
        | EstimatedLayerSizes::AllItemsWithSumItem(key, ..)
        | EstimatedLayerSizes::AllReferencesWithSumItem(key, ..) => u32::from(*key),
        EstimatedLayerSizes::Mix {
            subtrees_size,
            items_size,
            references_size,
            items_with_sum_item_size,
            references_with_sum_item_size,
        } => [
            subtrees_size.map(|(key, .., weight)| (key, weight)),
            items_size.map(|(key, .., weight)| (key, weight)),
            references_size.map(|(key, .., weight)| (key, weight)),
            items_with_sum_item_size.map(|(key, .., weight)| (key, weight)),
            references_with_sum_item_size.map(|(key, .., weight)| (key, weight)),
        ]
        .into_iter()
        .flatten()
        .filter(|(_, weight)| *weight > 0)
        .map(|(key, _)| key)
        .max()
        .map_or(0, u32::from),
    }
}

/// The axes whose secondaries an indexed tree can name: its one axis, and
/// for a tree that names its axes the three it may have.
fn axes_of(tree_type: TreeType) -> &'static [IndexAxis] {
    match tree_type {
        TreeType::ProvableCountIndexedTree => &[IndexAxis::Count],
        TreeType::ProvableSumIndexedTree => &[IndexAxis::Sum],
        TreeType::ProvableBigSumIndexedTree => &[IndexAxis::BigSum],
        TreeType::ProvableCountProvableSumIndexedTree => {
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
        }
        TreeType::ProvableCountProvableBigSumIndexedTree => {
            &[IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]
        }
        _ => &[],
    }
}

/// The hash calls that the roots a tree element names add to the hash of
/// the element. The replacement of a tree element is estimated with the
/// hash of its cost size and its flags. Its bytes are those, and the root
/// key of its Merk; for an indexed tree the root key of each secondary,
/// which is the key of an entry behind the sort key of the axis; each with
/// its length as an element is serialized; and the length of the whole,
/// which the value is hashed with. A wrapped element has the byte of its
/// wrapper before them.
///
/// Each root key is taken at the size the layer gives the keys of the Merk:
/// the entry that is the root of a secondary need not be the root of the
/// primary. A Merk that its layer estimates to be empty names none, and
/// neither does a tree that keeps its data flat.
fn hash_calls_of_the_roots(
    host_layer: &EstimatedLayerInformation,
    hosted: &EstimatedLayerInformation,
    wrapper: u32,
) -> Result<u32, grovedb_merk::Error> {
    let flags = *host_layer.estimated_layer_sizes.layered_flags_size()?;
    let cost_size = hosted.tree_type.cost_size();
    let counted = cost_size + flags.map_or(0, |flags| flags + flags.required_space() as u32);
    let mut element =
        wrapper + cost_size + flags.map_or(0, |flags| flags + serialized_length_len(flags));
    // A sum of 128 bits is 17 bytes at its longest where its cost is 16.
    if matches!(
        hosted.tree_type,
        TreeType::BigSumTree
            | TreeType::CountBigSumTree
            | TreeType::ProvableBigSumTree
            | TreeType::ProvableCountProvableBigSumTree
            | TreeType::ProvableBigSumIndexedTree
            | TreeType::ProvableCountProvableBigSumIndexedTree
    ) {
        element += 1;
    }
    // A tree that keeps its data flat has no Merk, and names no root key.
    if !hosted.estimated_layer_count.estimated_to_be_empty()
        && !hosted.tree_type.uses_non_merk_data_storage()
    {
        let root_key = layer_key_size(&hosted.estimated_layer_sizes);
        element += serialized_length_len(root_key) + root_key;
        for axis in axes_of(hosted.tree_type) {
            let secondary = (root_key + axis_sort_key_len(*axis) as u32).min(u32::from(u8::MAX));
            element += serialized_length_len(secondary) + secondary;
        }
    }
    let hashed = element + element.required_space() as u32;
    Ok(hash_blocks(hashed).saturating_sub(hash_blocks(counted)))
}

impl GroveDb {
    /// `average_case_finalize` v0: see the module documentation.
    pub(super) fn average_case_finalize_v0(
        stop_points: &[KeyInfoPath],
        wrapped: &HashSet<KeyInfoPath>,
        paths: &HashMap<KeyInfoPath, EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        // The estimate in full, or a refusal with nothing added.
        match Self::average_case_finalize_cost(stop_points, wrapped, paths, grove_version) {
            Ok(cost) => Ok(()).wrap_with_cost(cost),
            Err(error) => Err(error).wrap_with_cost(OperationCost::default()),
        }
    }

    fn average_case_finalize_cost(
        stop_points: &[KeyInfoPath],
        wrapped: &HashSet<KeyInfoPath>,
        paths: &HashMap<KeyInfoPath, EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        let mut cost = OperationCost::default();
        for (host_path, keys) in marked_hosts(stop_points) {
            let layer = paths
                .get(&host_path)
                .ok_or_else(|| missing_layer("average case finalize", &host_path))?;
            // Before any size of the layer is added to.
            layer
                .estimated_layer_sizes
                .check_sizes()
                .map_err(Error::MerkError)?;
            let marked = marked_count(keys.len())?;
            // The root Merk has no host and is opened by its root key
            // record, whose bytes are those of the root key: it is taken at
            // the size the layer gives the keys of the root Merk.
            let mut host = OperationCost::default();
            Self::add_average_case_get_merk_at_path_with_root_key(
                &mut host,
                &host_path,
                false,
                layer.tree_type,
                Some(layer_key_size(&layer.estimated_layer_sizes)),
                grove_version,
            )?;
            for key in &keys {
                let mut below = host_path.clone();
                below.push(key.clone());
                let hosted = paths
                    .get(&below)
                    .ok_or_else(|| missing_layer("average case finalize", &below))?;
                let mut element = Self::average_case_merk_replace_tree(
                    key,
                    layer,
                    hosted.tree_type,
                    false,
                    grove_version,
                )
                .cost_as_result()?;
                // A wrapper is a byte of the element: of its cost, which is
                // the element's own and a byte, and of what is hashed.
                let wrapper = u32::from(wrapped.contains(&below));
                add_in_full(
                    &mut element,
                    OperationCost {
                        storage_cost: StorageCost {
                            replaced_bytes: wrapper,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )?;
                // A tree that names its axes is taken to name the most it
                // can: the digest of three roots is as long as that of two.
                add_in_full(
                    &mut element,
                    hash_calls(indexed_tree_hash_calls(
                        hosted.tree_type,
                        MOST_AXES_OF_AN_INDEXED_TREE,
                    )?),
                )?;
                add_in_full(
                    &mut element,
                    hash_calls(
                        hash_calls_of_the_roots(layer, hosted, wrapper)
                            .map_err(Error::MerkError)?,
                    ),
                )?;
                add_in_full(&mut host, element)?;
            }
            let mut in_the_merk = OperationCost::default();
            add_average_case_merk_finalize(&mut in_the_merk, layer, marked, grove_version)
                .map_err(Error::MerkError)?;
            add_in_full(&mut host, in_the_merk)?;
            if host_path.is_empty() {
                // The root Merk has no host, so its root is not asked for.
                // The node at its root is among the changed nodes of this
                // Merk, a marked element or a node above one, each with its
                // two hash calls. `host` is a sum in full, so they are in it.
                host.hash_node_calls = host.hash_node_calls.saturating_sub(2);
            }
            add_in_full(&mut cost, host)?;
        }
        Ok(cost)
    }
}
