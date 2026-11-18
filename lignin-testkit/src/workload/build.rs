//! Small constructors shared by the generators.

use grovedb::{AggregateSumPathQuery, Element, ElementFlags, PathQuery, SizedQuery};
use grovedb_merk::proofs::query::AggregateSumQuery;

use super::{
    op::{AppendEntry, BatchOp, Op, PartialBatch, Path, Query, QueryItem, Read, Write},
    rng::Rng,
};

/// Random bytes, `shortest..=longest` of them.
pub(crate) fn value(rng: &mut Rng, shortest: u64, longest: u64) -> Vec<u8> {
    let len = rng.between(shortest, longest) as u32;
    rng.bytes(len)
}

/// A sum in `-1_000_000..=1_000_000`.
pub(crate) fn sum(rng: &mut Rng) -> i64 {
    rng.between(0, 2_000_000) as i64 - 1_000_000
}

/// A path from its segments.
pub(crate) fn path(segments: &[&[u8]]) -> Path {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

/// `parent` extended by `key`.
pub(crate) fn child(parent: &[Vec<u8>], key: &[u8]) -> Path {
    let mut path = parent.to_vec();
    path.push(key.to_vec());
    path
}

/// Epoch storage flags in the form Drive writes them: a type byte, an
/// optional 32-byte owner, a big-endian epoch.
pub(crate) fn storage_flags(epoch: u16, owner: Option<&[u8; 32]>) -> Option<ElementFlags> {
    let mut flags = Vec::with_capacity(35);
    match owner {
        Some(owner) => {
            flags.push(2);
            flags.extend_from_slice(owner);
        }
        None => flags.push(0),
    }
    flags.extend_from_slice(&epoch.to_be_bytes());
    Some(flags)
}

pub(crate) fn insert(path: &[Vec<u8>], key: &[u8], element: Element) -> Op {
    Op::Write(Write::Insert {
        path: path.to_vec(),
        key: key.to_vec(),
        element,
    })
}

pub(crate) fn delete(path: &[Vec<u8>], key: &[u8]) -> Op {
    Op::Write(Write::Delete {
        path: path.to_vec(),
        key: key.to_vec(),
    })
}

pub(crate) fn clear(path: &[Vec<u8>]) -> Op {
    Op::Write(Write::ClearSubtree {
        path: path.to_vec(),
    })
}

pub(crate) fn append(tree: &[Vec<u8>], entry: AppendEntry) -> Op {
    Op::Write(Write::Append {
        path: tree.to_vec(),
        entry,
    })
}

pub(crate) fn get(path: &[Vec<u8>], key: &[u8]) -> Op {
    Op::Read(Read::Get {
        path: path.to_vec(),
        key: key.to_vec(),
    })
}

/// A query selecting `items` in `path`, ascending, with an optional limit,
/// following references.
pub(crate) fn select(path: &[Vec<u8>], items: Vec<QueryItem>, limit: Option<u16>) -> Query {
    Query {
        path: path.to_vec(),
        items,
        subquery_path: None,
        subquery_all: false,
        limit,
        left_to_right: true,
        raw: false,
    }
}

/// [`select`], returning the elements as stored.
pub(crate) fn select_raw(path: &[Vec<u8>], items: Vec<QueryItem>, limit: Option<u16>) -> Query {
    Query {
        raw: true,
        ..select(path, items, limit)
    }
}

/// Whether the `done`-th of `total` state transitions, counted from one,
/// closes a block of `per_block` transitions.
pub(crate) fn ends_block(done: u64, total: u64, per_block: u64) -> bool {
    done.is_multiple_of(per_block) || done == total
}

pub(crate) fn query(query: Query) -> Op {
    Op::Read(Read::Query(query))
}

/// A path query in GroveDB's own form that the target proves.
pub(crate) fn proven(path_query: PathQuery) -> Op {
    path_query_read(path_query, true)
}

/// A path query in GroveDB's own form, proven by the target when `proven`
/// is set and otherwise only run.
pub(crate) fn path_query_read(path_query: PathQuery, proven: bool) -> Op {
    Op::Read(Read::PathQuery {
        query: Box::new(path_query),
        proven,
    })
}

/// A path query in GroveDB's own form run by the readers that return rows.
pub(crate) fn rows(path_query: PathQuery, raw: bool) -> Op {
    Op::Read(Read::Rows {
        query: Box::new(path_query),
        raw,
    })
}

/// `query` at `path` with a limit and an offset.
pub(crate) fn sized(
    path: &[Vec<u8>],
    query: grovedb::Query,
    limit: Option<u16>,
    offset: Option<u16>,
) -> PathQuery {
    PathQuery::new(path.to_vec(), SizedQuery::new(query, limit, offset))
}

/// A walk over the sum items `items` select in `path`, stopping at
/// `sum_limit`. Elements without a sum are skipped and references followed.
pub(crate) fn sum_budget(
    path: &[Vec<u8>],
    items: Vec<grovedb::QueryItem>,
    left_to_right: bool,
    sum_limit: u64,
    limit: Option<u16>,
) -> Op {
    Op::Read(Read::SumBudget {
        query: Box::new(AggregateSumPathQuery::new(
            path.to_vec(),
            AggregateSumQuery {
                items,
                left_to_right,
                sum_limit,
                limit_of_items_to_check: limit,
            },
        )),
        skip_non_sum_items: true,
        ignore_references: false,
    })
}

pub(crate) fn put_aux(key: &[u8], value: Vec<u8>) -> Op {
    Op::Write(Write::PutAux {
        key: key.to_vec(),
        value,
    })
}

pub(crate) fn delete_aux(key: &[u8]) -> Op {
    Op::Write(Write::DeleteAux { key: key.to_vec() })
}

pub(crate) fn get_aux(key: &[u8]) -> Op {
    Op::Read(Read::GetAux { key: key.to_vec() })
}

pub(crate) fn aux_by_prefix(prefix: &[u8]) -> Op {
    Op::Read(Read::AuxByPrefix {
        prefix: prefix.to_vec(),
    })
}

pub(crate) fn drop_flat(path: &[Vec<u8>], key: &[u8]) -> Op {
    Op::Write(Write::DropFlat {
        path: path.to_vec(),
        key: key.to_vec(),
    })
}

pub(crate) fn partial_batch(
    ops: Vec<BatchOp>,
    add_on: Vec<BatchOp>,
    pause_height: Option<u8>,
) -> Op {
    Op::PartialBatch(PartialBatch {
        ops,
        add_on,
        pause_height,
    })
}

/// Constructors for the operations of a batch.
pub(crate) mod batch {
    use super::*;

    pub(crate) fn insert_or_replace(path: &[Vec<u8>], key: &[u8], element: Element) -> BatchOp {
        BatchOp::InsertOrReplace {
            path: path.to_vec(),
            key: key.to_vec(),
            element,
        }
    }

    pub(crate) fn insert_only(path: &[Vec<u8>], key: &[u8], element: Element) -> BatchOp {
        BatchOp::InsertOnly {
            path: path.to_vec(),
            key: key.to_vec(),
            element,
        }
    }

    pub(crate) fn insert_if_not_exists(path: &[Vec<u8>], key: &[u8], element: Element) -> BatchOp {
        BatchOp::InsertIfNotExists {
            path: path.to_vec(),
            key: key.to_vec(),
            element,
        }
    }

    pub(crate) fn replace(path: &[Vec<u8>], key: &[u8], element: Element) -> BatchOp {
        BatchOp::Replace {
            path: path.to_vec(),
            key: key.to_vec(),
            element,
        }
    }

    pub(crate) fn delete(path: &[Vec<u8>], key: &[u8]) -> BatchOp {
        BatchOp::Delete {
            path: path.to_vec(),
            key: key.to_vec(),
        }
    }

    pub(crate) fn delete_tree(
        path: &[Vec<u8>],
        key: &[u8],
        tree_type: grovedb::TreeType,
    ) -> BatchOp {
        BatchOp::DeleteTree {
            path: path.to_vec(),
            key: key.to_vec(),
            tree_type,
        }
    }

    pub(crate) fn append(tree: &[Vec<u8>], entry: AppendEntry) -> BatchOp {
        BatchOp::Append {
            path: tree.to_vec(),
            entry,
        }
    }

    pub(crate) fn patch(
        path: &[Vec<u8>],
        key: &[u8],
        element: Element,
        change_in_bytes: i32,
    ) -> BatchOp {
        BatchOp::Patch {
            path: path.to_vec(),
            key: key.to_vec(),
            element,
            change_in_bytes,
        }
    }

    pub(crate) fn move_key(path: &[Vec<u8>], key: &[u8], new_key: &[u8]) -> BatchOp {
        BatchOp::Move {
            path: path.to_vec(),
            key: key.to_vec(),
            new_key: new_key.to_vec(),
        }
    }

    pub(crate) fn drop_flat(path: &[Vec<u8>], key: &[u8], tree_type: grovedb::TreeType) -> BatchOp {
        BatchOp::DropFlat {
            path: path.to_vec(),
            key: key.to_vec(),
            tree_type,
        }
    }

    /// An insert as its own write when `batched` is false, otherwise as an
    /// insert-only operation added to `pending`.
    pub(crate) fn insert_or_queue(
        batched: bool,
        ops: &mut Vec<Op>,
        pending: &mut Vec<BatchOp>,
        path: &[Vec<u8>],
        key: &[u8],
        element: Element,
    ) {
        if batched {
            pending.push(insert_only(path, key, element));
        } else {
            ops.push(super::insert(path, key, element));
        }
    }
}
