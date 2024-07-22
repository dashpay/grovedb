use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};

pub type Key = Vec<u8>;
pub type Path = Vec<PathSegment>;
pub type PathSegment = Vec<u8>;

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeFetchRequest {
    #[serde_as(as = "Vec<Base64>")]
    pub path: Path,
    #[serde_as(as = "Base64")]
    pub key: Key,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RootFetchRequest;

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeUpdate {
    #[serde_as(as = "Option<Base64>")]
    pub left_child: Option<Key>,
    #[serde_as(as = "Option<Base64>")]
    pub right_child: Option<Key>,
    #[serde_as(as = "Vec<Base64>")]
    pub path: Path,
    #[serde_as(as = "Base64")]
    pub key: Key,
    pub element: Element,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Element {
    Subtree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
    },
    Sumtree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        sum: i64,
    },
    Item {
        #[serde_as(as = "Base64")]
        value: Vec<u8>,
    },
    SumItem {
        value: i64,
    },
    AbsolutePathReference {
        #[serde_as(as = "Vec<Base64>")]
        path: Path,
    },
    UpstreamRootHeightReference {
        n_keep: u32,
        #[serde_as(as = "Vec<Base64>")]
        path_append: Vec<PathSegment>,
    },
    UpstreamRootHeightWithParentPathAdditionReference {
        n_keep: u32,
        #[serde_as(as = "Vec<Base64>")]
        path_append: Vec<PathSegment>,
    },
    UpstreamFromElementHeightReference {
        n_remove: u32,
        #[serde_as(as = "Vec<Base64>")]
        path_append: Vec<PathSegment>,
    },
    CousinReference {
        #[serde_as(as = "Base64")]
        swap_parent: PathSegment,
    },
    RemovedCousinReference {
        #[serde_as(as = "Vec<Base64>")]
        swap_parent: Vec<PathSegment>,
    },
    SiblingReference {
        #[serde_as(as = "Base64")]
        sibling_key: Key,
    },
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathQuery {
    #[serde_as(as = "Vec<Base64>")]
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

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryItem {
    Key(#[serde_as(as = "Base64")] Vec<u8>),
    Range {
        #[serde_as(as = "Base64")]
        start: Key,
        #[serde_as(as = "Base64")]
        end: Key,
    },
    RangeInclusive {
        #[serde_as(as = "Base64")]
        start: Key,
        #[serde_as(as = "Base64")]
        end: Key,
    },
    RangeFull,
    RangeFrom(#[serde_as(as = "Base64")] Key),
    RangeTo(#[serde_as(as = "Base64")] Key),
    RangeToInclusive(#[serde_as(as = "Base64")] Key),
    RangeAfter(#[serde_as(as = "Base64")] Key),
    RangeAfterTo {
        #[serde_as(as = "Base64")]
        after: Key,
        #[serde_as(as = "Base64")]
        to: Key,
    },
    RangeAfterToInclusive {
        #[serde_as(as = "Base64")]
        after: Key,
        #[serde_as(as = "Base64")]
        to: Key,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubqueryBranch {
    pub subquery_path: Option<Vec<PathSegment>>,
    pub subquery: Option<Box<Query>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proof {
    pub root_layer: ProofLayer,
    pub prove_options: ProveOptions,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProofLayer {
    pub merk_proof: Vec<MerkProofOp>,
    #[serde_as(as = "BTreeMap<Base64, _>")]
    pub lower_layers: BTreeMap<Key, ProofLayer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MerkProofOp {
    Push(MerkProofNode),
    PushInverted(MerkProofNode),
    Parent,
    Child,
    ParentInverted,
    ChildInverted,
}

pub type CryptoHash = [u8; 32];

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MerkProofNode {
    Hash(#[serde_as(as = "Base64")] CryptoHash),
    KVHash(#[serde_as(as = "Base64")] CryptoHash),
    KVDigest(
        #[serde_as(as = "Base64")] Key,
        #[serde_as(as = "Base64")] CryptoHash,
    ),
    KV(
        #[serde_as(as = "Base64")] Key,
        #[serde_as(as = "Base64")] Vec<u8>,
    ),
    KVValueHash(
        #[serde_as(as = "Base64")] Key,
        #[serde_as(as = "Base64")] Vec<u8>,
        #[serde_as(as = "Base64")] CryptoHash,
    ),
    KVValueHashFeatureType(
        #[serde_as(as = "Base64")] Key,
        #[serde_as(as = "Base64")] Vec<u8>,
        #[serde_as(as = "Base64")] CryptoHash,
        TreeFeatureType,
    ),
    KVRefValueHash(
        #[serde_as(as = "Base64")] Key,
        #[serde_as(as = "Base64")] Vec<u8>,
        #[serde_as(as = "Base64")] CryptoHash,
    ),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TreeFeatureType {
    BasicMerkNode,
    SummedMerkNode(i64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProveOptions {
    pub decrease_limit_on_empty_sub_query_result: bool,
}
