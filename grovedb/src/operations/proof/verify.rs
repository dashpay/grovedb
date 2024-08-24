use std::collections::{BTreeMap, BTreeSet};

use grovedb_merk::{
    proofs::{
        query::{PathKey, VerifyOptions},
        Query,
    },
    tree::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_version::{
    check_grovedb_v0, error::GroveVersionError, version::GroveVersion, TryFromVersioned,
    TryIntoVersioned,
};

#[cfg(feature = "proof_debug")]
use crate::operations::proof::util::{
    hex_to_ascii, path_as_slices_hex_to_ascii, path_hex_to_ascii,
};
use crate::{
    operations::proof::{
        util::{ProvedPathKeyOptionalValue, ProvedPathKeyValues},
        GroveDBProof, GroveDBProofV0, LayerProof, ProveOptions,
    },
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
            .with_no_limit();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, result) =
            Self::verify_proof_internal(&grovedb_proof, query, options, grove_version)?;

        Ok((root_hash, result))
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
            .with_no_limit();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, result) = Self::verify_proof_raw_internal(
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
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_v0_internal(proof_v0, query, options, grove_version)
            }
        }
    }

    fn verify_proof_v0_internal(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let root_hash = Self::verify_layer_proof(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
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

        Ok((root_hash, result))
    }

    pub(crate) fn verify_proof_raw_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, ProvedPathKeyValues), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_raw_internal_v0(proof_v0, query, options, grove_version)
            }
        }
    }

    fn verify_proof_raw_internal_v0(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, ProvedPathKeyValues), Error> {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let root_hash = Self::verify_layer_proof(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
            &options,
            grove_version,
        )?;
        Ok((root_hash, result))
    }

    fn verify_layer_proof<T>(
        layer_proof: &LayerProof,
        prove_options: &ProveOptions,
        query: &PathQuery,
        limit_left: &mut Option<u16>,
        current_path: &[&[u8]],
        result: &mut Vec<T>,
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
                Error::InvalidProof(format!("invalid proof verification parameters: {}", e))
            })?;
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
                limit_left.as_mut().map(|limit| *limit -= 1);
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
                            Element::Tree(Some(_), _) | Element::SumTree(Some(_), ..) => {
                                path.push(key);
                                let lower_hash = Self::verify_layer_proof(
                                    lower_layer,
                                    prove_options,
                                    query,
                                    limit_left,
                                    &path,
                                    result,
                                    options,
                                    grove_version,
                                )?;
                                let combined_root_hash =
                                    combine_hash(value_hash(value_bytes).value(), &lower_hash)
                                        .value()
                                        .to_owned();
                                if hash != &combined_root_hash {
                                    return Err(Error::InvalidProof(format!(
                                        "Mismatch in lower layer hash, expected {}, got {}",
                                        hex::encode(hash),
                                        hex::encode(combined_root_hash)
                                    )));
                                }
                                if limit_left == &Some(0) {
                                    break;
                                }
                            }
                            Element::Tree(None, _)
                            | Element::SumTree(None, ..)
                            | Element::SumItem(..)
                            | Element::Item(..)
                            | Element::Reference(..) => {
                                return Err(Error::InvalidProof(
                                    "Proof has lower layer for a non Tree".into(),
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

                        limit_left.as_mut().map(|limit| *limit -= 1);
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
                return Err(Error::InvalidProof(format!(
                    "root hash for different path queries do not match, first is {}, this one is \
                     {}",
                    hex::encode(last_root_hash),
                    hex::encode(new_root_hash)
                )));
            }

            results.push(new_elements);
        }

        Ok(())
    }
}
