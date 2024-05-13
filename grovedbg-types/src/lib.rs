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
