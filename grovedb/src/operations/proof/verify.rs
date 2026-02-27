use std::collections::{BTreeMap, BTreeSet};

use grovedb_merk::{
    calculate_chunk_depths, calculate_max_tree_depth_from_count,
    element::tree_type::ElementTreeTypeExtensions,
    proofs::{
        execute,
        query::{PathKey, QueryProofVerify, VerifyOptions},
        Decoder, Node, Op, Query,
    },
    tree::{combine_hash, value_hash, NULL_HASH},
    CryptoHash, TreeFeatureType,
};
use grovedb_version::{
    check_grovedb_v0, version::GroveVersion, TryFromVersioned, TryIntoVersioned,
};

#[cfg(feature = "proof_debug")]
use crate::operations::proof::util::{
    hex_to_ascii, path_as_slices_hex_to_ascii, path_hex_to_ascii,
};
use crate::{
    operations::proof::{
        util::{ProvedPathKeyOptionalValue, ProvedPathKeyValues},
        GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof, ProofBytes,
        ProveOptions,
    },
    query::{GroveTrunkQueryResult, PathTrunkChunkQuery},
    query_result_type::PathKeyOptionalElementTrio,
    Element, Error, GroveDb, PathQuery,
};

impl GroveDb {
    pub fn verify_query_with_options(
        proof: &[u8],
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_query_with_options",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );
        if options.absence_proofs_for_non_existing_searched_keys {
            // must have a limit
            query.query.limit.ok_or(Error::NotSupported(
                "limits must be set in verify_query_with_absence_proof".to_string(),
            ))?;
        }

        // must have no offset
        if query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "offsets in path queries are not supported for proofs".to_string(),
            ));
        }

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, _, result) =
            Self::verify_proof_internal(&grovedb_proof, query, options, grove_version)?;

        Ok((root_hash, result))
    }

    /// The point of this query is to get the parent tree information which will
    /// be present because we are querying in a subtree
    pub fn verify_query_get_parent_tree_info_with_options(
        proof: &[u8],
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, TreeFeatureType, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_query_get_parent_tree_info_with_options",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_get_parent_tree_info_with_options
        );

        if query.query.query.has_subquery() {
            return Err(Error::NotSupported(
                "getting the parent tree info is not available when using subqueries".to_string(),
            ));
        }
        if options.absence_proofs_for_non_existing_searched_keys {
            // must have a limit
            query.query.limit.ok_or(Error::NotSupported(
                "limits must be set in verify_query_with_absence_proof".to_string(),
            ))?;
        }

        // must have no offset
        if query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "offsets in path queries are not supported for proofs".to_string(),
            ));
        }

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, tree_feature_type, result) =
            Self::verify_proof_internal(&grovedb_proof, query, options, grove_version)?;

        let tree_feature_type = tree_feature_type.ok_or(Error::InvalidProof(
            query.clone(),
            "query had no parent tree info, maybe it was for for root tree".to_string(),
        ))?;

        Ok((root_hash, tree_feature_type, result))
    }

    pub fn verify_query_raw(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, ProvedPathKeyValues), Error> {
        check_grovedb_v0!(
            "verify_query_raw",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_raw
        );
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, _, result) = Self::verify_proof_raw_internal(
            &grovedb_proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )?;

        Ok((root_hash, result))
    }

    pub(crate) fn verify_proof_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<
        (
            CryptoHash,
            Option<TreeFeatureType>,
            Vec<PathKeyOptionalElementTrio>,
        ),
        Error,
    > {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_v0_internal(proof_v0, query, options, grove_version)
            }
            GroveDBProof::V1(proof_v1) => {
                Self::verify_proof_v1_internal(proof_v1, query, options, grove_version)
            }
        }
    }

    fn verify_proof_v0_internal(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<
        (
            CryptoHash,
            Option<TreeFeatureType>,
            Vec<PathKeyOptionalElementTrio>,
        ),
        Error,
    > {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let mut last_tree_feature_type = None;
        let root_hash = Self::verify_layer_proof(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
            &mut last_tree_feature_type,
            &options,
            grove_version,
        )?;

        if options.absence_proofs_for_non_existing_searched_keys {
            // must have a limit
            let max_results = query.query.limit.ok_or(Error::NotSupported(
                "limits must be set in verify_query_with_absence_proof".to_string(),
            ))? as usize;

            let terminal_keys = query.terminal_keys(max_results, grove_version)?;

            // convert the result set to a btree map
            let mut result_set_as_map: BTreeMap<PathKey, Option<Element>> = result
                .into_iter()
                .map(|(path, key, element)| ((path, key), element))
                .collect();
            #[cfg(feature = "proof_debug")]
            {
                println!(
                    "terminal keys are [{}] \n result set is [{}]",
                    terminal_keys
                        .iter()
                        .map(|(path, key)| format!(
                            "path: {} key: {}",
                            path_hex_to_ascii(path),
                            hex_to_ascii(key)
                        ))
                        .collect::<Vec<_>>()
                        .join(", "),
                    result_set_as_map
                        .iter()
                        .map(|((path, key), e)| {
                            let element_string = if let Some(e) = e {
                                e.to_string()
                            } else {
                                "None".to_string()
                            };
                            format!(
                                "path: {} key: {} element: {}",
                                path_hex_to_ascii(path),
                                hex_to_ascii(key),
                                element_string,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            result = terminal_keys
                .into_iter()
                .map(|terminal_key| {
                    let element = result_set_as_map.remove(&terminal_key).flatten();
                    (terminal_key.0, terminal_key.1, element)
                })
                .collect();
        }

        Ok((root_hash, last_tree_feature_type, result))
    }

    pub(crate) fn verify_proof_raw_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Option<TreeFeatureType>, ProvedPathKeyValues), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_raw_internal_v0(proof_v0, query, options, grove_version)
            }
            GroveDBProof::V1(proof_v1) => {
                Self::verify_proof_v1_raw_internal(proof_v1, query, options, grove_version)
            }
        }
    }

    fn verify_proof_raw_internal_v0(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Option<TreeFeatureType>, ProvedPathKeyValues), Error> {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let mut last_tree_feature_type = None;
        let root_hash = Self::verify_layer_proof(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
            &mut last_tree_feature_type,
            &options,
            grove_version,
        )?;
        Ok((root_hash, last_tree_feature_type, result))
    }

    fn verify_proof_v1_internal(
        proof: &GroveDBProofV1,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<
        (
            CryptoHash,
            Option<TreeFeatureType>,
            Vec<PathKeyOptionalElementTrio>,
        ),
        Error,
    > {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let mut last_tree_feature_type = None;
        let root_hash = Self::verify_layer_proof_v1(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
            &mut last_tree_feature_type,
            &options,
            grove_version,
        )?;

        if options.absence_proofs_for_non_existing_searched_keys {
            let max_results = query.query.limit.ok_or(Error::NotSupported(
                "limits must be set in verify_query_with_absence_proof".to_string(),
            ))? as usize;

            let terminal_keys = query.terminal_keys(max_results, grove_version)?;

            let mut result_set_as_map: BTreeMap<PathKey, Option<Element>> = result
                .into_iter()
                .map(|(path, key, element)| ((path, key), element))
                .collect();

            result = terminal_keys
                .into_iter()
                .map(|terminal_key| {
                    let element = result_set_as_map.remove(&terminal_key).flatten();
                    (terminal_key.0, terminal_key.1, element)
                })
                .collect();
        }

        Ok((root_hash, last_tree_feature_type, result))
    }

    fn verify_proof_v1_raw_internal(
        proof: &GroveDBProofV1,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Option<TreeFeatureType>, ProvedPathKeyValues), Error> {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let mut last_tree_feature_type = None;
        let root_hash = Self::verify_layer_proof_v1(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
            &mut last_tree_feature_type,
            &options,
            grove_version,
        )?;
        Ok((root_hash, last_tree_feature_type, result))
    }

    fn verify_layer_proof_v1<T>(
        layer_proof: &LayerProof,
        prove_options: &ProveOptions,
        query: &PathQuery,
        limit_left: &mut Option<u16>,
        current_path: &[&[u8]],
        result: &mut Vec<T>,
        last_parent_tree_type: &mut Option<TreeFeatureType>,
        options: &VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<CryptoHash, Error>
    where
        T: TryFromVersioned<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFromVersioned<ProvedPathKeyOptionalValue>>::Error>,
    {
        // The merk proof at this layer must be Merk type
        let merk_proof_bytes = match &layer_proof.merk_proof {
            ProofBytes::Merk(bytes) => bytes,
            ProofBytes::MMR(_)
            | ProofBytes::BulkAppendTree(_)
            | ProofBytes::DenseTree(_)
            | ProofBytes::CommitmentTree(_) => {
                return Err(Error::InvalidProof(
                    query.clone(),
                    "Expected Merk proof at this layer, got non-Merk proof type".to_string(),
                ));
            }
        };

        let internal_query = query
            .query_items_at_path(current_path, grove_version)?
            .ok_or(Error::CorruptedPath(format!(
                "verify v1: path {} should be part of path_query {}",
                current_path
                    .iter()
                    .map(hex::encode)
                    .collect::<Vec<_>>()
                    .join("/"),
                query
            )))?;

        let level_query = Query {
            items: internal_query.items.to_vec(),
            left_to_right: internal_query.left_to_right,
            ..Default::default()
        };

        let (root_hash, merk_result) = level_query
            .execute_proof(merk_proof_bytes, *limit_left, internal_query.left_to_right)
            .unwrap()
            .map_err(|e| {
                Error::InvalidProof(
                    query.clone(),
                    format!("Invalid V1 proof verification parameters: {}", e),
                )
            })?;

        let mut verified_keys = BTreeSet::new();

        if merk_result.result_set.is_empty() {
            if prove_options.decrease_limit_on_empty_sub_query_result {
                limit_left.iter_mut().for_each(|limit| *limit -= 1);
            }
        } else {
            for proved_key_value in merk_result.result_set {
                let mut path = current_path.to_vec();
                let key = &proved_key_value.key;
                let hash = &proved_key_value.proof;
                if let Some(value_bytes) = &proved_key_value.value {
                    let element = Element::deserialize(value_bytes, grove_version)?;

                    verified_keys.insert(key.clone());

                    if let Some(lower_layer) = layer_proof.lower_layers.get(key) {
                        // MmrTree/BulkAppendTree have root_key=None (no child Merk data),
                        // so they match on (..) rather than (Some(_), ..)
                        match element {
                            Element::Tree(Some(_), _)
                            | Element::SumTree(Some(_), ..)
                            | Element::BigSumTree(Some(_), ..)
                            | Element::CountTree(Some(_), ..)
                            | Element::CountSumTree(Some(_), ..)
                            | Element::ProvableCountTree(Some(_), ..)
                            | Element::ProvableCountSumTree(Some(_), ..)
                            | Element::CommitmentTree(..)
                            | Element::MmrTree(..)
                            | Element::BulkAppendTree(..)
                            | Element::DenseAppendOnlyFixedSizeTree(..) => {
                                path.push(key);
                                *last_parent_tree_type = element.tree_feature_type();
                                if query.query_items_at_path(&path, grove_version)?.is_none() {
                                    // Query targets the tree itself, not its contents
                                    let path_key_optional_value =
                                        ProvedPathKeyOptionalValue::from_proved_key_value(
                                            path.iter().map(|p| p.to_vec()).collect(),
                                            proved_key_value,
                                        );
                                    result.push(
                                        path_key_optional_value
                                            .try_into_versioned(grove_version)?,
                                    );
                                    limit_left.iter_mut().for_each(|limit| *limit -= 1);
                                    if limit_left == &Some(0) {
                                        break;
                                    }
                                } else {
                                    if query.should_add_parent_tree_at_path(
                                        current_path,
                                        grove_version,
                                    )? {
                                        let path_key_optional_value =
                                            ProvedPathKeyOptionalValue::from_proved_key_value(
                                                path.iter().map(|p| p.to_vec()).collect(),
                                                proved_key_value.clone(),
                                            );
                                        result.push(
                                            path_key_optional_value
                                                .try_into_versioned(grove_version)?,
                                        );
                                    }

                                    // Dispatch based on lower layer proof type
                                    let lower_hash = match &lower_layer.merk_proof {
                                        ProofBytes::Merk(_) => {
                                            // Standard Merk subtree - recurse
                                            Self::verify_layer_proof_v1(
                                                lower_layer,
                                                prove_options,
                                                query,
                                                limit_left,
                                                &path,
                                                result,
                                                last_parent_tree_type,
                                                options,
                                                grove_version,
                                            )?
                                        }
                                        ProofBytes::MMR(mmr_bytes) => Self::verify_mmr_lower_layer(
                                            mmr_bytes,
                                            &element,
                                            &path,
                                            limit_left,
                                            result,
                                            query,
                                            grove_version,
                                        )?,
                                        ProofBytes::BulkAppendTree(bulk_bytes) => {
                                            Self::verify_bulk_append_lower_layer(
                                                bulk_bytes,
                                                &element,
                                                &path,
                                                limit_left,
                                                result,
                                                query,
                                                grove_version,
                                            )?
                                        }
                                        ProofBytes::DenseTree(dense_bytes) => {
                                            Self::verify_dense_tree_lower_layer(
                                                dense_bytes,
                                                &element,
                                                &path,
                                                limit_left,
                                                result,
                                                query,
                                                grove_version,
                                            )?
                                        }
                                        ProofBytes::CommitmentTree(ct_bytes) => {
                                            Self::verify_commitment_tree_lower_layer(
                                                ct_bytes,
                                                &element,
                                                &path,
                                                limit_left,
                                                result,
                                                query,
                                                grove_version,
                                            )?
                                        }
                                    };

                                    let combined_root_hash =
                                        combine_hash(value_hash(value_bytes).value(), &lower_hash)
                                            .value()
                                            .to_owned();

                                    if hash != &combined_root_hash {
                                        return Err(Error::InvalidProof(
                                            query.clone(),
                                            format!(
                                                "V1 mismatch in lower layer hash, expected {}, \
                                                 got {}",
                                                hex::encode(hash),
                                                hex::encode(combined_root_hash)
                                            ),
                                        ));
                                    }
                                    if limit_left == &Some(0) {
                                        break;
                                    }
                                }
                            }
                            // MmrTree/BulkAppendTree/CommitmentTree handled above
                            Element::Tree(None, _)
                            | Element::SumTree(None, ..)
                            | Element::BigSumTree(None, ..)
                            | Element::CountTree(None, ..)
                            | Element::CountSumTree(None, ..)
                            | Element::ProvableCountTree(None, ..)
                            | Element::ProvableCountSumTree(None, ..)
                            | Element::SumItem(..)
                            | Element::Item(..)
                            | Element::ItemWithSumItem(..)
                            | Element::Reference(..) => {
                                return Err(Error::InvalidProof(
                                    query.clone(),
                                    "V1 proof has lower layer for a non-tree element.".to_string(),
                                ));
                            }
                        }
                    } else if element.is_any_item()
                        || !internal_query.has_subquery_or_matching_in_path_on_key(key)
                            && (options.include_empty_trees_in_result
                                || !matches!(element, Element::Tree(None, _)))
                    {
                        let path_key_optional_value =
                            ProvedPathKeyOptionalValue::from_proved_key_value(
                                path.iter().map(|p| p.to_vec()).collect(),
                                proved_key_value,
                            );
                        result.push(path_key_optional_value.try_into_versioned(grove_version)?);
                        limit_left.iter_mut().for_each(|limit| *limit -= 1);
                        if limit_left == &Some(0) {
                            break;
                        }
                    }
                }
            }
        }

        Ok(root_hash)
    }

    /// Verify an MMR lower layer proof and add results.
    /// Returns the computed MMR root hash, which the caller uses as the
    /// child hash for Merk authentication (`combine_hash(value_hash ||
    /// mmr_root)`).
    fn verify_mmr_lower_layer<T>(
        mmr_bytes: &[u8],
        element: &Element,
        path: &[&[u8]],
        limit_left: &mut Option<u16>,
        result: &mut Vec<T>,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<CryptoHash, Error>
    where
        T: TryFromVersioned<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFromVersioned<ProvedPathKeyOptionalValue>>::Error>,
    {
        // Extract the MMR size from the element
        let element_mmr_size = match element {
            Element::MmrTree(mmr_size, ..) => *mmr_size,
            _ => {
                return Err(Error::InvalidProof(
                    query.clone(),
                    "MMR proof attached to non-MmrTree element".to_string(),
                ))
            }
        };

        let mmr_proof = grovedb_merkle_mountain_range::MmrTreeProof::decode_from_slice(mmr_bytes)
            .map_err(|e| Error::CorruptedData(format!("{}", e)))?;

        // Cross-validate: proof's mmr_size must match the element's mmr_size
        if mmr_proof.mmr_size() != element_mmr_size {
            return Err(Error::InvalidProof(
                query.clone(),
                format!(
                    "MMR proof mmr_size {} does not match element mmr_size {}",
                    mmr_proof.mmr_size(),
                    element_mmr_size
                ),
            ));
        }

        // Compute root from the proof — the Merk child hash mechanism
        // authenticates this root via combine_hash(value_hash || mmr_root)
        let (mmr_root, verified_leaves) = mmr_proof
            .verify_and_get_root()
            .map_err(|e| Error::InvalidProof(query.clone(), format!("{}", e)))?;

        // Get the sub-query items for this path to enforce succinctness.
        let sub_query =
            query
                .query_items_at_path(path, grove_version)?
                .ok_or(Error::InvalidProof(
                    query.clone(),
                    "MMR path not found in query".to_string(),
                ))?;

        // Build expected and proved sets for completeness + soundness.
        let leaf_count = grovedb_merkle_mountain_range::mmr_size_to_leaf_count(element_mmr_size);
        let expected_indices = Self::expand_query_to_u64_positions(&sub_query.items, leaf_count)?;
        let proved_indices: BTreeSet<u64> = verified_leaves.iter().map(|(idx, _)| *idx).collect();

        // Soundness: no unrequested leaves in the proof.
        let extra: Vec<u64> = proved_indices
            .difference(&expected_indices)
            .copied()
            .collect();
        if !extra.is_empty() {
            return Err(Error::InvalidProof(
                query.clone(),
                format!("MMR proof contains unrequested leaf indices {:?}", extra),
            ));
        }

        // Completeness: every requested leaf must be in the proof.
        let missing: Vec<u64> = expected_indices
            .difference(&proved_indices)
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(Error::InvalidProof(
                query.clone(),
                format!("MMR proof missing requested leaf indices {:?}", missing),
            ));
        }

        // Add each verified leaf to the result set
        for (leaf_index, value) in verified_leaves {
            let key = leaf_index.to_be_bytes().to_vec();
            let element = Element::new_item(value);
            let serialized = element.serialize(grove_version).map_err(|e| {
                Error::CorruptedData(format!("failed to serialize MMR leaf element: {}", e))
            })?;

            let path_key_optional_value = ProvedPathKeyOptionalValue {
                path: path.iter().map(|p| p.to_vec()).collect(),
                key,
                value: Some(serialized),
                proof: [0u8; 32],
            };
            result.push(path_key_optional_value.try_into_versioned(grove_version)?);

            limit_left
                .iter_mut()
                .for_each(|limit| *limit = limit.saturating_sub(1));
            if limit_left == &Some(0) {
                break;
            }
        }

        // Return the computed MMR root as the child hash
        Ok(mmr_root)
    }

    /// Verify a BulkAppendTree lower layer proof and add results.
    ///
    /// For both `BulkAppendTree` and `CommitmentTree` elements: verifies
    /// internal consistency and returns the computed state_root as the lower
    /// hash (authenticated via child Merk hash).
    fn verify_bulk_append_lower_layer<T>(
        bulk_bytes: &[u8],
        element: &Element,
        path: &[&[u8]],
        limit_left: &mut Option<u16>,
        result: &mut Vec<T>,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<CryptoHash, Error>
    where
        T: TryFromVersioned<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFromVersioned<ProvedPathKeyOptionalValue>>::Error>,
    {
        let (element_total_count, element_height) = match element {
            Element::BulkAppendTree(total_count, height, _) => (*total_count, *height),
            Element::CommitmentTree(total_count, height, _) => (*total_count, *height),
            _ => {
                return Err(Error::InvalidProof(
                    query.clone(),
                    "BulkAppendTree proof attached to incompatible element".to_string(),
                ))
            }
        };

        let bulk_proof =
            grovedb_bulk_append_tree::BulkAppendTreeProof::decode_from_slice(bulk_bytes)
                .map_err(|e| Error::CorruptedData(format!("{}", e)))?;

        let (bulk_state_root, proof_result) = bulk_proof
            .verify_and_compute_root(element_height, element_total_count)
            .map_err(|e| Error::InvalidProof(query.clone(), format!("{}", e)))?;

        // Get the query range from the path query to extract matching values
        let sub_query =
            query
                .query_items_at_path(path, grove_version)?
                .ok_or(Error::InvalidProof(
                    query.clone(),
                    "BulkAppendTree path not found in query".to_string(),
                ))?;

        // Extract a bounding range from query items to pull values from the
        // proof result. This may be broader than the actual query (e.g. for
        // disjoint queries), so we filter each position below.
        let (start, end) = Self::extract_range_from_query_items(&sub_query.items)?;
        let end = end.min(proof_result.total_count);

        let values = proof_result
            .values_in_range(start, end)
            .map_err(|e| Error::CorruptedData(format!("{}", e)))?;

        // Completeness: every position the query expects must be present in
        // the proof values. Build the expected set and check coverage.
        let expected_positions =
            Self::expand_query_to_u64_positions(&sub_query.items, element_total_count)?;
        let proved_positions: BTreeSet<u64> = values.iter().map(|(pos, _)| *pos).collect();
        let missing: Vec<u64> = expected_positions
            .difference(&proved_positions)
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(Error::InvalidProof(
                query.clone(),
                format!(
                    "BulkAppendTree proof missing requested positions {:?}",
                    missing
                ),
            ));
        }

        // Filter values by checking each position against the actual query
        // items. This enforces soundness: only positions matching the
        // original query are included in results.
        for (position, value) in values {
            let key = position.to_be_bytes().to_vec();
            if !sub_query.items.iter().any(|item| item.contains(&key)) {
                continue;
            }
            let element = Element::new_item(value);
            let serialized = element.serialize(grove_version).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to serialize BulkAppendTree entry element: {}",
                    e
                ))
            })?;

            let path_key_optional_value = ProvedPathKeyOptionalValue {
                path: path.iter().map(|p| p.to_vec()).collect(),
                key,
                value: Some(serialized),
                proof: [0u8; 32],
            };
            result.push(path_key_optional_value.try_into_versioned(grove_version)?);

            limit_left
                .iter_mut()
                .for_each(|limit| *limit = limit.saturating_sub(1));
            if limit_left == &Some(0) {
                break;
            }
        }

        // Return computed state_root as child Merk hash
        Ok(bulk_state_root)
    }

    /// Verify a CommitmentTree lower layer proof and add results.
    ///
    /// The proof bytes are `sinsemilla_root (32 bytes) || bulk_append_proof`.
    /// Verifies the BulkAppendTree proof to get `bulk_state_root`, then returns
    /// `blake3("ct_state" || sinsemilla_root || bulk_state_root)` as the
    /// authenticated child hash.
    fn verify_commitment_tree_lower_layer<T>(
        ct_bytes: &[u8],
        element: &Element,
        path: &[&[u8]],
        limit_left: &mut Option<u16>,
        result: &mut Vec<T>,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<CryptoHash, Error>
    where
        T: TryFromVersioned<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFromVersioned<ProvedPathKeyOptionalValue>>::Error>,
    {
        if ct_bytes.len() < 32 {
            return Err(Error::InvalidProof(
                query.clone(),
                "CommitmentTree proof too short (missing sinsemilla_root)".to_string(),
            ));
        }

        let sinsemilla_root: [u8; 32] = ct_bytes[..32]
            .try_into()
            .expect("sinsemilla_root is exactly 32 bytes");
        let bulk_proof_bytes = &ct_bytes[32..];

        // Verify the bulk append proof to get bulk_state_root
        let bulk_state_root = Self::verify_bulk_append_lower_layer(
            bulk_proof_bytes,
            element,
            path,
            limit_left,
            result,
            query,
            grove_version,
        )?;

        // Combine sinsemilla_root with bulk_state_root to produce the
        // authenticated child hash.
        // Inlined from grovedb_commitment_tree::compute_commitment_tree_state_root
        // to avoid pulling the heavy orchard crate into the verify feature.
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ct_state");
        hasher.update(&sinsemilla_root);
        hasher.update(&bulk_state_root);
        Ok(*hasher.finalize().as_bytes())
    }

    /// Verify a DenseAppendOnlyFixedSizeTree lower layer proof and add results.
    /// Returns NULL_HASH since DenseTree has no child Merk.
    fn verify_dense_tree_lower_layer<T>(
        dense_bytes: &[u8],
        element: &Element,
        path: &[&[u8]],
        limit_left: &mut Option<u16>,
        result: &mut Vec<T>,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<CryptoHash, Error>
    where
        T: TryFromVersioned<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFromVersioned<ProvedPathKeyOptionalValue>>::Error>,
    {
        // Extract the count and height from the authenticated element
        let (element_count, element_height) = match element {
            Element::DenseAppendOnlyFixedSizeTree(count, height, _) => (*count, *height),
            _ => {
                return Err(Error::InvalidProof(
                    query.clone(),
                    "DenseTree proof attached to non-DenseTree element".to_string(),
                ))
            }
        };

        let dense_proof =
            grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof::decode_from_slice(dense_bytes)
                .map_err(|e| Error::CorruptedData(format!("{}", e)))?;

        // Get the sub-query items for this path to build a query for
        // verify_for_query, which enforces both completeness and soundness.
        let sub_query =
            query
                .query_items_at_path(path, grove_version)?
                .ok_or(Error::InvalidProof(
                    query.clone(),
                    "DenseTree path not found in query".to_string(),
                ))?;
        let level_query = Query {
            items: sub_query.items.to_vec(),
            left_to_right: sub_query.left_to_right,
            ..Default::default()
        };

        // verify_for_query validates height+count from the authenticated Element,
        // checks completeness (all queried positions present) and soundness
        // (no unrequested positions), then returns only matching entries.
        let (computed_root, verified_entries): ([u8; 32], Vec<(u16, Vec<u8>)>) = dense_proof
            .verify_for_query(&level_query, element_height, element_count)
            .map_err(|e| Error::InvalidProof(query.clone(), format!("{}", e)))?;

        // Add each verified entry to the result set
        for (position, value) in verified_entries {
            let key = position.to_be_bytes().to_vec();
            let elem = Element::new_item(value);
            let serialized = elem.serialize(grove_version).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to serialize DenseTree entry element: {}",
                    e
                ))
            })?;

            let path_key_optional_value = ProvedPathKeyOptionalValue {
                path: path.iter().map(|p| p.to_vec()).collect(),
                key,
                value: Some(serialized),
                proof: [0u8; 32],
            };
            result.push(path_key_optional_value.try_into_versioned(grove_version)?);

            limit_left
                .iter_mut()
                .for_each(|limit| *limit = limit.saturating_sub(1));
            if limit_left == &Some(0) {
                break;
            }
        }

        // Return computed dense root as the child hash for Merk verification
        Ok(computed_root)
    }

    /// Extract a position range from query items (used by BulkAppendTree
    /// verification).
    fn extract_range_from_query_items(
        items: &[grovedb_merk::proofs::query::QueryItem],
    ) -> Result<(u64, u64), Error> {
        use grovedb_merk::proofs::query::QueryItem;

        let mut min_start = u64::MAX;
        let mut max_end = 0u64;

        for item in items {
            match item {
                QueryItem::Key(k) => {
                    if k.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree key must be 8 bytes (BE u64)",
                        ));
                    }
                    let pos = u64::from_be_bytes(k[..8].try_into().unwrap());
                    min_start = min_start.min(pos);
                    max_end = max_end.max(pos.saturating_add(1));
                }
                QueryItem::Range(r) => {
                    if r.start.len() != 8 || r.end.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range keys must be 8 bytes (BE u64)",
                        ));
                    }
                    let s = u64::from_be_bytes(r.start[..8].try_into().unwrap());
                    let e = u64::from_be_bytes(r.end[..8].try_into().unwrap());
                    min_start = min_start.min(s);
                    max_end = max_end.max(e);
                }
                QueryItem::RangeInclusive(r) => {
                    if r.start().len() != 8 || r.end().len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range keys must be 8 bytes (BE u64)",
                        ));
                    }
                    let s = u64::from_be_bytes(r.start()[..8].try_into().unwrap());
                    let e = u64::from_be_bytes(r.end()[..8].try_into().unwrap());
                    min_start = min_start.min(s);
                    max_end = max_end.max(e.saturating_add(1));
                }
                QueryItem::RangeFull(..) => {
                    min_start = 0;
                    max_end = u64::MAX;
                }
                QueryItem::RangeFrom(r) => {
                    if r.start.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range key must be 8 bytes (BE u64)",
                        ));
                    }
                    let s = u64::from_be_bytes(r.start[..8].try_into().unwrap());
                    min_start = min_start.min(s);
                    max_end = u64::MAX;
                }
                QueryItem::RangeTo(r) => {
                    if r.end.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range key must be 8 bytes (BE u64)",
                        ));
                    }
                    let e = u64::from_be_bytes(r.end[..8].try_into().unwrap());
                    min_start = 0;
                    max_end = max_end.max(e);
                }
                QueryItem::RangeToInclusive(r) => {
                    if r.end.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range key must be 8 bytes (BE u64)",
                        ));
                    }
                    let e = u64::from_be_bytes(r.end[..8].try_into().unwrap());
                    min_start = 0;
                    max_end = max_end.max(e.saturating_add(1));
                }
                QueryItem::RangeAfter(r) => {
                    if r.start.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range key must be 8 bytes (BE u64)",
                        ));
                    }
                    let s = u64::from_be_bytes(r.start[..8].try_into().unwrap());
                    min_start = min_start.min(s.saturating_add(1));
                    max_end = u64::MAX;
                }
                QueryItem::RangeAfterTo(r) => {
                    if r.start.len() != 8 || r.end.len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range keys must be 8 bytes (BE u64)",
                        ));
                    }
                    let s = u64::from_be_bytes(r.start[..8].try_into().unwrap());
                    let e = u64::from_be_bytes(r.end[..8].try_into().unwrap());
                    min_start = min_start.min(s.saturating_add(1));
                    max_end = max_end.max(e);
                }
                QueryItem::RangeAfterToInclusive(r) => {
                    if r.start().len() != 8 || r.end().len() != 8 {
                        return Err(Error::InvalidInput(
                            "BulkAppendTree range keys must be 8 bytes (BE u64)",
                        ));
                    }
                    let s = u64::from_be_bytes(r.start()[..8].try_into().unwrap());
                    let e = u64::from_be_bytes(r.end()[..8].try_into().unwrap());
                    min_start = min_start.min(s.saturating_add(1));
                    max_end = max_end.max(e.saturating_add(1));
                }
            }
        }

        if min_start == u64::MAX {
            return Err(Error::InvalidInput(
                "No valid range items found in BulkAppendTree query",
            ));
        }

        Ok((min_start, max_end))
    }

    /// Expand query items (with BE u64 keys) into a set of individual positions
    /// bounded by `count`. Used for completeness checking in non-Merk
    /// verifiers.
    fn expand_query_to_u64_positions(
        items: &[grovedb_merk::proofs::query::QueryItem],
        count: u64,
    ) -> Result<BTreeSet<u64>, Error> {
        use grovedb_merk::proofs::query::QueryItem;

        fn be_u64(key: &[u8]) -> Result<u64, Error> {
            let arr: [u8; 8] = key.try_into().map_err(|_| {
                Error::InvalidInput("position key must be exactly 8 bytes (BE u64)")
            })?;
            Ok(u64::from_be_bytes(arr))
        }

        if count == 0 {
            return Ok(BTreeSet::new());
        }

        const MAX_POSITIONS: usize = 10_000_000;
        let max_idx = count - 1;
        let mut positions = BTreeSet::new();

        macro_rules! check_cap {
            ($positions:expr) => {
                if $positions.len() > MAX_POSITIONS {
                    return Err(Error::InvalidInput("query range too large"));
                }
            };
        }

        for item in items {
            match item {
                QueryItem::Key(key) => {
                    let idx = be_u64(key)?;
                    if idx < count {
                        positions.insert(idx);
                    }
                }
                QueryItem::RangeInclusive(range) => {
                    let s = be_u64(range.start())?;
                    let e = be_u64(range.end())?.min(max_idx);
                    for idx in s..=e {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::Range(range) => {
                    let s = be_u64(&range.start)?;
                    let e = be_u64(&range.end)?.min(count);
                    for idx in s..e {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeFrom(range) => {
                    let s = be_u64(&range.start)?;
                    for idx in s..count {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeTo(range) => {
                    let e = be_u64(&range.end)?.min(count);
                    for idx in 0..e {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeToInclusive(range) => {
                    let e = be_u64(&range.end)?.min(max_idx);
                    for idx in 0..=e {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeFull(..) => {
                    for idx in 0..count {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeAfter(range) => {
                    let s = be_u64(&range.start)?;
                    for idx in s.saturating_add(1)..count {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeAfterTo(range) => {
                    let s = be_u64(&range.start)?;
                    let e = be_u64(&range.end)?.min(count);
                    for idx in s.saturating_add(1)..e {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
                QueryItem::RangeAfterToInclusive(range) => {
                    let s = be_u64(range.start())?;
                    let e = be_u64(range.end())?.min(max_idx);
                    for idx in s.saturating_add(1)..=e {
                        positions.insert(idx);
                        check_cap!(positions);
                    }
                }
            }
        }

        Ok(positions)
    }

    fn verify_layer_proof<T>(
        layer_proof: &MerkOnlyLayerProof,
        prove_options: &ProveOptions,
        query: &PathQuery,
        limit_left: &mut Option<u16>,
        current_path: &[&[u8]],
        result: &mut Vec<T>,
        last_parent_tree_type: &mut Option<TreeFeatureType>,
        options: &VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<CryptoHash, Error>
    where
        T: TryFromVersioned<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFromVersioned<ProvedPathKeyOptionalValue>>::Error>,
    {
        check_grovedb_v0!(
            "verify_layer_proof",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_layer_proof
        );
        let internal_query = query
            .query_items_at_path(current_path, grove_version)?
            .ok_or(Error::CorruptedPath(format!(
                "verify raw: path {} should be part of path_query {}",
                current_path
                    .iter()
                    .map(hex::encode)
                    .collect::<Vec<_>>()
                    .join("/"),
                query
            )))?;

        let level_query = Query {
            items: internal_query.items.to_vec(),
            left_to_right: internal_query.left_to_right,
            ..Default::default()
        };

        let (root_hash, merk_result) = level_query
            .execute_proof(
                &layer_proof.merk_proof,
                *limit_left,
                internal_query.left_to_right,
            )
            .unwrap()
            .map_err(|e| {
                eprintln!("{e}");
                Error::InvalidProof(
                    query.clone(),
                    format!("Invalid proof verification parameters: {}", e),
                )
            })?;

        #[cfg(feature = "proof_debug")]
        {
            println!(
                "\nDEBUG: Layer proof verification at path {:?}",
                current_path.iter().map(hex::encode).collect::<Vec<_>>()
            );
            println!("  Calculated root hash: {}", hex::encode(&root_hash));
            if let Some(parent_type) = last_parent_tree_type {
                println!("  Parent tree type: {:?}", parent_type);
            }
        }
        #[cfg(feature = "proof_debug")]
        {
            println!(
                "current path {} \n merk result is {}",
                path_as_slices_hex_to_ascii(current_path),
                merk_result
            );
        }

        let mut verified_keys = BTreeSet::new();

        if merk_result.result_set.is_empty() {
            if prove_options.decrease_limit_on_empty_sub_query_result {
                limit_left.iter_mut().for_each(|limit| *limit -= 1);
            }
        } else {
            for proved_key_value in merk_result.result_set {
                let mut path = current_path.to_vec();
                let key = &proved_key_value.key;
                let hash = &proved_key_value.proof;
                if let Some(value_bytes) = &proved_key_value.value {
                    let element = Element::deserialize(value_bytes, grove_version)?;

                    verified_keys.insert(key.clone());

                    if let Some(lower_layer) = layer_proof.lower_layers.get(key) {
                        #[cfg(feature = "proof_debug")]
                        {
                            println!("lower layer had key {}", hex_to_ascii(key));
                        }
                        match element {
                            Element::Tree(Some(_), _)
                            | Element::SumTree(Some(_), ..)
                            | Element::BigSumTree(Some(_), ..)
                            | Element::CountTree(Some(_), ..)
                            | Element::CountSumTree(Some(_), ..)
                            | Element::ProvableCountTree(Some(_), ..)
                            | Element::ProvableCountSumTree(Some(_), ..) => {
                                path.push(key);
                                *last_parent_tree_type = element.tree_feature_type();
                                if query.query_items_at_path(&path, grove_version)?.is_none() {
                                    // We are actually looking for the tree
                                    let path_key_optional_value =
                                        ProvedPathKeyOptionalValue::from_proved_key_value(
                                            path.iter().map(|p| p.to_vec()).collect(),
                                            proved_key_value,
                                        );
                                    #[cfg(feature = "proof_debug")]
                                    {
                                        println!(
                                            "pushing {} limit left after is {:?}",
                                            &path_key_optional_value, limit_left
                                        );
                                    }
                                    result.push(
                                        path_key_optional_value
                                            .try_into_versioned(grove_version)?,
                                    );

                                    limit_left.iter_mut().for_each(|limit| *limit -= 1);
                                    if limit_left == &Some(0) {
                                        break;
                                    }
                                } else {
                                    if query.should_add_parent_tree_at_path(
                                        current_path,
                                        grove_version,
                                    )? {
                                        let path_key_optional_value =
                                            ProvedPathKeyOptionalValue::from_proved_key_value(
                                                path.iter().map(|p| p.to_vec()).collect(),
                                                proved_key_value.clone(),
                                            );

                                        result.push(
                                            path_key_optional_value
                                                .try_into_versioned(grove_version)?,
                                        );
                                    }
                                    let lower_hash = Self::verify_layer_proof(
                                        lower_layer,
                                        prove_options,
                                        query,
                                        limit_left,
                                        &path,
                                        result,
                                        last_parent_tree_type,
                                        options,
                                        grove_version,
                                    )?;

                                    #[cfg(feature = "proof_debug")]
                                    {
                                        println!("\nDEBUG: Lower layer verification completed");
                                        println!(
                                            "  Path: {:?}",
                                            path.iter()
                                                .map(|p| hex_to_ascii(p))
                                                .collect::<Vec<_>>()
                                        );
                                        println!(
                                            "  Lower layer root hash: {}",
                                            hex::encode(&lower_hash)
                                        );
                                        println!("  Parent tree type: {:?}", last_parent_tree_type);
                                    }
                                    let combined_root_hash =
                                        combine_hash(value_hash(value_bytes).value(), &lower_hash)
                                            .value()
                                            .to_owned();

                                    #[cfg(feature = "proof_debug")]
                                    {
                                        println!("\nDEBUG: Tree element verification");
                                        println!("  Key: {}", hex_to_ascii(key));
                                        println!(
                                            "  Element type: {:?}",
                                            element.tree_feature_type()
                                        );
                                        println!("  Value bytes: {}", hex::encode(value_bytes));
                                        println!(
                                            "  Value bytes hash: {}",
                                            hex::encode(value_hash(value_bytes).value())
                                        );
                                        println!(
                                            "  Lower layer hash: {}",
                                            hex::encode(&lower_hash)
                                        );
                                        println!(
                                            "  Combined hash: {}",
                                            hex::encode(&combined_root_hash)
                                        );
                                        println!("  Expected hash: {}", hex::encode(hash));
                                    }
                                    if hash != &combined_root_hash {
                                        return Err(Error::InvalidProof(
                                            query.clone(),
                                            format!(
                                                "Mismatch in lower layer hash, expected {}, got {}",
                                                hex::encode(hash),
                                                hex::encode(combined_root_hash)
                                            ),
                                        ));
                                    }
                                    if limit_left == &Some(0) {
                                        break;
                                    }
                                }
                            }
                            Element::Tree(None, _)
                            | Element::SumTree(None, ..)
                            | Element::BigSumTree(None, ..)
                            | Element::CountTree(None, ..)
                            | Element::CountSumTree(None, ..)
                            | Element::ProvableCountTree(None, ..)
                            | Element::ProvableCountSumTree(None, ..)
                            | Element::CommitmentTree(..)
                            | Element::MmrTree(..)
                            | Element::BulkAppendTree(..)
                            | Element::DenseAppendOnlyFixedSizeTree(..)
                            | Element::SumItem(..)
                            | Element::Item(..)
                            | Element::ItemWithSumItem(..)
                            | Element::Reference(..) => {
                                return Err(Error::InvalidProof(
                                    query.clone(),
                                    "Proof has lower layer for a non Tree.".to_string(),
                                ));
                            }
                        }
                    } else if element.is_any_item()
                        || !internal_query.has_subquery_or_matching_in_path_on_key(key)
                            && (options.include_empty_trees_in_result
                                || !matches!(element, Element::Tree(None, _)))
                    {
                        let path_key_optional_value =
                            ProvedPathKeyOptionalValue::from_proved_key_value(
                                path.iter().map(|p| p.to_vec()).collect(),
                                proved_key_value,
                            );
                        #[cfg(feature = "proof_debug")]
                        {
                            println!(
                                "pushing {} limit left after is {:?}",
                                &path_key_optional_value, limit_left
                            );
                        }
                        result.push(path_key_optional_value.try_into_versioned(grove_version)?);

                        limit_left.iter_mut().for_each(|limit| *limit -= 1);
                        if limit_left == &Some(0) {
                            break;
                        }
                    } else {
                        #[cfg(feature = "proof_debug")]
                        {
                            println!(
                                "we have subquery on key {} with value {}: {}",
                                hex_to_ascii(key),
                                element,
                                level_query
                            )
                        }
                    }
                }
            }
        }

        Ok(root_hash)
    }

    pub fn verify_query(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_query",
            grove_version.grovedb_versions.operations.proof.verify_query
        );
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
    }

    pub fn verify_subset_query(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_subset_query",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_subset_query
        );
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
    }

    /// The point of this query is to get the parent tree information which will
    /// be present because we are querying in a subtree
    pub fn verify_query_get_parent_tree_info(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, TreeFeatureType, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_query_get_parent_tree_info",
            grove_version.grovedb_versions.operations.proof.verify_query
        );
        Self::verify_query_get_parent_tree_info_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
    }

    /// The point of this query is to get the parent tree information which will
    /// be present because we are querying in a subtree
    pub fn verify_subset_query_get_parent_tree_info(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, TreeFeatureType, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_subset_query_get_parent_tree_info",
            grove_version.grovedb_versions.operations.proof.verify_query
        );
        Self::verify_query_get_parent_tree_info_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
    }

    pub fn verify_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_query_with_absence_proof",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_absence_proof
        );
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
    }

    pub fn verify_subset_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        check_grovedb_v0!(
            "verify_subset_query_with_absence_proof",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_subset_query_with_absence_proof
        );
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
    }

    /// Verify subset proof with a chain of path query functions.
    /// After subset verification with the first path query, the result if
    /// passed to the next path query generation function which generates a
    /// new path query Apply the new path query, and pass the result to the
    /// next ... This is useful for verifying proofs with multiple path
    /// queries that depend on one another.
    pub fn verify_query_with_chained_path_queries<C>(
        proof: &[u8],
        first_query: &PathQuery,
        chained_path_queries: Vec<C>,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<Vec<PathKeyOptionalElementTrio>>), Error>
    where
        C: Fn(Vec<PathKeyOptionalElementTrio>) -> Option<PathQuery>,
    {
        check_grovedb_v0!(
            "verify_query_with_chained_path_queries",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_chained_path_queries
        );
        let mut results = vec![];

        let (last_root_hash, elements) =
            Self::verify_subset_query(proof, first_query, grove_version)?;
        results.push(elements);

        // Process the chained path queries
        Self::process_chained_path_queries(
            proof,
            last_root_hash,
            chained_path_queries,
            grove_version,
            &mut results,
        )?;

        Ok((last_root_hash, results))
    }

    /// Processes each chained path query and verifies it.
    pub(in crate::operations::proof) fn process_chained_path_queries<C>(
        proof: &[u8],
        last_root_hash: CryptoHash,
        chained_path_queries: Vec<C>,
        grove_version: &GroveVersion,
        results: &mut Vec<Vec<PathKeyOptionalElementTrio>>,
    ) -> Result<(), Error>
    where
        C: Fn(Vec<PathKeyOptionalElementTrio>) -> Option<PathQuery>,
    {
        for path_query_generator in chained_path_queries {
            let new_path_query = path_query_generator(results[results.len() - 1].clone()).ok_or(
                Error::InvalidInput("one of the path query generators returns no path query"),
            )?;

            let (new_root_hash, new_elements) =
                Self::verify_subset_query(proof, &new_path_query, grove_version)?;

            if new_root_hash != last_root_hash {
                return Err(Error::InvalidProof(
                    new_path_query,
                    format!(
                        "Root hash for different path queries do not match, first is {}, this one \
                         is {}",
                        hex::encode(last_root_hash),
                        hex::encode(new_root_hash)
                    ),
                ));
            }

            results.push(new_elements);
        }

        Ok(())
    }

    /// Verifies a trunk chunk proof and returns a `GroveTrunkQueryResult`.
    ///
    /// This method verifies a proof generated by `prove_trunk_chunk`, walking
    /// through the path layers and verifying each one. At the target tree,
    /// it decodes and executes the trunk proof to extract the elements and
    /// leaf keys.
    ///
    /// # Arguments
    /// * `proof` - The serialized proof bytes
    /// * `query` - The path trunk chunk query (used to navigate the proof)
    /// * `grove_version` - The GroveDB version for element deserialization
    ///
    /// # Returns
    /// A tuple of:
    /// * `CryptoHash` - The root hash of the entire GroveDB
    /// * `GroveTrunkQueryResult` - The verified result with elements, leaf
    ///   keys, chunk depths, and tree depth
    pub fn verify_trunk_chunk_proof(
        proof: &[u8],
        query: &PathTrunkChunkQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, GroveTrunkQueryResult), Error> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        match grovedb_proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_trunk_chunk_proof_v0(&proof_v0, query, grove_version)
            }
            GroveDBProof::V1(_) => Err(Error::NotSupported(
                "V1 trunk chunk proof verification not yet implemented".to_string(),
            )),
        }
    }

    fn verify_trunk_chunk_proof_v0(
        proof: &GroveDBProofV0,
        query: &PathTrunkChunkQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, GroveTrunkQueryResult), Error> {
        // Collect layer info as we walk down the path for later verification
        struct LayerInfo {
            value_bytes: Vec<u8>,
            expected_hash: CryptoHash,
            /// The root hash of this layer's merk tree (used as child hash for
            /// parent layer)
            layer_root_hash: CryptoHash,
        }
        let mut layer_infos: Vec<LayerInfo> = Vec::new();

        let mut current_layer = &proof.root_layer;
        let mut current_path: Vec<Vec<u8>> = Vec::new();
        let mut count: Option<u64> = None;
        let mut grovedb_root_hash: Option<CryptoHash> = None;

        // Walk through each path segment, verifying layer proofs
        for (i, path_segment) in query.path.iter().enumerate() {
            // Create a simple key query for this path segment
            let key_query = Query {
                items: vec![grovedb_merk::proofs::query::QueryItem::Key(
                    path_segment.clone(),
                )],
                ..Default::default()
            };

            // Execute the proof to verify and get the root hash
            let (layer_root_hash, result) = key_query
                .execute_proof(&current_layer.merk_proof, None, true)
                .unwrap()
                .map_err(|e| {
                    Error::InvalidProof(
                        PathQuery::new_unsized(current_path.clone(), key_query.clone()),
                        format!("Invalid proof at path layer {}: {}", i, e),
                    )
                })?;

            // Store the root hash of the first layer as the GroveDB root hash
            if i == 0 {
                grovedb_root_hash = Some(layer_root_hash);
            }

            // Find the element for this key in the result set
            let mut found_value_bytes: Option<Vec<u8>> = None;
            let mut found_hash: Option<CryptoHash> = None;

            for proved_key_value in &result.result_set {
                if proved_key_value.key == *path_segment {
                    found_hash = Some(proved_key_value.proof);
                    if let Some(value_bytes) = &proved_key_value.value {
                        found_value_bytes = Some(value_bytes.clone());

                        // On the last path segment, extract the count from the CountTree element
                        if i == query.path.len() - 1 {
                            let element = Element::deserialize(value_bytes, grove_version)?;
                            count = Self::extract_count_from_element(&element);
                        }
                    }
                    break;
                }
            }

            let value_bytes = found_value_bytes.ok_or_else(|| {
                Error::InvalidProof(
                    PathQuery::new_unsized(current_path.clone(), key_query.clone()),
                    format!(
                        "Path segment {} not found in proof result",
                        hex::encode(path_segment)
                    ),
                )
            })?;

            let expected_hash = found_hash.ok_or_else(|| {
                Error::InvalidProof(
                    PathQuery::new_unsized(current_path.clone(), key_query.clone()),
                    format!(
                        "No hash found for path segment {}",
                        hex::encode(path_segment)
                    ),
                )
            })?;

            // Store layer info for later verification
            layer_infos.push(LayerInfo {
                value_bytes,
                expected_hash,
                layer_root_hash,
            });

            // Move to the next layer
            current_layer = current_layer
                .lower_layers
                .get(path_segment)
                .ok_or_else(|| {
                    Error::InvalidProof(
                        PathQuery::new_unsized(current_path.clone(), key_query),
                        format!(
                            "Missing lower layer for path segment {}",
                            hex::encode(path_segment)
                        ),
                    )
                })?;

            current_path.push(path_segment.clone());
        }

        // Ensure we got a count from the element
        let count = count.ok_or_else(|| {
            Error::InvalidProof(
                PathQuery::new_unsized(current_path.clone(), Query::default()),
                "Could not extract count from path - target is not a count tree element"
                    .to_string(),
            )
        })?;

        // Empty tree — return an empty result.
        // The parent layers' merk proofs already verified the hash chain integrity,
        // including the element's value_hash. The count is authenticated by the
        // parent proof. With count==0 there is no child data to verify against.
        if count == 0 {
            let grovedb_root_hash = grovedb_root_hash.ok_or_else(|| {
                Error::InvalidProof(
                    PathQuery::new_unsized(Vec::new(), Query::default()),
                    "Empty path - no root hash computed".to_string(),
                )
            })?;

            return Ok((
                grovedb_root_hash,
                GroveTrunkQueryResult {
                    elements: BTreeMap::new(),
                    leaf_keys: BTreeMap::new(),
                    chunk_depths: vec![],
                    max_tree_depth: 0,
                    tree: grovedb_merk::proofs::tree::Tree::from(Node::Hash(NULL_HASH)),
                },
            ));
        }

        let tree_depth = calculate_max_tree_depth_from_count(count);
        let chunk_depths = calculate_chunk_depths(tree_depth, query.max_depth);

        // Now we're at the target layer - decode and execute the trunk proof
        let decoder = Decoder::new(&current_layer.merk_proof);
        let ops: Vec<Op> = decoder
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::CorruptedData(format!("Failed to decode trunk proof: {}", e)))?;

        // Execute the proof to build the tree structure and get its root hash
        // Use collapse=false to preserve the full tree structure for element extraction
        let target_tree = execute(ops.iter().map(|op| Ok(op.clone())), false, |_| Ok(()))
            .unwrap()
            .map_err(|e| {
                Error::InvalidProof(
                    PathQuery::new_unsized(current_path.clone(), Query::default()),
                    format!("Failed to execute trunk proof: {}", e),
                )
            })?;

        let mut lower_hash = target_tree.hash().unwrap();

        // Verify the cryptographic chain from trunk up through all layers
        // Walk backwards through the layer infos, verifying each step
        for (i, layer_info) in layer_infos.iter().rev().enumerate() {
            let combined_hash =
                combine_hash(value_hash(&layer_info.value_bytes).value(), &lower_hash)
                    .value()
                    .to_owned();

            if combined_hash != layer_info.expected_hash {
                return Err(Error::InvalidProof(
                    PathQuery::new_unsized(current_path.clone(), Query::default()),
                    format!(
                        "Hash mismatch at layer {} from bottom: expected {}, got {}",
                        i,
                        hex::encode(layer_info.expected_hash),
                        hex::encode(combined_hash)
                    ),
                ));
            }

            // For the next iteration, use this layer's merk tree root hash.
            // This is what the parent layer uses as the child hash for this subtree.
            lower_hash = layer_info.layer_root_hash;
        }

        // Extract elements and leaf keys from the proof tree
        // The max depth for the trunk is the first chunk depth
        let max_depth = chunk_depths.first().copied().unwrap_or(0) as usize;
        let mut elements = BTreeMap::new();
        let mut leaf_keys = BTreeMap::new();
        Self::extract_elements_and_leaf_keys(
            &target_tree,
            &mut elements,
            &mut leaf_keys,
            0,
            max_depth,
            grove_version,
        )?;

        let grovedb_root_hash = grovedb_root_hash.ok_or_else(|| {
            Error::InvalidProof(
                PathQuery::new_unsized(Vec::new(), Query::default()),
                "Empty path - no root hash computed".to_string(),
            )
        })?;

        let trunk_result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths,
            max_tree_depth: tree_depth,
            tree: target_tree,
        };

        Ok((grovedb_root_hash, trunk_result))
    }

    /// Recursively extract elements and leaf keys from a proof tree.
    ///
    /// Elements are nodes with key-value data that can be deserialized.
    /// Leaf keys are nodes that have at least one `Node::Hash` child, mapped
    /// to their LeafInfo (hash + optional count for branch verification).
    ///
    /// # Arguments
    /// * `tree` - The proof tree to extract from
    /// * `elements` - Output map of key -> Element
    /// * `leaf_keys` - Output map of key -> LeafInfo (hash + count)
    /// * `current_depth` - Current depth in the tree (0 = root)
    /// * `max_depth` - Maximum allowed depth (nodes beyond this should be Hash)
    /// * `grove_version` - Version for Element deserialization
    fn extract_elements_and_leaf_keys(
        tree: &grovedb_merk::proofs::tree::Tree,
        elements: &mut BTreeMap<Vec<u8>, Element>,
        leaf_keys: &mut BTreeMap<Vec<u8>, crate::query::LeafInfo>,
        current_depth: usize,
        max_depth: usize,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        // Check that we haven't exceeded the max depth
        if current_depth > max_depth {
            return Err(Error::InvalidProof(
                PathQuery::new_unsized(Vec::new(), Query::default()),
                format!(
                    "Trunk proof exceeds max depth: current depth {} > max depth {}",
                    current_depth, max_depth
                ),
            ));
        }

        // Check for inconsistent depth: if one child is Hash and the other exists
        // but is not Hash, the proof has inconsistent truncation depth
        let left_is_hash = tree
            .left
            .as_ref()
            .map(|c| matches!(c.tree.node, Node::Hash(_)));
        let right_is_hash = tree
            .right
            .as_ref()
            .map(|c| matches!(c.tree.node, Node::Hash(_)));

        // If both children exist, they must both be Hash or both be non-Hash
        if let (Some(left_hash), Some(right_hash)) = (left_is_hash, right_is_hash) {
            if left_hash != right_hash {
                return Err(Error::InvalidProof(
                    PathQuery::new_unsized(Vec::new(), Query::default()),
                    "Inconsistent trunk proof depth: one child is Hash while the other is not"
                        .to_string(),
                ));
            }
        }

        // Extract key and value from this node - must exist for valid trunk proofs
        let (key, value) = Self::get_key_value_from_node(&tree.node).ok_or_else(|| {
            Error::InvalidProof(
                PathQuery::new_unsized(Vec::new(), Query::default()),
                format!(
                    "Trunk proof contains node without key/value data: {:?}",
                    tree.node
                ),
            )
        })?;

        let element = Element::deserialize(&value, grove_version)?;
        elements.insert(key.clone(), element);

        // Check if this node has Hash children (making it a leaf)
        let has_hash_child = left_is_hash.unwrap_or(false) || right_is_hash.unwrap_or(false);

        if has_hash_child {
            // Store the node's hash and count as LeafInfo for branch queries.
            // When a branch query is made for this key, the branch proof's root hash
            // should match this node's hash.
            let node_hash = tree.hash().unwrap();

            // Extract count from TreeFeatureType if available
            // Note: KVHashCount is not included as it never reaches this code path
            let count = match &tree.node {
                Node::KVValueHashFeatureType(_, _, _, feature_type) => feature_type.count(),
                Node::KVCount(_, _, count) => Some(*count),
                Node::KVRefValueHashCount(_, _, _, count) => Some(*count),
                _ => None,
            };

            leaf_keys.insert(
                key,
                crate::query::LeafInfo {
                    hash: node_hash,
                    count,
                },
            );
        }

        // Recurse into non-Hash children
        if let Some(left) = &tree.left {
            if !matches!(left.tree.node, Node::Hash(_)) {
                Self::extract_elements_and_leaf_keys(
                    &left.tree,
                    elements,
                    leaf_keys,
                    current_depth + 1,
                    max_depth,
                    grove_version,
                )?;
            }
        }
        if let Some(right) = &tree.right {
            if !matches!(right.tree.node, Node::Hash(_)) {
                Self::extract_elements_and_leaf_keys(
                    &right.tree,
                    elements,
                    leaf_keys,
                    current_depth + 1,
                    max_depth,
                    grove_version,
                )?;
            }
        }

        Ok(())
    }

    /// Extract key and value from a node if it has both.
    fn get_key_value_from_node(node: &Node) -> Option<(Vec<u8>, Vec<u8>)> {
        match node {
            Node::KV(key, value)
            | Node::KVValueHash(key, value, ..)
            | Node::KVValueHashFeatureType(key, value, ..)
            | Node::KVCount(key, value, ..)
            | Node::KVRefValueHash(key, value, ..)
            | Node::KVRefValueHashCount(key, value, ..) => Some((key.clone(), value.clone())),
            // These nodes don't have values, only key+hash or just hash
            Node::KVDigest(..)
            | Node::KVDigestCount(..)
            | Node::Hash(_)
            | Node::KVHash(_)
            | Node::KVHashCount(..) => None,
        }
    }

    /// Extract the count from a CountTree, CountSumTree, ProvableCountTree,
    /// or ProvableCountSumTree element.
    fn extract_count_from_element(element: &Element) -> Option<u64> {
        match element {
            Element::CountTree(_, count, _)
            | Element::CountSumTree(_, count, ..)
            | Element::ProvableCountTree(_, count, _)
            | Element::ProvableCountSumTree(_, count, ..) => Some(*count),
            _ => None,
        }
    }

    /// Verify a serialized branch chunk proof.
    ///
    /// # Arguments
    /// * `proof` - The serialized proof bytes
    /// * `query` - The path branch chunk query
    /// * `expected_root_hash` - The expected root hash of the branch (from
    ///   parent trunk/branch proof)
    /// * `grove_version` - The GroveDB version for element deserialization
    ///
    /// # Returns
    /// `GroveBranchQueryResult` containing:
    /// - Deserialized GroveDB Elements
    /// - Leaf keys with their hashes for subsequent branch queries (if more
    ///   depth remains)
    /// - The verified branch root hash
    pub fn verify_branch_chunk_proof(
        proof: &[u8],
        query: &crate::query::PathBranchChunkQuery,
        expected_root_hash: CryptoHash,
        grove_version: &GroveVersion,
    ) -> Result<crate::query::GroveBranchQueryResult, Error> {
        // Decode the proof ops
        let decoder = Decoder::new(proof);
        let ops: Vec<Op> = decoder
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::CorruptedData(format!("Failed to decode branch proof: {}", e)))?;

        // Execute the proof to build the tree structure and get its root hash
        // Use collapse=false to preserve the full tree structure for element extraction
        let branch_tree = execute(ops.iter().map(|op| Ok(op.clone())), false, |_| Ok(()))
            .unwrap()
            .map_err(|e| {
                Error::InvalidProof(
                    PathQuery::new_unsized(query.path.clone(), Query::default()),
                    format!("Failed to execute branch proof: {}", e),
                )
            })?;

        let branch_root_hash = branch_tree.hash().unwrap();

        // Verify the computed hash matches the expected hash from the parent proof
        if branch_root_hash != expected_root_hash {
            return Err(Error::InvalidProof(
                PathQuery::new_unsized(query.path.clone(), Query::default()),
                format!(
                    "Branch root hash mismatch: expected {}, got {}",
                    hex::encode(expected_root_hash),
                    hex::encode(branch_root_hash)
                ),
            ));
        }

        // Extract elements and leaf keys from the proof tree
        let mut elements = BTreeMap::new();
        let mut leaf_keys = BTreeMap::new();
        Self::extract_elements_and_leaf_keys(
            &branch_tree,
            &mut elements,
            &mut leaf_keys,
            0,
            query.depth as usize,
            grove_version,
        )?;

        Ok(crate::query::GroveBranchQueryResult {
            elements,
            leaf_keys,
            branch_root_hash,
            tree: branch_tree,
        })
    }
}
