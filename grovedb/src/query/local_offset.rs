//! Validation for per-instance committed-count-unit pagination.

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{Error, PathQuery, Query, QueryItem};

pub(crate) fn validate_nodes(query: &Query) -> Result<(), Error> {
    if query.offset.is_some()
        && (query.read_mode.is_some()
            || query.add_parent_tree_on_subquery
            || query.default_subquery_branch.subquery.is_some()
            || query.default_subquery_branch.subquery_path.is_some()
            || query
                .conditional_subquery_branches
                .as_ref()
                .is_some_and(|b| !b.is_empty())
            || !matches!(query.items.as_slice(), [item] if item.is_range())
            || query.items.iter().any(|item| {
                matches!(
                    item,
                    QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateSumOnRange(_)
                        | QueryItem::AggregateCountAndSumOnRange(_)
                )
            }))
    {
        return Err(Error::InvalidQuery(
                "Query::offset requires a plain single-range terminal without subqueries or parent echo",
            ));
    }
    if let Some(subquery) = query.default_subquery_branch.subquery.as_deref() {
        validate_nodes(subquery)?;
    }
    if let Some(branches) = &query.conditional_subquery_branches {
        for branch in branches.values() {
            if let Some(subquery) = branch.subquery.as_deref() {
                validate_nodes(subquery)?;
            }
        }
    }
    Ok(())
}

fn has_parent_echo(query: &Query) -> bool {
    query.add_parent_tree_on_subquery
        || query
            .default_subquery_branch
            .subquery
            .as_deref()
            .is_some_and(has_parent_echo)
        || query
            .conditional_subquery_branches
            .as_ref()
            .is_some_and(|branches| {
                branches
                    .values()
                    .any(|branch| branch.subquery.as_deref().is_some_and(has_parent_echo))
            })
}

pub(crate) fn gate(query: &Query, grove_version: &GroveVersion) -> Result<(), Error> {
    let has_offset = query.has_instance_offset_anywhere();
    if !has_offset && !has_parent_echo(query) {
        return Ok(());
    }
    match grove_version
        .grovedb_versions
        .path_query_methods
        .per_instance_query_offsets
    {
        0 if has_offset => Err(Error::NotSupported(
            "Query::offset requires GROVE_V5 local count-unit pagination".to_string(),
        )),
        0 => match grove_version.grovedb_versions.element.path_query_push {
            0 | 1 => Ok(()),
            2 => Err(Error::CorruptedCodeExecution(
                "the v2 parent-echo engine requires its serving capability",
            )),
            received => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "path_query_push".to_string(),
                    known_versions: vec![0, 1, 2],
                    received,
                },
            )),
        },
        1 => {
            match grove_version.grovedb_versions.element.path_query_push {
                2 => {}
                0 | 1 => {
                    return Err(Error::CorruptedCodeExecution(
                        "local offsets require the v2 trusted query engine",
                    ))
                }
                received => {
                    return Err(Error::VersionError(
                        GroveVersionError::UnknownVersionMismatch {
                            method: "path_query_push".to_string(),
                            known_versions: vec![0, 1, 2],
                            received,
                        },
                    ))
                }
            }
            if has_offset
                && (query.has_read_mode_anywhere()
                    || query.has_aggregate_count_on_range_anywhere()
                    || query.has_aggregate_sum_on_range_anywhere()
                    || query.has_aggregate_count_and_sum_on_range_anywhere())
            {
                return Err(Error::InvalidQuery(
                    "local count-unit offsets cannot accompany read modes or aggregates",
                ));
            }
            validate_nodes(query)
        }
        received => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "per_instance_query_offsets".to_string(),
                known_versions: vec![0, 1],
                received,
            },
        )),
    }
}

/// Whether this selected path is a prefix of an offset-bearing query node.
fn leads_to_local_offset(query: &Query, path: &[&[u8]]) -> bool {
    let Some((key, remaining)) = path.split_first() else {
        return query.has_instance_offset_anywhere();
    };
    if !query.items.iter().any(|item| item.contains(key)) {
        return false;
    }
    let branch = query
        .conditional_subquery_branches
        .as_ref()
        .and_then(|branches| {
            branches
                .iter()
                .find(|(item, _)| item.contains(key))
                .map(|(_, branch)| branch)
        })
        .unwrap_or(&query.default_subquery_branch);
    let Some(subquery) = branch.subquery.as_deref() else {
        return false;
    };
    if let Some(suffix) = &branch.subquery_path {
        if !remaining
            .iter()
            .zip(suffix)
            .all(|(key, expected)| *key == expected.as_slice())
        {
            return false;
        }
        if remaining.len() <= suffix.len() {
            return subquery.has_instance_offset_anywhere();
        }
        leads_to_local_offset(subquery, &remaining[suffix.len()..])
    } else {
        leads_to_local_offset(subquery, remaining)
    }
}

impl PathQuery {
    pub(crate) fn validate_local_offset_host(
        &self,
        parent: &[&[u8]],
        key: &[u8],
        element: &crate::Element,
        v: &GroveVersion,
    ) -> Result<(), Error> {
        if !self.query.query.has_instance_offset_anywhere() {
            return Ok(());
        }
        let mut child = parent.to_vec();
        child.push(key);
        let on_offset_path = if child.len() <= self.path.len() {
            child
                .iter()
                .zip(&self.path)
                .all(|(key, expected)| *key == expected.as_slice())
        } else {
            self.path
                .iter()
                .zip(&child)
                .all(|(expected, key)| expected.as_slice() == *key)
                && leads_to_local_offset(&self.query.query, &child[self.path.len()..])
        };
        if on_offset_path && (!element.is_any_tree() || element.uses_non_merk_data_storage()) {
            return Err(Error::InvalidQuery(
                "local offsets require Merk tree intermediates along their selected path",
            ));
        }
        if self
            .query_items_at_path(&child, v)?
            .is_some_and(|q| q.instance_offset.is_some())
            && !matches!(
                element,
                crate::Element::ProvableCountTree(..)
                    | crate::Element::ProvableCountSumTree(..)
                    | crate::Element::ProvableCountProvableSumTree(..)
            )
        {
            return Err(Error::InvalidQuery(
                "local count-unit offsets require a provable count-bearing host",
            ));
        }
        Ok(())
    }

    pub(crate) fn reject_unserved_local_offsets(&self, v: &GroveVersion) -> Result<(), Error> {
        gate(&self.query.query, v)?;
        if self.path.is_empty() && self.query.query.offset.is_some() {
            return Err(Error::InvalidQuery(
                "the GroveDB root cannot host a local count-unit offset",
            ));
        }
        if self.query.query.has_instance_offset_anywhere() && self.query.offset.is_some() {
            return Err(Error::InvalidQuery(
                "global and local offsets cannot be combined",
            ));
        }
        Ok(())
    }

    pub(crate) fn reject_local_offsets(&self, context: &str) -> Result<(), Error> {
        if self.query.query.has_instance_offset_anywhere() {
            Err(Error::NotSupported(format!(
                "{context} cannot project local count-unit offsets"
            )))
        } else {
            Ok(())
        }
    }
}
