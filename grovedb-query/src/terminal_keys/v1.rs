//! `Query::terminal_keys` — **v1** (`GROVE_V4`+).
//!
//! Fixes issue #689: terminal keys are computed by iterating the queried
//! items, resolving for each key the first conditional subquery branch that
//! contains it (IndexMap insertion order) and falling back to the default
//! branch when none matches — the same branch resolution runtime query
//! execution performs (`subquery_paths_and_value_for_sized_query`). A
//! conditional selector that no queried item matches contributes nothing,
//! and a `(None, None)` conditional override keeps the matching key itself
//! terminal.
//!
//! [`super::v0`] holds the legacy walk frozen for `GROVE_V1`..`GROVE_V3`.

use crate::{error::Error, Query, SubqueryBranch};

impl Query {
    /// Pushes terminal key paths and keys to `result`, no more than
    /// `max_results`. Returns the number of terminal keys added.
    ///
    /// Terminal keys are the keys of a path query below which there are no
    /// more subqueries. In other words they're the keys of the terminal
    /// queries of a path query.
    pub fn terminal_keys_v1(
        &self,
        current_path: Vec<Vec<u8>>,
        max_results: usize,
        result: &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>,
    ) -> Result<usize, Error> {
        self.terminal_keys_inner_v1(current_path, max_results, result, 0)
    }

    fn terminal_keys_inner_v1(
        &self,
        current_path: Vec<Vec<u8>>,
        max_results: usize,
        result: &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>,
        depth: usize,
    ) -> Result<usize, Error> {
        if depth >= Self::MAX_TERMINAL_KEYS_DEPTH {
            return Err(Error::NotSupported(
                "terminal_keys subquery nesting depth exceeded".to_string(),
            ));
        }
        let mut added = 0;
        if let Some(conditional_subquery_branches) = &self.conditional_subquery_branches {
            for conditional_query_item in conditional_subquery_branches.keys() {
                // unbounded ranges can not be supported
                if conditional_query_item.is_unbounded_range() {
                    return Err(Error::NotSupported(
                        "terminal keys are not supported with conditional unbounded ranges"
                            .to_string(),
                    ));
                }
            }
        }
        for item in self.items.iter() {
            if item.is_unbounded_range() {
                return Err(Error::NotSupported(
                    "terminal keys are not supported with unbounded ranges".to_string(),
                ));
            }
            let keys = item.keys()?;
            for key in keys.into_iter() {
                // First matching conditional branch wins, mirroring runtime
                // query execution; the default branch is the fallback.
                let branch = self
                    .conditional_subquery_branches
                    .as_ref()
                    .and_then(|conditional_subquery_branches| {
                        conditional_subquery_branches
                            .iter()
                            .find(|(conditional_query_item, _)| {
                                conditional_query_item.contains(&key)
                            })
                            .map(|(_, subquery_branch)| subquery_branch)
                    })
                    .unwrap_or(&self.default_subquery_branch);

                added += Self::terminal_keys_for_branch(
                    branch,
                    key,
                    current_path.as_slice(),
                    max_results,
                    result,
                    depth,
                )?;
            }
        }
        Ok(added)
    }

    fn terminal_keys_for_branch(
        subquery_branch: &SubqueryBranch,
        key: Vec<u8>,
        current_path: &[Vec<u8>],
        max_results: usize,
        result: &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>,
        depth: usize,
    ) -> Result<usize, Error> {
        let mut path = current_path.to_vec();
        if let Some(subquery_path) = &subquery_branch.subquery_path {
            if let Some(subquery) = &subquery_branch.subquery {
                // a subquery path with a subquery
                // push the key and the subquery path to the path, then
                // recurse onto the lower level
                path.push(key);
                path.extend(subquery_path.iter().cloned());
                subquery.terminal_keys_inner_v1(path, max_results, result, depth + 1)
            } else {
                if result.len() >= max_results {
                    return Err(Error::RequestAmountExceeded(format!(
                        "terminal keys limit exceeded when subquery path but no subquery, set max \
                         is {max_results}, current len is {}",
                        result.len(),
                    )));
                }
                // a subquery path but no subquery
                // split the subquery path and remove the last element
                // push the key to the path with the front elements,
                // and set the tail of the subquery path as the terminal key
                path.push(key);
                if let Some((last_key, front_keys)) = subquery_path.split_last() {
                    path.extend(front_keys.iter().cloned());
                    result.push((path, last_key.clone()));
                    Ok(1)
                } else {
                    Err(Error::CorruptedCodeExecution(
                        "subquery_path set but doesn't contain any values",
                    ))
                }
            }
        } else if let Some(subquery) = &subquery_branch.subquery {
            // a subquery without a subquery path
            // push the key to the path and recurse onto the lower level
            path.push(key);
            subquery.terminal_keys_inner_v1(path, max_results, result, depth + 1)
        } else {
            if result.len() >= max_results {
                return Err(Error::RequestAmountExceeded(format!(
                    "terminal keys limit exceeded without subquery or subquery path, set max is \
                     {max_results}, current len is {}",
                    result.len(),
                )));
            }
            // no subquery branch: the key itself is terminal
            result.push((path, key));
            Ok(1)
        }
    }
}
