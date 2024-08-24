//! Query
//! Implements functions in Element for querying

use std::fmt;

#[cfg(feature = "full")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostResult, CostsExt,
    OperationCost,
};
#[cfg(feature = "full")]
use grovedb_merk::proofs::query::query_item::QueryItem;
#[cfg(feature = "full")]
use grovedb_merk::proofs::query::SubqueryBranch;
#[cfg(feature = "full")]
use grovedb_merk::proofs::Query;
#[cfg(feature = "full")]
use grovedb_path::SubtreePath;
#[cfg(feature = "full")]
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, StorageContext};
#[cfg(feature = "full")]
use grovedb_version::{
    check_grovedb_v0, check_grovedb_v0_with_cost, error::GroveVersionError, version::GroveVersion,
};

#[cfg(feature = "full")]
use crate::operations::proof::util::hex_to_ascii;
#[cfg(any(feature = "full", feature = "verify"))]
use crate::Element;
#[cfg(feature = "full")]
use crate::{
    element::helpers::raw_decode,
    query_result_type::{
        KeyElementPair, QueryResultElement, QueryResultElements, QueryResultType,
        QueryResultType::{
            QueryElementResultType, QueryKeyElementPairResultType,
            QueryPathKeyElementTrioResultType,
        },
    },
    util::{merk_optional_tx, merk_optional_tx_internal_error, storage_context_optional_tx},
    Error, PathQuery, TransactionArg,
};
#[cfg(feature = "full")]
use crate::{query_result_type::Path, SizedQuery};

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Copy, Clone, Debug)]
pub struct QueryOptions {
    pub allow_get_raw: bool,
    pub allow_cache: bool,
    /// Should we decrease the limit of elements found when we have no
    /// subelements in the subquery? This should generally be set to true,
    /// as having it false could mean very expensive queries. The queries
    /// would be expensive because we could go through many many trees where the
    /// sub elements have no matches, hence the limit would not decrease and
    /// hence we would continue on the increasingly expensive query.
    pub decrease_limit_on_range_with_no_sub_elements: bool,
    pub error_if_intermediate_path_tree_not_present: bool,
}

#[cfg(any(feature = "full", feature = "verify"))]
impl fmt::Display for QueryOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "QueryOptions {{")?;
        writeln!(f, "  allow_get_raw: {}", self.allow_get_raw)?;
        writeln!(f, "  allow_cache: {}", self.allow_cache)?;
        writeln!(
            f,
            "  decrease_limit_on_range_with_no_sub_elements: {}",
            self.decrease_limit_on_range_with_no_sub_elements
        )?;
        writeln!(
            f,
            "  error_if_intermediate_path_tree_not_present: {}",
            self.error_if_intermediate_path_tree_not_present
        )?;
        write!(f, "}}")
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
            allow_get_raw: false,
            allow_cache: true,
            decrease_limit_on_range_with_no_sub_elements: true,
            error_if_intermediate_path_tree_not_present: true,
        }
    }
}

#[cfg(feature = "full")]
/// Path query push arguments
pub struct PathQueryPushArgs<'db, 'ctx, 'a>
where
    'db: 'ctx,
{
    pub storage: &'db RocksDbStorage,
    pub transaction: TransactionArg<'db, 'ctx>,
    pub key: Option<&'a [u8]>,
    pub element: Element,
    pub path: &'a [&'a [u8]],
    pub subquery_path: Option<Path>,
    pub subquery: Option<Query>,
    pub left_to_right: bool,
    pub query_options: QueryOptions,
    pub result_type: QueryResultType,
    pub results: &'a mut Vec<QueryResultElement>,
    pub limit: &'a mut Option<u16>,
    pub offset: &'a mut Option<u16>,
}

#[cfg(feature = "full")]
fn format_query(query: &Query, indent: usize) -> String {
    let indent_str = " ".repeat(indent);
    let mut output = format!("{}Query {{\n", indent_str);

    output += &format!("{}  items: [\n", indent_str);
    for item in &query.items {
        output += &format!("{}    {},\n", indent_str, item);
    }
    output += &format!("{}  ],\n", indent_str);

    output += &format!(
        "{}  default_subquery_branch: {}\n",
        indent_str,
        format_subquery_branch(&query.default_subquery_branch, indent + 2)
    );

    if let Some(ref branches) = query.conditional_subquery_branches {
        output += &format!("{}  conditional_subquery_branches: {{\n", indent_str);
        for (item, branch) in branches {
            output += &format!(
                "{}    {}: {},\n",
                indent_str,
                item,
                format_subquery_branch(branch, indent + 4)
            );
        }
        output += &format!("{}  }},\n", indent_str);
    }

    output += &format!("{}  left_to_right: {}\n", indent_str, query.left_to_right);
    output += &format!("{}}}", indent_str);

    output
}

#[cfg(feature = "full")]
fn format_subquery_branch(branch: &SubqueryBranch, indent: usize) -> String {
    let indent_str = " ".repeat(indent);
    let mut output = "SubqueryBranch {{\n".to_string();

    if let Some(ref path) = branch.subquery_path {
        output += &format!("{}  subquery_path: {:?},\n", indent_str, path);
    }

    if let Some(ref subquery) = branch.subquery {
        output += &format!(
            "{}  subquery: {},\n",
            indent_str,
            format_query(subquery, indent + 2)
        );
    }

    output += &format!("{}}}", " ".repeat(indent));

    output
}

#[cfg(feature = "full")]
impl<'db, 'ctx, 'a> fmt::Display for PathQueryPushArgs<'db, 'ctx, 'a>
where
    'db: 'ctx,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "PathQueryPushArgs {{")?;
        writeln!(
            f,
            "  key: {}",
            self.key.map_or("None".to_string(), hex_to_ascii)
        )?;
        writeln!(f, "  element: {}", self.element)?;
        writeln!(
            f,
            "  path: [{}]",
            self.path
                .iter()
                .map(|p| hex_to_ascii(p))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        writeln!(
            f,
            "  subquery_path: {}",
            self.subquery_path
                .as_ref()
                .map_or("None".to_string(), |p| format!(
                    "[{}]",
                    p.iter()
                        .map(|e| hex_to_ascii(e.as_slice()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
        )?;
        writeln!(
            f,
            "  subquery: {}",
            self.subquery
                .as_ref()
                .map_or("None".to_string(), |q| format!("\n{}", format_query(q, 4)))
        )?;
        writeln!(f, "  left_to_right: {}", self.left_to_right)?;
        writeln!(f, "  query_options: {}", self.query_options)?;
        writeln!(f, "  result_type: {}", self.result_type)?;
        writeln!(
            f,
            "  results: [{}]",
            self.results
                .iter()
                .map(|r| format!("{}", r))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        writeln!(f, "  limit: {:?}", self.limit)?;
        writeln!(f, "  offset: {:?}", self.offset)?;
        write!(f, "}}")
    }
}

impl Element {
    #[cfg(feature = "full")]
    /// Returns a vector of result elements based on given query
    pub fn get_query(
        storage: &RocksDbStorage,
        merk_path: &[&[u8]],
        query: &Query,
        query_options: QueryOptions,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<QueryResultElements, Error> {
        check_grovedb_v0_with_cost!(
            "insert_subtree_into_batch_operations",
            grove_version.grovedb_versions.element.get_query
        );

        let sized_query = SizedQuery::new(query.clone(), None, None);
        Element::get_sized_query(
            storage,
            merk_path,
            &sized_query,
            query_options,
            result_type,
            transaction,
            grove_version,
        )
        .map_ok(|(elements, _)| elements)
    }

    #[cfg(feature = "full")]
    /// Get values of result elements coming from given query
    pub fn get_query_values(
        storage: &RocksDbStorage,
        merk_path: &[&[u8]],
        query: &Query,
        query_options: QueryOptions,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Element>, Error> {
        check_grovedb_v0_with_cost!(
            "get_query_values",
            grove_version.grovedb_versions.element.get_query_values
        );

        Element::get_query(
            storage,
            merk_path,
            query,
            query_options,
            QueryElementResultType,
            transaction,
            grove_version,
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

    #[cfg(feature = "full")]
    /// Returns a vector of result elements and the number of skipped items
    /// based on given query
    pub fn get_query_apply_function(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        query_options: QueryOptions,
        result_type: QueryResultType,
        transaction: TransactionArg,
        add_element_function: fn(PathQueryPushArgs, &GroveVersion) -> CostResult<(), Error>,
        grove_version: &GroveVersion,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        check_grovedb_v0_with_cost!(
            "get_query_apply_function",
            grove_version
                .grovedb_versions
                .element
                .get_query_apply_function
        );

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
                        query_options,
                        result_type,
                        add_element_function,
                        grove_version,
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
                        query_options,
                        result_type,
                        add_element_function,
                        grove_version,
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

    #[cfg(feature = "full")]
    /// Returns a vector of elements excluding trees, and the number of skipped
    /// elements
    pub fn get_path_query(
        storage: &RocksDbStorage,
        path_query: &PathQuery,
        query_options: QueryOptions,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        check_grovedb_v0_with_cost!(
            "get_path_query",
            grove_version.grovedb_versions.element.get_path_query
        );

        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_query_apply_function(
            storage,
            path_slices.as_slice(),
            &path_query.query,
            query_options,
            result_type,
            transaction,
            Element::path_query_push,
            grove_version,
        )
    }

    #[cfg(feature = "full")]
    /// Returns a vector of elements, and the number of skipped elements
    pub fn get_sized_query(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        query_options: QueryOptions,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        check_grovedb_v0_with_cost!(
            "get_sized_query",
            grove_version.grovedb_versions.element.get_sized_query
        );

        Element::get_query_apply_function(
            storage,
            path,
            sized_query,
            query_options,
            result_type,
            transaction,
            Element::path_query_push,
            grove_version,
        )
    }

    #[cfg(feature = "full")]
    /// Push arguments to path query
    fn path_query_push(
        args: PathQueryPushArgs,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "path_query_push",
            grove_version.grovedb_versions.element.path_query_push
        );

        // println!("path_query_push {} \n", args);

        let mut cost = OperationCost::default();

        let PathQueryPushArgs {
            storage,
            transaction,
            key,
            element,
            path,
            subquery_path,
            subquery,
            left_to_right,
            query_options,
            result_type,
            results,
            limit,
            offset,
        } = args;
        let QueryOptions {
            allow_get_raw,
            allow_cache,
            decrease_limit_on_range_with_no_sub_elements,
            ..
        } = query_options;
        if element.is_any_tree() {
            let mut path_vec = path.to_vec();
            let key = cost_return_on_error_no_add!(
                &cost,
                key.ok_or(Error::MissingParameter(
                    "the key must be provided when using a subquery path",
                ))
            );
            path_vec.push(key);

            if let Some(subquery) = subquery {
                if let Some(subquery_path) = &subquery_path {
                    path_vec.extend(subquery_path.iter().map(|k| k.as_slice()));
                }

                let inner_query = SizedQuery::new(subquery, *limit, *offset);
                let path_vec_owned = path_vec.iter().map(|x| x.to_vec()).collect();
                let inner_path_query = PathQuery::new(path_vec_owned, inner_query);

                let (mut sub_elements, skipped) = cost_return_on_error!(
                    &mut cost,
                    Element::get_path_query(
                        storage,
                        &inner_path_query,
                        query_options,
                        result_type,
                        transaction,
                        grove_version,
                    )
                );

                if let Some(limit) = limit {
                    if sub_elements.is_empty() && decrease_limit_on_range_with_no_sub_elements {
                        // we should decrease by 1 in this case
                        *limit = limit.saturating_sub(1);
                    } else {
                        *limit = limit.saturating_sub(sub_elements.len() as u16);
                    }
                }
                if let Some(offset) = offset {
                    *offset = offset.saturating_sub(skipped);
                }
                results.append(&mut sub_elements.elements);
            } else if let Some(subquery_path) = subquery_path {
                if offset.unwrap_or(0) == 0 {
                    if let Some((subquery_path_last_key, subquery_path_front_keys)) =
                        &subquery_path.split_last()
                    {
                        path_vec.extend(subquery_path_front_keys.iter().map(|k| k.as_slice()));

                        let subtree_path: SubtreePath<_> = path_vec.as_slice().into();

                        match result_type {
                            QueryElementResultType => {
                                merk_optional_tx!(
                                    &mut cost,
                                    storage,
                                    subtree_path,
                                    None,
                                    transaction,
                                    subtree,
                                    grove_version,
                                    {
                                        results.push(QueryResultElement::ElementResultItem(
                                            cost_return_on_error!(
                                                &mut cost,
                                                Element::get_with_absolute_refs(
                                                    &subtree,
                                                    path_vec.as_slice(),
                                                    subquery_path_last_key.as_slice(),
                                                    allow_cache,
                                                    grove_version,
                                                )
                                            ),
                                        ));
                                    }
                                );
                            }
                            QueryKeyElementPairResultType => {
                                merk_optional_tx!(
                                    &mut cost,
                                    storage,
                                    subtree_path,
                                    None,
                                    transaction,
                                    subtree,
                                    grove_version,
                                    {
                                        results.push(QueryResultElement::KeyElementPairResultItem(
                                            (
                                                subquery_path_last_key.to_vec(),
                                                cost_return_on_error!(
                                                    &mut cost,
                                                    Element::get_with_absolute_refs(
                                                        &subtree,
                                                        path_vec.as_slice(),
                                                        subquery_path_last_key.as_slice(),
                                                        allow_cache,
                                                        grove_version,
                                                    )
                                                ),
                                            ),
                                        ));
                                    }
                                );
                            }
                            QueryPathKeyElementTrioResultType => {
                                merk_optional_tx!(
                                    &mut cost,
                                    storage,
                                    subtree_path,
                                    None,
                                    transaction,
                                    subtree,
                                    grove_version,
                                    {
                                        results.push(
                                            QueryResultElement::PathKeyElementTrioResultItem((
                                                path_vec.iter().map(|p| p.to_vec()).collect(),
                                                subquery_path_last_key.to_vec(),
                                                cost_return_on_error!(
                                                    &mut cost,
                                                    Element::get_with_absolute_refs(
                                                        &subtree,
                                                        path_vec.as_slice(),
                                                        subquery_path_last_key.as_slice(),
                                                        allow_cache,
                                                        grove_version,
                                                    )
                                                ),
                                            )),
                                        );
                                    }
                                );
                            }
                        }
                    } else {
                        return Err(Error::CorruptedCodeExecution(
                            "subquery_paths can not be empty",
                        ))
                        .wrap_with_cost(cost);
                    };

                    if let Some(limit) = limit {
                        *limit -= 1;
                    }
                } else if let Some(offset) = offset {
                    *offset -= 1;
                }
            } else if allow_get_raw {
                cost_return_on_error_no_add!(
                    &cost,
                    Element::basic_push(
                        PathQueryPushArgs {
                            storage,
                            transaction,
                            key: Some(key),
                            element,
                            path,
                            subquery_path,
                            subquery,
                            left_to_right,
                            query_options,
                            result_type,
                            results,
                            limit,
                            offset,
                        },
                        grove_version
                    )
                );
            } else {
                return Err(Error::InvalidPath(
                    "you must provide a subquery or a subquery_path when interacting with a Tree \
                     of trees"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }
        } else {
            cost_return_on_error_no_add!(
                &cost,
                Element::basic_push(
                    PathQueryPushArgs {
                        storage,
                        transaction,
                        key,
                        element,
                        path,
                        subquery_path,
                        subquery,
                        left_to_right,
                        query_options,
                        result_type,
                        results,
                        limit,
                        offset,
                    },
                    grove_version
                )
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    /// Takes a sized query and a key and returns subquery key and subquery as
    /// tuple
    fn subquery_paths_and_value_for_sized_query(
        sized_query: &SizedQuery,
        key: &[u8],
    ) -> (Option<Path>, Option<Query>) {
        if let Some(conditional_subquery_branches) =
            &sized_query.query.conditional_subquery_branches
        {
            for (query_item, subquery_branch) in conditional_subquery_branches {
                if query_item.contains(key) {
                    let subquery_path = subquery_branch.subquery_path.clone();
                    let subquery = subquery_branch
                        .subquery
                        .as_ref()
                        .map(|query| *query.clone());
                    return (subquery_path, subquery);
                }
            }
        }
        let subquery_path = sized_query
            .query
            .default_subquery_branch
            .subquery_path
            .clone();
        let subquery = sized_query
            .query
            .default_subquery_branch
            .subquery
            .as_ref()
            .map(|query| *query.clone());
        (subquery_path, subquery)
    }

    /// `decrease_limit_on_range_with_no_sub_elements` should generally be set
    /// to true, as having it false could mean very expensive queries.
    /// The queries would be expensive because we could go through many many
    /// trees where the sub elements have no matches, hence the limit would
    /// not decrease and hence we would continue on the increasingly
    /// expensive query.
    #[cfg(feature = "full")]
    // TODO: refactor
    fn query_item(
        storage: &RocksDbStorage,
        item: &QueryItem,
        results: &mut Vec<QueryResultElement>,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        transaction: TransactionArg,
        limit: &mut Option<u16>,
        offset: &mut Option<u16>,
        query_options: QueryOptions,
        result_type: QueryResultType,
        add_element_function: fn(PathQueryPushArgs, &GroveVersion) -> CostResult<(), Error>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "query_item",
            grove_version.grovedb_versions.element.query_item
        );

        let mut cost = OperationCost::default();

        let subtree_path: SubtreePath<_> = path.into();

        if !item.is_range() {
            // this is a query on a key
            if let QueryItem::Key(key) = item {
                let element_res = merk_optional_tx_internal_error!(
                    &mut cost,
                    storage,
                    subtree_path,
                    None,
                    transaction,
                    subtree,
                    grove_version,
                    {
                        Element::get(&subtree, key, query_options.allow_cache, grove_version)
                            .unwrap_add_cost(&mut cost)
                    }
                );
                match element_res {
                    Ok(element) => {
                        let (subquery_path, subquery) =
                            Self::subquery_paths_and_value_for_sized_query(sized_query, key);
                        match add_element_function(
                            PathQueryPushArgs {
                                storage,
                                transaction,
                                key: Some(key.as_slice()),
                                element,
                                path,
                                subquery_path,
                                subquery,
                                left_to_right: sized_query.query.left_to_right,
                                query_options,
                                result_type,
                                results,
                                limit,
                                offset,
                            },
                            grove_version,
                        )
                        .unwrap_add_cost(&mut cost)
                        {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                if !query_options.error_if_intermediate_path_tree_not_present {
                                    match e {
                                        Error::PathParentLayerNotFound(_) => Ok(()),
                                        _ => Err(e),
                                    }
                                } else {
                                    Err(e)
                                }
                            }
                        }
                    }
                    Err(Error::PathKeyNotFound(_)) => Ok(()),
                    Err(e) => {
                        if !query_options.error_if_intermediate_path_tree_not_present {
                            match e {
                                Error::PathParentLayerNotFound(_) => Ok(()),
                                _ => Err(e),
                            }
                        } else {
                            Err(e)
                        }
                    }
                }
            } else {
                Err(Error::InternalError(
                    "QueryItem must be a Key if not a range".to_string(),
                ))
            }
        } else {
            // this is a query on a range
            storage_context_optional_tx!(storage, subtree_path, None, transaction, ctx, {
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
                                .expect("if key exists then value should too"),
                            grove_version
                        )
                    );
                    let key = iter
                        .key()
                        .unwrap_add_cost(&mut cost)
                        .expect("key should exist");
                    let (subquery_path, subquery) =
                        Self::subquery_paths_and_value_for_sized_query(sized_query, key);
                    let result_with_cost = add_element_function(
                        PathQueryPushArgs {
                            storage,
                            transaction,
                            key: Some(key),
                            element,
                            path,
                            subquery_path,
                            subquery,
                            left_to_right: sized_query.query.left_to_right,
                            query_options,
                            result_type,
                            results,
                            limit,
                            offset,
                        },
                        grove_version,
                    );
                    let result = result_with_cost.unwrap_add_cost(&mut cost);
                    match result {
                        Ok(x) => x,
                        Err(e) => {
                            if !query_options.error_if_intermediate_path_tree_not_present {
                                match e {
                                    Error::PathKeyNotFound(_)
                                    | Error::PathParentLayerNotFound(_) => (),
                                    _ => return Err(e).wrap_with_cost(cost),
                                }
                            } else {
                                return Err(e).wrap_with_cost(cost);
                            }
                        }
                    }
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

    #[cfg(feature = "full")]
    fn basic_push(args: PathQueryPushArgs, grove_version: &GroveVersion) -> Result<(), Error> {
        check_grovedb_v0!(
            "basic_push",
            grove_version.grovedb_versions.element.basic_push
        );

        // println!("basic_push {}", args);
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
                    let key = key.ok_or(Error::CorruptedPath(
                        "basic push must have a key".to_string(),
                    ))?;
                    results.push(QueryResultElement::KeyElementPairResultItem((
                        Vec::from(key),
                        element,
                    )));
                }
                QueryResultType::QueryPathKeyElementTrioResultType => {
                    let key = key.ok_or(Error::CorruptedPath(
                        "basic push must have a key".to_string(),
                    ))?;
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

    #[cfg(feature = "full")]
    /// Iterator
    pub fn iterator<I: RawIterator>(mut raw_iter: I) -> CostContext<ElementsIterator<I>> {
        let mut cost = OperationCost::default();
        raw_iter.seek_to_first().unwrap_add_cost(&mut cost);
        ElementsIterator::new(raw_iter).wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::Query;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::{
        element::{query::QueryOptions, *},
        query_result_type::{
            KeyElementPair, QueryResultElement, QueryResultElements,
            QueryResultType::{QueryKeyElementPairResultType, QueryPathKeyElementTrioResultType},
        },
        tests::{make_test_grovedb, TEST_LEAF},
        SizedQuery,
    };

    #[test]
    fn test_get_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        assert_eq!(
            Element::get_query_values(
                &db.db,
                &[TEST_LEAF],
                &query,
                QueryOptions::default(),
                None,
                grove_version
            )
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
            Element::get_query_values(
                &db.db,
                &[TEST_LEAF],
                &query,
                QueryOptions::default(),
                None,
                grove_version
            )
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
            Element::get_query_values(
                &db.db,
                &[TEST_LEAF],
                &query,
                QueryOptions::default(),
                None,
                grove_version
            )
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
            Element::get_query_values(
                &db.db,
                &[TEST_LEAF],
                &query,
                QueryOptions::default(),
                None,
                grove_version
            )
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
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
            grove_version,
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
                QueryOptions::default(),
                QueryPathKeyElementTrioResultType,
                None,
                grove_version
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
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let batch = StorageBatch::new();
        let storage = &db.db;
        let mut merk = db
            .open_non_transactional_merk_at_path(
                [TEST_LEAF].as_ref().into(),
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("cannot open Merk"); // TODO implement costs

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("expected successful batch commit");

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            storage,
            &[TEST_LEAF],
            &ascending_query,
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        let elements: Vec<KeyElementPair> = elements
            .into_iterator()
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
            storage,
            &[TEST_LEAF],
            &backwards_query,
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        let elements: Vec<KeyElementPair> = elements
            .into_iterator()
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
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let batch = StorageBatch::new();

        let storage = &db.db;
        let mut merk = db
            .open_non_transactional_merk_at_path(
                [TEST_LEAF].as_ref().into(),
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("cannot open Merk");

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("expected successful batch commit");

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
                storage,
                &[TEST_LEAF],
                &ascending_query,
                QueryOptions::default(),
                QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful get_query"),
            false,
        );

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(
                storage,
                &[TEST_LEAF],
                &backwards_query,
                QueryOptions::default(),
                QueryKeyElementPairResultType,
                None,
                grove_version,
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
                storage,
                &[TEST_LEAF],
                &backwards_query,
                QueryOptions::default(),
                QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful get_query"),
            true,
        );
    }

    #[test]
    fn test_get_limit_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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
            QueryOptions::default(),
            QueryKeyElementPairResultType,
            None,
            grove_version,
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

#[cfg(feature = "full")]
pub struct ElementsIterator<I: RawIterator> {
    raw_iter: I,
}

#[cfg(feature = "full")]
impl<I: RawIterator> ElementsIterator<I> {
    pub fn new(raw_iter: I) -> Self {
        ElementsIterator { raw_iter }
    }

    pub fn next_element(
        &mut self,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<KeyElementPair>, Error> {
        let mut cost = OperationCost::default();

        Ok(if self.raw_iter.valid().unwrap_add_cost(&mut cost) {
            if let Some((key, value)) = self
                .raw_iter
                .key()
                .unwrap_add_cost(&mut cost)
                .zip(self.raw_iter.value().unwrap_add_cost(&mut cost))
            {
                let element = cost_return_on_error_no_add!(&cost, raw_decode(value, grove_version));
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
