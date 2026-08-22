//! `Query::terminal_keys` — **v0** (legacy, `GROVE_V1`..`GROVE_V3`).
//!
//! CONSENSUS-RELEVANT FOR VERIFIERS. This is the walk shipped with the live
//! v1..v3 protocol versions, preserved bug-for-bug (issue #689):
//!
//! * Conditional subquery branches are expanded independently BEFORE the
//!   queried items, so a conditional selector that no queried item matches
//!   still contributes terminal keys.
//! * A key matched by a `(None, None)` conditional override is recorded in
//!   `already_added_keys` without pushing a terminal key, so the queried key
//!   silently disappears from the terminal set.
//!
//! Absence-proof verification rebuilds its result set from these terminal
//! keys, so changing this walk would change verification results on released
//! versions. Do not fix bugs here — [`super::v1`] holds the corrected
//! per-item walk selected by `GROVE_V4`+.

use std::collections::HashSet;

use crate::{error::Error, Query};

impl Query {
    /// Pushes terminal key paths and keys to `result`, no more than
    /// `max_results`. Returns the number of terminal keys added.
    ///
    /// Terminal keys are the keys of a path query below which there are no more
    /// subqueries. In other words they're the keys of the terminal queries
    /// of a path query.
    pub fn terminal_keys_v0(
        &self,
        current_path: Vec<Vec<u8>>,
        max_results: usize,
        result: &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>,
    ) -> Result<usize, Error> {
        self.terminal_keys_inner_v0(current_path, max_results, result, 0)
    }

    fn terminal_keys_inner_v0(
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
        let mut current_len = result.len();
        let mut added = 0;
        let mut already_added_keys = HashSet::new();
        if let Some(conditional_subquery_branches) = &self.conditional_subquery_branches {
            for (conditional_query_item, subquery_branch) in conditional_subquery_branches {
                // unbounded ranges can not be supported
                if conditional_query_item.is_unbounded_range() {
                    return Err(Error::NotSupported(
                        "terminal keys are not supported with conditional unbounded ranges"
                            .to_string(),
                    ));
                }
                let conditional_keys = conditional_query_item.keys()?;
                for key in conditional_keys.into_iter() {
                    if current_len > max_results {
                        return Err(Error::RequestAmountExceeded(format!(
                            "terminal keys limit exceeded for conditional subqueries, set max is \
                             {max_results}, current length is {current_len}",
                        )));
                    }
                    already_added_keys.insert(key.clone());
                    let mut path = current_path.clone();
                    if let Some(subquery_path) = &subquery_branch.subquery_path {
                        if let Some(subquery) = &subquery_branch.subquery {
                            // a subquery path with a subquery
                            // push the key to the path
                            path.push(key);
                            // push the subquery path to the path
                            path.extend(subquery_path.iter().cloned());
                            // recurse onto the lower level
                            let added_here = subquery.terminal_keys_inner_v0(
                                path,
                                max_results,
                                result,
                                depth + 1,
                            )?;
                            added += added_here;
                            current_len += added_here;
                        } else {
                            if current_len == max_results {
                                return Err(Error::RequestAmountExceeded(format!(
                                    "terminal keys limit exceeded when subquery path but no \
                                     subquery, set max is {max_results}, current length is \
                                     {current_len}",
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
                            } else {
                                return Err(Error::CorruptedCodeExecution(
                                    "subquery_path set but doesn't contain any values",
                                ));
                            }

                            added += 1;
                            current_len += 1;
                        }
                    } else if let Some(subquery) = &subquery_branch.subquery {
                        // a subquery without a subquery path
                        // push the key to the path
                        path.push(key);
                        // recurse onto the lower level
                        let added_here = subquery.terminal_keys_inner_v0(
                            path,
                            max_results,
                            result,
                            depth + 1,
                        )?;
                        added += added_here;
                        current_len += added_here;
                    }
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
                if already_added_keys.contains(&key) {
                    // we already had this key in the conditional subqueries
                    continue; // skip this key
                }
                if current_len > max_results {
                    return Err(Error::RequestAmountExceeded(format!(
                        "terminal keys limit exceeded for items, set max is {max_results}, \
                         current len is {current_len}",
                    )));
                }
                let mut path = current_path.clone();
                if let Some(subquery_path) = &self.default_subquery_branch.subquery_path {
                    if let Some(subquery) = &self.default_subquery_branch.subquery {
                        // a subquery path with a subquery
                        // push the key to the path
                        path.push(key);
                        // push the subquery path to the path
                        path.extend(subquery_path.iter().cloned());
                        // recurse onto the lower level
                        let added_here = subquery.terminal_keys_inner_v0(
                            path,
                            max_results,
                            result,
                            depth + 1,
                        )?;
                        added += added_here;
                        current_len += added_here;
                    } else {
                        if current_len == max_results {
                            return Err(Error::RequestAmountExceeded(format!(
                                "terminal keys limit exceeded when subquery path but no subquery, \
                                 set max is {max_results}, current len is {current_len}",
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
                        } else {
                            return Err(Error::CorruptedCodeExecution(
                                "subquery_path set but doesn't contain any values",
                            ));
                        }
                        added += 1;
                        current_len += 1;
                    }
                } else if let Some(subquery) = &self.default_subquery_branch.subquery {
                    // a subquery without a subquery path
                    // push the key to the path
                    path.push(key);
                    // recurse onto the lower level
                    let added_here =
                        subquery.terminal_keys_inner_v0(path, max_results, result, depth + 1)?;
                    added += added_here;
                    current_len += added_here;
                } else {
                    if current_len == max_results {
                        return Err(Error::RequestAmountExceeded(format!(
                            "terminal keys limit exceeded without subquery or subquery path, set \
                             max is {max_results}, current len is {current_len}",
                        )));
                    }
                    result.push((path, key));
                    added += 1;
                    current_len += 1;
                }
            }
        }
        Ok(added)
    }
}
