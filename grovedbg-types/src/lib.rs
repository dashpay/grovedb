use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};

pub type Key = Vec<u8>;
pub type Path = Vec<PathSegment>;
pub type PathSegment = Vec<u8>;
pub type SessionId = u64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WithSession<R> {
    pub session_id: SessionId,
    pub request: R,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewSessionResponse {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DropSessionRequest {
    pub session_id: SessionId,
}

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
    pub left_merk_hash: Option<CryptoHash>,
    #[serde_as(as = "Option<Base64>")]
    pub right_child: Option<Key>,
    #[serde_as(as = "Option<Base64>")]
    pub right_merk_hash: Option<CryptoHash>,
    #[serde_as(as = "Vec<Base64>")]
    pub path: Path,
    #[serde_as(as = "Base64")]
    pub key: Key,
    pub element: Element,
    pub feature_type: TreeFeatureType,
    #[serde_as(as = "Base64")]
    pub value_hash: CryptoHash,
    #[serde_as(as = "Base64")]
    pub kv_digest_hash: CryptoHash,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Reference {
    AbsolutePathReference {
        #[serde_as(as = "Vec<Base64>")]
        path: Path,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    UpstreamRootHeightReference {
        n_keep: u32,
        #[serde_as(as = "Vec<Base64>")]
        path_append: Vec<PathSegment>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    UpstreamRootHeightWithParentPathAdditionReference {
        n_keep: u32,
        #[serde_as(as = "Vec<Base64>")]
        path_append: Vec<PathSegment>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    UpstreamFromElementHeightReference {
        n_remove: u32,
        #[serde_as(as = "Vec<Base64>")]
        path_append: Vec<PathSegment>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    CousinReference {
        #[serde_as(as = "Base64")]
        swap_parent: PathSegment,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    RemovedCousinReference {
        #[serde_as(as = "Vec<Base64>")]
        swap_parent: Vec<PathSegment>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    SiblingReference {
        #[serde_as(as = "Base64")]
        sibling_key: Key,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Element {
    Subtree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    Sumtree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        sum: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    BigSumTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        sum: i128,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    CountTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        count: u64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    CountSumTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        count: u64,
        sum: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    ProvableCountTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        count: u64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    ProvableCountSumTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        count: u64,
        sum: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    ProvableSumTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        sum: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    ProvableCountProvableSumTree {
        #[serde_as(as = "Option<Base64>")]
        root_key: Option<Key>,
        count: u64,
        sum: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    Item {
        #[serde_as(as = "Base64")]
        value: Vec<u8>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    SumItem {
        value: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    ItemWithSumItem {
        #[serde_as(as = "Base64")]
        value: Vec<u8>,
        sum_item_value: i64,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    Reference(Reference),
    /// A reference that also carries an explicit `i64` sum-item value
    /// contributed to a sum-bearing parent (independent of the
    /// resolved target's value). The `reference` field encodes the
    /// same path-discriminant shape and `element_flags` as a plain
    /// [`Element::Reference`]; `sum_item_value` is the additional
    /// carried weight.
    ReferenceWithSumItem {
        reference: Reference,
        sum_item_value: i64,
    },
    /// An `Item` that supports being targeted by bidirectional
    /// references. The referrer list itself is not carried over the
    /// wire; only its declared capacity and, for stored nodes, current
    /// occupancy are. Proofs omit referrer lists, so their occupancy is
    /// unknown rather than zero.
    ItemWithBackwardsReferences {
        #[serde_as(as = "Base64")]
        value: Vec<u8>,
        /// How many referrers the element accepts (part of the
        /// element's identity and inner hash).
        max_incoming_references: u16,
        /// Number of currently registered referrers, or `None` when
        /// converting a proof whose referrer list was omitted.
        backward_references_count: Option<u16>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    /// A `SumItem` that supports being targeted by bidirectional
    /// references. Carries the same capacity/occupancy summary as
    /// [`Element::ItemWithBackwardsReferences`].
    SumItemWithBackwardsReferences {
        value: i64,
        max_incoming_references: u16,
        backward_references_count: Option<u16>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    /// An `ItemWithSumItem` that supports being targeted by
    /// bidirectional references. Carries the same capacity/occupancy
    /// summary as [`Element::ItemWithBackwardsReferences`].
    ItemWithSumItemWithBackwardsReferences {
        #[serde_as(as = "Base64")]
        value: Vec<u8>,
        sum_item_value: i64,
        max_incoming_references: u16,
        backward_references_count: Option<u16>,
        #[serde_as(as = "Option<Base64>")]
        element_flags: Option<Vec<u8>>,
    },
    /// A reference that registers itself in its target's
    /// backward-reference storage so target updates propagate back (or
    /// cascade-delete the referrer). The `reference` field reuses the
    /// plain [`Element::Reference`] wire shape (path discriminant plus
    /// `element_flags`); the extra fields describe the bidirectional
    /// behavior.
    BidirectionalReference {
        reference: Reference,
        /// Whether overwriting/deleting the target may cascade-delete
        /// this reference (otherwise such an update errors).
        cascade_on_update: bool,
        /// Referrers registered on this reference itself (it can in
        /// turn be targeted by other bidirectional references). `None`
        /// means the list was omitted from a proof.
        backward_references_count: Option<u16>,
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
    /// Mirrors `grovedb_query::Query::add_parent_tree_on_subquery`.
    /// Defaults to `false` so payloads from frontends that predate the
    /// field keep decoding.
    #[serde(default)]
    pub add_parent_tree_on_subquery: bool,
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
    KV(#[serde_as(as = "Base64")] Key, Element),
    KVValueHash(
        #[serde_as(as = "Base64")] Key,
        Element,
        #[serde_as(as = "Base64")] CryptoHash,
    ),
    KVValueHashFeatureType(
        #[serde_as(as = "Base64")] Key,
        Element,
        #[serde_as(as = "Base64")] CryptoHash,
        TreeFeatureType,
    ),
    KVRefValueHash(
        #[serde_as(as = "Base64")] Key,
        Element,
        #[serde_as(as = "Base64")] CryptoHash,
    ),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TreeFeatureType {
    BasicMerkNode,
    SummedMerkNode(i64),
    BigSummedMerkNode(i128),
    CountedMerkNode(u64),
    CountedSummedMerkNode(u64, i64),
    ProvableCountedMerkNode(u64),
    ProvableCountedSummedMerkNode(u64, i64),
    /// Provable sum node: sum included in node hash. Mirrors
    /// `SummedMerkNode` for serialization; the debugger renders both
    /// identically (the on-the-wire distinction is by node hash, not by
    /// serialization shape).
    ProvableSummedMerkNode(i64),
    /// Provable count + provable sum node: BOTH count and sum included in
    /// the node hash via `node_hash_with_count_and_sum`. Mirrors
    /// `ProvableCountedSummedMerkNode` for serialization; the debugger
    /// renders them identically.
    ProvableCountedAndProvableSummedMerkNode(u64, i64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProveOptions {
    pub decrease_limit_on_empty_sub_query_result: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JSON wire round-trip pin for `Element::ReferenceWithSumItem`.
    /// The variant carries a nested `Reference` (with the full
    /// path-discriminant + element_flags shape) and an independent
    /// `sum_item_value`. A future renumbering or rename of either
    /// piece trips this test.
    #[test]
    fn reference_with_sum_item_json_round_trip_absolute() {
        let element = Element::ReferenceWithSumItem {
            reference: Reference::AbsolutePathReference {
                path: vec![b"leaf".to_vec(), b"target".to_vec()],
                element_flags: Some(vec![1, 2, 3]),
            },
            sum_item_value: 1_000_000_000,
        };
        let json = serde_json::to_string(&element).expect("serialize");
        let back: Element = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, element);
    }

    /// Negative-sum and `None`-flags round-trip cleanly too.
    /// Exercises a non-absolute reference path discriminant to lock
    /// in the wire shape across the full set.
    #[test]
    fn reference_with_sum_item_json_round_trip_sibling_no_flags() {
        let element = Element::ReferenceWithSumItem {
            reference: Reference::SiblingReference {
                sibling_key: b"sib".to_vec(),
                element_flags: None,
            },
            sum_item_value: -42,
        };
        let json = serde_json::to_string(&element).expect("serialize");
        let back: Element = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, element);
    }

    /// JSON wire round-trip pins for the backward-references element
    /// variants. Only capacity/occupancy summaries travel over the
    /// wire, never the referrer list itself.
    #[test]
    fn backwards_references_items_json_round_trip() {
        let elements = [
            Element::ItemWithBackwardsReferences {
                value: b"payload".to_vec(),
                max_incoming_references: 32,
                backward_references_count: Some(2),
                element_flags: Some(vec![7]),
            },
            Element::SumItemWithBackwardsReferences {
                value: -9,
                max_incoming_references: 1,
                backward_references_count: Some(0),
                element_flags: None,
            },
            Element::ItemWithSumItemWithBackwardsReferences {
                value: b"both".to_vec(),
                sum_item_value: 55,
                max_incoming_references: 4,
                backward_references_count: Some(4),
                element_flags: None,
            },
        ];
        for element in elements {
            let json = serde_json::to_string(&element).expect("serialize");
            let back: Element = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, element);
        }
    }

    /// JSON wire round-trip pin for `Element::BidirectionalReference`:
    /// the nested `Reference` keeps the plain-reference shape and the
    /// bidirectional extras ride alongside it.
    #[test]
    fn bidirectional_reference_json_round_trip() {
        let element = Element::BidirectionalReference {
            reference: Reference::UpstreamRootHeightReference {
                n_keep: 2,
                path_append: vec![b"target".to_vec()],
                element_flags: Some(vec![1, 2]),
            },
            cascade_on_update: true,
            backward_references_count: Some(1),
        };
        let json = serde_json::to_string(&element).expect("serialize");
        let back: Element = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, element);
    }

    #[test]
    fn backward_reference_counts_distinguish_unknown_empty_and_occupied() {
        for count in [None, Some(0), Some(2)] {
            let elements = [
                Element::ItemWithBackwardsReferences {
                    value: b"payload".to_vec(),
                    max_incoming_references: 8,
                    backward_references_count: count,
                    element_flags: None,
                },
                Element::SumItemWithBackwardsReferences {
                    value: -3,
                    max_incoming_references: 8,
                    backward_references_count: count,
                    element_flags: None,
                },
                Element::ItemWithSumItemWithBackwardsReferences {
                    value: b"payload".to_vec(),
                    sum_item_value: 3,
                    max_incoming_references: 8,
                    backward_references_count: count,
                    element_flags: None,
                },
                Element::BidirectionalReference {
                    reference: Reference::SiblingReference {
                        sibling_key: b"target".to_vec(),
                        element_flags: None,
                    },
                    cascade_on_update: true,
                    backward_references_count: count,
                },
            ];
            for element in elements {
                let json = serde_json::to_value(&element).expect("serialize");
                let fields = json.as_object().unwrap().values().next().unwrap();
                assert_eq!(
                    fields["backward_references_count"],
                    serde_json::json!(count)
                );
                assert_eq!(serde_json::from_value::<Element>(json).unwrap(), element);
            }
        }
    }
}
