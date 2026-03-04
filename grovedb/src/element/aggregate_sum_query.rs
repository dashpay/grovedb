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
use grovedb_merk::proofs::aggregate_sum_query::AggregateSumQuery;
#[cfg(feature = "minimal")]
use grovedb_merk::proofs::query::query_item::QueryItem;
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, StorageContext};
#[cfg(feature = "minimal")]
use grovedb_version::{check_grovedb_v0, check_grovedb_v0_with_cost, version::GroveVersion};

#[derive(Copy, Clone, Debug)]
pub struct AggregateSumQueryOptions {
    pub allow_get_raw: bool,
    pub allow_cache: bool,
    pub error_if_intermediate_path_tree_not_present: bool,
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
        write!(f, "}}")
    }
}

impl Default for AggregateSumQueryOptions {
    fn default() -> Self {
        AggregateSumQueryOptions {
            allow_get_raw: false,
            allow_cache: true,
            error_if_intermediate_path_tree_not_present: true,
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

        let mut sum_limit = aggregate_sum_query.sum_limit as SumValue;

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
                        grove_version,
                    )
                );
                if sum_limit <= 0 {
                    break;
                }
                if limit == Some(0) {
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
                        grove_version,
                    )
                );
                if sum_limit <= 0 {
                    break;
                }
                if limit == Some(0) {
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
        check_grovedb_v0_with_cost!(
            "path_query_push",
            grove_version
                .grovedb_versions
                .element
                .aggregate_sum_path_query_push
        );

        let cost = OperationCost::default();

        if !args.element.is_sum_item() {
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
                let element = cost_return_on_error_into_no_add!(
                    cost,
                    Element::raw_decode(
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

        let Element::SumItem(value, _) = element else {
            return Err(Error::InvalidInput("Only sum items are allowed"));
        };

        let key = key.ok_or(Error::CorruptedPath(
            "basic push must have a key".to_string(),
        ))?;
        results.push((key.to_vec(), value));
        if let Some(limit) = limit {
            *limit -= 1;
        }

        *sum_limit_left -= value;

        Ok(())
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::aggregate_sum_query::AggregateSumQuery;
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::GroveVersion;

    use crate::element::aggregate_sum_query::{
        AggregateSumQueryOptions, ElementAggregateSumQueryExtensions,
    };
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

    // =========================================================================
    // Group A: Error paths
    // =========================================================================

    #[test]
    fn test_aggregate_sum_query_non_sum_item_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Insert a regular Item into the sum tree (allowed by insert, but
        // aggregate_sum_path_query_push should reject it)
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

        // Also insert a non-sum item — we use new_item with raw bytes
        // Actually, a sum tree only allows SumItem elements on insert,
        // so we query with a custom add_element_function to test the guard.
        // Instead, test via the aggregate_sum_path_query_push function directly.
        use crate::element::aggregate_sum_query::{
            AggregateSumPathQueryPushArgs, ElementAggregateSumQueryExtensions,
        };

        let mut results = Vec::new();
        let mut limit = Some(10u16);
        let mut sum_limit_left = 100i64;

        let path_slices: &[&[u8]] = &[TEST_LEAF];

        let result = Element::aggregate_sum_path_query_push(
            AggregateSumPathQueryPushArgs {
                storage: &db.db,
                transaction: None,
                key: Some(b"a"),
                element: Element::new_item(vec![1, 2, 3]),
                path: path_slices,
                left_to_right: true,
                query_options: AggregateSumQueryOptions::default(),
                results: &mut results,
                limit: &mut limit,
                sum_limit_left: &mut sum_limit_left,
            },
            grove_version,
        )
        .unwrap();

        assert!(result.is_err());
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(msg.contains("only expecting sum items"));
            }
            other => panic!("expected InvalidPath error, got {:?}", other),
        }
    }

    #[test]
    fn test_aggregate_sum_query_missing_key_silent_skip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Insert one element, then query a key that doesn't exist
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

        // Query for key "z" which doesn't exist — should return empty, not error
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(vec![b"z".to_vec()], 100, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query (empty)");

        assert!(results.is_empty());
    }

    #[test]
    fn test_aggregate_sum_query_missing_path_not_error() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Query a path that doesn't exist with error_if_intermediate_path_tree_not_present = false
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 100, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![b"nonexistent_path".to_vec()],
            aggregate_sum_query,
        };

        let options = AggregateSumQueryOptions {
            allow_get_raw: false,
            allow_cache: true,
            error_if_intermediate_path_tree_not_present: false,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            options,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query (empty, no error)");

        assert!(results.is_empty());
    }

    #[test]
    fn test_aggregate_sum_query_missing_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Query a path that doesn't exist with error_if_intermediate_path_tree_not_present = true
        let aggregate_sum_query = AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 100, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![b"nonexistent_path".to_vec()],
            aggregate_sum_query,
        };

        let options = AggregateSumQueryOptions {
            allow_get_raw: false,
            allow_cache: true,
            error_if_intermediate_path_tree_not_present: true,
        };

        let result = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            options,
            None,
            grove_version,
        )
        .unwrap();

        assert!(result.is_err(), "expected error for missing path");
    }

    // =========================================================================
    // Group B: Limit/Options combinations
    // =========================================================================

    #[test]
    fn test_aggregate_sum_query_item_limit_exhaustion() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        // Insert 4 items
        for (key, val) in [(b"a", 2), (b"b", 3), (b"c", 4), (b"d", 5)] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_sum_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("cannot insert element");
        }

        // Query with limit_of_items_to_check = Some(2) and high sum_limit
        // Should stop after checking 2 items even though sum_limit not reached
        let aggregate_sum_query = AggregateSumQuery::new(1000, Some(2));

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0], (b"a".to_vec(), 2));
        assert_eq!(results[1], (b"b".to_vec(), 3));
    }

    #[test]
    fn test_aggregate_sum_query_range_descending() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        for (key, val) in [(b"a", 2), (b"b", 3), (b"c", 4), (b"d", 5)] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_sum_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("cannot insert element");
        }

        // Range query (not RangeFull) with left_to_right: false
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item_with_direction(
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
            false,
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        // Should be in reverse order: d, c, b
        assert_eq!(
            results,
            vec![(b"d".to_vec(), 5), (b"c".to_vec(), 4), (b"b".to_vec(), 3),]
        );
    }

    #[test]
    fn test_aggregate_sum_query_allow_cache_false() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");

        let aggregate_sum_query = AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 100, None);

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let options = AggregateSumQueryOptions {
            allow_get_raw: false,
            allow_cache: false,
            error_if_intermediate_path_tree_not_present: true,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            options,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(results, vec![(b"a".to_vec(), 10)]);
    }

    // =========================================================================
    // Group E: Range type variants
    // =========================================================================

    /// Helper: set up a sum tree with keys a..f and known values
    fn setup_sum_tree_a_to_f(grove_version: &GroveVersion) -> crate::tests::TempGroveDb {
        let db = make_test_sum_tree_grovedb(grove_version);
        for (key, val) in [
            (b"a" as &[u8], 2),
            (b"b", 3),
            (b"c", 4),
            (b"d", 5),
            (b"e", 6),
            (b"f", 7),
        ] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_sum_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("cannot insert element");
        }
        db
    }

    #[test]
    fn test_aggregate_sum_query_range_from() {
        let grove_version = GroveVersion::latest();
        let db = setup_sum_tree_a_to_f(grove_version);

        // RangeFrom(b"c"..) — c, d, e, f
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeFrom(b"c".to_vec()..),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            results,
            vec![
                (b"c".to_vec(), 4),
                (b"d".to_vec(), 5),
                (b"e".to_vec(), 6),
                (b"f".to_vec(), 7),
            ]
        );
    }

    #[test]
    fn test_aggregate_sum_query_range_to() {
        let grove_version = GroveVersion::latest();
        let db = setup_sum_tree_a_to_f(grove_version);

        // RangeTo(..b"d") — a, b, c
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeTo(..b"d".to_vec()),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            results,
            vec![(b"a".to_vec(), 2), (b"b".to_vec(), 3), (b"c".to_vec(), 4),]
        );
    }

    #[test]
    fn test_aggregate_sum_query_range_to_inclusive() {
        let grove_version = GroveVersion::latest();
        let db = setup_sum_tree_a_to_f(grove_version);

        // RangeToInclusive(..=b"d") — a, b, c, d
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeToInclusive(..=b"d".to_vec()),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            results,
            vec![
                (b"a".to_vec(), 2),
                (b"b".to_vec(), 3),
                (b"c".to_vec(), 4),
                (b"d".to_vec(), 5),
            ]
        );
    }

    #[test]
    fn test_aggregate_sum_query_range_after() {
        let grove_version = GroveVersion::latest();
        let db = setup_sum_tree_a_to_f(grove_version);

        // RangeAfter(b"b"..) — exclusive start, so c, d, e, f
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeAfter(b"b".to_vec()..),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            results,
            vec![
                (b"c".to_vec(), 4),
                (b"d".to_vec(), 5),
                (b"e".to_vec(), 6),
                (b"f".to_vec(), 7),
            ]
        );
    }

    #[test]
    fn test_aggregate_sum_query_range_after_to() {
        let grove_version = GroveVersion::latest();
        let db = setup_sum_tree_a_to_f(grove_version);

        // RangeAfterTo(b"a"..b"d") — exclusive start a, exclusive end d → b, c
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeAfterTo(b"a".to_vec()..b"d".to_vec()),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(results, vec![(b"b".to_vec(), 3), (b"c".to_vec(), 4),]);
    }

    #[test]
    fn test_aggregate_sum_query_range_after_to_inclusive() {
        let grove_version = GroveVersion::latest();
        let db = setup_sum_tree_a_to_f(grove_version);

        // RangeAfterToInclusive(b"a"..=b"d") — exclusive start a, inclusive end d → b, c, d
        let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
            QueryItem::RangeAfterToInclusive(b"a".to_vec()..=b"d".to_vec()),
            100,
            None,
        );

        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let results = Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful get_query");

        assert_eq!(
            results,
            vec![(b"b".to_vec(), 3), (b"c".to_vec(), 4), (b"d".to_vec(), 5),]
        );
    }
}
