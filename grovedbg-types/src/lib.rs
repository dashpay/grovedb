use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type Key = Vec<u8>;
pub type Path = Vec<PathSegment>;
pub type PathSegment = Vec<u8>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeFetchRequest {
    pub path: Path,
    pub key: Key,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RootFetchRequest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeUpdate {
    pub left_child: Option<Key>,
    pub right_child: Option<Key>,
    pub path: Path,
    pub key: Key,
    pub element: Element,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Element {
    Subtree {
        root_key: Option<Key>,
    },
    Sumtree {
        root_key: Option<Key>,
        sum: i64,
    },
    Item {
        value: Vec<u8>,
    },
    SumItem {
        value: i64,
    },
    AbsolutePathReference {
        path: Path,
    },
    UpstreamRootHeightReference {
        n_keep: u32,
        path_append: Vec<PathSegment>,
    },
    UpstreamRootHeightWithParentPathAdditionReference {
        n_keep: u32,
        path_append: Vec<PathSegment>,
    },
    UpstreamFromElementHeightReference {
        n_remove: u32,
        path_append: Vec<PathSegment>,
    },
    CousinReference {
        swap_parent: PathSegment,
    },
    RemovedCousinReference {
        swap_parent: Vec<PathSegment>,
    },
    SiblingReference {
        sibling_key: Key,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathQuery {
    pub path: Path,
    pub query: SizedQuery,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SizedQuery {
    pub query: Query,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Query {
    pub items: Vec<QueryItem>,
    pub default_subquery_branch: SubqueryBranch,
    pub conditional_subquery_branches: Vec<(QueryItem, SubqueryBranch)>,
    pub left_to_right: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryItem {
    Key(Vec<u8>),
    Range { start: Key, end: Key },
    RangeInclusive { start: Key, end: Key },
    RangeFull,
    RangeFrom(Key),
    RangeTo(Key),
    RangeToInclusive(Key),
    RangeAfter(Key),
    RangeAfterTo { after: Key, to: Key },
    RangeAfterToInclusive { after: Key, to: Key },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubqueryBranch {
    pub subquery_path: Option<Vec<PathSegment>>,
    pub subquery: Option<Box<Query>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proof {
    merk_proof: MerkProof,
    lower_layers: BTreeMap<Key, Proof>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerkProof {}
