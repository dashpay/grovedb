//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

use core::fmt;

use bincode::Options;
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostResult, CostsExt,
    OperationCost,
};
use integer_encoding::VarInt;
use merk::{
    proofs::{query::QueryItem, Query},
    tree::Tree,
    BatchEntry, Op, TreeFeatureType,
    TreeFeatureType::BasicMerk,
    HASH_LENGTH,
};
use serde::{Deserialize, Serialize};
use storage::{rocksdb_storage::RocksDbStorage, RawIterator, StorageContext};
use visualize::visualize_to_vec;

use crate::{
    query_result_type::{
        KeyElementPair, QueryResultElement, QueryResultElements, QueryResultType,
        QueryResultType::QueryElementResultType,
    },
    util::{merk_optional_tx, storage_context_optional_tx},
    Error, Merk, PathQuery, SizedQuery, TransactionArg,
};

/// Optional single byte meta-data to be stored per element
pub type ElementFlags = Option<Vec<u8>>;
/// int 64 sum value
pub type SumValue = i64;

/// Variants of GroveDB stored entities
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>, ElementFlags),
    /// A reference to an object by its path
    Reference(Vec<Vec<u8>>, ElementFlags),
    /// A subtree, contains a root hash of the underlying Merk.
    /// Hash is stored to make Merk become different when its subtrees have
    /// changed, otherwise changes won't be reflected in parent trees.
    Tree([u8; 32], ElementFlags),
    /// Vector encoded integer value that can be totaled in a sum tree
    // TODO: Look into enforcing the integer nature during insertion
    SumItem(Vec<u8>, ElementFlags),
    /// Same as Element::Tree but underlying Merk sums value of it's summable
    /// nodes
    SumTree([u8; 32], SumValue, ElementFlags),
}

impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self);

        f.write_str(&String::from_utf8_lossy(&v))
    }
}

pub struct PathQueryPushArgs<'db, 'ctx, 'a>
where
    'db: 'ctx,
{
    pub storage: &'db RocksDbStorage,
    pub transaction: TransactionArg<'db, 'ctx>,
    pub key: Option<&'a [u8]>,
    pub element: Element,
    pub path: &'a [&'a [u8]],
    pub subquery_key: Option<Vec<u8>>,
    pub subquery: Option<Query>,
    pub left_to_right: bool,
    pub allow_get_raw: bool,
    pub result_type: QueryResultType,
    pub results: &'a mut Vec<QueryResultElement>,
    pub limit: &'a mut Option<u16>,
    pub offset: &'a mut Option<u16>,
}

impl Element {
    // TODO: improve API to avoid creation of Tree elements with uncertain state
    pub fn empty_tree() -> Self {
        Element::new_tree(Default::default())
    }

    pub fn empty_tree_with_flags(flags: ElementFlags) -> Self {
        Element::new_tree_with_flags(Default::default(), flags)
    }

    pub fn empty_sum_tree() -> Self {
        Element::new_sum_tree(Default::default())
    }

    pub fn empty_sum_tree_with_flags(flags: ElementFlags) -> Self {
        Element::new_sum_tree_with_flags(Default::default(), flags)
    }

    pub fn new_item(item_value: Vec<u8>) -> Self {
        Element::Item(item_value, None)
    }

    pub fn new_item_with_flags(item_value: Vec<u8>, flags: ElementFlags) -> Self {
        Element::Item(item_value, flags)
    }

    pub fn new_sum_item(value: i64) -> Self {
        Element::SumItem(value.encode_var_vec(), None)
    }

    pub fn new_sum_item_with_flags(value: i64, flags: ElementFlags) -> Self {
        Element::SumItem(value.encode_var_vec(), flags)
    }

    pub fn new_reference(reference_path: Vec<Vec<u8>>) -> Self {
        Element::Reference(reference_path, None)
    }

    pub fn new_reference_with_flags(reference_path: Vec<Vec<u8>>, flags: ElementFlags) -> Self {
        Element::Reference(reference_path, flags)
    }

    pub fn new_tree(tree_hash: [u8; 32]) -> Self {
        Element::Tree(tree_hash, None)
    }

    pub fn new_tree_with_flags(tree_hash: [u8; 32], flags: ElementFlags) -> Self {
        Element::Tree(tree_hash, flags)
    }

    pub fn new_sum_tree(tree_hash: [u8; 32]) -> Self {
        Element::SumTree(tree_hash, 0, None)
    }

    pub fn new_sum_tree_with_flags(tree_hash: [u8; 32], flags: ElementFlags) -> Self {
        Element::SumTree(tree_hash, 0, flags)
    }

    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else
    pub fn sum_value(&self) -> Option<i64> {
        match self {
            Element::SumItem(value, _) => {
                i64::decode_var(value).map(|(encoded_value, _)| encoded_value)
            }
            // TODO: should this be None instead??
            _ => Some(0),
        }
    }

    /// Grab the optional flag stored in an element
    pub fn get_flags(&self) -> &ElementFlags {
        match self {
            Element::Tree(_, flags)
            | Element::SumTree(_, _, flags)
            | Element::Item(_, flags)
            | Element::SumItem(_, flags)
            | Element::Reference(_, flags) => flags,
        }
    }

    /// Get the size of an element in bytes
    pub fn byte_size(&self) -> usize {
        match self {
            Element::Item(item, element_flag) | Element::SumItem(item, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() + item.len()
                } else {
                    item.len()
                }
            }
            Element::Reference(path_reference, element_flag) => {
                let path_length = path_reference
                    .iter()
                    .map(|inner| inner.len())
                    .sum::<usize>()
                    + 1;

                if let Some(flag) = element_flag {
                    flag.len() + path_length
                } else {
                    path_length
                }
            }
            Element::Tree(_, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() + 32
                } else {
                    32
                }
            }
            Element::SumTree(_, _, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() + 32 + 8
                } else {
                    32 + 8
                }
            }
        }
    }

    pub fn required_item_space(len: usize, flag_len: usize) -> usize {
        len + len.required_space() + flag_len + flag_len.required_space() + 1
    }

    /// Get the size of the serialization of an element in bytes
    pub fn serialized_byte_size(&self) -> usize {
        match self {
            Element::Item(item, element_flag) | Element::SumItem(item, element_flag) => {
                let item_len = item.len();
                let flag_len = if let Some(flag) = element_flag {
                    flag.len() + 1
                } else {
                    0
                };
                Self::required_item_space(item_len, flag_len)
            }
            Element::Reference(path_reference, element_flag) => {
                let flag_len = if let Some(flag) = element_flag {
                    flag.len() + 1
                } else {
                    0
                };

                path_reference
                    .iter()
                    .map(|inner| {
                        let inner_len = inner.len();
                        inner_len + inner_len.required_space()
                    })
                    .sum::<usize>()
                    + path_reference.len().required_space()
                    + flag_len
                    + flag_len.required_space()
                    + 1 // + 1 for enum
            }
            Element::Tree(_, element_flag) => {
                let flag_len = if let Some(flag) = element_flag {
                    flag.len() + 1
                } else {
                    0
                };
                32 + flag_len + flag_len.required_space() + 1 // + 1 for enum
            }
            Element::SumTree(_, sum_value, element_flag) => {
                let flag_len = if let Some(flag) = element_flag {
                    flag.len() + 1
                } else {
                    0
                };
                32 + sum_value.required_space() + flag_len + flag_len.required_space() + 1
                // + 1 for enum
            }
        }
    }

    /// Get the size that the element will occupy on disk
    pub fn node_byte_size(&self, key_len: usize) -> usize {
        // todo v23: this is just an approximation for now
        let serialized_value_size = self.serialized_byte_size();
        Self::calculate_node_byte_size(serialized_value_size, key_len)
    }

    /// Get the size that the element will occupy on disk
    pub fn calculate_node_byte_size(serialized_value_size: usize, key_len: usize) -> usize {
        let node_value_size = serialized_value_size + serialized_value_size.required_space();
        let node_key_size = key_len + key_len.required_space();
        // Each node stores the key and value, the value hash and the key_value hash
        let node_size = node_value_size + node_key_size + HASH_LENGTH + HASH_LENGTH;
        // The node will be a child of another node which stores it's key and hash
        let parent_additions = node_key_size + HASH_LENGTH;
        let child_sizes = 2_usize;
        node_size + parent_additions + child_sizes
    }

    /// Delete an element from Merk under a key
    pub fn delete<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
    ) -> CostResult<(), Error> {
        // TODO: delete references on this element
        let batch = [(key, Op::Delete, None)];
        merk.apply::<_, Vec<u8>>(&batch, &[])
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Delete an element from Merk under a key to batch operations
    pub fn delete_into_batch_operations<K: AsRef<[u8]>>(
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        // TODO: Tree reference type should be optional, doesn't make sense in delete
        let entry = (key, Op::Delete, None);
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        key: K,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();

        let value_opt = cost_return_on_error!(
            &mut cost,
            merk.get(key.as_ref())
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let value = cost_return_on_error_no_add!(
            &cost,
            value_opt.ok_or_else(|| {
                Error::PathKeyNotFound(format!("key not found in Merk: {}", hex::encode(key)))
            })
        );
        let element = cost_return_on_error_no_add!(
            &cost,
            Self::deserialize(value.as_slice())
                .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
        );
        Ok(element).wrap_with_cost(cost)
    }

    pub fn get_query(
        storage: &RocksDbStorage,
        merk_path: &[&[u8]],
        query: &Query,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<QueryResultElements, Error> {
        let sized_query = SizedQuery::new(query.clone(), None, None);
        Element::get_sized_query(storage, merk_path, &sized_query, result_type, transaction)
            .map_ok(|(elements, _)| elements)
    }

    pub fn get_query_values(
        storage: &RocksDbStorage,
        merk_path: &[&[u8]],
        query: &Query,
        transaction: TransactionArg,
    ) -> CostResult<Vec<Element>, Error> {
        Element::get_query(
            storage,
            merk_path,
            query,
            QueryElementResultType,
            transaction,
        )
        .flat_map_ok(|result_items| {
            let elements: Vec<Element> = result_items
                .elements
                .into_iter()
                .filter_map(|result_item| match result_item {
                    QueryResultElement::ElementResultItem(element) => Some(element),
                    QueryResultElement::KeyElementPairResultItem(_) => None,
                    QueryResultElement::PathKeyElementTrioResultItem(_) => None,
                })
                .collect();
            Ok(elements).wrap_with_cost(OperationCost::default())
        })
    }

    fn basic_push(args: PathQueryPushArgs) -> Result<(), Error> {
        let PathQueryPushArgs {
            path,
            key,
            element,
            result_type,
            results,
            limit,
            offset,
            ..
        } = args;
        if offset.unwrap_or(0) == 0 {
            match result_type {
                QueryResultType::QueryElementResultType => {
                    results.push(QueryResultElement::ElementResultItem(element));
                }
                QueryResultType::QueryKeyElementPairResultType => {
                    let key = key.ok_or(Error::CorruptedPath("basic push must have a key"))?;
                    results.push(QueryResultElement::KeyElementPairResultItem((
                        Vec::from(key),
                        element,
                    )));
                }
                QueryResultType::QueryPathKeyElementTrioResultType => {
                    let key = key.ok_or(Error::CorruptedPath("basic push must have a key"))?;
                    let path = path.iter().map(|a| a.to_vec()).collect();
                    results.push(QueryResultElement::PathKeyElementTrioResultItem((
                        path,
                        Vec::from(key),
                        element,
                    )));
                }
            }
            if let Some(limit) = limit {
                *limit -= 1;
            }
        } else if let Some(offset) = offset {
            *offset -= 1;
        }
        Ok(())
    }

    fn path_query_push(args: PathQueryPushArgs) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let PathQueryPushArgs {
            storage,
            transaction,
            key,
            element,
            path,
            subquery_key,
            subquery,
            left_to_right,
            allow_get_raw,
            result_type,
            results,
            limit,
            offset,
        } = args;
        match element {
            Element::Tree(..) => {
                let mut path_vec = path.to_vec();
                let key = cost_return_on_error_no_add!(
                    &cost,
                    key.ok_or(Error::MissingParameter(
                        "the key must be provided when using a subquery key",
                    ))
                );
                path_vec.push(key);

                if let Some(subquery) = subquery {
                    if let Some(subquery_key) = &subquery_key {
                        path_vec.push(subquery_key.as_slice());
                    }

                    let inner_query = SizedQuery::new(subquery, *limit, *offset);
                    let path_vec_owned = path_vec.iter().map(|x| x.to_vec()).collect();
                    let inner_path_query = PathQuery::new(path_vec_owned, inner_query);

                    let (mut sub_elements, skipped) = cost_return_on_error!(
                        &mut cost,
                        Element::get_path_query(
                            storage,
                            &inner_path_query,
                            result_type,
                            transaction
                        )
                    );

                    if let Some(limit) = limit {
                        *limit -= sub_elements.len() as u16;
                    }
                    if let Some(offset) = offset {
                        *offset -= skipped;
                    }
                    results.append(&mut sub_elements.elements);
                } else if let Some(subquery_key) = subquery_key {
                    if offset.unwrap_or(0) == 0 {
                        match result_type {
                            QueryResultType::QueryElementResultType => {
                                merk_optional_tx!(
                                    &mut cost,
                                    storage,
                                    path_vec.iter().copied(),
                                    transaction,
                                    subtree,
                                    {
                                        results.push(QueryResultElement::ElementResultItem(
                                            cost_return_on_error!(
                                                &mut cost,
                                                Element::get(&subtree, subquery_key.as_slice())
                                            ),
                                        ));
                                    }
                                );
                            }
                            QueryResultType::QueryKeyElementPairResultType => {
                                merk_optional_tx!(
                                    &mut cost,
                                    storage,
                                    path_vec.iter().copied(),
                                    transaction,
                                    subtree,
                                    {
                                        results.push(QueryResultElement::KeyElementPairResultItem(
                                            (
                                                subquery_key.clone(),
                                                cost_return_on_error!(
                                                    &mut cost,
                                                    Element::get(&subtree, subquery_key.as_slice())
                                                ),
                                            ),
                                        ));
                                    }
                                );
                            }
                            QueryResultType::QueryPathKeyElementTrioResultType => {
                                let original_path_vec = path.iter().map(|a| a.to_vec()).collect();
                                merk_optional_tx!(
                                    &mut cost,
                                    storage,
                                    path_vec.iter().copied(),
                                    transaction,
                                    subtree,
                                    {
                                        results.push(
                                            QueryResultElement::PathKeyElementTrioResultItem((
                                                original_path_vec,
                                                subquery_key.clone(),
                                                cost_return_on_error!(
                                                    &mut cost,
                                                    Element::get(&subtree, subquery_key.as_slice())
                                                ),
                                            )),
                                        );
                                    }
                                );
                            }
                        }
                        if let Some(limit) = limit {
                            *limit -= 1;
                        }
                    } else if let Some(offset) = offset {
                        *offset -= 1;
                    }
                } else {
                    if allow_get_raw {
                        cost_return_on_error_no_add!(
                            &cost,
                            Element::basic_push(PathQueryPushArgs {
                                storage,
                                transaction,
                                key: Some(key),
                                element,
                                path,
                                subquery_key,
                                subquery,
                                left_to_right,
                                allow_get_raw,
                                result_type,
                                results,
                                limit,
                                offset,
                            })
                        );
                    } else {
                        return Err(Error::InvalidPath(
                            "you must provide a subquery or a subquery_key when interacting with \
                             a Tree of trees",
                        ))
                        .wrap_with_cost(cost);
                    }
                }
            }
            _ => {
                cost_return_on_error_no_add!(
                    &cost,
                    Element::basic_push(PathQueryPushArgs {
                        storage,
                        transaction,
                        key,
                        element,
                        path,
                        subquery_key,
                        subquery,
                        left_to_right,
                        allow_get_raw,
                        result_type,
                        results,
                        limit,
                        offset,
                    })
                );
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    pub fn subquery_paths_for_sized_query(
        sized_query: &SizedQuery,
        key: &[u8],
    ) -> (Option<Vec<u8>>, Option<Query>) {
        for (query_item, subquery_branch) in &sized_query.query.conditional_subquery_branches {
            if query_item.contains(key) {
                let subquery_key = subquery_branch.subquery_key.clone();
                let subquery = subquery_branch
                    .subquery
                    .as_ref()
                    .map(|query| *query.clone());
                return (subquery_key, subquery);
            }
        }
        let subquery_key = sized_query
            .query
            .default_subquery_branch
            .subquery_key
            .clone();
        let subquery = sized_query
            .query
            .default_subquery_branch
            .subquery
            .as_ref()
            .map(|query| *query.clone());
        (subquery_key, subquery)
    }

    // TODO: refactor
    #[allow(clippy::too_many_arguments)]
    fn query_item(
        storage: &RocksDbStorage,
        item: &QueryItem,
        results: &mut Vec<QueryResultElement>,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        transaction: TransactionArg,
        limit: &mut Option<u16>,
        offset: &mut Option<u16>,
        allow_get_raw: bool,
        result_type: QueryResultType,
        add_element_function: fn(PathQueryPushArgs) -> CostResult<(), Error>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if !item.is_range() {
            // this is a query on a key
            if let QueryItem::Key(key) = item {
                let element_res = merk_optional_tx!(
                    &mut cost,
                    storage,
                    path.iter().copied(),
                    transaction,
                    subtree,
                    { Element::get(&subtree, key).unwrap_add_cost(&mut cost) }
                );
                match element_res {
                    Ok(element) => {
                        let (subquery_key, subquery) =
                            Self::subquery_paths_for_sized_query(sized_query, key);
                        add_element_function(PathQueryPushArgs {
                            storage,
                            transaction,
                            key: Some(key.as_slice()),
                            element,
                            path,
                            subquery_key,
                            subquery,
                            left_to_right: sized_query.query.left_to_right,
                            allow_get_raw,
                            result_type,
                            results,
                            limit,
                            offset,
                        })
                        .unwrap_add_cost(&mut cost)
                    }
                    Err(Error::PathKeyNotFound(_)) => Ok(()),
                    Err(e) => Err(e),
                }
            } else {
                Err(Error::InternalError(
                    "QueryItem must be a Key if not a range",
                ))
            }
        } else {
            // this is a query on a range
            storage_context_optional_tx!(storage, path.iter().copied(), transaction, ctx, {
                let ctx = ctx.unwrap_add_cost(&mut cost);
                let mut iter = ctx.raw_iter();

                item.seek_for_iter(&mut iter, sized_query.query.left_to_right)
                    .unwrap_add_cost(&mut cost);

                while item
                    .iter_is_valid_for_type(&iter, *limit, sized_query.query.left_to_right)
                    .unwrap_add_cost(&mut cost)
                {
                    let element = cost_return_on_error_no_add!(
                        &cost,
                        raw_decode(
                            iter.value()
                                .unwrap_add_cost(&mut cost)
                                .expect("if key exists then value should too")
                        )
                    );
                    let key = iter
                        .key()
                        .unwrap_add_cost(&mut cost)
                        .expect("key should exist");
                    let (subquery_key, subquery) =
                        Self::subquery_paths_for_sized_query(sized_query, key);
                    cost_return_on_error!(
                        &mut cost,
                        add_element_function(PathQueryPushArgs {
                            storage,
                            transaction,
                            key: Some(key),
                            element,
                            path,
                            subquery_key,
                            subquery,
                            left_to_right: sized_query.query.left_to_right,
                            allow_get_raw,
                            result_type,
                            results,
                            limit,
                            offset,
                        })
                    );
                    if sized_query.query.left_to_right {
                        iter.next().unwrap_add_cost(&mut cost);
                    } else {
                        iter.prev().unwrap_add_cost(&mut cost);
                    }
                    cost.seek_count += 1;
                }
                Ok(())
            })
        }
        .wrap_with_cost(cost)
    }

    pub fn get_query_apply_function(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        allow_get_raw: bool,
        result_type: QueryResultType,
        transaction: TransactionArg,
        add_element_function: fn(PathQueryPushArgs) -> CostResult<(), Error>,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        let mut cost = OperationCost::default();

        let mut results = Vec::new();

        let mut limit = sized_query.limit;
        let original_offset = sized_query.offset;
        let mut offset = original_offset;

        if sized_query.query.left_to_right {
            for item in sized_query.query.iter() {
                cost_return_on_error!(
                    &mut cost,
                    Self::query_item(
                        storage,
                        item,
                        &mut results,
                        path,
                        sized_query,
                        transaction,
                        &mut limit,
                        &mut offset,
                        allow_get_raw,
                        result_type,
                        add_element_function,
                    )
                );
                if limit == Some(0) {
                    break;
                }
            }
        } else {
            for item in sized_query.query.rev_iter() {
                cost_return_on_error!(
                    &mut cost,
                    Self::query_item(
                        storage,
                        item,
                        &mut results,
                        path,
                        sized_query,
                        transaction,
                        &mut limit,
                        &mut offset,
                        allow_get_raw,
                        result_type,
                        add_element_function,
                    )
                );
                if limit == Some(0) {
                    break;
                }
            }
        }

        let skipped = if let Some(original_offset_unwrapped) = original_offset {
            original_offset_unwrapped - offset.unwrap()
        } else {
            0
        };
        Ok((QueryResultElements::from_elements(results), skipped)).wrap_with_cost(cost)
    }

    // Returns a vector of elements excluding trees, and the number of skipped
    // elements
    pub fn get_path_query(
        storage: &RocksDbStorage,
        path_query: &PathQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_query_apply_function(
            storage,
            path_slices.as_slice(),
            &path_query.query,
            false,
            result_type,
            transaction,
            Element::path_query_push,
        )
    }

    // Returns a vector of elements including trees, and the number of skipped
    // elements
    pub fn get_raw_path_query(
        storage: &RocksDbStorage,
        path_query: &PathQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_query_apply_function(
            storage,
            path_slices.as_slice(),
            &path_query.query,
            true,
            result_type,
            transaction,
            Element::path_query_push,
        )
    }

    /// Returns a vector of elements, and the number of skipped elements
    pub fn get_sized_query(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        Element::get_query_apply_function(
            storage,
            path,
            sized_query,
            false,
            result_type,
            transaction,
            Element::path_query_push,
        )
    }

    /// Helper function that returns whether an element at the key for the
    /// element already exists.
    pub fn element_at_key_already_exists<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
    ) -> CostResult<bool, Error> {
        merk.exists(key.as_ref())
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        is_sum_tree: bool,
    ) -> CostResult<(), Error> {
        // TODO: Fix this
        let feature_type = match is_sum_tree {
            false => Some(TreeFeatureType::BasicMerk),
            // TODO: Remove unwrap
            true => Some(TreeFeatureType::SummedMerk(self.sum_value().unwrap())),
        };
        dbg!(feature_type);

        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        let batch_operations = [(key, Op::Put(serialized), feature_type)];
        merk.apply::<_, Vec<u8>>(&batch_operations, &[])
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn insert_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        // TODO: build the feature type here
        let entry = (key, Op::Put(serialized), Some(BasicMerk));
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    /// Insert an element in Merk under a key if it doesn't yet exist; path
    /// should be resolved and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert_if_not_exists<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        is_sum_tree: bool,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();
        let exists =
            cost_return_on_error!(&mut cost, self.element_at_key_already_exists(merk, key));
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(&mut cost, self.insert(merk, key, is_sum_tree));
            Ok(true).wrap_with_cost(cost)
        }
    }

    pub fn insert_if_not_exists_into_batch_operations<
        'db,
        S: StorageContext<'db>,
        K: AsRef<[u8]>,
    >(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();
        let exists = cost_return_on_error!(
            &mut cost,
            self.element_at_key_already_exists(merk, key.as_ref())
        );
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(
                &mut cost,
                self.insert_into_batch_operations(key, batch_operations)
            );
            Ok(true).wrap_with_cost(cost)
        }
    }

    /// Insert a reference element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert_reference<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        referenced_value: Vec<u8>,
        is_sum_tree: bool,
    ) -> CostResult<(), Error> {
        // TODO: Fix this
        let feature_type = match is_sum_tree {
            false => Some(TreeFeatureType::BasicMerk),
            // TODO: Remove unwrap
            true => Some(TreeFeatureType::SummedMerk(self.sum_value().unwrap())),
        };
        dbg!(feature_type);

        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        // TODO: Build feature type here
        let batch_operations = [(
            key,
            Op::PutReference(serialized, referenced_value),
            feature_type,
        )];
        merk.apply::<_, Vec<u8>>(&batch_operations, &[])
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn insert_reference_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        referenced_value: Vec<u8>,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };
        // TODO: Build feature type here
        let entry = (
            key,
            Op::PutReference(serialized, referenced_value),
            Some(BasicMerk),
        );
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .serialize(self)
            .map_err(|_| Error::CorruptedData(String::from("unable to serialize element")))
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, Error> {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
    }

    pub fn iterator<I: RawIterator>(mut raw_iter: I) -> CostContext<ElementsIterator<I>> {
        let mut cost = OperationCost::default();
        raw_iter.seek_to_first().unwrap_add_cost(&mut cost);
        ElementsIterator::new(raw_iter).wrap_with_cost(cost)
    }
}

pub struct ElementsIterator<I: RawIterator> {
    raw_iter: I,
}

pub fn raw_decode(bytes: &[u8]) -> Result<Element, Error> {
    let tree = Tree::decode_raw(bytes).map_err(|e| Error::CorruptedData(e.to_string()))?;
    let element: Element = Element::deserialize(tree.value())?;
    Ok(element)
}

impl<I: RawIterator> ElementsIterator<I> {
    pub fn new(raw_iter: I) -> Self {
        ElementsIterator { raw_iter }
    }

    pub fn next(&mut self) -> CostResult<Option<KeyElementPair>, Error> {
        let mut cost = OperationCost::default();

        Ok(if self.raw_iter.valid().unwrap_add_cost(&mut cost) {
            if let Some((key, value)) = self
                .raw_iter
                .key()
                .unwrap_add_cost(&mut cost)
                .zip(self.raw_iter.value().unwrap_add_cost(&mut cost))
            {
                let element = cost_return_on_error_no_add!(&cost, raw_decode(value));
                let key_vec = key.to_vec();
                self.raw_iter.next().unwrap_add_cost(&mut cost);
                Some((key_vec, element))
            } else {
                None
            }
        } else {
            None
        })
        .wrap_with_cost(cost)
    }

    pub fn fast_forward(&mut self, key: &[u8]) -> Result<(), Error> {
        while self.raw_iter.valid().unwrap() {
            if self.raw_iter.key().unwrap().unwrap() == key {
                break;
            } else {
                self.raw_iter.next().unwrap();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use merk::test_utils::TempMerk;
    use storage::Storage;

    use super::*;
    use crate::{
        subtree::QueryResultType::{
            QueryKeyElementPairResultType, QueryPathKeyElementTrioResultType,
        },
        tests::{make_grovedb, TEST_LEAF},
    };

    #[test]
    fn test_success_insert() {
        let mut merk = TempMerk::new();
        Element::empty_tree()
            .insert(&mut merk, b"mykey", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", false)
            .unwrap()
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key")
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_serialization() {
        let empty_tree = Element::empty_tree();
        let serialized = empty_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 34);
        assert_eq!(serialized.len(), empty_tree.serialized_byte_size());
        // The tree is fixed length 32 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(
            hex::encode(serialized),
            "02000000000000000000000000000000000000000000000000000000000000000000"
        );

        let empty_tree = Element::new_tree_with_flags([0; 32], Some(vec![5]));
        let serialized = empty_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 36);
        assert_eq!(
            hex::encode(serialized),
            "020000000000000000000000000000000000000000000000000000000000000000010105"
        );

        let item = Element::new_item(hex::decode("abcdef").expect("expected to decode"));
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 6);
        assert_eq!(serialized.len(), item.serialized_byte_size());
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "0003abcdef00");

        let item = Element::new_item_with_flags(
            hex::decode("abcdef").expect("expected to decode"),
            Some(vec![1]),
        );
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 8);
        assert_eq!(serialized.len(), item.serialized_byte_size());
        assert_eq!(hex::encode(serialized), "0003abcdef010101");

        let reference = Element::new_reference(vec![
            vec![0],
            hex::decode("abcd").expect("expected to decode"),
            vec![5],
        ]);
        let serialized = reference.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 10);
        assert_eq!(serialized.len(), reference.serialized_byte_size());
        // The item is variable length 2 bytes, so it's enum 1 then 1 byte for length,
        // then 1 byte for 0, then 1 byte 02 for abcd, then 1 byte '1' for 05
        assert_eq!(hex::encode(serialized), "0103010002abcd010500");

        let reference = Element::new_reference_with_flags(
            vec![
                vec![0],
                hex::decode("abcd").expect("expected to decode"),
                vec![5],
            ],
            Some(vec![1, 2, 3]),
        );
        let serialized = reference.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 14);
        assert_eq!(serialized.len(), reference.serialized_byte_size());
        assert_eq!(hex::encode(serialized), "0103010002abcd01050103010203");

        let empty_sum_tree = Element::empty_sum_tree();
        let serialized = empty_sum_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 35);
        assert_eq!(serialized.len(), empty_sum_tree.serialized_byte_size());
        // The tree is fixed length 32 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(
            hex::encode(serialized),
            "0400000000000000000000000000000000000000000000000000000000000000000000"
        );

        let empty_sum_tree = Element::new_sum_tree_with_flags([0; 32], Some(vec![5]));
        let serialized = empty_sum_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 37);
        assert_eq!(
            hex::encode(serialized),
            "04000000000000000000000000000000000000000000000000000000000000000000010105"
        );

        let sum_item = Element::SumItem(hex::decode("abcdef").expect("expected to decode"), None);
        let serialized = sum_item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 6);
        assert_eq!(serialized.len(), sum_item.serialized_byte_size());
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "0303abcdef00");

        let sum_item = Element::SumItem(
            hex::decode("abcdef").expect("expected to decode"),
            Some(vec![1]),
        );
        let serialized = sum_item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 8);
        assert_eq!(serialized.len(), sum_item.serialized_byte_size());
        assert_eq!(hex::encode(serialized), "0303abcdef010101");
    }

    #[test]
    fn test_get_query() {
        let db = make_grovedb();

        let storage = &db.db;
        let storage_context = storage.get_storage_context([TEST_LEAF]).unwrap();
        let mut merk = Merk::open(storage_context)
            .unwrap()
            .expect("cannot open Merk"); // TODO implement costs

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", false)
            .unwrap()
            .expect("expected successful insertion");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());
        assert_eq!(
            Element::get_query_values(&storage, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayya".to_vec()),
                Element::new_item(b"ayyc".to_vec())
            ]
        );

        // Test range query
        let mut query = Query::new();
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query_values(&storage, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayya".to_vec()),
                Element::new_item(b"ayyb".to_vec()),
                Element::new_item(b"ayyc".to_vec())
            ]
        );

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query_values(&storage, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayyb".to_vec()),
                Element::new_item(b"ayyc".to_vec()),
                Element::new_item(b"ayyd".to_vec())
            ]
        );

        // Test overlaps
        let mut query = Query::new();
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query_values(&storage, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayya".to_vec()),
                Element::new_item(b"ayyb".to_vec()),
                Element::new_item(b"ayyc".to_vec())
            ]
        );
    }

    #[test]
    fn test_get_query_with_path() {
        let db = make_grovedb();

        let storage = &db.db;
        let storage_context = storage.get_storage_context([TEST_LEAF]).unwrap();
        let mut merk = Merk::open(storage_context)
            .unwrap()
            .expect("cannot open Merk"); // TODO implement costs

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", false)
            .unwrap()
            .expect("expected successful insertion");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());
        assert_eq!(
            Element::get_query(
                &storage,
                &[TEST_LEAF],
                &query,
                QueryPathKeyElementTrioResultType,
                None
            )
            .unwrap()
            .expect("expected successful get_query")
            .to_path_key_elements(),
            vec![
                (
                    vec![TEST_LEAF.to_vec()],
                    b"a".to_vec(),
                    Element::new_item(b"ayya".to_vec())
                ),
                (
                    vec![TEST_LEAF.to_vec()],
                    b"c".to_vec(),
                    Element::new_item(b"ayyc".to_vec())
                )
            ]
        );
    }

    #[test]
    fn test_get_range_query() {
        let db = make_grovedb();

        let storage = &db.db;
        let storage_context = storage.get_storage_context([TEST_LEAF]).unwrap();
        let mut merk = Merk::open(storage_context)
            .unwrap()
            .expect("cannot open Merk"); // TODO implement costs

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", false)
            .unwrap()
            .expect("expected successful insertion");

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &ascending_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");

        let elements: Vec<KeyElementPair> = elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_element) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(_) => None,
            })
            .collect();
        assert_eq!(
            elements,
            vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");

        let elements: Vec<KeyElementPair> = elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_element) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(_) => None,
            })
            .collect();
        assert_eq!(
            elements,
            vec![
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_get_range_inclusive_query() {
        let db = make_grovedb();

        let storage = &db.db;
        let storage_context = storage.get_storage_context([TEST_LEAF]).unwrap();
        let mut merk = Merk::open(storage_context)
            .unwrap()
            .expect("cannot open Merk");

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", false)
            .unwrap()
            .expect("expected successful insertion");

        // Test range inclusive query
        let mut query = Query::new_with_direction(true);
        query.insert_range_inclusive(b"a".to_vec()..=b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        fn check_elements_no_skipped(
            (elements, skipped): (QueryResultElements, u16),
            reverse: bool,
        ) {
            let mut expected = vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"d".to_vec(), Element::new_item(b"ayyd".to_vec())),
            ];
            if reverse {
                expected.reverse();
            }
            assert_eq!(elements.to_key_elements(), expected);
            assert_eq!(skipped, 0);
        }

        check_elements_no_skipped(
            Element::get_sized_query(
                &storage,
                &[TEST_LEAF],
                &ascending_query,
                QueryKeyElementPairResultType,
                None,
            )
            .unwrap()
            .expect("expected successful get_query"),
            false,
        );

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(
                &storage,
                &[TEST_LEAF],
                &backwards_query,
                QueryKeyElementPairResultType,
                None,
            )
            .unwrap()
            .expect("expected successful get_query"),
            true,
        );

        // Test range inclusive query
        let mut query = Query::new_with_direction(false);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(
                &storage,
                &[TEST_LEAF],
                &backwards_query,
                QueryKeyElementPairResultType,
                None,
            )
            .unwrap()
            .expect("expected successful get_query"),
            true,
        );
    }

    #[test]
    fn test_get_limit_query() {
        let db = make_grovedb();

        let storage = &db.db;
        let storage_context = storage.get_storage_context([TEST_LEAF]).unwrap();
        let mut merk = Merk::open(storage_context)
            .unwrap()
            .expect("cannot open Merk");

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", false)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", false)
            .unwrap()
            .expect("expected successful insertion");

        // Test queries by key
        let mut query = Query::new_with_direction(true);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        // Test queries by key
        let mut query = Query::new_with_direction(false);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        // The limit will mean we will only get back 1 item
        let limit_query = SizedQuery::new(query.clone(), Some(1), None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![(b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),]
        );
        assert_eq!(skipped, 0);

        // Test range query
        let mut query = Query::new_with_direction(true);
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        let limit_query = SizedQuery::new(query.clone(), Some(2), None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec()))
            ]
        );
        assert_eq!(skipped, 0);

        let limit_offset_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_offset_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec()))
            ]
        );
        assert_eq!(skipped, 1);

        // Test range query
        let mut query = Query::new_with_direction(false);
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());

        let limit_offset_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_offset_backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec()))
            ]
        );
        assert_eq!(skipped, 1);

        // Test range inclusive query
        let mut query = Query::new_with_direction(true);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        let limit_full_query = SizedQuery::new(query.clone(), Some(5), Some(0));
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_full_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"d".to_vec(), Element::new_item(b"ayyd".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        let mut query = Query::new_with_direction(false);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());

        let limit_offset_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_offset_backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
            ]
        );
        assert_eq!(skipped, 1);

        // Test overlaps
        let mut query = Query::new_with_direction(false);
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        let limit_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &limit_backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
            ]
        );
        assert_eq!(skipped, 1);
    }
}
