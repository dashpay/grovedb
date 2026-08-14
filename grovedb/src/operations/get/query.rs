//! Query operations

#[cfg(feature = "minimal")]
use crate::element::SumValue;
use crate::{
    element::{
        aggregate_sum_query::{
            AggregateSumQueryOptions, AggregateSumQueryResult, ElementAggregateSumQueryExtensions,
        },
        query::ElementQueryExtensions,
        query_options::QueryOptions,
        BigSumValue, CountValue,
    },
    operations::proof::ProveOptions,
    query_result_type::PathKeyOptionalElementTrio,
    AggregateSumPathQuery,
};
#[cfg(feature = "minimal")]
use crate::{
    query_result_type::{QueryResultElement, QueryResultElements, QueryResultType},
    reference_path::ReferencePathType,
    util::TxRef,
    Element, Error, GroveDb, PathQuery, SizedQuery, TransactionArg,
};
use grovedb_costs::cost_return_on_error_default;
#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
use grovedb_version::{check_grovedb_v0, check_grovedb_v0_with_cost, version::GroveVersion};
#[cfg(feature = "minimal")]
use integer_encoding::VarInt;

#[cfg(feature = "minimal")]
#[derive(Debug, Eq, PartialEq, Clone)]
/// A return type for query_item_value_or_sum
pub enum QueryItemOrSumReturnType {
    /// an Item in serialized form
    ItemData(Vec<u8>),
    /// A sum item or a sum tree value
    SumValue(SumValue),
    /// A big sum tree value
    BigSumValue(BigSumValue),
    /// A count value
    CountValue(CountValue),
    /// A count and sum value
    CountSumValue(CountValue, SumValue),
    /// an Item in serialized form with a Sum Value
    ItemDataWithSumValue(Vec<u8>, SumValue),
}

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Encoded query for multiple path queries
    #[deprecated]
    pub fn query_encoded_many(
        &self,
        path_queries: &[&PathQuery],
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Vec<u8>>, Error> {
        check_grovedb_v0_with_cost!(
            "query_encoded_many",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_encoded_many
        );

        let mut cost = OperationCost::default();

        let elements = cost_return_on_error!(
            &mut cost,
            self.query_many_raw(
                path_queries,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                QueryResultType::QueryElementResultType,
                transaction,
                grove_version
            )
        );
        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => {
                    // Look through `NonCounted` so a wrapped reference still
                    // resolves; the wrapper is transparent at the query
                    // layer.
                    match element.into_underlying() {
                        Element::Reference(reference_path, ..)
                        | Element::ReferenceWithSumItem(reference_path, ..) => match reference_path
                        {
                            ReferencePathType::AbsolutePathReference(absolute_path) => {
                                // While `map` on iterator is lazy, we should accumulate costs
                                // even if `collect` will end in `Err`, so we'll use
                                // external costs accumulator instead of
                                // returning costs from `map` call.
                                let maybe_item = self
                                    .follow_reference(
                                        absolute_path.as_slice().into(),
                                        allow_cache,
                                        transaction,
                                        grove_version,
                                    )
                                    .unwrap_add_cost(&mut cost)?;

                                // Same treatment for the resolved value.
                                match maybe_item.into_underlying() {
                                    Element::Item(item, _) => Ok(item),
                                    Element::ItemWithSumItem(item, ..) => Ok(item),
                                    Element::SumItem(value, _) => Ok(value.encode_var_vec()),
                                    _ => Err(Error::InvalidQuery(
                                        "the reference must result in an item",
                                    )),
                                }
                            }
                            _ => Err(Error::CorruptedCodeExecution(
                                "reference after query must have absolute paths",
                            )),
                        },
                        _ => Err(Error::InvalidQuery(
                            "path_queries can only refer to references",
                        )),
                    }
                }
                _ => Err(Error::InvalidQuery(
                    "path_queries can only refer to references",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>();

        results_wrapped.wrap_with_cost(cost)
    }

    /// Raw query for multiple path queries
    pub fn query_many_raw(
        &self,
        path_queries: &[&PathQuery],
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<QueryResultElements, Error>
where {
        check_grovedb_v0_with_cost!(
            "query_many_raw",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_many_raw
        );
        let mut cost = OperationCost::default();

        let query = cost_return_on_error_no_add!(
            cost,
            PathQuery::merge(path_queries.to_vec(), grove_version)
        );
        let (result, _) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                &query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                result_type,
                transaction,
                grove_version
            )
        );
        Ok(result).wrap_with_cost(cost)
    }

    /// Prove a path query as either verbose or non-verbose
    pub fn get_proved_path_query(
        &self,
        path_query: &PathQuery,
        prove_options: Option<ProveOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        check_grovedb_v0_with_cost!(
            "get_proved_path_query",
            grove_version
                .grovedb_versions
                .operations
                .query
                .get_proved_path_query
        );
        if transaction.is_some() {
            Err(Error::NotSupported(
                "transactions are not currently supported".to_string(),
            ))
            .wrap_with_cost(Default::default())
        } else {
            self.prove_query(path_query, prove_options, grove_version)
        }
    }

    fn follow_element(
        &self,
        element: Element,
        allow_cache: bool,
        cost: &mut OperationCost,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> Result<Element, Error> {
        check_grovedb_v0!(
            "follow_element",
            grove_version
                .grovedb_versions
                .operations
                .query
                .follow_element
        );
        // Look through NonCounted: a NonCounted-wrapped reference still
        // resolves; a NonCounted item still returns itself. The wrapper's
        // sole effect is on parent count aggregation.
        let element = element.into_underlying();
        match element {
            Element::Reference(reference_path, ..)
            | Element::ReferenceWithSumItem(reference_path, ..) => {
                match reference_path {
                    ReferencePathType::AbsolutePathReference(absolute_path) => {
                        // While `map` on iterator is lazy, we should accumulate costs
                        // even if `collect` will
                        // end in `Err`, so we'll use
                        // external costs accumulator instead of
                        // returning costs from `map` call.
                        // Normalize the resolved value too, so a Reference
                        // pointing at NonCounted(Item) returns the same shape
                        // as a directly-queried NonCounted(Item).
                        // `ReferenceWithSumItem` follows the same resolution
                        // path; the sum carried on the source element does
                        // not affect what `follow_reference` returns.
                        let maybe_item = self
                            .follow_reference(
                                absolute_path.as_slice().into(),
                                allow_cache,
                                transaction,
                                grove_version,
                            )
                            .unwrap_add_cost(cost)?
                            .into_underlying();

                        if maybe_item.is_any_item() {
                            Ok(maybe_item)
                        } else {
                            Err(Error::InvalidQuery("the reference must result in an item"))
                        }
                    }
                    _ => Err(Error::CorruptedCodeExecution(
                        "reference after query must have absolute paths",
                    )),
                }
            }
            Element::Item(..)
            | Element::SumItem(..)
            | Element::ItemWithSumItem(..)
            | Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountTree(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountTree(..)
            | Element::ProvableCountSumTree(..)
            | Element::ProvableSumTree(..)
            | Element::ProvableCountProvableSumTree(..) => Ok(element),
            Element::Tree(..)
            | Element::CommitmentTree(..)
            | Element::MmrTree(..)
            | Element::BulkAppendTree(..)
            | Element::DenseAppendOnlyFixedSizeTree(..)
            | Element::ProvableSumIndexedTree(..)
            | Element::ProvableCountProvableSumIndexedTree(..)
            | Element::ProvableCountIndexedTree(..) => {
                Err(Error::InvalidQuery("path_queries can not refer to trees"))
            }
            Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_) => {
                unreachable!("unwrapped above")
            }
        }
    }

    /// Returns the result set after applying a path query
    pub fn query(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        check_grovedb_v0_with_cost!(
            "query",
            grove_version.grovedb_versions.operations.query.query
        );
        let mut cost = OperationCost::default();

        let (elements, skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                result_type,
                transaction,
                grove_version
            )
        );

        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| {
                result_item.map_element(|element| {
                    self.follow_element(element, allow_cache, &mut cost, transaction, grove_version)
                })
            })
            .collect::<Result<Vec<QueryResultElement>, Error>>();

        let results = cost_return_on_error_no_add!(cost, results_wrapped);
        Ok((QueryResultElements { elements: results }, skipped)).wrap_with_cost(cost)
    }

    /// Queries the backing store and returns element items by their value,
    /// Sum Items are encoded as var vec
    pub fn query_item_value(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(Vec<Vec<u8>>, u16), Error> {
        check_grovedb_v0_with_cost!(
            "query_item_value",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_item_value
        );
        let mut cost = OperationCost::default();

        let (elements, skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                QueryResultType::QueryElementResultType,
                transaction,
                grove_version
            )
        );

        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => {
                    // NonCounted is transparent at this layer.
                    let element = element.into_underlying();
                    match element {
                        // `ReferenceWithSumItem` resolves to the target item
                        // the same way `Reference` does; the carried sum is
                        // ignored at this layer (use `query_item_value_or_sum`
                        // to see it).
                        Element::Reference(reference_path, ..)
                        | Element::ReferenceWithSumItem(reference_path, ..) => {
                            match reference_path {
                                ReferencePathType::AbsolutePathReference(absolute_path) => {
                                    // While `map` on iterator is lazy, we should accumulate costs
                                    // even if `collect` will
                                    // end in `Err`, so we'll use
                                    // external costs accumulator instead of
                                    // returning costs from `map` call.
                                    let maybe_item = self
                                        .follow_reference(
                                            absolute_path.as_slice().into(),
                                            allow_cache,
                                            transaction,
                                            grove_version,
                                        )
                                        .unwrap_add_cost(&mut cost)?;

                                    match maybe_item.into_underlying() {
                                        Element::Item(item, _)
                                        | Element::ItemWithSumItem(item, ..) => Ok(item),
                                        Element::SumItem(item, _) => Ok(item.encode_var_vec()),
                                        _ => Err(Error::InvalidQuery(
                                            "the reference must result in an item",
                                        )),
                                    }
                                }
                                _ => Err(Error::CorruptedCodeExecution(
                                    "reference after query must have absolute paths",
                                )),
                            }
                        }
                        Element::Item(item, _) | Element::ItemWithSumItem(item, ..) => Ok(item),
                        Element::SumItem(item, _) => Ok(item.encode_var_vec()),
                        Element::Tree(..)
                        | Element::SumTree(..)
                        | Element::BigSumTree(..)
                        | Element::CountTree(..)
                        | Element::CountSumTree(..)
                        | Element::ProvableCountTree(..)
                        | Element::ProvableCountSumTree(..)
                        | Element::ProvableSumTree(..)
                        | Element::ProvableCountProvableSumTree(..)
                        | Element::CommitmentTree(..)
                        | Element::MmrTree(..)
                        | Element::BulkAppendTree(..)
                        | Element::DenseAppendOnlyFixedSizeTree(..)
                        | Element::ProvableSumIndexedTree(..)
                        | Element::ProvableCountProvableSumIndexedTree(..)
                        | Element::ProvableCountIndexedTree(..) => Err(Error::InvalidQuery(
                            "path_queries can only refer to items and references",
                        )),
                        Element::NonCounted(_)
                        | Element::NotSummed(_)
                        | Element::NotCountedOrSummed(_) => {
                            unreachable!("unwrapped above")
                        }
                    }
                }
                _ => Err(Error::CorruptedCodeExecution(
                    "query returned incorrect result type",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>();

        let results = cost_return_on_error_no_add!(cost, results_wrapped);
        Ok((results, skipped)).wrap_with_cost(cost)
    }

    /// Queries the backing store and returns element items by their value,
    /// Sum Items are returned
    pub fn query_item_value_or_sum(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(Vec<QueryItemOrSumReturnType>, u16), Error> {
        check_grovedb_v0_with_cost!(
            "query_item_value_or_sum",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_item_value_or_sum
        );
        let mut cost = OperationCost::default();

        let (elements, skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                QueryResultType::QueryElementResultType,
                transaction,
                grove_version
            )
        );

        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => {
                    // NonCounted is transparent at this layer.
                    let element = element.into_underlying();
                    match element {
                        // `ReferenceWithSumItem` resolves to the target item
                        // exactly like `Reference`; the carried sum value
                        // does not show up here (it's an aggregate-only
                        // property that propagates to the parent tree).
                        Element::Reference(reference_path, ..)
                        | Element::ReferenceWithSumItem(reference_path, ..) => {
                            match reference_path {
                                ReferencePathType::AbsolutePathReference(absolute_path) => {
                                    // While `map` on iterator is lazy, we should accumulate costs
                                    // even if `collect` will
                                    // end in `Err`, so we'll use
                                    // external costs accumulator instead of
                                    // returning costs from `map` call.
                                    let maybe_item = self
                                        .follow_reference(
                                            absolute_path.as_slice().into(),
                                            allow_cache,
                                            transaction,
                                            grove_version,
                                        )
                                        .unwrap_add_cost(&mut cost)?;

                                    match maybe_item.into_underlying() {
                                        Element::Item(item, _) => {
                                            Ok(QueryItemOrSumReturnType::ItemData(item))
                                        }
                                        Element::SumItem(sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                                        }
                                        Element::ItemWithSumItem(item, sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::ItemDataWithSumValue(
                                                item, sum_value,
                                            ))
                                        }
                                        Element::SumTree(_, sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                                        }
                                        Element::BigSumTree(_, big_sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::BigSumValue(big_sum_value))
                                        }
                                        Element::CountTree(_, count_value, _) => {
                                            Ok(QueryItemOrSumReturnType::CountValue(count_value))
                                        }
                                        Element::CountSumTree(_, count_value, sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::CountSumValue(
                                                count_value,
                                                sum_value,
                                            ))
                                        }
                                        Element::ProvableCountTree(_, count_value, _) => {
                                            Ok(QueryItemOrSumReturnType::CountValue(count_value))
                                        }
                                        Element::ProvableCountSumTree(
                                            _,
                                            count_value,
                                            sum_value,
                                            _,
                                        ) => Ok(QueryItemOrSumReturnType::CountSumValue(
                                            count_value,
                                            sum_value,
                                        )),
                                        Element::ProvableSumTree(_, sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                                        }
                                        Element::ProvableCountProvableSumTree(
                                            _,
                                            count_value,
                                            sum_value,
                                            _,
                                        ) => Ok(QueryItemOrSumReturnType::CountSumValue(
                                            count_value,
                                            sum_value,
                                        )),
                                        Element::ProvableCountIndexedTree(.., count_value, _) => {
                                            Ok(QueryItemOrSumReturnType::CountValue(count_value))
                                        }
                                        Element::ProvableSumIndexedTree(_, _, sum_value, _) => {
                                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                                        }
                                        Element::ProvableCountProvableSumIndexedTree(
                                            _,
                                            count_value,
                                            sum_value,
                                            _,
                                            _,
                                        ) => Ok(QueryItemOrSumReturnType::CountSumValue(
                                            count_value,
                                            sum_value,
                                        )),
                                        _ => Err(Error::InvalidQuery(
                                            "the reference must result in an item",
                                        )),
                                    }
                                }
                                _ => Err(Error::CorruptedCodeExecution(
                                    "reference after query must have absolute paths",
                                )),
                            }
                        }
                        Element::Item(item, _) => Ok(QueryItemOrSumReturnType::ItemData(item)),
                        Element::SumItem(sum_value, _) => {
                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                        }
                        Element::ItemWithSumItem(item, sum_value, _) => Ok(
                            QueryItemOrSumReturnType::ItemDataWithSumValue(item, sum_value),
                        ),
                        Element::SumTree(_, sum_value, _) => {
                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                        }
                        Element::BigSumTree(_, big_sum_value, _) => {
                            Ok(QueryItemOrSumReturnType::BigSumValue(big_sum_value))
                        }
                        Element::CountTree(_, count_value, _) => {
                            Ok(QueryItemOrSumReturnType::CountValue(count_value))
                        }
                        Element::CountSumTree(_, count_value, sum_value, _) => Ok(
                            QueryItemOrSumReturnType::CountSumValue(count_value, sum_value),
                        ),
                        Element::ProvableCountTree(_, count_value, _) => {
                            Ok(QueryItemOrSumReturnType::CountValue(count_value))
                        }
                        Element::ProvableCountSumTree(_, count_value, sum_value, _) => Ok(
                            QueryItemOrSumReturnType::CountSumValue(count_value, sum_value),
                        ),
                        Element::ProvableSumTree(_, sum_value, _) => {
                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                        }
                        Element::ProvableCountProvableSumTree(_, count_value, sum_value, _) => Ok(
                            QueryItemOrSumReturnType::CountSumValue(count_value, sum_value),
                        ),
                        Element::ProvableCountIndexedTree(.., count_value, _) => {
                            Ok(QueryItemOrSumReturnType::CountValue(count_value))
                        }
                        Element::ProvableSumIndexedTree(_, _, sum_value, _) => {
                            Ok(QueryItemOrSumReturnType::SumValue(sum_value))
                        }
                        Element::ProvableCountProvableSumIndexedTree(
                            _,
                            count_value,
                            sum_value,
                            _,
                            _,
                        ) => Ok(QueryItemOrSumReturnType::CountSumValue(
                            count_value,
                            sum_value,
                        )),
                        Element::Tree(..)
                        | Element::CommitmentTree(..)
                        | Element::MmrTree(..)
                        | Element::BulkAppendTree(..)
                        | Element::DenseAppendOnlyFixedSizeTree(..) => Err(Error::InvalidQuery(
                            "path_queries can only refer to items, sum items, references and sum \
                             trees",
                        )),
                        Element::NonCounted(_)
                        | Element::NotSummed(_)
                        | Element::NotCountedOrSummed(_) => {
                            unreachable!("unwrapped above")
                        }
                    }
                }
                _ => Err(Error::CorruptedCodeExecution(
                    "query returned incorrect result type",
                )),
            })
            .collect::<Result<Vec<QueryItemOrSumReturnType>, Error>>();

        let results = cost_return_on_error_no_add!(cost, results_wrapped);
        Ok((results, skipped)).wrap_with_cost(cost)
    }

    /// Execute an `AggregateSumOnRange` path query without producing a
    /// proof, returning the in-range signed sum directly.
    ///
    /// This is the no-proof counterpart of
    /// [`Self::prove_query`] +
    /// [`Self::verify_aggregate_sum_query`](GroveDb::verify_aggregate_sum_query)
    /// for `AggregateSumOnRange` queries: it performs the same merk-level
    /// boundary walk the prover does (using each internal node's stored
    /// aggregate sum to short-circuit Contained / Disjoint subtrees) but
    /// skips proof generation, serialization, and verification entirely.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_leaf_aggregate_sum_on_range`] — strictly the
    /// **leaf** shape: a single `AggregateSumOnRange(_)` item, no
    /// subqueries, no pagination, a non-empty path, and an inner range
    /// that isn't `Key`, `RangeFull`, or another aggregate variant.
    /// Carrier-shape queries (outer `Keys` + `AggregateSumOnRange`
    /// subquery) are rejected here because this entry point returns one
    /// `i64` and has no way to surface per-outer-key sums; use
    /// [`Self::prove_query`] +
    /// [`Self::verify_aggregate_sum_query_per_key`](GroveDb::verify_aggregate_sum_query_per_key)
    /// for those. Any other shape is rejected up front with
    /// `Error::InvalidQuery` before any merk reads happen.
    ///
    /// The subtree at `path_query.path` must be a `ProvableSumTree` — the
    /// merk-level walk rejects any other tree type. If the subtree is
    /// missing (path does not resolve), this returns the same
    /// `PathNotFound` / `PathParentLayerNotFound` errors as other
    /// path-based reads.
    ///
    /// Sum-side mirror of [`Self::query_aggregate_count`] for the
    /// signed-sum axis.
    ///
    /// The returned sum is **not** independently verifiable — callers are
    /// trusting their own merk read path. For a verifiable sum, use
    /// [`Self::prove_query`] +
    /// [`Self::verify_aggregate_sum_query`](GroveDb::verify_aggregate_sum_query).
    pub fn query_aggregate_sum(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<i64, Error> {
        check_grovedb_v0_with_cost!(
            "query_aggregate_sum",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_sum_on_range
        );

        let mut cost = OperationCost::default();

        // Up-front shape validation. Strictly the leaf shape — this entry
        // point returns a single `i64` and has no way to surface
        // per-outer-key carrier results. Catches malformed leaf
        // aggregate-sum queries (illegal inner range, pagination, etc.)
        // AND carrier-shape queries before any storage reads. Mirrors
        // `query_aggregate_count`'s use of the strict-leaf validator.
        let inner_range = cost_return_on_error_no_add!(
            cost,
            path_query.validate_leaf_aggregate_sum_on_range().cloned()
        );

        let tx = TxRef::new(&self.db, transaction);

        // Open the leaf merk and ask it for the sum. The merk-level entry
        // point enforces `tree_type == ProvableSumTree` and handles the
        // empty-merk case (returns 0).
        let path_slices: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();
        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                SubtreePath::from(path_slices.as_slice()),
                tx.as_ref(),
                None,
                grove_version,
            )
        );

        let sum = cost_return_on_error!(
            &mut cost,
            subtree
                .sum_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "query_aggregate_sum at path {:?}: {}",
                    path_slices, e
                )))
        );

        Ok(sum).wrap_with_cost(cost)
    }

    /// Retrieves SumItem values using an [`AggregateSumPathQuery`] with
    /// budget-limited scanning (max elements scanned is capped by
    /// [`GroveDBQueryLimits::max_aggregate_sum_query_elements_scanned`]).
    ///
    /// Returns an [`AggregateSumQueryResult`] containing both the accumulated
    /// sum and per-item results with detailed error handling options.
    ///
    /// Uses default options: errors on non-sum items, follows references.
    /// For full control over skip/ignore behavior, use
    /// [`query_aggregate_sums_with_options`](Self::query_aggregate_sums_with_options).
    ///
    /// **See also:** [`query_sums`](Self::query_sums) for a simpler API that
    /// uses a regular [`PathQuery`] and returns raw `Vec<i64>` values without
    /// aggregate scanning limits.
    pub fn query_aggregate_sums(
        &self,
        aggregate_sum_path_query: &AggregateSumPathQuery,
        allow_cache: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AggregateSumQueryResult, Error> {
        check_grovedb_v0_with_cost!(
            "query_sums",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_sums
        );

        Element::get_aggregate_sum_query(
            &self.db,
            aggregate_sum_path_query,
            AggregateSumQueryOptions {
                allow_cache,
                error_if_intermediate_path_tree_not_present,
                error_if_non_sum_item_found: true,
                ignore_references: false,
            },
            transaction,
            grove_version,
        )
    }

    /// Retrieves SumItem values using an [`AggregateSumPathQuery`] with full
    /// control over query behavior via [`AggregateSumQueryOptions`].
    ///
    /// Like [`query_aggregate_sums`](Self::query_aggregate_sums) but lets the
    /// caller configure error handling for non-sum items and reference
    /// following.
    pub fn query_aggregate_sums_with_options(
        &self,
        aggregate_sum_path_query: &AggregateSumPathQuery,
        query_options: AggregateSumQueryOptions,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AggregateSumQueryResult, Error> {
        check_grovedb_v0_with_cost!(
            "query_sums",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_sums
        );

        Element::get_aggregate_sum_query(
            &self.db,
            aggregate_sum_path_query,
            query_options,
            transaction,
            grove_version,
        )
    }

    /// Execute an `AggregateCountOnRange` path query without producing a
    /// proof, returning the in-range count directly.
    ///
    /// This is the no-proof counterpart of
    /// [`Self::prove_query`] +
    /// [`Self::verify_aggregate_count_query`](GroveDb::verify_aggregate_count_query)
    /// for `AggregateCountOnRange` queries: it performs the same merk-level
    /// boundary walk the prover does (using each internal node's stored
    /// aggregate count to short-circuit Contained / Disjoint subtrees) but
    /// skips proof generation, serialization, and verification entirely.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_leaf_aggregate_count_on_range`] — strictly the
    /// **leaf** shape: a single `AggregateCountOnRange(_)` item, no
    /// subqueries, no pagination, and an inner range that isn't `Key`,
    /// `RangeFull`, or another `AggregateCountOnRange`. Carrier-shape
    /// queries (outer `Keys` + `AggregateCountOnRange` subquery) are
    /// rejected here because this entry point returns one `u64` and has
    /// no way to surface per-outer-key counts; use
    /// [`Self::prove_query`] +
    /// [`Self::verify_aggregate_count_query_per_key`](GroveDb::verify_aggregate_count_query_per_key)
    /// for those. Any other shape is rejected up front with
    /// `Error::InvalidQuery` before any merk reads happen.
    ///
    /// The subtree at `path_query.path` must be a `ProvableCountTree` or
    /// `ProvableCountSumTree` — the merk-level walk rejects any other tree
    /// type. If the subtree is missing (path does not resolve), this returns
    /// the same `PathNotFound` / `PathParentLayerNotFound` errors as other
    /// path-based reads.
    ///
    /// The returned count is **not** independently verifiable — callers are
    /// trusting their own merk read path. For a verifiable count, use
    /// [`Self::prove_query`] +
    /// [`Self::verify_aggregate_count_query`](GroveDb::verify_aggregate_count_query).
    pub fn query_aggregate_count(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u64, Error> {
        check_grovedb_v0_with_cost!(
            "query_aggregate_count",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_count_on_range
        );

        let mut cost = OperationCost::default();

        // Up-front shape validation. Strictly the leaf shape — this
        // entry point returns a single `u64` and has no way to surface
        // per-outer-key carrier results. Catches malformed leaf
        // aggregate-count queries (illegal inner range, pagination,
        // etc.) AND carrier-shape queries before any storage reads.
        let inner_range = cost_return_on_error_no_add!(
            cost,
            path_query.validate_leaf_aggregate_count_on_range().cloned()
        );

        let tx = TxRef::new(&self.db, transaction);

        // Open the leaf merk and ask it for the count. The merk-level entry
        // point enforces `tree_type ∈ {ProvableCountTree, ProvableCountSumTree}`
        // and handles the empty-merk case (returns 0).
        let path_slices: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();
        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                SubtreePath::from(path_slices.as_slice()),
                tx.as_ref(),
                None,
                grove_version,
            )
        );

        let count = cost_return_on_error!(
            &mut cost,
            subtree
                .count_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "query_aggregate_count at path {:?}: {}",
                    path_slices, e
                )))
        );

        Ok(count).wrap_with_cost(cost)
    }

    /// Execute an `AggregateCountAndSumOnRange` path query without
    /// producing a proof, returning the in-range `(count, sum)` pair
    /// from a single merk-internal traversal.
    ///
    /// No-prove sibling of [`Self::query_aggregate_sum`] and
    /// [`Self::query_aggregate_count`] for the combined-axis flavor —
    /// the same call shape, just yielding both metrics at once. The
    /// returned tuple matches what
    /// [`GroveDb::verify_aggregate_count_and_sum_query`] extracts from
    /// the prove-side equivalent for the same path query, so consumers
    /// can swap between the two paths without changing call sites.
    ///
    /// Internally this runs ONE classification walk over the leaf
    /// merk (the same shape the combined prover walks) and accumulates
    /// both axes in parallel; it is strictly cheaper than calling
    /// `query_aggregate_count` and `query_aggregate_sum` separately.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_leaf_aggregate_count_and_sum_on_range`] —
    /// strictly the **leaf** shape: a single
    /// `AggregateCountAndSumOnRange(_)` item, no subqueries, no
    /// pagination, and an inner range that isn't `Key`, `RangeFull`,
    /// or another aggregate variant. Carrier-shape queries are
    /// rejected here because this entry point returns one `(u64, i64)`
    /// and has no way to surface per-outer-key carrier results. Any
    /// other shape is rejected up front with `Error::InvalidQuery`
    /// before any merk reads happen.
    ///
    /// The subtree at `path_query.path` must be a
    /// `ProvableCountProvableSumTree` — the merk-level walk rejects
    /// any other tree type with the same `WrongElementType`-shape
    /// error the sibling no-prove accumulators return. If the subtree
    /// is missing (path does not resolve), this returns the same
    /// `PathNotFound` / `PathParentLayerNotFound` errors as other
    /// path-based reads.
    ///
    /// The returned pair is **not** independently verifiable — callers
    /// are trusting their own merk read path. For a verifiable
    /// `(count, sum)`, use [`Self::prove_query`] +
    /// [`GroveDb::verify_aggregate_count_and_sum_query`].
    pub fn query_aggregate_count_and_sum(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(u64, i64), Error> {
        let version_slot = grove_version
            .grovedb_versions
            .operations
            .query
            .query_aggregate_count_and_sum_on_range;
        check_grovedb_v0_with_cost!("query_aggregate_count_and_sum", version_slot);

        let mut cost = OperationCost::default();

        // Up-front shape validation. Strictly the leaf shape — this
        // entry point returns a single `(u64, i64)` and has no way to
        // surface per-outer-key carrier results. Catches malformed
        // leaf combined-aggregate queries (illegal inner range,
        // pagination, etc.) AND carrier-shape queries before any
        // storage reads.
        let inner_range = cost_return_on_error_no_add!(
            cost,
            path_query
                .validate_leaf_aggregate_count_and_sum_on_range()
                .cloned()
        );

        let tx = TxRef::new(&self.db, transaction);

        // Open the leaf merk and ask it for the (count, sum). The
        // merk-level entry point enforces
        // `tree_type == ProvableCountProvableSumTree` and handles the
        // empty-merk case (returns (0, 0)).
        let path_slices: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();
        let subtree = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                SubtreePath::from(path_slices.as_slice()),
                tx.as_ref(),
                None,
                grove_version,
            )
        );

        let count_and_sum = cost_return_on_error!(
            &mut cost,
            subtree
                .count_and_sum_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "query_aggregate_count_and_sum at path {:?}: {}",
                    path_slices, e
                )))
        );

        Ok(count_and_sum).wrap_with_cost(cost)
    }

    /// Executes an `AggregateCountOnRange` query in either the **leaf** or
    /// **carrier** shape without generating a proof, returning one
    /// `(outer_key, count)` pair per matched outer key.
    ///
    /// This is the no-proof counterpart of
    /// [`GroveDb::verify_aggregate_count_query_per_key`]: it performs the
    /// same merk-level boundary walks the per-key verifier reconstructs
    /// from a proof but skips proof generation, encoding, decoding, and
    /// chain verification entirely.
    ///
    /// For a **leaf** query the returned vector contains exactly one
    /// entry whose key is an empty byte string and whose count is the
    /// same `u64` [`Self::query_aggregate_count`] would have returned.
    /// This matches the per-key verifier's leaf behavior, so callers
    /// that always handle `Vec<(Vec<u8>, u64)>` don't need to branch on
    /// the shape.
    ///
    /// For a **carrier** query the outer items must be `Key(_)` /
    /// `Range*(_)` and the `default_subquery_branch.subquery` must
    /// validate as a leaf `AggregateCountOnRange`. The optional
    /// `subquery_path` is followed exactly (single-key step per element)
    /// before the count walk. The returned vector has one entry per
    /// matched outer key in query-direction order (ascending lex when
    /// `left_to_right = true`, descending otherwise). Outer-key
    /// candidates that don't exist contribute no entry; outer-key
    /// candidates whose leaf subtree is empty contribute `(key, 0)`.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_count_on_range`] in either
    /// shape. Pagination rules differ by shape: for **leaf** queries
    /// both `SizedQuery::limit` and `SizedQuery::offset` are rejected
    /// (a leaf returns a single `u64` and pagination would silently
    /// change the answer); for **carrier** queries `SizedQuery::limit`
    /// is accepted and caps the number of outer-key matches the walk
    /// returns (each matched outer key still produces a complete
    /// leaf-ACOR `u64`, the inner range is not capped), while
    /// `SizedQuery::offset` is still rejected. Each leaf subtree the
    /// walk terminates in must be a `ProvableCountTree` or
    /// `ProvableCountSumTree` — the merk-level walk rejects any other
    /// tree type.
    ///
    /// The returned counts are **not** independently verifiable —
    /// callers are trusting their own merk read path. For verifiable
    /// counts, use [`Self::prove_query`] +
    /// [`GroveDb::verify_aggregate_count_query_per_key`].
    pub fn query_aggregate_count_per_key(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(Vec<u8>, u64)>, Error> {
        check_grovedb_v0_with_cost!(
            "query_aggregate_count_per_key",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_count_on_range
        );

        let mut cost = OperationCost::default();

        // Up-front shape validation: accept both leaf and carrier shapes.
        // We classify by what the top-level query owns: a direct
        // `AggregateCountOnRange` item means leaf; otherwise the
        // dispatcher already confirmed a valid carrier subquery exists.
        let inner_range = cost_return_on_error_no_add!(
            cost,
            path_query.validate_aggregate_count_on_range().cloned()
        );

        if path_query.query.query.aggregate_count_on_range().is_some() {
            // Leaf shape: delegate to the existing single-`u64` entry
            // point and wrap as a one-entry vector with an empty key.
            let count = cost_return_on_error!(
                &mut cost,
                self.query_aggregate_count(path_query, transaction, grove_version)
            );
            return Ok(vec![(Vec::new(), count)]).wrap_with_cost(cost);
        }

        // Carrier shape: enumerate matched outer keys at the carrier
        // subtree, then per match navigate `subquery_path` and run the
        // merk-level count walk on the leaf.
        let q = &path_query.query.query;
        let outer_items = q.items.clone();
        let subquery_path = q
            .default_subquery_branch
            .subquery_path
            .clone()
            .unwrap_or_default();
        let left_to_right = q.left_to_right;

        // Build a "shallow" path query that enumerates the carrier's
        // outer items at `path_query.path` without descending into the
        // subquery — we want just the matched outer keys, not the
        // (unproven) results of the leaf aggregate-count.
        //
        // Propagate `SizedQuery::limit` (validated as carrier-only
        // above): it caps the number of outer-key matches the walk
        // returns. Each matched outer key still produces a complete
        // leaf-ACOR `u64` below. `offset` is rejected at validation, so
        // we don't propagate it here.
        let mut shallow_query = grovedb_query::Query::new_with_direction(left_to_right);
        shallow_query.items = outer_items;
        let shallow_pq = PathQuery::new(
            path_query.path.clone(),
            SizedQuery::new(shallow_query, path_query.query.limit, None),
        );

        let (matched, _skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                &shallow_pq,
                true,  // allow_cache
                false, // decrease_limit_on_range_with_no_sub_elements
                true,  // error_if_intermediate_path_tree_not_present
                QueryResultType::QueryKeyElementPairResultType,
                transaction,
                grove_version,
            )
        );

        let key_elements = matched.to_key_elements();
        let mut results: Vec<(Vec<u8>, u64)> = Vec::with_capacity(key_elements.len());
        let tx = TxRef::new(&self.db, transaction);

        for (key, element) in key_elements {
            // Refuse non-tree matches: aggregate-count requires
            // descending into the matched element to find the leaf
            // count subtree.
            if !element.is_any_tree() {
                return Err(Error::InvalidQuery(
                    "carrier aggregate-count matched a non-tree element; outer items must \
                     resolve to tree elements",
                ))
                .wrap_with_cost(cost);
            }

            // Build the path to the leaf count subtree:
            // `path_query.path / outer_key / subquery_path...`.
            let mut leaf_path_owned: Vec<Vec<u8>> = path_query.path.clone();
            leaf_path_owned.push(key.clone());
            leaf_path_owned.extend(subquery_path.iter().cloned());
            let leaf_path: Vec<&[u8]> = leaf_path_owned.iter().map(|p| p.as_slice()).collect();

            let leaf_subtree = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    SubtreePath::from(leaf_path.as_slice()),
                    tx.as_ref(),
                    None,
                    grove_version,
                )
            );

            let count = cost_return_on_error!(
                &mut cost,
                leaf_subtree
                    .count_aggregate_on_range(&inner_range, grove_version)
                    .map_err(Error::MerkError)
            );

            results.push((key, count));
        }

        Ok(results).wrap_with_cost(cost)
    }

    /// Retrieves SumItem values that match a regular [`PathQuery`], returning
    /// a `Vec<i64>` of the raw sum values and the number of skipped elements.
    ///
    /// This is a simpler alternative to
    /// [`query_aggregate_sums`](Self::query_aggregate_sums) — it uses a
    /// standard [`PathQuery`] (not [`AggregateSumPathQuery`]) and has no
    /// aggregate scanning budget limit.
    ///
    /// References are followed only for `AbsolutePathReference`; the resolved
    /// element must be a `SumItem`.
    pub fn query_sums(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(Vec<i64>, u16), Error> {
        check_grovedb_v0_with_cost!(
            "query_sums",
            grove_version.grovedb_versions.operations.query.query_sums
        );
        let mut cost = OperationCost::default();

        let (elements, skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                QueryResultType::QueryElementResultType,
                transaction,
                grove_version
            )
        );

        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => {
                    // NonCounted is transparent at this layer.
                    let element = element.into_underlying();
                    match element {
                        // For `ReferenceWithSumItem` we follow the reference
                        // just like `Reference` — the carried sum is a
                        // parent-aggregation property, not the queryable
                        // leaf value. Target must still be a SumItem for
                        // `query_sums` to succeed.
                        Element::Reference(reference_path, ..)
                        | Element::ReferenceWithSumItem(reference_path, ..) => {
                            match reference_path {
                                ReferencePathType::AbsolutePathReference(absolute_path) => {
                                    // While `map` on iterator is lazy, we should accumulate costs
                                    // even if `collect` will
                                    // end in `Err`, so we'll use
                                    // external costs accumulator instead of
                                    // returning costs from `map` call.
                                    let maybe_item = self
                                        .follow_reference(
                                            absolute_path.as_slice().into(),
                                            allow_cache,
                                            transaction,
                                            grove_version,
                                        )
                                        .unwrap_add_cost(&mut cost)?;

                                    if let Element::SumItem(item, _) = maybe_item.into_underlying()
                                    {
                                        Ok(item)
                                    } else {
                                        Err(Error::InvalidQuery(
                                            "the reference must result in a sum item",
                                        ))
                                    }
                                }
                                _ => Err(Error::CorruptedCodeExecution(
                                    "reference after query must have absolute paths",
                                )),
                            }
                        }
                        Element::SumItem(item, _) | Element::ItemWithSumItem(_, item, _) => {
                            Ok(item)
                        }
                        Element::Tree(..)
                        | Element::SumTree(..)
                        | Element::BigSumTree(..)
                        | Element::CountTree(..)
                        | Element::CountSumTree(..)
                        | Element::ProvableCountTree(..)
                        | Element::ProvableCountSumTree(..)
                        | Element::ProvableSumTree(..)
                        | Element::ProvableCountProvableSumTree(..)
                        | Element::CommitmentTree(..)
                        | Element::MmrTree(..)
                        | Element::BulkAppendTree(..)
                        | Element::DenseAppendOnlyFixedSizeTree(..)
                        | Element::ProvableSumIndexedTree(..)
                        | Element::ProvableCountProvableSumIndexedTree(..)
                        | Element::ProvableCountIndexedTree(..)
                        | Element::Item(..) => Err(Error::InvalidQuery(
                            "path_queries over sum items can only refer to sum items and \
                             references",
                        )),
                        Element::NonCounted(_)
                        | Element::NotSummed(_)
                        | Element::NotCountedOrSummed(_) => {
                            unreachable!("unwrapped above")
                        }
                    }
                }
                _ => Err(Error::CorruptedCodeExecution(
                    "query returned incorrect result type",
                )),
            })
            .collect::<Result<Vec<i64>, Error>>();

        let results = cost_return_on_error_no_add!(cost, results_wrapped);
        Ok((results, skipped)).wrap_with_cost(cost)
    }

    /// Returns result elements and number of elements skipped given path query
    #[allow(clippy::too_many_arguments)]
    pub fn query_raw(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        check_grovedb_v0_with_cost!(
            "query_raw",
            grove_version.grovedb_versions.operations.query.query_raw
        );
        // Read-mode gate: axis / sum-budget reads are not served by the
        // key-selection read path. Fail closed rather than walking an
        // axis query's (empty) items and returning an empty result that
        // looks like real absence. `query_raw` is the funnel every
        // key-selection read entry point flows through.
        if let Err(e) = path_query.reject_unserved_read_mode() {
            return Err(e).wrap_with_cost(OperationCost::default());
        }
        Element::get_path_query(
            &self.db,
            path_query,
            QueryOptions {
                allow_get_raw: true,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
            },
            result_type,
            transaction,
            grove_version,
        )
    }

    /// Splits the result set of a path query by query path.
    /// If max_results is exceeded we return an error.
    pub fn query_keys_optional(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<PathKeyOptionalElementTrio>, Error> {
        check_grovedb_v0_with_cost!(
            "query_keys_optional",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_keys_optional
        );
        let max_results = cost_return_on_error_default!(path_query.query.limit.ok_or(
            Error::NotSupported("limits must be set in query_keys_optional".to_string())
        )) as usize;
        if path_query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "offsets are not supported in query_raw_keys_optional".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        let mut cost = OperationCost::default();

        let terminal_keys = cost_return_on_error_no_add!(
            cost,
            path_query.terminal_keys(max_results, grove_version)
        );

        let (elements, _) = cost_return_on_error!(
            &mut cost,
            self.query(
                path_query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                QueryResultType::QueryPathKeyElementTrioResultType,
                transaction,
                grove_version
            )
        );

        let mut elements_map = elements.to_path_key_elements_btree_map();

        Ok(terminal_keys
            .into_iter()
            .map(|path_key| {
                let element = elements_map.remove(&path_key);
                (path_key.0, path_key.1, element)
            })
            .collect())
        .wrap_with_cost(cost)
    }

    /// If max_results is exceeded we return an error
    pub fn query_raw_keys_optional(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<PathKeyOptionalElementTrio>, Error> {
        check_grovedb_v0_with_cost!(
            "query_raw_keys_optional",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_raw_keys_optional
        );
        let max_results = cost_return_on_error_default!(path_query.query.limit.ok_or(
            Error::NotSupported("limits must be set in query_raw_keys_optional".to_string())
        )) as usize;
        if path_query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "offsets are not supported in query_raw_keys_optional".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        let mut cost = OperationCost::default();

        let terminal_keys = cost_return_on_error_no_add!(
            cost,
            path_query.terminal_keys(max_results, grove_version)
        );

        let (elements, _) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                allow_cache,
                decrease_limit_on_range_with_no_sub_elements,
                error_if_intermediate_path_tree_not_present,
                QueryResultType::QueryPathKeyElementTrioResultType,
                transaction,
                grove_version
            )
        );

        let mut elements_map = elements.to_path_key_elements_btree_map();

        Ok(terminal_keys
            .into_iter()
            .map(|path_key| {
                let element = elements_map.remove(&path_key);
                (path_key.0, path_key.1, element)
            })
            .collect())
        .wrap_with_cost(cost)
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};
    use grovedb_version::version::GroveVersion;
    use pretty_assertions::assert_eq;

    use crate::{
        reference_path::ReferencePathType::AbsolutePathReference,
        tests::{make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, PathQuery, SizedQuery,
    };

    #[test]
    fn test_query_raw_keys_options() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"3",
            Element::new_item(b"hello too".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"5",
            Element::new_item(b"bye".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_key(b"1".to_vec());
        query.insert_key(b"2".to_vec());
        query.insert_key(b"5".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path.clone(), SizedQuery::new(query, Some(5), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("should get successfully");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        assert_eq!(raw_result.len(), 3);
        assert_eq!(raw_result.get(&(path.clone(), b"4".to_vec())), None);
        assert_eq!(raw_result.get(&(path.clone(), b"2".to_vec())), Some(&None));
        assert_eq!(
            raw_result.get(&(path, b"5".to_vec())),
            Some(&Some(Element::new_item(b"bye".to_vec())))
        );
    }

    #[test]
    fn test_query_raw_keys_options_with_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"3",
            Element::new_item(b"hello too".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"5",
            Element::new_item(b"bye".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_range(b"1".to_vec()..b"3".to_vec());
        query.insert_key(b"5".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path.clone(), SizedQuery::new(query, Some(5), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("should get successfully");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        assert_eq!(raw_result.len(), 3);
        assert_eq!(raw_result.get(&(path.clone(), b"4".to_vec())), None);
        assert_eq!(raw_result.get(&(path.clone(), b"2".to_vec())), Some(&None));
        assert_eq!(
            raw_result.get(&(path.clone(), b"5".to_vec())),
            Some(&Some(Element::new_item(b"bye".to_vec())))
        );
        assert_eq!(raw_result.get(&(path, b"3".to_vec())), None);
    }

    #[test]
    fn test_query_raw_keys_options_with_range_inclusive() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"3",
            Element::new_item(b"hello too".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"5",
            Element::new_item(b"bye".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_range_inclusive(b"1".to_vec()..=b"3".to_vec());
        query.insert_key(b"5".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path.clone(), SizedQuery::new(query, Some(5), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("should get successfully");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        assert_eq!(raw_result.len(), 4);
        assert_eq!(raw_result.get(&(path.clone(), b"4".to_vec())), None);
        assert_eq!(raw_result.get(&(path.clone(), b"2".to_vec())), Some(&None));
        assert_eq!(
            raw_result.get(&(path.clone(), b"5".to_vec())),
            Some(&Some(Element::new_item(b"bye".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(path, b"3".to_vec())),
            Some(&Some(Element::new_item(b"hello too".to_vec())))
        );
    }

    #[test]
    fn test_query_raw_keys_options_with_range_bounds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::new_item(b"empty".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"3",
            Element::new_item(b"hello too".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"5",
            Element::new_item(b"bye".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"g".to_vec());

        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(4), None));
        db.query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect_err("range a should error");

        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"c".to_vec()); // 2
        query.insert_key(b"5".to_vec()); // 3
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(3), None));
        db.query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("range b should not error");

        let mut query = Query::new();
        query.insert_range_inclusive(b"a".to_vec()..=b"c".to_vec()); // 3
        query.insert_key(b"5".to_vec()); // 4
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(3), None));
        db.query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect_err("range c should error");

        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"c".to_vec()); // 2
        query.insert_key(b"5".to_vec()); // 3
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(2), None));
        db.query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect_err("range d should error");

        let mut query = Query::new();
        query.insert_range(b"z".to_vec()..b"10".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        db.query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect_err("range using 2 bytes should error");
    }

    #[test]
    fn test_query_raw_keys_options_with_empty_start_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::new_item(b"empty".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"3",
            Element::new_item(b"hello too".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"5",
            Element::new_item(b"bye".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path.clone(), SizedQuery::new(query, Some(1000), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("range starting with null should not error");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        assert_eq!(raw_result.len(), 100); // because is 99 ascii, and we have empty too
        assert_eq!(raw_result.get(&(path.clone(), b"4".to_vec())), Some(&None));
        assert_eq!(raw_result.get(&(path.clone(), b"2".to_vec())), Some(&None));
        assert_eq!(
            raw_result.get(&(path.clone(), b"5".to_vec())),
            Some(&Some(Element::new_item(b"bye".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(path.clone(), b"3".to_vec())),
            Some(&Some(Element::new_item(b"hello too".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(path, b"".to_vec())),
            Some(&Some(Element::new_item(b"empty".to_vec())))
        );
    }

    #[test]
    fn test_query_raw_keys_options_with_subquery_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"",
            Element::new_item(b"null in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"1",
            Element::new_item(b"1 in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"1",
            Element::new_item(b"1 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"5",
            Element::new_item(b"5 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        db.query_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect_err("range should error because we didn't subquery");

        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        query.set_subquery_key(b"1".to_vec());
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("query with subquery should not error");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        assert_eq!(raw_result.len(), 100); // because is 99 ascii, and we have empty too
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"1".to_vec())),
            Some(&None)
        ); // because we are sub-querying 1
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"4".to_vec())),
            None
        );

        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"1".to_vec())),
            Some(&Some(Element::new_item(b"1 in null".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"2".to_vec()], b"1".to_vec())),
            Some(&Some(Element::new_item(b"1 in 2".to_vec())))
        );
    }

    #[test]
    fn test_query_raw_keys_options_with_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"",
            Element::new_item(b"null in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"1",
            Element::new_item(b"1 in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"1",
            Element::new_item(b"1 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"5",
            Element::new_item(b"5 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"2",
            Element::new_item(b"2 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut sub_query = Query::new();
        sub_query.insert_key(b"1".to_vec());
        sub_query.insert_key(b"2".to_vec());
        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        query.set_subquery(sub_query);
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("query with subquery should not error");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        // because is 99 ascii, and we have empty too = 100 then x 2
        assert_eq!(raw_result.len(), 200);
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"1".to_vec())),
            Some(&None)
        ); // because we are sub-querying 1
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"2".to_vec())),
            Some(&None)
        ); // because we are sub-querying 1
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"4".to_vec())),
            None
        );

        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"1".to_vec())),
            Some(&Some(Element::new_item(b"1 in null".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"2".to_vec()], b"1".to_vec())),
            Some(&Some(Element::new_item(b"1 in 2".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"2".to_vec()], b"2".to_vec())),
            Some(&Some(Element::new_item(b"2 in 2".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"2".to_vec()], b"5".to_vec())),
            None
        ); // because we didn't query for it
    }

    #[test]
    fn test_query_raw_keys_options_with_subquery_having_intermediate_paths_missing() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"1"].as_ref(),
            b"deep_1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"1", b"deep_1"].as_ref(),
            b"deeper_1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"1", b"deep_1", b"deeper_1"].as_ref(),
            b"2",
            Element::new_item(b"found_me".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"1",
            Element::new_item(b"1 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"5",
            Element::new_item(b"5 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"2",
            Element::new_item(b"2 in 2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut sub_query = Query::new();
        sub_query.insert_key(b"1".to_vec());
        sub_query.insert_key(b"2".to_vec());
        let mut query = Query::new();
        query.insert_keys(vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()]);
        query.set_subquery_path(vec![b"deep_1".to_vec(), b"deeper_1".to_vec()]);
        query.set_subquery(sub_query);
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));

        db.query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect_err(
                "query with subquery should error if error_if_intermediate_path_tree_not_present \
                 is set to true",
            );

        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, false, None, GroveVersion::latest())
            .unwrap()
            .expect("query with subquery should not error");

        // because is 99 ascii, and we have empty too = 100 then x 2
        assert_eq!(raw_result.len(), 6);

        let expected_result = vec![
            (
                vec![
                    b"test_leaf".to_vec(),
                    b"1".to_vec(),
                    b"deep_1".to_vec(),
                    b"deeper_1".to_vec(),
                ],
                b"1".to_vec(),
                None,
            ),
            (
                vec![
                    b"test_leaf".to_vec(),
                    b"1".to_vec(),
                    b"deep_1".to_vec(),
                    b"deeper_1".to_vec(),
                ],
                b"2".to_vec(),
                Some(Element::new_item(b"found_me".to_vec())),
            ),
            (
                vec![
                    b"test_leaf".to_vec(),
                    b"2".to_vec(),
                    b"deep_1".to_vec(),
                    b"deeper_1".to_vec(),
                ],
                b"1".to_vec(),
                None,
            ),
            (
                vec![
                    b"test_leaf".to_vec(),
                    b"2".to_vec(),
                    b"deep_1".to_vec(),
                    b"deeper_1".to_vec(),
                ],
                b"2".to_vec(),
                None,
            ),
            (
                vec![
                    b"test_leaf".to_vec(),
                    b"3".to_vec(),
                    b"deep_1".to_vec(),
                    b"deeper_1".to_vec(),
                ],
                b"1".to_vec(),
                None,
            ),
            (
                vec![
                    b"test_leaf".to_vec(),
                    b"3".to_vec(),
                    b"deep_1".to_vec(),
                    b"deeper_1".to_vec(),
                ],
                b"2".to_vec(),
                None,
            ),
        ];

        assert_eq!(raw_result, expected_result);
    }

    #[test]
    fn test_query_raw_keys_options_with_subquery_and_subquery_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"",
            Element::new_item(b"null in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"", b"1"].as_ref(),
            b"2",
            Element::new_item(b"2 in null/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2", b"1"].as_ref(),
            b"2",
            Element::new_item(b"2 in 2/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2", b"1"].as_ref(),
            b"5",
            Element::new_item(b"5 in 2/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        // Our tree should be
        //      Test_Leaf
        //   ""        "2"
        //    |       /   \
        //   "1"     "1"   "2"
        //    |     /   \
        //   "2"   "2"  "5"

        let mut sub_query = Query::new();
        sub_query.insert_key(b"1".to_vec());
        sub_query.insert_key(b"2".to_vec());
        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        query.set_subquery_key(b"1".to_vec());
        query.set_subquery(sub_query);
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("query with subquery should not error");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        // because is 99 ascii, and we have empty too = 100 then x 2
        assert_eq!(raw_result.len(), 200);
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"1".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"2".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"4".to_vec())),
            None
        );

        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"".to_vec(), b"1".to_vec()],
                b"2".to_vec()
            )),
            Some(&Some(Element::new_item(b"2 in null/1".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"1".to_vec()
            )),
            Some(&None)
        );
        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"5".to_vec()
            )),
            None
        ); // because we didn't query for it
        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"2".to_vec()
            )),
            Some(&Some(Element::new_item(b"2 in 2/1".to_vec())))
        );
    }

    #[test]
    fn test_query_raw_keys_options_with_subquery_and_conditional_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"",
            Element::new_item(b"null in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"", b"1"].as_ref(),
            b"2",
            Element::new_item(b"2 in null/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2", b"1"].as_ref(),
            b"2",
            Element::new_item(b"2 in 2/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2", b"1"].as_ref(),
            b"5",
            Element::new_item(b"5 in 2/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        // Our tree should be
        //      Test_Leaf
        //   ""        "2"
        //    |       /   \
        //   "1"     "1"   "2"
        //    |     /   \
        //   "2"   "2"  "5"

        let mut sub_query = Query::new();
        sub_query.insert_key(b"1".to_vec());
        sub_query.insert_key(b"2".to_vec());
        let mut conditional_sub_query = Query::new();
        conditional_sub_query.insert_key(b"5".to_vec());
        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        query.set_subquery_key(b"1".to_vec());
        query.set_subquery(sub_query);
        query.add_conditional_subquery(
            QueryItem::Key(b"2".to_vec()),
            Some(vec![b"1".to_vec()]),
            Some(conditional_sub_query),
        );
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("query with subquery should not error");

        let raw_result: HashMap<_, _> = raw_result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        // 1 less than 200, because of the conditional subquery of 1 element that takes
        // 1 instead of 2
        assert_eq!(raw_result.len(), 199);
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"1".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"2".to_vec())),
            None
        );
        assert_eq!(
            raw_result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"4".to_vec())),
            None
        );

        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"".to_vec(), b"1".to_vec()],
                b"2".to_vec()
            )),
            Some(&Some(Element::new_item(b"2 in null/1".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"1".to_vec()
            )),
            None
        ); // conditional subquery overrides this
        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"5".to_vec()
            )),
            Some(&Some(Element::new_item(b"5 in 2/1".to_vec())))
        );
        assert_eq!(
            raw_result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"2".to_vec()
            )),
            None
        ); // because we didn't query for it
    }

    #[test]
    fn test_query_keys_options_with_subquery_and_conditional_subquery_and_reference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"5",
            Element::new_item(b"ref result".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"",
            Element::new_item(b"null in null".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b""].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"", b"1"].as_ref(),
            b"2",
            Element::new_item(b"2 in null/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2"].as_ref(),
            b"2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2", b"1"].as_ref(),
            b"2",
            Element::new_item(b"2 in 2/1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF, b"2", b"1"].as_ref(),
            b"5",
            Element::new_reference_with_hops(
                AbsolutePathReference(vec![ANOTHER_TEST_LEAF.to_vec(), b"5".to_vec()]),
                Some(1),
            ),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        // Our tree should be
        //      Test_Leaf         ANOTHER_TEST_LEAF
        //   ""        "2"              "5": "ref result"
        //    |       /   \
        //   "1"     "1"   "2"
        //    |     /   \
        //   "2"   "2"  "5"

        let mut sub_query = Query::new();
        sub_query.insert_key(b"1".to_vec());
        sub_query.insert_key(b"2".to_vec());
        let mut conditional_sub_query = Query::new();
        conditional_sub_query.insert_key(b"5".to_vec());
        let mut query = Query::new();
        query.insert_range(b"".to_vec()..b"c".to_vec());
        query.set_subquery_key(b"1".to_vec());
        query.set_subquery(sub_query);
        query.add_conditional_subquery(
            QueryItem::Key(b"2".to_vec()),
            Some(vec![b"1".to_vec()]),
            Some(conditional_sub_query),
        );
        let path = vec![TEST_LEAF.to_vec()];
        let path_query = PathQuery::new(path, SizedQuery::new(query, Some(1000), None));
        let result = db
            .query_keys_optional(&path_query, true, true, true, None, GroveVersion::latest())
            .unwrap()
            .expect("query with subquery should not error");

        let result: HashMap<_, _> = result
            .into_iter()
            .map(|(path, key, element)| ((path, key), element))
            .collect();

        // 1 less than 200, because of the conditional subquery of 1 element that takes
        // 1 instead of 2
        assert_eq!(result.len(), 199);
        assert_eq!(result.get(&(vec![TEST_LEAF.to_vec()], b"4".to_vec())), None);
        assert_eq!(
            result.get(&(vec![TEST_LEAF.to_vec(), b"".to_vec()], b"4".to_vec())),
            None
        );
        assert_eq!(
            result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"1".to_vec())),
            None
        );
        assert_eq!(
            result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"2".to_vec())),
            None
        );
        assert_eq!(
            result.get(&(vec![TEST_LEAF.to_vec(), b"4".to_vec()], b"4".to_vec())),
            None
        );

        assert_eq!(
            result.get(&(
                vec![TEST_LEAF.to_vec(), b"".to_vec(), b"1".to_vec()],
                b"2".to_vec()
            )),
            Some(&Some(Element::new_item(b"2 in null/1".to_vec())))
        );
        assert_eq!(
            result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"1".to_vec()
            )),
            None
        ); // conditional subquery overrides this
        assert_eq!(
            result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"5".to_vec()
            )),
            Some(&Some(Element::new_item(b"ref result".to_vec())))
        );
        assert_eq!(
            result.get(&(
                vec![TEST_LEAF.to_vec(), b"2".to_vec(), b"1".to_vec()],
                b"2".to_vec()
            )),
            None
        ); // because we didn't query for it
    }

    #[test]
    fn test_query_aggregate_sums() {
        use grovedb_merk::proofs::query::AggregateSumQuery;

        use crate::{
            tests::{make_test_sum_tree_grovedb, TEST_LEAF},
            AggregateSumPathQuery,
        };

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

        let aggregate_sum_query = AggregateSumQuery::new(20, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        let result = db
            .query_aggregate_sums(&aggregate_sum_path_query, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful query");

        assert_eq!(result.results, vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)]);
        assert!(!result.hard_limit_reached);
    }

    #[test]
    fn test_query_aggregate_sums_with_options() {
        use grovedb_merk::proofs::query::AggregateSumQuery;

        use crate::{
            element::aggregate_sum_query::AggregateSumQueryOptions,
            tests::{make_test_sum_tree_grovedb, TEST_LEAF},
            AggregateSumPathQuery,
        };

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
            Element::new_item(b"not_a_sum".to_vec()),
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

        let aggregate_sum_query = AggregateSumQuery::new(100, None);
        let aggregate_sum_path_query = AggregateSumPathQuery {
            path: vec![TEST_LEAF.to_vec()],
            aggregate_sum_query,
        };

        // With error_if_non_sum_item_found=false, Item "b" is skipped
        let result = db
            .query_aggregate_sums_with_options(
                &aggregate_sum_path_query,
                AggregateSumQueryOptions {
                    error_if_non_sum_item_found: false,
                    ..AggregateSumQueryOptions::default()
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful query");

        assert_eq!(result.results, vec![(b"a".to_vec(), 7), (b"c".to_vec(), 3)]);
        assert!(!result.hard_limit_reached);
    }
}
