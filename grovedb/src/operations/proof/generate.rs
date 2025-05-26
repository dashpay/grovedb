//! Generate proof operations

use std::{borrow::Cow, collections::BTreeMap, ops::Deref};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_merk::{
    proofs::{encode_into, query::QueryItem, Node, Op},
    tree::value_hash,
    Merk, ProofWithoutEncodingResult,
};
use grovedb_storage::StorageContext;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

#[cfg(feature = "proof_debug")]
use crate::query_result_type::QueryResultType;
use crate::{
    operations::proof::{
        util::hex_to_ascii, GroveDBProof, GroveDBProofV0, LayerProof, ProveOptions,
    },
    reference_path::path_from_reference_path_type,
    Element, Error, GroveDb, PathQuery, Transaction, TransactionArg,
};

pub enum OwnedOrBorrowedTransaction<'db> {
    BorrowedTransaction(&'db Transaction<'db>),
    OwnedTransaction(Transaction<'db>),
}

impl<'db> Deref for OwnedOrBorrowedTransaction<'db> {
    type Target = Transaction<'db>;

    /// Returns a reference to the underlying `Transaction`, regardless of whether it is owned or borrowed.
    ///
    /// # Examples
    ///
    /// ```
    /// let tx = Transaction::default();
    /// let owned = OwnedOrBorrowedTransaction::OwnedTransaction(tx);
    /// let reference: &Transaction = owned.deref();
    /// ```
    fn deref(&self) -> &Self::Target {
        match self {
            OwnedOrBorrowedTransaction::BorrowedTransaction(borrowed) => borrowed,
            OwnedOrBorrowedTransaction::OwnedTransaction(owned) => owned,
        }
    }
}

impl GroveDb {
    /// Prove one or more path queries.
    /// If we have more than one path query, we merge into a single path query
    /// Generates a cryptographic proof for multiple path queries, merging them if necessary.
    ///
    /// If more than one query is provided, merges them into a single query before generating the proof. Returns the serialized proof as a byte vector, or an error with operation cost if proof generation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// let queries = vec![&path_query1, &path_query2];
    /// let proof = grovedb.prove_query_many(queries, None, tx_arg, &grove_version)?;
    /// assert!(!proof.is_empty());
    /// ```
    pub fn prove_query_many(
        &self,
        query: Vec<&PathQuery>,
        prove_options: Option<ProveOptions>,
        tx: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_with_cost!(
            "prove_query_many",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .prove_query_many
        );
        if query.len() > 1 {
            let query = cost_return_on_error_default!(PathQuery::merge(query, grove_version));
            self.prove_query(&query, prove_options, tx, grove_version)
        } else {
            self.prove_query(query[0], prove_options, tx, grove_version)
        }
    }

    /// Generate a minimalistic proof for a given path query
    /// doesn't allow for subset verification
    /// Proofs generated with this can only be verified by the path query used
    /// Generates a serialized cryptographic proof for a single database query.
    ///
    /// Produces a minimalistic proof for the provided `PathQuery`, serializes it using big-endian encoding, and returns the resulting byte vector. The proof is generated within the context of the specified transaction and GroveDB version, and can be customized with optional proof options.
    ///
    /// # Returns
    /// A byte vector containing the serialized proof, or an error wrapped with operation cost if proof generation or serialization fails.
    ///
    /// # Examples
    ///
    /// ```
    /// let proof_bytes = grovedb.prove_query(
    ///     &path_query,
    ///     Some(prove_options),
    ///     transaction_arg,
    ///     &grove_version,
    /// )?;
    /// assert!(!proof_bytes.is_empty());
    /// ```
    pub fn prove_query(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        tx: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_with_cost!(
            "prove_query",
            grove_version.grovedb_versions.operations.proof.prove_query
        );
        let mut cost = OperationCost::default();
        let proof = cost_return_on_error!(
            &mut cost,
            self.prove_query_non_serialized(path_query, prove_options, tx, grove_version)
        );
        #[cfg(feature = "proof_debug")]
        {
            println!("constructed proof is {}", proof);
        }
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let encoded_proof = cost_return_on_error_no_add!(
            cost,
            bincode::encode_to_vec(proof, config)
                .map_err(|e| Error::CorruptedData(format!("unable to encode proof {}", e)))
        );
        Ok(encoded_proof).wrap_with_cost(cost)
    }

    /// Generates a cryptographic proof for a single path query without serializing the result.
    ///
    /// Validates the query parameters, then constructs a minimalistic proof structure representing the database state for the specified query. The proof includes all necessary information to verify the query result externally. Returns the proof as a `GroveDBProof` variant, or an error if the query parameters are invalid.
    ///
    /// # Returns
    /// A `GroveDBProof` containing the proof structure for the query, or an error if the query is invalid (e.g., non-zero offset or zero limit).
    ///
    /// # Examples
    ///
    /// ```
    /// let proof = grovedb.prove_query_non_serialized(
    ///     &path_query,
    ///     None,
    ///     tx_arg,
    ///     &grove_version,
    /// );
    /// assert!(proof.is_ok());
    /// ```
    pub fn prove_query_non_serialized(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        tx: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<GroveDBProof, Error> {
        let mut cost = OperationCost::default();

        let prove_options = prove_options.unwrap_or_default();

        if path_query.query.offset.is_some() && path_query.query.offset != Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not have offsets",
            ))
            .wrap_with_cost(cost);
        }

        if path_query.query.limit == Some(0) {
            return Err(Error::InvalidQuery(
                "proved path queries can not be for limit 0",
            ))
            .wrap_with_cost(cost);
        }

        #[cfg(feature = "proof_debug")]
        {
            // we want to query raw because we want the references to not be resolved at
            // this point

            let values = cost_return_on_error!(
                &mut cost,
                self.query_raw(
                    path_query,
                    false,
                    prove_options.decrease_limit_on_empty_sub_query_result,
                    false,
                    QueryResultType::QueryPathKeyElementTrioResultType,
                    tx,
                    grove_version,
                )
            )
            .0;

            println!("values are {}", values);

            let precomputed_result_map = cost_return_on_error!(
                &mut cost,
                self.query_raw(
                    path_query,
                    false,
                    prove_options.decrease_limit_on_empty_sub_query_result,
                    false,
                    QueryResultType::QueryPathKeyElementTrioResultType,
                    tx,
                    grove_version,
                )
            )
            .0
            .to_btree_map_level_results();

            println!("precomputed results are {}", precomputed_result_map);
        }

        let mut limit = path_query.query.limit;

        let root_layer = cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(
                vec![],
                path_query,
                &mut limit,
                &prove_options,
                tx,
                grove_version
            )
        );

        Ok(GroveDBProof::V0(GroveDBProofV0 {
            root_layer,
            prove_options,
        }))
        .wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// Recursively generates a layered cryptographic proof for a path query and its subqueries.
    ///
    /// Traverses the GroveDB tree according to the provided `PathQuery`, generating a Merk proof for each relevant subtree and recursively including proofs for subqueries. Handles reference resolution, limit enforcement, and proof composition across multiple tree levels. Returns a `LayerProof` containing the serialized Merk proof for the current layer and any lower-layer proofs.
    ///
    /// # Parameters
    /// - `path`: The current path within the GroveDB tree being traversed.
    /// - `path_query`: The query specifying which elements and subqueries to prove.
    /// - `overall_limit`: Mutable reference to the remaining result limit, decremented as results are included in the proof.
    /// - `prove_options`: Options controlling proof generation behavior, such as limit handling.
    /// - `tx`: The transaction context for database operations.
    /// - `grove_version`: The GroveDB version to use for compatibility.
    ///
    /// # Returns
    /// A `LayerProof` containing the serialized Merk proof for the current layer and any lower-layer proofs, wrapped with operation cost and error handling.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut limit = Some(10);
    /// let proof = grovedb.prove_subqueries(
    ///     vec![b"root"],
    ///     &path_query,
    ///     &mut limit,
    ///     &prove_options,
    ///     tx,
    ///     &grove_version,
    /// );
    /// assert!(proof.is_ok());
    /// ```
    fn prove_subqueries(
        &self,
        path: Vec<&[u8]>,
        path_query: &PathQuery,
        overall_limit: &mut Option<u16>,
        prove_options: &ProveOptions,
        tx: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<LayerProof, Error> {
        let mut cost = OperationCost::default();

        let cow_tx = match tx {
            Some(tx) => OwnedOrBorrowedTransaction::BorrowedTransaction(tx),
            None => OwnedOrBorrowedTransaction::OwnedTransaction(self.start_transaction()),
        };

        let query = cost_return_on_error_no_add!(
            cost,
            path_query
                .query_items_at_path(path.as_slice(), grove_version)
                .and_then(|query_items| {
                    query_items.ok_or(Error::CorruptedPath(format!(
                        "prove subqueries: path {} should be part of path_query {}",
                        path.iter()
                            .map(|a| hex_to_ascii(a))
                            .collect::<Vec<_>>()
                            .join("/"),
                        path_query
                    )))
                })
        );

        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.as_slice().into(),
                &cow_tx,
                None,
                grove_version
            )
        );

        let limit = if path.len() < path_query.path.len() {
            // There is no need for a limit because we are only asking for a single item
            None
        } else {
            *overall_limit
        };

        let mut merk_proof = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(
                &subtree,
                &query.items,
                query.left_to_right,
                limit,
                grove_version
            )
        );

        #[cfg(feature = "proof_debug")]
        {
            println!(
                "generated merk proof at level path level [{}], limit is {:?}, {}",
                path.iter()
                    .map(|a| hex_to_ascii(a))
                    .collect::<Vec<_>>()
                    .join("/"),
                overall_limit,
                if query.left_to_right {
                    "left to right"
                } else {
                    "right to left"
                }
            );
        }

        let mut lower_layers = BTreeMap::new();

        let mut has_a_result_at_level = false;
        let mut done_with_results = false;

        for op in merk_proof.proof.iter_mut() {
            done_with_results |= overall_limit == &Some(0);
            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(key, value) | Node::KVValueHash(key, value, ..)
                        if !done_with_results =>
                    {
                        let elem = Element::deserialize(value, grove_version);
                        match elem {
                            Ok(Element::Reference(reference_path, ..)) => {
                                let absolute_path = cost_return_on_error!(
                                    &mut cost,
                                    path_from_reference_path_type(
                                        reference_path,
                                        &path.to_vec(),
                                        Some(key.as_slice())
                                    )
                                    .wrap_with_cost(OperationCost::default())
                                );

                                let referenced_elem = cost_return_on_error!(
                                    &mut cost,
                                    self.follow_reference(
                                        absolute_path.as_slice().into(),
                                        true,
                                        tx,
                                        grove_version
                                    )
                                );

                                let serialized_referenced_elem =
                                    referenced_elem.serialize(grove_version);
                                if serialized_referenced_elem.is_err() {
                                    return Err(Error::CorruptedData(String::from(
                                        "unable to serialize element",
                                    )))
                                    .wrap_with_cost(cost);
                                }

                                *node = Node::KVRefValueHash(
                                    key.to_owned(),
                                    serialized_referenced_elem.expect("confirmed ok above"),
                                    value_hash(value).unwrap_add_cost(&mut cost),
                                );
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            Ok(Element::Item(..)) if !done_with_results => {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!("found {}", hex_to_ascii(key));
                                }
                                *node = Node::KV(key.to_owned(), value.to_owned());
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            Ok(Element::Tree(Some(_), _))
                            | Ok(Element::SumTree(Some(_), ..))
                            | Ok(Element::BigSumTree(Some(_), ..))
                            | Ok(Element::CountTree(Some(_), ..))
                                if !done_with_results
                                    && query.has_subquery_or_matching_in_path_on_key(key) =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!(
                                        "found tree {}, query is {}",
                                        hex_to_ascii(key),
                                        query
                                    );
                                }
                                // We only want to check in sub nodes for the proof if the tree has
                                // elements
                                let mut lower_path = path.clone();
                                lower_path.push(key.as_slice());

                                let previous_limit = *overall_limit;

                                let layer_proof = cost_return_on_error!(
                                    &mut cost,
                                    self.prove_subqueries(
                                        lower_path,
                                        path_query,
                                        overall_limit,
                                        prove_options,
                                        tx,
                                        grove_version,
                                    )
                                );

                                if previous_limit != *overall_limit {
                                    // a lower layer updated the limit, don't subtract 1 at this
                                    // level
                                    has_a_result_at_level |= true;
                                }
                                lower_layers.insert(key.clone(), layer_proof);
                            }

                            Ok(Element::Tree(..)) | Ok(Element::SumTree(..))
                                if !done_with_results =>
                            {
                                #[cfg(feature = "proof_debug")]
                                {
                                    println!(
                                        "found tree {}, no subquery query is {:?}",
                                        hex_to_ascii(key),
                                        query
                                    );
                                }
                                if let Some(limit) = overall_limit.as_mut() {
                                    *limit -= 1;
                                }
                                has_a_result_at_level |= true;
                            }
                            // todo: transform the unused trees into a Hash or KVHash to make proof
                            // smaller Ok(Element::Tree(..)) if
                            // done_with_results => {     *node =
                            // Node::Hash()     // we are done with the
                            // results, we can modify the proof to alter
                            // }
                            _ => continue,
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }

        if !has_a_result_at_level
            && !done_with_results
            && prove_options.decrease_limit_on_empty_sub_query_result
        {
            #[cfg(feature = "proof_debug")]
            {
                println!(
                    "no results at level {}",
                    path.iter()
                        .map(|a| hex_to_ascii(a))
                        .collect::<Vec<_>>()
                        .join("/")
                );
            }
            if let Some(limit) = overall_limit.as_mut() {
                *limit -= 1;
            }
        }

        let mut serialized_merk_proof = Vec::with_capacity(1024);
        encode_into(merk_proof.proof.iter(), &mut serialized_merk_proof);

        Ok(LayerProof {
            merk_proof: serialized_merk_proof,
            lower_layers,
        })
        .wrap_with_cost(cost)
    }

    /// Generates query proof given a subtree and appends the result to a proof
    /// list
    fn generate_merk_proof<'a, S>(
        &self,
        subtree: &'a Merk<S>,
        query_items: &[QueryItem],
        left_to_right: bool,
        limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofWithoutEncodingResult, Error>
    where
        S: StorageContext<'a> + 'a,
    {
        subtree
            .prove_unchecked_query_items(query_items, limit, left_to_right, grove_version)
            .map_ok(|(proof, limit)| ProofWithoutEncodingResult::new(proof, limit))
            .map_err(|e| {
                Error::InternalError(format!(
                    "failed to generate proof for query_items [{}] error is : {}",
                    query_items
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    e
                ))
            })
    }
}
