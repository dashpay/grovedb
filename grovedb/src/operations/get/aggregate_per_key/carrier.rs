//! The one carrier walk shared by every per-key aggregate axis.
//!
//! See [`super`] for the leaf-vs-carrier vocabulary and the per-axis entry
//! points that delegate here.
//!
//! ## The leaf shape's empty stand-in key
//!
//! Each entry point handles the leaf shape itself, before reaching this
//! driver, by delegating to its single-value sibling
//! (`query_aggregate_{count,sum,count_and_sum}`) and wrapping the result
//! as a one-entry vector keyed by `Vec::new()`.
//!
//! That empty key is deliberate, not incidental. It is the convention all
//! three per-key *verifiers* already collapse a leaf proof to, so keeping
//! it is what lets a caller swap a trusted read for `prove_query` +
//! `verify_aggregate_*_query_per_key` (or back) and compare results
//! element-for-element without branching on shape. A leaf query has no
//! outer key to report, so *some* stand-in is unavoidable; matching the
//! already-shipped proof-side convention is worth more than a prettier
//! one. It is also unambiguous: outer keys are never empty in a valid
//! carrier, since validation requires every `subquery_path` element to be
//! a non-empty key.

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::{error::Error as GrovedbMerkError, Merk};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::PrefixedRocksDbTransactionContext;
use grovedb_version::version::GroveVersion;

use crate::{
    query_result_type::QueryResultType, Error, GroveDb, PathQuery, QueryItem, SizedQuery,
    Transaction, TransactionArg,
};

impl GroveDb {
    /// Shared carrier walk behind the three `query_aggregate_*_per_key`
    /// entry points.
    ///
    /// Everything the carrier shape needs is aggregate-agnostic: the
    /// "shallow" outer-key enumeration (deliberately *not* descending
    /// into the subquery), the `SizedQuery::limit` propagation, the
    /// non-tree-match rejection, the `path / outer_key /
    /// subquery_path...` leaf-path assembly, and the per-match merk
    /// open. The only axis-specific step is which merk-level aggregate
    /// primitive terminates each walk, supplied by the caller as
    /// `merk_walk` — one of [`Merk::count_aggregate_on_range`],
    /// [`Merk::sum_aggregate_on_range`], or
    /// [`Merk::count_and_sum_aggregate_on_range`]. All three share the
    /// same signature shape and the same O(log n) Contained / Disjoint
    /// short-circuit, so the driver never needs to know which axis it is
    /// running; `T` is the axis's per-key payload (`u64`, `i64`, or
    /// `(u64, i64)`).
    ///
    /// This helper performs **no** shape validation of its own. Callers
    /// must have already run the matching
    /// `validate_aggregate_*_on_range` (which is where `inner_range`
    /// comes from, and where `SizedQuery::offset` is rejected) and must
    /// have handled the leaf shape before calling — this drives the
    /// carrier shape only.
    ///
    /// `non_tree_match_error` is the axis-specific message used when an
    /// outer-key match resolves to a non-tree element;
    /// `Error::InvalidQuery` carries a `&'static str`, so the message
    /// cannot be formatted here.
    ///
    /// `tx` is supplied by the caller rather than opened here so that
    /// `'db` stays a single *named* lifetime. `Merk`'s storage context
    /// carries its lifetime inside a type parameter, and a higher-ranked
    /// `for<'a> Fn(&Merk<PrefixedRocksDbTransactionContext<'a>>, ..)`
    /// bound cannot be satisfied by a function item — so naming the
    /// lifetime is what lets callers pass the three merk methods
    /// directly instead of wrapping each in a closure.
    pub(super) fn query_aggregate_carrier_per_key<'db, T, WalkFn>(
        &'db self,
        path_query: &PathQuery,
        inner_range: &QueryItem,
        non_tree_match_error: &'static str,
        merk_walk: WalkFn,
        tx: &'db Transaction,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(Vec<u8>, T)>, Error>
    where
        WalkFn: Fn(
            &Merk<PrefixedRocksDbTransactionContext<'db>>,
            &QueryItem,
            &GroveVersion,
        ) -> CostResult<T, GrovedbMerkError>,
    {
        let mut cost = OperationCost::default();

        // Enumerate matched outer keys at the carrier subtree, then per
        // match navigate `subquery_path` and run the merk-level
        // aggregate walk on the leaf.
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
        // (unproven) results of the leaf aggregate.
        //
        // Propagate `SizedQuery::limit` (validated as carrier-only by
        // the caller): it caps the number of outer-key matches the walk
        // returns. Each matched outer key still produces a complete
        // leaf aggregate below. `offset` is rejected at validation, so
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
        let mut results: Vec<(Vec<u8>, T)> = Vec::with_capacity(key_elements.len());

        for (key, element) in key_elements {
            // Refuse non-tree matches: every aggregate axis requires
            // descending into the matched element to find the leaf
            // aggregate subtree.
            if !element.is_any_tree() {
                return Err(Error::InvalidQuery(non_tree_match_error)).wrap_with_cost(cost);
            }

            // Build the path to the leaf aggregate subtree:
            // `path_query.path / outer_key / subquery_path...`.
            let mut leaf_path_owned: Vec<Vec<u8>> = path_query.path.clone();
            leaf_path_owned.push(key.clone());
            leaf_path_owned.extend(subquery_path.iter().cloned());
            let leaf_path: Vec<&[u8]> = leaf_path_owned.iter().map(|p| p.as_slice()).collect();

            let leaf_subtree = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    SubtreePath::from(leaf_path.as_slice()),
                    tx,
                    None,
                    grove_version,
                )
            );

            let value = cost_return_on_error!(
                &mut cost,
                merk_walk(&leaf_subtree, inner_range, grove_version).map_err(Error::MerkError)
            );

            results.push((key, value));
        }

        Ok(results).wrap_with_cost(cost)
    }
}
