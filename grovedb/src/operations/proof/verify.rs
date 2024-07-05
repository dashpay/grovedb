use std::collections::BTreeSet;

use grovedb_merk::{
    execute_proof,
    proofs::Query,
    tree::{combine_hash, value_hash},
};

use crate::{
    operations::proof::{
        generate::{GroveDBProof, GroveDBProofV0, LayerProof},
        util::{ProvedPathKeyValue, ProvedPathKeyValues},
        ProveOptions,
    },
    query_result_type::PathKeyOptionalElementTrio,
    Element, Error, GroveDb, PathQuery,
};

impl GroveDb {
    pub fn verify_query(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, result) = Self::verify_proof_internal(&grovedb_proof, query, false)?;

        Ok((root_hash, result))
    }

    pub fn verify_query_raw(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], ProvedPathKeyValues), Error> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, result) = Self::verify_proof_raw_internal(&grovedb_proof, query, false)?;

        Ok((root_hash, result))
    }

    fn verify_proof_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        is_subset: bool,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_internal_v0(proof_v0, query, is_subset)
            }
        }
    }

    fn verify_proof_internal_v0(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        is_subset: bool,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        let mut result = Vec::new();
        let root_hash = Self::verify_layer_proof(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &[],
            &mut result,
            is_subset,
        )?;
        Ok((root_hash, result))
    }

    fn verify_proof_raw_internal(
        proof: &GroveDBProof,
        query: &PathQuery,
        is_subset: bool,
    ) -> Result<([u8; 32], ProvedPathKeyValues), Error> {
        match proof {
            GroveDBProof::V0(proof_v0) => {
                Self::verify_proof_raw_internal_v0(proof_v0, query, is_subset)
            }
        }
    }

    fn verify_proof_raw_internal_v0(
        proof: &GroveDBProofV0,
        query: &PathQuery,
        is_subset: bool,
    ) -> Result<([u8; 32], ProvedPathKeyValues), Error> {
        let mut result = Vec::new();
        let mut limit = query.query.limit;
        let root_hash = Self::verify_layer_proof_raw(
            &proof.root_layer,
            &proof.prove_options,
            query,
            &mut limit,
            &[],
            &mut result,
            is_subset,
        )?;
        Ok((root_hash, result))
    }

    fn verify_layer_proof(
        layer_proof: &LayerProof,
        prove_options: &ProveOptions,
        query: &PathQuery,
        current_path: &[&[u8]],
        result: &mut Vec<PathKeyOptionalElementTrio>,
        is_subset: bool,
    ) -> Result<[u8; 32], Error> {
        let internal_query =
            query
                .query_items_at_path(current_path)
                .ok_or(Error::CorruptedPath(format!(
                    "verify: path {} should be part of path_query {}",
                    current_path
                        .iter()
                        .map(hex::encode)
                        .collect::<Vec<_>>()
                        .join("/"),
                    query
                )))?;

        let level_query = Query {
            items: internal_query.items.to_vec(),
            default_subquery_branch: internal_query.default_subquery_branch.into_owned(),
            conditional_subquery_branches: internal_query
                .conditional_subquery_branches
                .map(|a| a.into_owned()),
            left_to_right: internal_query.left_to_right,
        };

        let (root_hash, merk_result) = execute_proof(
            &layer_proof.merk_proof,
            &level_query,
            Some(layer_proof.lower_layers.len() as u16),
            internal_query.left_to_right,
        )
        .unwrap()
        .map_err(|e| {
            eprintln!("{e}");
            Error::InvalidProof(format!("invalid proof verification parameters: {}", e))
        })?;

        let mut verified_keys = BTreeSet::new();

        for proved_key_value in merk_result.result_set {
            let mut path = current_path.to_vec();
            let key = proved_key_value.key;
            let value = proved_key_value.value;
            path.push(&key);

            verified_keys.insert(key.clone());

            if let Some(lower_layer) = layer_proof.lower_layers.get(&key) {
                let lower_hash = Self::verify_layer_proof(
                    lower_layer,
                    prove_options,
                    query,
                    &path,
                    result,
                    is_subset,
                )?;
                if lower_hash != value_hash(&value).value {
                    return Err(Error::InvalidProof("Mismatch in lower layer hash".into()));
                }
            } else {
                let element = Element::deserialize(&value)?;
                result.push((
                    path.iter().map(|p| p.to_vec()).collect(),
                    key,
                    Some(element),
                ));
            }
        }

        // if !is_subset {
        //     // Verify completeness only if not doing subset verification
        //     self.verify_completeness(&query_items, &merk_result.result_set,
        // current_path)?; }

        Ok(root_hash)
    }

    fn verify_layer_proof_raw(
        layer_proof: &LayerProof,
        prove_options: &ProveOptions,
        query: &PathQuery,
        limit_left: &mut Option<u16>,
        current_path: &[&[u8]],
        result: &mut ProvedPathKeyValues,
        is_subset: bool,
    ) -> Result<[u8; 32], Error> {
        let in_path_proving = current_path.len() < query.path.len();
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
            default_subquery_branch: internal_query.default_subquery_branch.into_owned(),
            conditional_subquery_branches: internal_query
                .conditional_subquery_branches
                .map(|a| a.into_owned()),
            left_to_right: internal_query.left_to_right,
        };

        let (root_hash, merk_result) = execute_proof(
            &layer_proof.merk_proof,
            &level_query,
            *limit_left,
            internal_query.left_to_right,
        )
        .unwrap()
        .map_err(|e| {
            eprintln!("{e}");
            Error::InvalidProof(format!("invalid proof verification parameters: {}", e))
        })?;

        println!("merk result is {}", merk_result);

        let mut verified_keys = BTreeSet::new();

        if merk_result.result_set.is_empty() {
            limit_left.as_mut().map(|limit| *limit -= 1);
        } else {
            for proved_key_value in merk_result.result_set {
                let mut path = current_path.to_vec();
                let key = &proved_key_value.key;
                let value = &proved_key_value.value;
                let element = Element::deserialize(value)?;
                let hash = &proved_key_value.proof;
                path.push(key);

                verified_keys.insert(key.clone());

                if let Some(lower_layer) = layer_proof.lower_layers.get(key) {
                    match element {
                        Element::Tree(Some(v), _) | Element::SumTree(Some(v), ..) => {
                            let lower_hash = Self::verify_layer_proof_raw(
                                lower_layer,
                                prove_options,
                                query,
                                limit_left,
                                &path,
                                result,
                                is_subset,
                            )?;
                            let combined_root_hash =
                                combine_hash(value_hash(value).value(), &lower_hash)
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
                } else if !in_path_proving {
                    let path_key_value = ProvedPathKeyValue::from_proved_key_value(
                        path.iter().map(|p| p.to_vec()).collect(),
                        proved_key_value,
                    );
                    println!(
                        "pushing {} limit left after is {:?}",
                        &path_key_value, limit_left
                    );
                    result.push(path_key_value);

                    limit_left.as_mut().map(|limit| *limit -= 1);
                    if limit_left == &Some(0) {
                        break;
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

    pub fn verify_subset_query(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let (root_hash, result) = Self::verify_proof_internal(&grovedb_proof, query, true)?;

        Ok((root_hash, result))
    }

    pub fn verify_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        // This is now handled within verify_proof_internal
        Self::verify_query(proof, query)
    }

    pub fn verify_subset_query_with_absence_proof(
        proof: &[u8],
        query: &PathQuery,
    ) -> Result<([u8; 32], Vec<PathKeyOptionalElementTrio>), Error> {
        // Subset queries don't verify absence, so this is the same as
        // verify_subset_query
        Self::verify_subset_query(proof, query)
    }
}
