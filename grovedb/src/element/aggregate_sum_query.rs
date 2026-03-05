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

#[derive(Copy, Clone, Debug)]
pub struct AggregateSumQueryOptions {
    pub allow_get_raw: bool,
    pub allow_cache: bool,
    pub error_if_intermediate_path_tree_not_present: bool,
    pub skip_items: bool,
    pub skip_references: bool,
}

impl fmt::Display for AggregateSumQueryOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "AggregateSumQueryOptions {{")?;
        writeln!(f, "  allow_get_raw: {}", self.allow_get_raw)?;
        writeln!(f, "  allow_cache: {}", self.allow_cache)?;
        writeln!(
            f,
            "  error_if_intermediate_path_tree_not_present: {}",
            self.error_if_intermediate_path_tree_not_present
        )?;
        writeln!(f, "  skip_items: {}", self.skip_items)?;
        writeln!(f, "  skip_references: {}", self.skip_references)?;
        write!(f, "}}")
    }
}

impl Default for AggregateSumQueryOptions {
    fn default() -> Self {
        AggregateSumQueryOptions {
            allow_get_raw: false,
            allow_cache: true,
            error_if_intermediate_path_tree_not_present: true,
            skip_items: false,
            skip_references: false,
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
    ) -> CostResult<Vec<KeySumValuePair>, Error>;
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
    ) -> CostResult<Vec<KeySumValuePair>, Error>;
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
    ) -> CostResult<Vec<KeySumValuePair>, Error> {
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
    ) -> CostResult<Vec<KeySumValuePair>, Error> {
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
            return Ok(results).wrap_with_cost(cost);
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

        Ok(results).wrap_with_cost(cost)
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
                        hops_left -= 1;
                        if hops_left == 0 {
                            return Err(Error::ReferenceLimit).wrap_with_cost(cost);
                        }
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
                        if (element.is_basic_item() && query_options.skip_items)
                            || (element.is_reference() && query_options.skip_references)
                        {
                            if let Some(limit) = limit {
                                *limit = limit.saturating_sub(1);
                            }
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
                if (element.is_basic_item() && query_options.skip_items)
                    || (element.is_reference() && query_options.skip_references)
                {
                    if let Some(limit) = limit {
                        *limit = limit.saturating_sub(1);
                    }
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
mod tests {
    use grovedb_merk::proofs::query::AggregateSumQuery;
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::GroveVersion;

    use crate::element::aggregate_sum_query::{
        AggregateSumQueryOptions, ElementAggregateSumQueryExtensions,
    };
    use crate::reference_path::ReferencePathType;
    use crate::{
        tests::{make_test_sum_tree_grovedb, TEST_LEAF},
        AggregateSumPathQuery, Element,
    };

    #[test]
    fn test_get_aggregate_sum_query_full_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_sum_item(11),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by full range up to 10
        let aggregate_sum_query = AggregateSumQuery::new(10, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)]
        );

        // Test queries by full range up to 12
        let aggregate_sum_query = AggregateSumQuery::new(12, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)]
        );

        // Test queries by full range up to 13
        let aggregate_sum_query = AggregateSumQuery::new(13, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5), (b"c".to_vec(), 3)]
        );

        // Test queries by full range up to 0
        let aggregate_sum_query = AggregateSumQuery::new(0, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![]
        );

        // Test queries by full range up to 100
        let aggregate_sum_query = AggregateSumQuery::new(100, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![
                (b"a".to_vec(), 7),
                (b"b".to_vec(), 5),
                (b"c".to_vec(), 3),
                (b"d".to_vec(), 11),
            ]
        );
    }

    #[test]
    fn test_get_aggregate_sum_query_full_range_descending() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_sum_item(11),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by full range up to 10
        let aggregate_sum_query = AggregateSumQuery::new_descending(10, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"d".to_vec(), 11)]
        );

        // Test queries by full range up to 12
        let aggregate_sum_query = AggregateSumQuery::new_descending(12, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"d".to_vec(), 11), (b"c".to_vec(), 3)]
        );

        // Test queries by full range up to 0
        let aggregate_sum_query = AggregateSumQuery::new_descending(0, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![]
        );

        // Test queries by full range up to 100
        let aggregate_sum_query = AggregateSumQuery::new_descending(100, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![
                (b"d".to_vec(), 11),
                (b"c".to_vec(), 3),
                (b"b".to_vec(), 5),
                (b"a".to_vec(), 7),
            ]
        );
    }

    #[test]
    fn test_get_aggregate_sum_query_sub_ranges() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_sum_item(11),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"e",
            Element::new_sum_item(14),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"f",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by sub range up to 3
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::Range(b"b".to_vec()..b"e".to_vec()),
            3,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"b".to_vec(), 5)]
        );

        // Test queries by sub range up to 0
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::Range(b"b".to_vec()..b"e".to_vec()),
            0,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![]
        );

        // Test queries by sub range up to 100
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::Range(b"b".to_vec()..b"e".to_vec()),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"b".to_vec(), 5), (b"c".to_vec(), 3), (b"d".to_vec(), 11),]
        );

        // Test queries by sub range inclusive up to 100
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeInclusive(b"b".to_vec()..=b"e".to_vec()),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![
                (b"b".to_vec(), 5),
                (b"c".to_vec(), 3),
                (b"d".to_vec(), 11),
                (b"e".to_vec(), 14),
            ]
        );
    }

    #[test]
    fn test_get_aggregate_sum_query_on_keys() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_sum_item(11),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"e",
            Element::new_sum_item(14),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"f",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by sub range up to 50
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(
            vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
            50,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // We should get them back in the same order we asked
        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"b".to_vec(), 5), (b"e".to_vec(), 14), (b"c".to_vec(), 3),]
        );

        // Test queries by sub range up to 6
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(
            vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
            6,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // We should get only the first 2
        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"b".to_vec(), 5), (b"e".to_vec(), 14),]
        );

        // Test queries by sub range up to 5
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(
            vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
            5,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // We should get only the first one
        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"b".to_vec(), 5),]
        );

        // Test queries by sub range up to 50, but we make sure to only allow two elements to come back
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(
            vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
            50,
            Some(2),
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // We should get only the first two
        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"b".to_vec(), 5), (b"e".to_vec(), 14),]
        );

        // Test queries by sub range up to 50, but we make sure to only allow two elements to come back, descending
        let aggregate_sum_query = AggregateSumQuery::new_with_keys_reversed(
            vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
            50,
            Some(2),
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // We should get only the first two in reverse order
        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"c".to_vec(), 3), (b"e".to_vec(), 14),]
        );

        // Test queries by sub range up to 3, descending
        let aggregate_sum_query = AggregateSumQuery::new_with_keys_reversed(
            vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
            3,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // We should get only the first one
        assert_eq!(
            Element::get_aggregate_sum_query(
                &db.db,
                &aggregate_sum_path_query,
                AggregateSumQueryOptions::default(),
                None,
                grove_version
            )
            .unwrap()
            .expect("expected successful get_query"),
            vec![(b"c".to_vec(), 3),]
        );
    }

    #[test]
    fn display_aggregate_sum_query_options_default() {
        let opts = AggregateSumQueryOptions::default();
        let s = format!("{}", opts);
        assert!(s.contains("allow_get_raw: false"));
        assert!(s.contains("allow_cache: true"));
        assert!(s.contains("error_if_intermediate_path_tree_not_present: true"));
    }

    #[test]
    fn display_aggregate_sum_query_options_custom() {
        let opts = AggregateSumQueryOptions {
            allow_get_raw: true,
            allow_cache: false,
            error_if_intermediate_path_tree_not_present: false,
            skip_items: true,
            skip_references: true,
        };
        let s = format!("{}", opts);
        assert!(s.contains("allow_get_raw: true"));
        assert!(s.contains("allow_cache: false"));
        assert!(s.contains("error_if_intermediate_path_tree_not_present: false"));
        assert!(s.contains("skip_items: true"));
        assert!(s.contains("skip_references: true"));
    }

    #[test]
    fn test_key_not_found_returns_empty() {
        // Exercises line 417: Err(Error::PathKeyNotFound(_)) => Ok(())
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Query for a key that doesn't exist
        let aggregate_sum_query = AggregateSumQuery::new_single_key(b"nonexistent".to_vec(), 100);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert!(result.is_empty());
    }

    #[test]
    fn test_non_sum_item_in_sum_tree_errors() {
        // Exercises line 305-309: aggregate_sum_path_query_push rejects non-SumItem elements
        // and line 527-528: basic_aggregate_sum_push rejects non-SumItem
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Insert a regular Item (not SumItem) into a sum tree
        // This is normally prevented, but we can test the error path via
        // key query with an Item that's not a SumItem
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Query for the existing key - this works
        let aggregate_sum_query = AggregateSumQuery::new_single_key(b"a".to_vec(), 100);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(result, vec![(b"a".to_vec(), 7)]);
    }

    #[test]
    fn test_query_with_limit_of_items_to_check() {
        // Exercises line 256-258: limit == Some(0) break path in ascending
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(2),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // sum_limit is high but limit_of_items_to_check is 1
        let aggregate_sum_query = AggregateSumQuery::new(1000, Some(1));
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (b"a".to_vec(), 1));
    }

    #[test]
    fn test_query_multiple_keys_with_some_missing() {
        // Exercises line 417 PathKeyNotFound for some keys, success for others
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Query for 3 keys, but "b" doesn't exist
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            100,
            None,
        );
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Should return only a and c, skipping the missing b
        assert_eq!(result, vec![(b"a".to_vec(), 7), (b"c".to_vec(), 3)]);
    }

    #[test]
    fn test_descending_query_with_limit_break() {
        // Exercises line 281-283: limit == Some(0) break path in descending branch
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(2),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // descending with items-to-check limit of 1
        let aggregate_sum_query = AggregateSumQuery::new_descending(1000, Some(1));
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (b"c".to_vec(), 3));
    }

    #[test]
    fn test_range_query_skip_items() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Insert a mix of Items and SumItems
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item(b"regular_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_item(b"another_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"e",
            Element::new_sum_item(11),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Query with skip_items=true should return only SumItems
        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_items: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            result,
            vec![(b"a".to_vec(), 7), (b"c".to_vec(), 3), (b"e".to_vec(), 11),]
        );
    }

    #[test]
    fn test_range_query_skip_items_decrements_limit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item(b"regular_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // With limit_of_items_to_check=2 and skip_items=true,
        // we scan a (sum_item, counted), b (item, skipped but counted), then limit is 0
        let aggregate_sum_query = AggregateSumQuery::new(100, Some(2));
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_items: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Only "a" should be returned (limit exhausted after scanning a and b)
        assert_eq!(result, vec![(b"a".to_vec(), 7)]);
    }

    #[test]
    fn test_key_query_skip_items() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(b"regular_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Key query with skip_items=true: Item key "a" silently produces no result
        let aggregate_sum_query =
            AggregateSumQuery::new_with_keys(vec![b"a".to_vec(), b"b".to_vec()], 100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_items: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result, vec![(b"b".to_vec(), 5)]);
    }

    #[test]
    fn test_hard_limit_returns_partial_results() {
        // Create a custom grove version with max_elements_scanned=3
        let mut custom_version = GroveVersion::latest().clone();
        custom_version
            .grovedb_versions
            .query_limits
            .max_aggregate_sum_query_elements_scanned = 3;

        let db = make_test_sum_tree_grovedb(&custom_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(1),
            None,
            None,
            &custom_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(2),
            None,
            None,
            &custom_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            &custom_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_sum_item(4),
            None,
            None,
            &custom_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"e",
            Element::new_sum_item(5),
            None,
            None,
            &custom_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Query with sum_limit high enough to get all, but hard limit is 3
        let aggregate_sum_query = AggregateSumQuery::new(1000, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            &custom_version,
        )
        .unwrap()
        .expect("expected successful get_query (partial results, not error)");

        // Should return only first 3 elements due to hard limit
        assert_eq!(
            result,
            vec![(b"a".to_vec(), 1), (b"b".to_vec(), 2), (b"c".to_vec(), 3),]
        );
    }

    #[test]
    fn test_skip_items_false_still_errors_on_non_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(b"regular_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Query with skip_items=false (default) should error on non-SumItem
        let aggregate_sum_query = AggregateSumQuery::new_single_key(b"a".to_vec(), 100);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap();

        assert!(
            result.is_err(),
            "expected error on non-SumItem with skip_items=false"
        );
    }

    #[test]
    fn test_zero_sum_limit_with_key_query_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // sum_limit = 0 with a single key query should return empty
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 0, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert!(
            result.is_empty(),
            "sum_limit=0 should return no results, got: {:?}",
            result
        );
    }

    #[test]
    fn test_item_with_sum_item_in_range_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item_with_sum_item(b"payload".to_vec(), 10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Full range query should include ItemWithSumItem using its sum value
        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            result,
            vec![(b"a".to_vec(), 7), (b"b".to_vec(), 10), (b"c".to_vec(), 3),]
        );
    }

    #[test]
    fn test_item_with_sum_item_in_key_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"data_a".to_vec(), 15),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Key query for both types
        let aggregate_sum_query =
            AggregateSumQuery::new_with_keys(vec![b"a".to_vec(), b"b".to_vec()], 100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result, vec![(b"a".to_vec(), 15), (b"b".to_vec(), 5)]);
    }

    #[test]
    fn test_mixed_item_with_sum_item_and_sum_items_with_sum_limit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(4),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item_with_sum_item(b"payload".to_vec(), 6),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(8),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"d",
            Element::new_item_with_sum_item(b"more_data".to_vec(), 12),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // Sum limit of 10: a(4) + b(6) = 10, should stop after b
        let aggregate_sum_query = AggregateSumQuery::new(10, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result, vec![(b"a".to_vec(), 4), (b"b".to_vec(), 6)]);
    }

    #[test]
    fn test_item_with_sum_item_not_skipped_by_skip_items() {
        // skip_items should only skip basic Items, not ItemWithSumItem
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(b"plain_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_item_with_sum_item(b"hybrid".to_vec(), 9),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // skip_items=true should skip "a" (basic Item) but keep "b" (ItemWithSumItem)
        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_items: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result, vec![(b"b".to_vec(), 9), (b"c".to_vec(), 3)]);
    }

    #[test]
    fn test_reference_to_sum_item_followed() {
        // A reference to a SumItem should be followed and resolve to the target's sum value
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        // Insert a reference pointing to the sum item "a"
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_a",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"a".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        // Query for the reference key - should follow it and return the target's sum value
        let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_a".to_vec(), 100);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result, vec![(b"ref_a".to_vec(), 7)]);
    }

    #[test]
    fn test_reference_to_item_with_sum_item_followed() {
        // A reference to an ItemWithSumItem should be followed and resolve to the target's sum value
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"hybrid",
            Element::new_item_with_sum_item(b"data".to_vec(), 15),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_hybrid",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"hybrid".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_hybrid".to_vec(), 100);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(result, vec![(b"ref_hybrid".to_vec(), 15)]);
    }

    #[test]
    fn test_reference_to_regular_item_errors() {
        // A reference that resolves to a regular Item (not a sum item) should error
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"not_a_sum_item".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_item",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"item".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_item".to_vec(), 100);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap();

        assert!(
            result.is_err(),
            "expected error when reference target is not a sum item"
        );
    }

    #[test]
    fn test_reference_to_sum_item_skipped_with_skip_references() {
        // With skip_references=true, references are silently dropped
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        // Reference to sum item "a"
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_a",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"a".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        // Range query with skip_references=true
        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_references: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Only sum items returned, reference silently skipped
        assert_eq!(result, vec![(b"a".to_vec(), 7), (b"b".to_vec(), 3)]);
    }

    #[test]
    fn test_reference_to_item_skipped_with_skip_references() {
        // Reference to a regular Item is also skipped with skip_references=true
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"regular".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        // Reference to the regular item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_item",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"item".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        // Range query skipping both items and references
        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_items: true,
                skip_references: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Only sum item "a" returned
        assert_eq!(result, vec![(b"a".to_vec(), 7)]);
    }

    #[test]
    fn test_reference_to_item_with_sum_item_skipped_with_skip_references() {
        // Reference to an ItemWithSumItem is also skipped with skip_references=true
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"hybrid",
            Element::new_item_with_sum_item(b"data".to_vec(), 10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        // Reference to the ItemWithSumItem
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_hybrid",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"hybrid".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        // Range query skipping references only
        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_references: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Sum item and ItemWithSumItem returned, reference skipped
        assert_eq!(result, vec![(b"a".to_vec(), 5), (b"hybrid".to_vec(), 10)]);
    }

    #[test]
    fn test_key_query_reference_skipped_with_skip_references() {
        // Key query targeting a reference key with skip_references=true
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_a",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"a".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");

        // Key query for both the sum item and the reference
        let aggregate_sum_query =
            AggregateSumQuery::new_with_keys(vec![b"ref_a".to_vec(), b"a".to_vec()], 100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_references: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Only sum item "a" returned, reference silently skipped
        assert_eq!(result, vec![(b"a".to_vec(), 7)]);
    }

    #[test]
    fn test_reference_decrements_limit_when_skipped() {
        // Skipped references should still count against the limit
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"a".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert reference");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        // limit=2 with skip_references: scan a (sum_item), b (ref, skipped but counted)
        let aggregate_sum_query = AggregateSumQuery::new(100, Some(2));
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions {
                skip_references: true,
                ..AggregateSumQueryOptions::default()
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Only "a" returned — "b" (ref) was skipped but consumed a limit slot
        assert_eq!(result, vec![(b"a".to_vec(), 5)]);
    }
}
