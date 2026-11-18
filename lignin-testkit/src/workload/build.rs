//! Small constructors shared by the generators.

use grovedb::{Element, ElementFlags};

use super::op::{AppendEntry, BatchOp, Op, Path, Query, QueryItem, Read, Write};

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

pub(crate) fn query(query: Query) -> Op {
    Op::Read(Read::Query(query))
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
}
