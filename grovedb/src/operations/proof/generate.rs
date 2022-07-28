use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::{
    proofs::{encode_into, Node, Op},
    KVIterator, Merk, ProofWithoutEncodingResult,
};
use storage::{rocksdb_storage::PrefixedRocksDbStorageContext, Storage, StorageContext};

use crate::{
    operations::proof::util::{write_to_vec, ProofType, EMPTY_TREE_HASH},
    subtree::raw_decode,
    Element, Error, GroveDb, PathQuery, Query,
};

impl GroveDb {
    pub fn prove_query_many(&self, query: Vec<&PathQuery>) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();
        if query.len() > 1 {
            let query = cost_return_on_error!(&mut cost, PathQuery::merge(query));
            self.prove_query(&query)
        } else {
            self.prove_query(query[0])
        }
    }

    pub fn prove_query(&self, query: &PathQuery) -> CostResult<Vec<u8>, Error> {
        let mut cost = OperationCost::default();

        // TODO: should it be possible to generate proofs for tree items (currently yes)
        let mut proof_result: Vec<u8> = vec![];
        let mut limit: Option<u16> = query.query.limit;
        let mut offset: Option<u16> = query.query.offset;

        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();
        // TODO: get rid of this error once root tree is also of type merk
        if path_slices.is_empty() {
            return Err(Error::InvalidPath("can't generate proof for empty path"))
                .wrap_with_cost(cost);
        }

        let subtree_exists = self
            .check_subtree_exists_path_not_found(path_slices.clone(), None)
            .unwrap_add_cost(&mut cost);

        match subtree_exists {
            Ok(_) => {}
            Err(_) => {
                write_to_vec(&mut proof_result, &[ProofType::AbsentPath.into()]);
                let mut current_path: Vec<&[u8]> = vec![];

                let mut split_path = path_slices.split_first();
                while let Some((key, path_slice)) = split_path {
                    let subtree = self
                        .open_subtree(current_path.iter().copied())
                        .unwrap_add_cost(&mut cost);

                    if subtree.is_err() {
                        break;
                    }

                    let has_item =
                        Element::get(subtree.as_ref().expect("confirmed not error above"), key)
                            .unwrap_add_cost(&mut cost);

                    let mut next_key_query = Query::new();
                    next_key_query.insert_key(key.to_vec());
                    cost_return_on_error!(
                        &mut cost,
                        self.generate_and_store_merk_proof(
                            &subtree.expect("confirmed not error above"),
                            &next_key_query,
                            None,
                            None,
                            ProofType::Merk,
                            &mut proof_result,
                        )
                    );

                    current_path.push(key);

                    if has_item.is_err() || path_slice.is_empty() {
                        // reached last key
                        break;
                    }

                    split_path = path_slice.split_first();
                }

                return Ok(proof_result).wrap_with_cost(cost);
            }
        }

        cost_return_on_error!(
            &mut cost,
            self.prove_subqueries(
                &mut proof_result,
                path_slices.clone(),
                query,
                &mut limit,
                &mut offset,
            )
        );
        cost_return_on_error!(&mut cost, self.prove_path(&mut proof_result, path_slices));

        Ok(proof_result).wrap_with_cost(cost)
    }

    /// Perform a pre-order traversal of the tree based on the provided
    /// subqueries
    fn prove_subqueries(
        &self,
        proofs: &mut Vec<u8>,
        path: Vec<&[u8]>,
        query: &PathQuery,
        current_limit: &mut Option<u16>,
        current_offset: &mut Option<u16>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let reached_limit = query.query.limit.is_some() && query.query.limit.unwrap() == 0;
        if reached_limit {
            return Ok(()).wrap_with_cost(cost);
        }

        let subtree = cost_return_on_error!(&mut cost, self.open_subtree(path.iter().copied()));
        if subtree.root_hash().unwrap_add_cost(&mut cost) == EMPTY_TREE_HASH {
            write_to_vec(proofs, &[ProofType::EmptyTree.into()]);
            return Ok(()).wrap_with_cost(cost);
        }

        let mut is_leaf_tree = true;

        let mut kv_iterator = KVIterator::new(subtree.storage.raw_iter(), &query.query.query)
            .unwrap_add_cost(&mut cost);
        while let Some((key, value_bytes)) = kv_iterator.next().unwrap_add_cost(&mut cost) {
            let (subquery_key, subquery_value) =
                Element::subquery_paths_for_sized_query(&query.query, &key);

            if subquery_value.is_none() && subquery_key.is_none() {
                continue;
            }

            let element = cost_return_on_error_no_add!(&cost, raw_decode(&value_bytes));
            match element {
                Element::Tree(tree_hash, _) => {
                    if tree_hash == EMPTY_TREE_HASH {
                        continue;
                    }

                    // if the element is a non empty tree then current tree is not a leaf tree
                    if is_leaf_tree {
                        is_leaf_tree = false;
                        cost_return_on_error!(
                            &mut cost,
                            self.generate_and_store_merk_proof(
                                &subtree,
                                &query.query.query,
                                None,
                                None,
                                ProofType::Merk,
                                proofs,
                            )
                        );
                    }

                    let mut new_path = path.clone();
                    new_path.push(key.as_ref());

                    let mut query = subquery_value;

                    if query.is_some() {
                        if subquery_key.is_some() {
                            // prove the subquery key first
                            let inner_subtree = cost_return_on_error!(
                                &mut cost,
                                self.open_subtree(new_path.iter().copied())
                            );

                            let mut key_as_query = Query::new();
                            key_as_query.insert_key(subquery_key.clone().unwrap());

                            cost_return_on_error!(
                                &mut cost,
                                self.generate_and_store_merk_proof(
                                    &inner_subtree,
                                    &key_as_query,
                                    None,
                                    None,
                                    ProofType::Merk,
                                    proofs,
                                )
                            );

                            new_path.push(subquery_key.as_ref().unwrap());
                        }
                    } else {
                        let mut key_as_query = Query::new();
                        key_as_query.insert_key(subquery_key.unwrap());
                        query = Some(key_as_query);
                    }

                    let new_path_owned = new_path.iter().map(|x| x.to_vec()).collect();
                    let new_path_query = PathQuery::new_unsized(new_path_owned, query.unwrap());

                    if self
                        .check_subtree_exists_path_not_found(new_path.clone(), None)
                        .unwrap_add_cost(&mut cost)
                        .is_err()
                    {
                        continue;
                    }

                    cost_return_on_error!(
                        &mut cost,
                        self.prove_subqueries(
                            proofs,
                            new_path,
                            &new_path_query,
                            current_limit,
                            current_offset,
                        )
                    );

                    if *current_limit == Some(0) {
                        break;
                    }
                }
                _ => {
                    // currently not handling trees with mixed types
                    // if a tree has been seen, we should see nothing but tree
                    if !is_leaf_tree {
                        return Err(Error::InvalidQuery("mixed tree types")).wrap_with_cost(cost);
                    }
                }
            }
        }

        if is_leaf_tree {
            // if no useful subtree, then we care about the result set of this subtree.
            // apply the sized query
            let limit_offset = cost_return_on_error!(
                &mut cost,
                self.generate_and_store_merk_proof(
                    &subtree,
                    &query.query.query,
                    *current_limit,
                    *current_offset,
                    ProofType::SizedMerk,
                    proofs,
                )
            );

            // update limit and offset values
            *current_limit = limit_offset.0;
            *current_offset = limit_offset.1;
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Given a path, construct and append a set of proofs that shows there is
    /// a valid path from the root of the db to that point.
    fn prove_path(
        &self,
        proof_result: &mut Vec<u8>,
        path_slices: Vec<&[u8]>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        // generate proof to show that the path leads up to the root
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            let subtree =
                cost_return_on_error!(&mut cost, self.open_subtree(path_slice.iter().copied()));
            let mut query = Query::new();
            query.insert_key(key.to_vec());

            cost_return_on_error!(
                &mut cost,
                self.generate_and_store_merk_proof(
                    &subtree,
                    &query,
                    None,
                    None,
                    ProofType::Merk,
                    proof_result,
                )
            );
            split_path = path_slice.split_last();
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Generates query proof given a subtree and appends the result to a proof
    /// list
    fn generate_and_store_merk_proof<'a, S: 'a>(
        &self,
        subtree: &'a Merk<S>,
        query: &Query,
        limit: Option<u16>,
        offset: Option<u16>,
        proof_type: ProofType,
        proofs: &mut Vec<u8>,
    ) -> CostResult<(Option<u16>, Option<u16>), Error>
    where
        S: StorageContext<'a>,
    {
        let mut cost = OperationCost::default();

        // TODO: How do you handle mixed tree types?
        // TODO implement costs
        let mut proof_result = subtree
            .prove_without_encoding(query.clone(), limit, offset)
            .unwrap()
            .expect("should generate proof");

        cost_return_on_error!(&mut cost, self.replace_references(&mut proof_result));

        let mut proof_bytes = Vec::with_capacity(128);
        encode_into(proof_result.proof.iter(), &mut proof_bytes);

        let proof_len_bytes: [u8; 8] = proof_bytes.len().to_be_bytes();
        write_to_vec(proofs, &[proof_type.into()]);
        write_to_vec(proofs, &proof_len_bytes);
        write_to_vec(proofs, &proof_bytes);

        Ok((proof_result.limit, proof_result.offset)).wrap_with_cost(cost)
    }

    /// Replaces references with the base item they point to
    fn replace_references(
        &self,
        proof_result: &mut ProofWithoutEncodingResult,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        for op in proof_result.proof.iter_mut() {
            match op {
                Op::Push(node) | Op::PushInverted(node) => match node {
                    Node::KV(_, value) => {
                        let elem = Element::deserialize(value);
                        if let Ok(Element::Reference(reference_path, _)) = elem {
                            let referenced_elem = cost_return_on_error!(
                                &mut cost,
                                self.follow_reference(reference_path, None)
                            );
                            *value = referenced_elem.serialize().unwrap();
                        }
                    }
                    _ => continue,
                },
                _ => continue,
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Opens merk at a given path without transaction
    fn open_subtree<'p, P>(&self, path: P) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.db.get_storage_context(path).flat_map(|storage| {
            Merk::open(storage)
                .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
        })
    }
}
