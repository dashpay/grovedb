//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

use core::fmt;

use bincode::Options;
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, storage_cost::removal::StorageRemovedBytes,
    CostContext, CostResult, CostsExt, OperationCost,
};
use integer_encoding::VarInt;
use merk::{
    anyhow,
    ed::Decode,
    estimated_costs::LAYER_COST_SIZE,
    proofs::{query::QueryItem, Query},
    tree::{kv::KV, Tree, TreeInner},
    BatchEntry, MerkOptions, Op,
    TreeFeatureType::BasicMerk,
};
use serde::{Deserialize, Serialize};
use storage::{rocksdb_storage::RocksDbStorage, RawIterator, StorageContext};
use visualize::visualize_to_vec;

use crate::{
    query_result_type::{
        KeyElementPair, QueryResultElement, QueryResultElements, QueryResultType,
        QueryResultType::QueryElementResultType,
    },
    reference_path::{path_from_reference_path_type, ReferencePathType},
    util::{
        merk_optional_tx, storage_context_optional_tx, storage_context_with_parent_optional_tx,
    },
    Error, Hash, Merk, PathQuery, SizedQuery, TransactionArg,
};

/// Optional meta-data to be stored per element
pub type ElementFlags = Vec<u8>;

/// Optional single byte to represent the maximum number of reference hop to
/// base element
pub type MaxReferenceHop = Option<u8>;

/// The cost of a tree
pub const TREE_COST_SIZE: u32 = LAYER_COST_SIZE; // 3

/// int 64 sum value
pub type SumValue = i64;

/// Variants of GroveDB stored entities
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>, Option<ElementFlags>),
    /// A reference to an object by its path
    Reference(ReferencePathType, MaxReferenceHop, Option<ElementFlags>),
    /// A subtree, contains the a prefixed key representing the root of the
    /// subtree.
    Tree(Option<Vec<u8>>, Option<ElementFlags>),
    /// Vector encoded integer value that can be totaled in a sum tree
    SumItem(Vec<u8>, Option<ElementFlags>),
    /// Same as Element::Tree but underlying Merk sums value of it's summable
    /// nodes
    SumTree(Option<Vec<u8>>, SumValue, Option<ElementFlags>),
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

    pub fn empty_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_tree_with_flags(Default::default(), flags)
    }

    pub fn empty_sum_tree() -> Self {
        Element::new_sum_tree(Default::default())
    }

    pub fn empty_sum_tree_with_flags(flags: Option<ElementFlags>) -> Self {
        Element::new_sum_tree_with_flags(Default::default(), flags)
    }

    pub fn new_item(item_value: Vec<u8>) -> Self {
        Element::Item(item_value, None)
    }

    pub fn new_item_with_flags(item_value: Vec<u8>, flags: Option<ElementFlags>) -> Self {
        Element::Item(item_value, flags)
    }

    pub fn new_sum_item(value: i64) -> Self {
        Element::SumItem(value.encode_var_vec(), None)
    }

    pub fn new_sum_item_with_flags(value: i64, flags: Option<ElementFlags>) -> Self {
        Element::SumItem(value.encode_var_vec(), flags)
    }

    pub fn new_reference(reference_path: ReferencePathType) -> Self {
        Element::Reference(reference_path, None, None)
    }

    pub fn new_reference_with_flags(
        reference_path: ReferencePathType,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Reference(reference_path, None, flags)
    }

    pub fn new_reference_with_hops(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
    ) -> Self {
        Element::Reference(reference_path, max_reference_hop, None)
    }

    pub fn new_reference_with_max_hops_and_flags(
        reference_path: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Reference(reference_path, max_reference_hop, flags)
    }

    pub fn new_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::Tree(maybe_root_key, None)
    }

    pub fn new_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::Tree(maybe_root_key, flags)
    }

    pub fn new_sum_tree(maybe_root_key: Option<Vec<u8>>) -> Self {
        Element::SumTree(maybe_root_key, 0, None)
    }

    pub fn new_sum_tree_with_flags(
        maybe_root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumTree(maybe_root_key, 0, flags)
    }

    pub fn new_sum_tree_with_flags_and_sum_value(
        maybe_root_key: Option<Vec<u8>>,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
    ) -> Self {
        Element::SumTree(maybe_root_key, sum_value, flags)
    }

    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else
    pub fn sum_value(&self) -> Option<i64> {
        match self {
            Element::SumItem(value, _) => {
                i64::decode_var(value).map(|(encoded_value, _)| encoded_value)
            }
            Element::SumTree(_, sum_value, _) => Some(*sum_value),
            _ => Some(0),
        }
    }

    pub fn is_sum_tree(&self) -> bool {
        match self {
            Element::SumTree(..) => true,
            _ => false,
        }
    }

    /// Grab the optional flag stored in an element
    pub fn get_flags(&self) -> &Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => flags,
        }
    }

    /// Grab the optional flag stored in an element
    pub fn get_flags_owned(self) -> Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => flags,
        }
    }

    /// Grab the optional flag stored in an element as mutable
    pub fn get_flags_mut(&mut self) -> &mut Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => flags,
        }
    }

    /// Get the size of an element in bytes
    pub fn byte_size(&self) -> u32 {
        match self {
            Element::Item(item, element_flag) | Element::SumItem(item, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + item.len() as u32
                } else {
                    item.len() as u32
                }
            }
            Element::Reference(path_reference, _, element_flag) => {
                let path_length = path_reference.encoding_length() as u32;

                if let Some(flag) = element_flag {
                    flag.len() as u32 + path_length
                } else {
                    path_length
                }
            }
            Element::Tree(_, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + 32
                } else {
                    32
                }
            }
            Element::SumTree(_, _, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + 32 + 8
                } else {
                    32 + 8
                }
            }
        }
    }

    pub fn required_item_space(len: u32, flag_len: u32) -> u32 {
        len + len.required_space() as u32 + flag_len + flag_len.required_space() as u32 + 1
    }

    /// Get the size that the element will occupy on disk
    pub fn node_byte_size(&self, key_len: u32) -> u32 {
        let serialized_value_size = self.serialized_size() as u32; // this includes the flags
        KV::node_byte_cost_size_for_key_and_value_lengths(key_len, serialized_value_size)
    }

    /// Delete an element from Merk under a key
    pub fn delete<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
    ) -> CostResult<(), Error> {
        // TODO: delete references on this element
        let op = if is_layered {
            Op::DeleteLayered
        } else {
            Op::Delete
        };
        let batch = [(key, op, None)];
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch, &[], merk_options, &|key, value| {
            Self::tree_costs_for_key_value(key, value).map_err(anyhow::Error::msg)
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Delete an element from Merk under a key
    pub fn delete_with_sectioned_removal_bytes<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> anyhow::Result<(StorageRemovedBytes, StorageRemovedBytes)>,
    ) -> CostResult<(), Error> {
        // TODO: delete references on this element
        let op = if is_layered {
            Op::DeleteLayered
        } else {
            Op::Delete
        };
        let batch = [(key, op, None)];
        merk.apply_with_costs_just_in_time_value_update::<_, Vec<u8>>(
            &batch,
            &[],
            merk_options,
            &|key, value| Self::tree_costs_for_key_value(key, value).map_err(anyhow::Error::msg),
            &mut |_costs, _old_value, _value| Ok((false, None)),
            sectioned_removal,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Delete an element from Merk under a key to batch operations
    pub fn delete_into_batch_operations<K: AsRef<[u8]>>(
        key: K,
        is_layered: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let op = if is_layered {
            Op::DeleteLayered
        } else {
            Op::Delete
        };
        let entry = (key, op, None);
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
                Error::PathKeyNotFound(format!(
                    "key not found in Merk for get: {}",
                    hex::encode(key)
                ))
            })
        );
        let element = cost_return_on_error_no_add!(
            &cost,
            Self::deserialize(value.as_slice())
                .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
        );
        Ok(element).wrap_with_cost(cost)
    }

    /// Get an element directly from storage under a key
    /// Merk does not need to be loaded
    pub fn get_from_storage<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        storage: &S,
        key: K,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();
        let node_value_opt = cost_return_on_error!(
            &mut cost,
            storage
                .get(key.as_ref())
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let node_value = cost_return_on_error_no_add!(
            &cost,
            node_value_opt.ok_or_else(|| {
                Error::PathKeyNotFound(format!(
                    "key not found in Merk for get from storage: {}",
                    hex::encode(key)
                ))
            })
        );
        let tree_inner: TreeInner = cost_return_on_error_no_add!(
            &cost,
            Decode::decode(node_value.as_slice()).map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let value = tree_inner.value_as_owned();
        let element = cost_return_on_error_no_add!(
            &cost,
            Self::deserialize(value.as_slice())
                .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
        );
        Ok(element).wrap_with_cost(cost)
    }

    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get_with_absolute_refs<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        path: &[&[u8]],
        key: K,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();

        let element = cost_return_on_error!(&mut cost, Self::get(merk, key.as_ref()));

        let absolute_element = cost_return_on_error_no_add!(
            &cost,
            element.convert_if_reference_to_absolute_reference(path, Some(key.as_ref()))
        );

        Ok(absolute_element).wrap_with_cost(cost)
    }

    /// Get an element's value hash from Merk under a key
    pub fn get_value_hash<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &Merk<S>,
        key: K,
    ) -> CostResult<Option<Hash>, Error> {
        let mut cost = OperationCost::default();

        let value_hash = cost_return_on_error!(
            &mut cost,
            merk.get_value_hash(key.as_ref())
                .map_err(|e| Error::CorruptedData(e.to_string()))
        );

        Ok(value_hash).wrap_with_cost(cost)
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

    fn convert_if_reference_to_absolute_reference(
        self,
        path: &[&[u8]],
        key: Option<&[u8]>,
    ) -> Result<Element, Error> {
        // Convert any non absolute reference type to an absolute one
        // we do this here because references are aggregated first then followed later
        // to follow non absolute references, we need the path they are stored at
        // this information is lost during the aggregation phase.
        Ok(match &self {
            Element::Reference(reference_path_type, ..) => match reference_path_type {
                ReferencePathType::AbsolutePathReference(..) => self,
                _ => {
                    // Element is a reference and is not absolute.
                    // build the stored path for this reference
                    let current_path = path.clone().to_vec();
                    let absolute_path = path_from_reference_path_type(
                        reference_path_type.clone(),
                        current_path,
                        key,
                    )?;
                    // return an absolute reference that contains this info
                    Element::Reference(
                        ReferencePathType::AbsolutePathReference(absolute_path),
                        None,
                        None,
                    )
                }
            },
            _ => self,
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

        let element = element.convert_if_reference_to_absolute_reference(path, key)?;

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
                                                Element::get_with_absolute_refs(
                                                    &subtree,
                                                    path_vec.as_slice(),
                                                    subquery_key.as_slice()
                                                )
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
                                                    Element::get_with_absolute_refs(
                                                        &subtree,
                                                        path_vec.as_slice(),
                                                        subquery_key.as_slice()
                                                    )
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
                                                    Element::get_with_absolute_refs(
                                                        &subtree,
                                                        path_vec.as_slice(),
                                                        subquery_key.as_slice()
                                                    )
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
                             a Tree of trees"
                                .to_owned(),
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
        options: Option<MerkOptions>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        // TODO: use correct feature type
        let batch_operations = [(key, Op::Put(serialized), Some(BasicMerk))];
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch_operations, &[], options, &|key, value| {
            Self::tree_costs_for_key_value(key, value).map_err(anyhow::Error::msg)
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn tree_costs_for_key_value(key: &Vec<u8>, value: &Vec<u8>) -> Result<u32, Error> {
        let element = Element::deserialize(value)?;
        match element {
            Element::Tree(_, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len, value_len,
                ))
            }
            _ => Err(Error::CorruptedCodeExecution(
                "only trees are supported for specialized costs",
            )),
        }
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

        // TODO: use correct feature type
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
        options: Option<MerkOptions>,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();
        let exists =
            cost_return_on_error!(&mut cost, self.element_at_key_already_exists(merk, key));
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(&mut cost, self.insert(merk, key, options));
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
        referenced_value: Hash,
        options: Option<MerkOptions>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        // TODO: use correct feature type
        let batch_operations = [(
            key,
            Op::PutCombinedReference(serialized, referenced_value),
            Some(BasicMerk),
        )];
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch_operations, &[], options, &|key, value| {
            Self::tree_costs_for_key_value(key, value).map_err(anyhow::Error::msg)
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn insert_reference_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        referenced_value: Hash,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        // TODO: use correct feature type
        let entry = (
            key,
            Op::PutCombinedReference(serialized, referenced_value),
            Some(BasicMerk),
        );
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    /// Insert a tree element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        subtree_root_hash: Hash,
        options: Option<MerkOptions>,
    ) -> CostResult<(), Error> {
        let cost = TREE_COST_SIZE
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };

        // TODO: use correct feature type
        let batch_operations = [(
            key,
            Op::PutLayeredReference(serialized, cost, subtree_root_hash),
            Some(BasicMerk),
        )];
        merk.apply_with_tree_costs::<_, Vec<u8>>(&batch_operations, &[], options, &|key, value| {
            Self::tree_costs_for_key_value(key, value).map_err(anyhow::Error::msg)
        })
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn insert_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        subtree_root_hash: Hash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let serialized = match self.serialize() {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(Default::default()),
        };
        let cost = TREE_COST_SIZE
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });

        // Replacing is more efficient, but should lead to the same costs
        // TODO: use correct feature type
        let entry = if is_replace {
            (
                key,
                Op::ReplaceLayeredReference(serialized, cost, subtree_root_hash),
                Some(BasicMerk),
            )
        } else {
            (
                key,
                Op::PutLayeredReference(serialized, cost, subtree_root_hash),
                Some(BasicMerk),
            )
        };
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

    pub fn serialized_size(&self) -> usize {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .serialized_size(self)
            .unwrap() as usize // this should not be able to error
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
    let tree = Tree::decode_raw(bytes, vec![]).map_err(|e| Error::CorruptedData(e.to_string()))?;
    let element: Element = Element::deserialize(tree.value_as_slice())?;
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
    use storage::rocksdb_storage::PrefixedRocksDbStorageContext;

    use super::*;
    use crate::{
        subtree::QueryResultType::{
            QueryKeyElementPairResultType, QueryPathKeyElementTrioResultType,
        },
        tests::{make_test_grovedb, TEST_LEAF},
    };

    #[test]
    fn test_success_insert() {
        let mut merk = TempMerk::new();
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None)
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
        assert_eq!(serialized.len(), 3);
        assert_eq!(serialized.len(), empty_tree.serialized_size());
        // The tree is fixed length 32 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "020000");

        let empty_tree = Element::new_tree_with_flags(None, Some(vec![5]));
        let serialized = empty_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 5);
        assert_eq!(serialized.len(), empty_tree.serialized_size());
        assert_eq!(hex::encode(serialized), "0200010105");

        let item = Element::new_item(hex::decode("abcdef").expect("expected to decode"));
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 6);
        assert_eq!(serialized.len(), item.serialized_size());
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "0003abcdef00");

        let item = Element::new_item_with_flags(
            hex::decode("abcdef").expect("expected to decode"),
            Some(vec![1]),
        );
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 8);
        assert_eq!(serialized.len(), item.serialized_size());
        assert_eq!(hex::encode(serialized), "0003abcdef010101");

        let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            vec![0],
            hex::decode("abcd").expect("expected to decode"),
            vec![5],
        ]));
        let serialized = reference.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 12);
        assert_eq!(serialized.len(), reference.serialized_size());
        // The item is variable length 2 bytes, so it's enum 1 then 1 byte for length,
        // then 1 byte for 0, then 1 byte 02 for abcd, then 1 byte '1' for 05
        assert_eq!(hex::encode(serialized), "010003010002abcd01050000");

        let reference = Element::new_reference_with_flags(
            ReferencePathType::AbsolutePathReference(vec![
                vec![0],
                hex::decode("abcd").expect("expected to decode"),
                vec![5],
            ]),
            Some(vec![1, 2, 3]),
        );
        let serialized = reference.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 16);
        assert_eq!(serialized.len(), reference.serialized_size());
        assert_eq!(hex::encode(serialized), "010003010002abcd0105000103010203");
    }

    #[test]
    fn test_get_query() {
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        assert_eq!(
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
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
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
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
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
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
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
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
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());
        assert_eq!(
            Element::get_query(
                &db.db,
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
        let db = make_test_grovedb();

        let storage = &db.db;
        let mut merk = db
            .open_non_transactional_merk_at_path([TEST_LEAF])
            .unwrap()
            .expect("cannot open Merk"); // TODO implement costs

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", None)
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
        let db = make_test_grovedb();

        let storage = &db.db;
        let mut merk: Merk<PrefixedRocksDbStorageContext> = db
            .open_non_transactional_merk_at_path([TEST_LEAF])
            .unwrap()
            .expect("cannot open Merk");

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", None)
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
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new_with_direction(true);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
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
            &db.db,
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
            &db.db,
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
            &db.db,
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
            &db.db,
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
            &db.db,
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
            &db.db,
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
            &db.db,
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
            &db.db,
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
