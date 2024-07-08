use std::collections::{BTreeMap, BTreeSet};

use grovedb_merk::{
    proofs::{
        query::{PathKey, VerifyOptions},
        Query,
    },
    tree::{combine_hash, value_hash},
    CryptoHash,
};

use crate::{
    operations::proof::{
        generate::{GroveDBProof, GroveDBProofV0, LayerProof},
        util::{
            element_hex_to_ascii, hex_to_ascii, path_as_slices_hex_to_ascii, path_hex_to_ascii,
            ProvedPathKeyOptionalValue, ProvedPathKeyValues,
        },
        ProveOptions,
    },
    query_result_type::PathKeyOptionalElementTrio,
    Element, Error, GroveDb, PathQuery,
};

impl GroveDb {
    pub fn verify_query_with_options(
        proof: &[u8],
        query: &PathQuery,
        options: VerifyOptions,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        if options.absence_proofs_for_non_existing_searched_keys {
            // must have a limit
            query.query.limit.ok_or(Error::NotSupported(
                "limits must be set in verify_query_with_absence_proof".to_string(),
            ))? as usize;
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

        let (root_hash, result) = Self::verify_proof_internal(&grovedb_proof, query, options)?;

        Ok((root_hash, result))
    }

    pub fn verify_query_raw(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<(CryptoHash, ProvedPathKeyValues), Error> {
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
        )?;

        Ok((root_hash, result))
    }

    fn verify_proof_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        options: VerifyOptions,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => Self::verify_proof_internal_v0(proof_v0, query, options),
        }
    }

    fn verify_proof_internal_v0(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        options: VerifyOptions,
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
        )?;

        if options.absence_proofs_for_non_existing_searched_keys {
            // must have a limit
            let max_results = query.query.limit.ok_or(Error::NotSupported(
                "limits must be set in verify_query_with_absence_proof".to_string(),
            ))? as usize;

            let terminal_keys = query.terminal_keys(max_results)?;

            // convert the result set to a btree map
            let mut result_set_as_map: BTreeMap<PathKey, Option<Element>> = result
                .into_iter()
                .map(|(path, key, element)| ((path, key), element))
                .collect();

            println!(
                "t{:?}, r{:?}",
                terminal_keys
                    .iter()
                    .map(|(path, key)| (path_hex_to_ascii(path), hex_to_ascii(key)))
                    .collect::<Vec<_>>(),
                result_set_as_map
                    .iter()
                    .map(|((path, key), e)| ((path_hex_to_ascii(path), hex_to_ascii(key)), e))
                    .collect::<BTreeMap<_, _>>()
            );

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

    fn verify_proof_raw_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        options: VerifyOptions,
    ) -> Result<(CryptoHash, ProvedPathKeyValues), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_raw_internal_v0(proof_v0, query, options)
            }
        }
    }

    fn verify_proof_raw_internal_v0(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        options: VerifyOptions,
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
    ) -> Result<CryptoHash, Error>
    where
        T: TryFrom<ProvedPathKeyOptionalValue>,
        Error: From<<T as TryFrom<ProvedPathKeyOptionalValue>>::Error>,
    {
        let internal_query =
            query
                .query_items_at_path(current_path)
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

        println!(
            "current path {} merk result is {}",
            path_as_slices_hex_to_ascii(current_path),
            merk_result
        );

        let mut verified_keys = BTreeSet::new();

        if merk_result.result_set.is_empty() {
            limit_left.as_mut().map(|limit| *limit -= 1);
        } else {
            for proved_key_value in merk_result.result_set {
                let mut path = current_path.to_vec();
                let key = &proved_key_value.key;
                let hash = &proved_key_value.proof;
                if let Some(value_bytes) = &proved_key_value.value {
                    let element = Element::deserialize(value_bytes)?;

                    verified_keys.insert(key.clone());

                    if let Some(lower_layer) = layer_proof.lower_layers.get(key) {
                        println!("lower layer had key {}", hex_to_ascii(key));
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
                        println!(
                            "pushing {} limit left after is {:?}",
                            &path_key_optional_value, limit_left
                        );
                        result.push(path_key_optional_value.try_into()?);

                        limit_left.as_mut().map(|limit| *limit -= 1);
                        if limit_left == &Some(0) {
                            break;
                        }
                    } else {
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

        Ok(root_hash)
    }

    // fn verify_completeness(
    //     &self,
    //     query_items: &[QueryItem],
    //     result_set: &[ProvedKeyValue],
    //     current_path: &[Vec<u8>],
    // ) -> Result<(), Error> {
    //     let mut result_iter = result_set.iter().peekable();
    //
    //     for query_item in query_items {
    //         match query_item {
    //             QueryItem::Key(key) => {
    //                 if !self.verify_key_completeness(key, &mut result_iter)? {
    //                     return Err(Error::InvalidProof(format!(
    //                         "Key {:?} is missing and its absence is not proven",
    //                         hex::encode(key)
    //                     )));
    //                 }
    //             },
    //             QueryItem::Range(range) => {
    //                 self.verify_range_completeness(range, &mut result_iter)?;
    //             },
    //             QueryItem::RangeInclusive(range) => {
    //                 self.verify_range_inclusive_completeness(range, &mut
    // result_iter)?;             },
    //             // Add cases for other QueryItem variants as needed
    //             _ => return Err(Error::InvalidProof("Unsupported query item
    // type".into())),         }
    //     }
    //
    //     // Ensure we've consumed all results
    //     if result_iter.peek().is_some() {
    //         return Err(Error::InvalidProof("Proof contains extra, unexpected
    // results".into()));     }
    //
    //     Ok(())
    // }
    //
    // fn verify_key_completeness(
    //     &self,
    //     key: &[u8],
    //     result_iter: &mut std::iter::Peekable<std::slice::Iter<'_,
    // ProvedKeyValue>>, ) -> Result<bool, Error> {
    //     if let Some(result) = result_iter.peek() {
    //         if result.key == key {
    //             result_iter.next();  // Consume the result
    //             Ok(true)
    //         } else if result.key > key {
    //             // The key is missing, but this is okay as long as we can prove
    // its absence             self.verify_key_absence(key, result)
    //         } else {
    //             // This shouldn't happen if the result set is properly ordered
    //             Err(Error::InvalidProof("Result set is not properly
    // ordered".into()))         }
    //     } else {
    //         // We've run out of results, need to prove absence
    //         Err(Error::InvalidProof("Ran out of results unexpectedly".into()))
    //     }
    // }
    //
    // fn verify_range_completeness(
    //     &self,
    //     range: &Range<Vec<u8>>,
    //     result_iter: &mut std::iter::Peekable<std::slice::Iter<'_,
    // ProvedKeyValue>>, ) -> Result<(), Error> {
    //     let mut current = range.start.clone();
    //     while current < range.end {
    //         if !self.verify_key_completeness(&current, result_iter)? {
    //             return Err(Error::InvalidProof(format!(
    //                 "Key {:?} in range is missing and its absence is not proven",
    //                 hex::encode(&current)
    //             )));
    //         }
    //         // Move to next key. This is a simplified approach and might need to
    // be adjusted         // based on your key structure.
    //         current = increment_key(&current);
    //     }
    //     Ok(())
    // }
    //
    // fn verify_range_inclusive_completeness(
    //     &self,
    //     range: &RangeInclusive<Vec<u8>>,
    //     result_iter: &mut std::iter::Peekable<std::slice::Iter<'_,
    // ProvedKeyValue>>, ) -> Result<(), Error> {
    //     let mut current = range.start().clone();
    //     while current <= *range.end() {
    //         if !self.verify_key_completeness(&current, result_iter)? {
    //             return Err(Error::InvalidProof(format!(
    //                 "Key {:?} in inclusive range is missing and its absence is
    // not proven",                 hex::encode(&current)
    //             )));
    //         }
    //         // Move to next key. This is a simplified approach and might need to
    // be adjusted         // based on your key structure.
    //         current = increment_key(&current);
    //     }
    //     Ok(())
    // }
    //
    // fn verify_key_absence(
    //     &self,
    //     key: &[u8],
    //     next_result: &ProvedKeyValue,
    // ) -> Result<bool, Error> {
    //     // This function should implement the logic to verify that a key's
    // absence is proven     // The exact implementation will depend on how your
    // system proves absences     // This might involve checking the hash of the
    // next present key, verifying that     // there's no possible key between
    // the absent key and the next present key, etc.
    //
    //     // For now, we'll just return Ok(false) as a placeholder
    //     Ok(false)
    // }
    //
    // fn increment_key(key: &[u8]) -> Vec<u8> {
    //     // This is a very simplified key incrementing function
    //     // You might need a more sophisticated approach depending on your key
    // structure     let mut new_key = key.to_vec();
    //     for byte in new_key.iter_mut().rev() {
    //         if *byte == 255 {
    //             *byte = 0;
    //         } else {
    //             *byte += 1;
    //             break;
    //         }
    //     }
    //     new_key
    // }

    pub fn verify_query(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
        )
    }

    pub fn verify_subset_query(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
        )
    }

    pub fn verify_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
        )
    }

    pub fn verify_subset_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        Self::verify_query_with_options(
            proof,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
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
    ) -> Result<(CryptoHash, Vec<Vec<PathKeyOptionalElementTrio>>), Error>
    where
        C: Fn(Vec<PathKeyOptionalElementTrio>) -> Option<PathQuery>,
    {
        let mut results = vec![];

        let (last_root_hash, elements) = Self::verify_subset_query(proof, first_query)?;
        results.push(elements);

        // we should iterate over each chained path queries
        for path_query_generator in chained_path_queries {
            let new_path_query = path_query_generator(results[results.len() - 1].clone()).ok_or(
                Error::InvalidInput("one of the path query generators returns no path query"),
            )?;
            let (new_root_hash, new_elements) = Self::verify_subset_query(proof, &new_path_query)?;
            if new_root_hash != last_root_hash {
                return Err(Error::InvalidProof(format!(
                    "root hash for different path queries do no match, first is {}, this one is {}",
                    hex::encode(last_root_hash),
                    hex::encode(new_root_hash)
                )));
            }
            results.push(new_elements);
        }

        Ok((last_root_hash, results))
    }
}
