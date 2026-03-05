//! Query
//! Implements functions in Element for querying

use std::fmt;

use crate::element::SumValue;
#[cfg(feature = "minimal")]
use crate::operations::proof::util::hex_to_ascii;
use crate::operations::proof::util::path_as_slices_hex_to_ascii;
use crate::query_result_type::KeySumValuePair;
use crate::{AggregateSumPathQuery, Element};
#[cfg(feature = "minimal")]
use crate::{Error, TransactionArg};
#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_merk::element::decode::ElementDecodeExtensions;
#[cfg(feature = "minimal")]
use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
#[cfg(feature = "minimal")]
use grovedb_merk::error::MerkErrorExt;
#[cfg(feature = "minimal")]
use grovedb_merk::proofs::query::query_item::QueryItem;
use grovedb_merk::proofs::query::AggregateSumQuery;
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, StorageContext};
#[cfg(feature = "minimal")]
use grovedb_version::{check_grovedb_v0, check_grovedb_v0_with_cost, version::GroveVersion};

#[cfg(feature = "minimal")]
const MAX_AGGREGATE_REFERENCE_HOPS: usize = 3;

/// Result of an aggregate sum query, including metadata about whether the
/// query was truncated.
#[derive(Clone, Debug, PartialEq)]
pub struct AggregateSumQueryResult {
    /// The matching key-sum pairs returned by the query.
    pub results: Vec<KeySumValuePair>,
    /// True if the system hard limit on elements scanned was reached before
    /// the query completed naturally. When true, more results may exist
    /// beyond what was returned.
    pub hard_limit_reached: bool,
}

/// Options controlling how an aggregate sum query is executed.
#[derive(Copy, Clone, Debug)]
pub struct AggregateSumQueryOptions {
    /// If true, allows reading from cache instead of forcing fresh disk reads.
    pub allow_cache: bool,
    /// If true, returns an error when an intermediate path tree does not exist.
    /// When false, a missing intermediate tree is silently treated as empty.
    pub error_if_intermediate_path_tree_not_present: bool,
    /// If true (default), returns an error when a non-sum-item element is
    /// encountered (e.g. `Item`, `Tree`). When false, such elements are
    /// silently skipped without consuming a user limit slot.
    /// `ItemWithSumItem` elements are always processed regardless of this
    /// setting. References are handled separately by `ignore_references`.
    pub error_if_non_sum_item_found: bool,
    /// If true, silently skips `Reference` elements instead of following them.
    /// When false (default), references are followed up to 3 hops to resolve
    /// the target element.
    pub ignore_references: bool,
}

impl fmt::Display for AggregateSumQueryOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "AggregateSumQueryOptions {{")?;
        writeln!(f, "  allow_cache: {}", self.allow_cache)?;
        writeln!(
            f,
            "  error_if_intermediate_path_tree_not_present: {}",
            self.error_if_intermediate_path_tree_not_present
        )?;
        writeln!(
            f,
            "  error_if_non_sum_item_found: {}",
            self.error_if_non_sum_item_found
        )?;
        writeln!(f, "  ignore_references: {}", self.ignore_references)?;
        write!(f, "}}")
    }
}

impl Default for AggregateSumQueryOptions {
    fn default() -> Self {
        AggregateSumQueryOptions {
            allow_cache: true,
            error_if_intermediate_path_tree_not_present: true,
            error_if_non_sum_item_found: true,
            ignore_references: false,
        }
    }
}

#[cfg(feature = "minimal")]
/// Aggregate Sum Path query push arguments
#[allow(dead_code)]
pub struct AggregateSumPathQueryPushArgs<'db, 'ctx, 'a>
where
    'db: 'ctx,
{
    pub storage: &'db RocksDbStorage,
    pub transaction: TransactionArg<'db, 'ctx>,
    pub key: Option<&'a [u8]>,
    pub element: Element,
    pub path: &'a [&'a [u8]],
    pub left_to_right: bool,
    pub query_options: AggregateSumQueryOptions,
    pub results: &'a mut Vec<KeySumValuePair>,
    pub limit: &'a mut Option<u16>,
    pub sum_limit_left: &'a mut SumValue,
    pub elements_scanned: &'a mut u16,
    pub max_elements_scanned: u16,
}

#[cfg(feature = "minimal")]
impl<'db, 'ctx> fmt::Display for AggregateSumPathQueryPushArgs<'db, 'ctx, '_>
where
    'db: 'ctx,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "AggregateSumPathQueryPushArgs {{")?;
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
        writeln!(f, "  left_to_right: {}", self.left_to_right)?;
        writeln!(f, "  query_options: {}", self.query_options)?;
        writeln!(
            f,
            "  results: [{}]",
            self.results
                .iter()
                .map(|(key, value)| format!("0x{}: {}", hex::encode(key), value))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        writeln!(f, "  limit: {:?}", self.limit)?;
        writeln!(f, "  sum_limit: {}", self.sum_limit_left)?;
        writeln!(f, "  elements_scanned: {}", self.elements_scanned)?;
        writeln!(f, "  max_elements_scanned: {}", self.max_elements_scanned)?;
        write!(f, "}}")
    }
}

#[cfg(feature = "minimal")]
pub trait ElementAggregateSumQueryExtensions {
    fn get_aggregate_sum_query(
        storage: &RocksDbStorage,
        aggregate_sum_path_query: &AggregateSumPathQuery,
        query_options: AggregateSumQueryOptions,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AggregateSumQueryResult, Error>;
    fn get_aggregate_sum_query_apply_function(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        aggregate_sum_query: &AggregateSumQuery,
        query_options: AggregateSumQueryOptions,
        transaction: TransactionArg,
        add_element_function: fn(
            AggregateSumPathQueryPushArgs,
            &GroveVersion,
        ) -> CostResult<(), Error>,
        grove_version: &GroveVersion,
    ) -> CostResult<AggregateSumQueryResult, Error>;
    fn aggregate_sum_path_query_push(
        args: AggregateSumPathQueryPushArgs,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;
    fn aggregate_sum_query_item(
        storage: &RocksDbStorage,
        item: &QueryItem,
        results: &mut Vec<KeySumValuePair>,
        path: &[&[u8]],
        left_to_right: bool,
        transaction: TransactionArg,
        limit: &mut Option<u16>,
        sum_limit_left: &mut SumValue,
        query_options: AggregateSumQueryOptions,
        add_element_function: fn(
            AggregateSumPathQueryPushArgs,
            &GroveVersion,
        ) -> CostResult<(), Error>,
        elements_scanned: &mut u16,
        max_elements_scanned: u16,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;
    fn basic_aggregate_sum_push(
        args: AggregateSumPathQueryPushArgs,
        grove_version: &GroveVersion,
    ) -> Result<(), Error>;
}

#[cfg(feature = "minimal")]
impl ElementAggregateSumQueryExtensions for Element {
    /// Returns a vector of result elements based on given query
    fn get_aggregate_sum_query(
        storage: &RocksDbStorage,
        aggregate_sum_path_query: &AggregateSumPathQuery,
        query_options: AggregateSumQueryOptions,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AggregateSumQueryResult, Error> {
        check_grovedb_v0_with_cost!(
            "get_aggregate_sum_query",
            grove_version
                .grovedb_versions
                .element
                .get_aggregate_sum_query
        );

        let path_slices = aggregate_sum_path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_aggregate_sum_query_apply_function(
            storage,
            path_slices.as_slice(),
            &aggregate_sum_path_query.aggregate_sum_query,
            query_options,
            transaction,
            Element::aggregate_sum_path_query_push,
            grove_version,
        )
    }

    /// Returns a vector of result sum items with keys
    /// based on given aggregate sum query
    fn get_aggregate_sum_query_apply_function(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        aggregate_sum_query: &AggregateSumQuery,
        query_options: AggregateSumQueryOptions,
        transaction: TransactionArg,
        add_element_function: fn(
            AggregateSumPathQueryPushArgs,
            &GroveVersion,
        ) -> CostResult<(), Error>,
        grove_version: &GroveVersion,
    ) -> CostResult<AggregateSumQueryResult, Error> {
        check_grovedb_v0_with_cost!(
            "get_aggregate_sum_query_apply_function",
            grove_version
                .grovedb_versions
                .element
                .get_aggregate_sum_query_apply_function
        );

        let mut cost = OperationCost::default();

        let mut results = Vec::new();

        let mut limit = aggregate_sum_query.limit_of_items_to_check;

        let mut sum_limit: SumValue = cost_return_on_error_no_add!(
            cost,
            aggregate_sum_query
                .sum_limit
                .try_into()
                .map_err(|_| Error::Overflow("sum_limit exceeds i64::MAX"))
        );

        if sum_limit <= 0 || limit == Some(0) {
            return Ok(AggregateSumQueryResult {
                results,
                hard_limit_reached: false,
            })
            .wrap_with_cost(cost);
        }

        let mut elements_scanned: u16 = 0;
        let max_elements_scanned = grove_version
            .grovedb_versions
            .query_limits
            .max_aggregate_sum_query_elements_scanned;

        if aggregate_sum_query.left_to_right {
            for item in aggregate_sum_query.iter() {
                cost_return_on_error!(
                    &mut cost,
                    Self::aggregate_sum_query_item(
                        storage,
                        item,
                        &mut results,
                        path,
                        aggregate_sum_query.left_to_right,
                        transaction,
                        &mut limit,
                        &mut sum_limit,
                        query_options,
                        add_element_function,
                        &mut elements_scanned,
                        max_elements_scanned,
                        grove_version,
                    )
                );
                if sum_limit <= 0 {
                    break;
                }
                if limit == Some(0) {
                    break;
                }
                if elements_scanned > max_elements_scanned {
                    break;
                }
            }
        } else {
            for item in aggregate_sum_query.rev_iter() {
                cost_return_on_error!(
                    &mut cost,
                    Self::aggregate_sum_query_item(
                        storage,
                        item,
                        &mut results,
                        path,
                        aggregate_sum_query.left_to_right,
                        transaction,
                        &mut limit,
                        &mut sum_limit,
                        query_options,
                        add_element_function,
                        &mut elements_scanned,
                        max_elements_scanned,
                        grove_version,
                    )
                );
                if sum_limit <= 0 {
                    break;
                }
                if limit == Some(0) {
                    break;
                }
                if elements_scanned > max_elements_scanned {
                    break;
                }
            }
        }

        Ok(AggregateSumQueryResult {
            hard_limit_reached: elements_scanned > max_elements_scanned,
            results,
        })
        .wrap_with_cost(cost)
    }

    /// Push arguments to path query
    fn aggregate_sum_path_query_push(
        args: AggregateSumPathQueryPushArgs,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        use crate::reference_path::{path_from_reference_qualified_path_type, ReferencePathType};
        use crate::util::{compat, TxRef};

        check_grovedb_v0_with_cost!(
            "path_query_push",
            grove_version
                .grovedb_versions
                .element
                .aggregate_sum_path_query_push
        );

        let mut cost = OperationCost::default();

        if args.element.is_reference() {
            if args.query_options.ignore_references {
                // Silently skip references when ignore_references is enabled
                return Ok(()).wrap_with_cost(cost);
            }
            // Follow the reference chain up to MAX_AGGREGATE_REFERENCE_HOPS
            let element = args
                .element
                .convert_if_reference_to_absolute_reference(args.path, args.key);
            let element = cost_return_on_error_into_no_add!(cost, element);

            let Element::Reference(ref_path, _, _) = element else {
                return Err(Error::InternalError(
                    "expected a reference after conversion".to_string(),
                ))
                .wrap_with_cost(cost);
            };

            let mut current_qualified_path = match ref_path {
                ReferencePathType::AbsolutePathReference(path) => path,
                _ => {
                    return Err(Error::InternalError(
                        "expected absolute reference after conversion".to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            };

            let tx = TxRef::new(args.storage, args.transaction);
            let mut hops_left = MAX_AGGREGATE_REFERENCE_HOPS;

            loop {
                let Some((key, ref_path_slices)) = current_qualified_path.split_last() else {
                    return Err(Error::CorruptedData("empty reference path".to_string()))
                        .wrap_with_cost(cost);
                };

                let ref_path_refs: Vec<&[u8]> =
                    ref_path_slices.iter().map(|s| s.as_slice()).collect();
                let subtree_path: SubtreePath<_> = ref_path_refs.as_slice().into();

                let merk_res = compat::merk_optional_tx(
                    args.storage,
                    subtree_path,
                    tx.as_ref(),
                    None,
                    grove_version,
                );

                let merk = cost_return_on_error!(&mut cost, merk_res);

                let resolved = cost_return_on_error!(
                    &mut cost,
                    Element::get(&merk, key, args.query_options.allow_cache, grove_version)
                        .map_err(|e| e.into())
                );

                match resolved {
                    Element::Reference(next_ref_path, _, _) => {
                        if hops_left == 0 {
                            return Err(Error::ReferenceLimit).wrap_with_cost(cost);
                        }
                        hops_left -= 1;
                        current_qualified_path = cost_return_on_error_into_no_add!(
                            cost,
                            path_from_reference_qualified_path_type(
                                next_ref_path,
                                &current_qualified_path
                            )
                            .map_err(|e| Error::CorruptedData(
                                format!("failed to resolve reference path: {}", e)
                            ))
                        );
                    }
                    resolved_element => {
                        // We followed the reference to its target.
                        // Replace the element in args and process it.
                        let new_args = AggregateSumPathQueryPushArgs {
                            element: resolved_element,
                            ..args
                        };
                        if !new_args.element.is_sum_item() {
                            if !args.query_options.error_if_non_sum_item_found {
                                return Ok(()).wrap_with_cost(cost);
                            }
                            return Err(Error::InvalidPath(
                                "reference target is not a sum item".to_owned(),
                            ))
                            .wrap_with_cost(cost);
                        }
                        cost_return_on_error_no_add!(
                            cost,
                            Element::basic_aggregate_sum_push(new_args, grove_version)
                        );
                        return Ok(()).wrap_with_cost(cost);
                    }
                }
            }
        } else if !args.element.is_sum_item() {
            if !args.query_options.error_if_non_sum_item_found {
                // Silently skip non-sum, non-reference elements
                return Ok(()).wrap_with_cost(cost);
            }
            return Err(Error::InvalidPath(
                "we are only expecting sum items in this path".to_owned(),
            ))
            .wrap_with_cost(cost);
        } else {
            cost_return_on_error_no_add!(
                cost,
                Element::basic_aggregate_sum_push(args, grove_version)
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// `decrease_limit_on_range_with_no_sub_elements` should generally be set
    /// to true, as having it false could mean very expensive queries.
    /// The queries would be expensive because we could go through many many
    /// trees where the sub elements have no matches, hence the limit would
    /// not decrease and hence we would continue on the increasingly
    /// expensive query.
    fn aggregate_sum_query_item(
        storage: &RocksDbStorage,
        item: &QueryItem,
        results: &mut Vec<KeySumValuePair>,
        path: &[&[u8]],
        left_to_right: bool,
        transaction: TransactionArg,
        limit: &mut Option<u16>,
        sum_limit_left: &mut SumValue,
        query_options: AggregateSumQueryOptions,
        add_element_function: fn(
            AggregateSumPathQueryPushArgs,
            &GroveVersion,
        ) -> CostResult<(), Error>,
        elements_scanned: &mut u16,
        max_elements_scanned: u16,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        use grovedb_storage::Storage;

        use crate::util::{compat, TxRef};

        check_grovedb_v0_with_cost!(
            "aggregate_sum_query_item",
            grove_version
                .grovedb_versions
                .element
                .aggregate_sum_query_item
        );

        let mut cost = OperationCost::default();
        let tx = TxRef::new(storage, transaction);

        let subtree_path: SubtreePath<_> = path.into();

        if !item.is_range() {
            // this is a query on a key
            if let QueryItem::Key(key) = item {
                let subtree_res = compat::merk_optional_tx(
                    storage,
                    subtree_path,
                    tx.as_ref(),
                    None,
                    grove_version,
                );

                if subtree_res.value().is_err()
                    && !matches!(subtree_res.value(), Err(Error::PathParentLayerNotFound(..)))
                {
                    // simulating old macro's behavior by letting this particular kind of error to
                    // pass and to short circuit with the rest
                    return subtree_res.map_ok(|_| ());
                }

                let element_res = subtree_res
                    .flat_map_ok(|subtree| {
                        Element::get(&subtree, key, query_options.allow_cache, grove_version)
                            .add_context(format!("path is {}", path_as_slices_hex_to_ascii(path)))
                            .map_err(|e| e.into())
                    })
                    .unwrap_add_cost(&mut cost);

                match element_res {
                    Ok(element) => {
                        *elements_scanned = elements_scanned.saturating_add(1);
                        if *elements_scanned > max_elements_scanned {
                            return Ok(()).wrap_with_cost(cost);
                        }
                        // Check if we should skip this element type
                        if (!element.is_sum_item()
                            && !element.is_reference()
                            && !query_options.error_if_non_sum_item_found)
                            || (element.is_reference() && query_options.ignore_references)
                        {
                            return Ok(()).wrap_with_cost(cost);
                        }
                        match add_element_function(
                            AggregateSumPathQueryPushArgs {
                                storage,
                                transaction,
                                key: Some(key.as_slice()),
                                element,
                                path,
                                left_to_right,
                                query_options,
                                results,
                                limit,
                                sum_limit_left,
                                elements_scanned,
                                max_elements_scanned,
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
            let ctx = storage
                .get_transactional_storage_context(subtree_path, None, tx.as_ref())
                .unwrap_add_cost(&mut cost);

            let mut iter = ctx.raw_iter();

            item.seek_for_iter(&mut iter, left_to_right)
                .unwrap_add_cost(&mut cost);

            while item
                .iter_is_valid_for_type(&iter, *limit, Some(*sum_limit_left), left_to_right)
                .unwrap_add_cost(&mut cost)
            {
                let value_bytes = cost_return_on_error_no_add!(
                    cost,
                    iter.value()
                        .unwrap_add_cost(&mut cost)
                        .ok_or(Error::CorruptedData(
                            "expected iterator value but got None".to_string(),
                        ))
                );
                let element = cost_return_on_error_into_no_add!(
                    cost,
                    Element::raw_decode(value_bytes, grove_version)
                );
                *elements_scanned = elements_scanned.saturating_add(1);
                if *elements_scanned > max_elements_scanned {
                    break;
                }
                // Check if we should skip this element type
                if (!element.is_sum_item()
                    && !element.is_reference()
                    && !query_options.error_if_non_sum_item_found)
                    || (element.is_reference() && query_options.ignore_references)
                {
                    if left_to_right {
                        iter.next().unwrap_add_cost(&mut cost);
                    } else {
                        iter.prev().unwrap_add_cost(&mut cost);
                    }
                    cost.seek_count += 1;
                    continue;
                }
                let key = cost_return_on_error_no_add!(
                    cost,
                    iter.key()
                        .unwrap_add_cost(&mut cost)
                        .ok_or(Error::CorruptedData(
                            "expected iterator key but got None".to_string(),
                        ))
                );
                let result_with_cost = add_element_function(
                    AggregateSumPathQueryPushArgs {
                        storage,
                        transaction,
                        key: Some(key),
                        element,
                        path,
                        left_to_right,
                        query_options,
                        results,
                        limit,
                        sum_limit_left,
                        elements_scanned,
                        max_elements_scanned,
                    },
                    grove_version,
                );
                let result = result_with_cost.unwrap_add_cost(&mut cost);
                match result {
                    Ok(x) => x,
                    Err(e) => {
                        if !query_options.error_if_intermediate_path_tree_not_present {
                            match e {
                                Error::PathKeyNotFound(_) | Error::PathParentLayerNotFound(_) => (),
                                _ => return Err(e).wrap_with_cost(cost),
                            }
                        } else {
                            return Err(e).wrap_with_cost(cost);
                        }
                    }
                }
                if left_to_right {
                    iter.next().unwrap_add_cost(&mut cost);
                } else {
                    iter.prev().unwrap_add_cost(&mut cost);
                }
                cost.seek_count += 1;
            }
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    fn basic_aggregate_sum_push(
        args: AggregateSumPathQueryPushArgs,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "basic_aggregate_sum_push",
            grove_version
                .grovedb_versions
                .element
                .basic_aggregate_sum_push
        );

        let AggregateSumPathQueryPushArgs {
            path,
            key,
            element,
            results,
            limit,
            sum_limit_left,
            ..
        } = args;

        let element = element.convert_if_reference_to_absolute_reference(path, key)?;

        let value = match element {
            Element::SumItem(value, _) => value,
            Element::ItemWithSumItem(_, value, _) => value,
            _ => return Err(Error::InvalidInput("Only sum items are allowed")),
        };

        let key = key.ok_or(Error::CorruptedPath(
            "basic push must have a key".to_string(),
        ))?;
        results.push((key.to_vec(), value));
        if let Some(limit) = limit {
            *limit = limit.saturating_sub(1);
        }

        *sum_limit_left = sum_limit_left.saturating_sub(value);

        Ok(())
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests;
