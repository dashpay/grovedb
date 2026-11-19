//! The hash bridge: Merk's hash rules for the hashes a tree page stores,
//! and which hashes of a node Merk holds when it writes the node.
//!
//! A cell stores four hashes of its node: the value hash, the key-value
//! hash and the hash of each child (`tree-page.md` TP9). They are the
//! hashes Merk holds, byte for byte, and Merk makes them: on existing
//! GroveVersions a commit hashes its tree to the root before it writes a
//! node. The tree layer can also make a hash that its writer left out, on
//! demand and once (`lignin_tree::hash`). It has no rule of its own for
//! that. This module gives it Merk's.
//!
//! # The rules
//!
//! [`MerkHashRules`] implements [`HashRules`] with the functions of
//! `merk::tree::hash`, so a hash the engine makes is the hash Merk would
//! have made:
//!
//! | Stored hash | Made as |
//! | --- | --- |
//! | key-value hash | `kv_digest_to_kv_hash(key, value_hash)` |
//! | hash of a child, root hash of a Merk | the node hash of the cell: `node_hash` of its key-value hash and the hashes of its children, with the node's aggregate in it where its feature is a provable one, as `TreeNode::commit` hashes a child |
//! | value hash | by the element the value is, see below |
//!
//! The aggregate of a node is made by `TreeNode::aggregate_data` from the
//! node's feature and the aggregates in its links, which a cell holds as
//! Merk does (`tree-page.md` TP12). The rule builds a node of Merk for it
//! with those parts and nothing else, one allocation for each hash, as the
//! first version of the store builds a node for each fetch.
//!
//! **The node hash goes by the node's feature**, as `TreeNode::commit` does
//! for the link of a parent. `Merk::root_hash` goes by the tree type of the
//! Merk. The two agree where every node of a tree has the feature of the
//! tree's type, which GroveDB sees to (`Element::get_feature_type`), and a
//! cell does not say what tree it is in.
//!
//! **The value hash goes by the element.** Merk is told how to make a
//! value hash by the operation that puts the value (`Op`), and GroveDB
//! chooses that by the element. A cell holds the value, so the rule reads
//! the element's type from it, as Merk's proofs do
//! (`ElementType::from_serialized_value`):
//!
//! | The value is | Its value hash | Merk's operation |
//! | --- | --- | --- |
//! | an item, a sum item, an item with a sum | `value_hash(value)` | `Put`, `PutWithSpecializedCost` |
//! | an item that takes backward references | `value_hash` of its serialization with the list of referrers emptied | `PutWithProvidedValueHash` |
//! | a tree whose data is a Merk | `combine_hash(value_hash(value), root)` with the root hash of the Merk it hosts | `PutLayeredReference` |
//! | an indexed tree with one secondary | `combine_hash_three(value_hash(value), primary, secondary)` | `PutLayeredCountIndexedReference` |
//! | an indexed tree with a list of axes | `combine_hash_three(value_hash(value), primary, axes_digest(axes))` | the same |
//! | a reference of any kind | **refused**: it is made from the value hash of the element the reference ends at, which no rule is given | `PutCombinedReference`, `PutWithProvidedValueHash` |
//! | a tree whose data is no Merk (MMR, bulk append, dense, commitment, private documents) | **refused**: it is made from the state root of that structure | `PutLayeredReference` |
//! | no element | **refused** | |
//!
//! The root hash of a hosted Merk is what the tree layer hands the rule
//! for each Merk the cell hosts, by position: the primary Merk first, then
//! the secondaries in the order the element lists them. A hosted Merk with
//! no root has the hash of an empty Merk, `NULL_HASH`, and so has a
//! position the cell has no pointer for: a cell points at the root of
//! every Merk it hosts that has one (`tree-layout.md` C3, `tree-page.md`
//! TP15), and the cell of a tree whose Merk never had a node hosts none.
//! A cell that hosts more Merks than its element has is refused.
//!
//! **A tree's value hash is the one a write below it gives it.** The value
//! hash of a tree element goes stale when a Merk it hosts gets another
//! root, and Merk then puts the element with the combined hash, on every
//! GroveVersion. That is the hash the rule makes. The insert of the element
//! itself is another matter, and its hash is its writer's to state:
//! `GROVE_V1` and `GROVE_V2` insert a count sum tree, a count big sum tree,
//! a provable count tree and a provable count sum tree outside a batch as
//! plain values (`add_element_on_transaction` v0, frozen for the chain
//! that was built with it). Such an element has `value_hash(value)` until
//! something is written below it, and the combined hash from then on, also
//! once its Merk is empty again, with the same bytes. So for these four
//! types the bytes and the roots do not say which of the two an element
//! with no root has ([`MerkHashRules::value_hash_at_insert`]). Whoever
//! copies existing nodes, a load or a migration, hands over the value
//! hashes they have.
//!
//! A refused hash stays marked, and its evaluation stops
//! ([`HashRefusal`]). A commit whose evaluation stops has failed, and its
//! transaction is ended (`transactions.md` CM6). So the writer of such a
//! value states its hash and never marks it, which is what the store does
//! for every value hash.
//!
//! The rules are those of a Merk whose values are elements of GroveDB. A
//! Merk that holds other bytes makes its own hashes: asked for the value
//! hash of bytes that happen to read as an element, the rule answers for
//! that element.
//!
//! # Checking
//!
//! [`lignin_tree::hash::check`] recomputes every hash a cell stores and
//! reports those that differ. A hash that the rules refuse cannot be
//! recomputed, and one refusal would end the check of its cell.
//! [`MerkHashRules::for_check`] answers such a value hash with the bytes
//! the cell stores, so the check reports nothing for it and still compares
//! the key-value hash and the hashes of the children. It answers the same
//! way for a tree with no root whose cell stores the hash its insert gave
//! it as a plain value, which is as right as the combined one. It is for
//! a check only: it does not make those hashes, so an evaluation with it
//! would leave a stale value hash in its cell and take the mark off.
//!
//! # What Merk holds when it writes a node
//!
//! A write states for each stored hash whether its writer made it
//! ([`NodePut::stale`](lignindb::txn::NodePut::stale)). The state of a link
//! says it for the hash of a child:
//!
//! | `Link` | Merk holds the child's hash | The hash in the cell |
//! | --- | --- | --- |
//! | `Reference` | yes: the node was pruned with it | stated |
//! | `Loaded` | yes | stated |
//! | `Uncommitted` | yes: the node changed since the last commit and was hashed since | stated |
//! | `Modified` | no | stale: left to the engine |
//!
//! A value hash and a key-value hash are always held: a node of Merk is
//! made with both (`KV`), whatever operation puts its value.
//!
//! On existing GroveVersions no node is written with a modified link:
//! `TreeNode::commit` hashes every modified child before the node above it
//! is written. So every hash is stated, nothing is marked and no rule runs
//! at the commit. A writer that stops hashing below the root, as iteration
//! two does at a stop point, hands over modified links, and the store then
//! writes what Merk holds: the link with the child's heights and its
//! aggregate, which are no hashes and are exact in every write, and no
//! hash. The aggregate of a modified child is made from the child in
//! memory, by `TreeNode::aggregate_data` as in a commit.
//!
//! The other way round, a hash that is stale in the transaction is no hash
//! to hand to Merk: a `Link` has no state for a child that is not in
//! memory and whose hash is unknown. The store refuses to read a node with
//! a stale hash ([`refuse_stale`]) until the hash is evaluated.

#[cfg(test)]
mod tests;

use std::{error::Error as StdError, fmt};

use grovedb_costs::OperationCost;
use grovedb_element::{Element, ElementType};
use grovedb_version::version::GroveVersion;
use lignin_format::tree_page::{Hash, HashField, Side};
use lignin_tree::{
    hash::{HashRules, Refusal, Stale},
    node::NodeRef,
};

use super::{
    feature::{decode_aggregate, decode_feature},
    refused,
};
use crate::{
    tree::{
        axes_digest, combine_hash, combine_hash_three, kv::KV, kv_digest_to_kv_hash, node_hash,
        node_hash_with_big_sum, node_hash_with_count, node_hash_with_count_and_big_sum,
        node_hash_with_count_and_sum, node_hash_with_sum, value_hash, AggregateData, TreeNode,
        TreeNodeInner, NULL_HASH,
    },
    CryptoHash, Error, Link, TreeFeatureType,
};

/// Why a value hash is its writer's to state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatedByWriter {
    /// The value is a reference: its value hash is made from the value
    /// hash of the element the reference ends at.
    Reference,
    /// The value is a tree whose data is no Merk: its value hash is made
    /// from the state root of that structure.
    NonMerkTree,
}

/// Why [`MerkHashRules`] did not make a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashRefusal {
    /// The value hash is made from something that no cell holds. Its
    /// writer states it and never marks it stale.
    StatedByWriter(StatedByWriter),
    /// The value is no element of GroveDB, or one whose value hash these
    /// rules do not know.
    NotAnElement(String),
    /// The cell hosts more Merks than its element has.
    Hosted {
        /// The Merks the element has.
        element: usize,
        /// The Merks the cell hosts.
        cell: usize,
    },
    /// The cell holds what is no node of Merk: a feature or an aggregate
    /// that does not decode, or aggregates that do not add up.
    Node(String),
}

impl fmt::Display for HashRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StatedByWriter(StatedByWriter::Reference) => f.write_str(
                "the value hash of a reference is made from the element it ends at: its writer \
                 states it",
            ),
            Self::StatedByWriter(StatedByWriter::NonMerkTree) => f.write_str(
                "the value hash of a tree whose data is no Merk is made from that data: its \
                 writer states it",
            ),
            Self::NotAnElement(why) => {
                write!(f, "the value is no element with a known value hash: {why}")
            }
            Self::Hosted { element, cell } => write!(
                f,
                "the element has {element} Merks and its cell hosts {cell}"
            ),
            Self::Node(why) => write!(f, "the cell holds no node of Merk: {why}"),
        }
    }
}

impl StdError for HashRefusal {}

/// How the value hash of an element is made.
enum ValueHash {
    /// From the value's bytes.
    Plain,
    /// From the element's serialization with its referrers taken out.
    Stripped,
    /// From the value's bytes and the root hash of one Merk.
    Tree,
    /// As [`Self::Tree`], for a type that `GROVE_V1` and `GROVE_V2` insert
    /// as a plain value.
    TreeInsertedPlain,
    /// From the value's bytes and the root hashes of two Merks.
    Indexed,
    /// From the value's bytes, the root hash of the primary Merk and the
    /// digest of the element's axes.
    IndexedByAxes,
}

impl ValueHash {
    fn of(value: &[u8]) -> Result<Self, HashRefusal> {
        use ElementType::*;
        let element = ElementType::from_serialized_value(value)
            .map_err(|error| HashRefusal::NotAnElement(error.to_string()))?;
        if element.is_reference() {
            return Err(HashRefusal::StatedByWriter(StatedByWriter::Reference));
        }
        if element.is_backward_references_item() {
            return Ok(Self::Stripped);
        }
        if element.has_simple_value_hash() {
            return Ok(Self::Plain);
        }
        if !element.is_tree() {
            return Err(HashRefusal::NotAnElement(format!(
                "no rule for {}",
                element.as_str()
            )));
        }
        Ok(match element.base() {
            CommitmentTree
            | MmrTree
            | BulkAppendTree
            | DenseAppendOnlyFixedSizeTree
            | PrivateDocumentStore => {
                return Err(HashRefusal::StatedByWriter(StatedByWriter::NonMerkTree))
            }
            ProvableCountProvableSumIndexedTree | ProvableCountProvableBigSumIndexedTree => {
                Self::IndexedByAxes
            }
            _ if element.is_indexed_tree() => Self::Indexed,
            CountSumTree | CountBigSumTree | ProvableCountTree | ProvableCountSumTree => {
                Self::TreeInsertedPlain
            }
            _ => Self::Tree,
        })
    }
}

/// A node of Merk with what its aggregate and its hash are made from, and
/// nothing else: no key, no value and no child in memory.
fn bare_node(
    feature: TreeFeatureType,
    kv_hash: CryptoHash,
    links: [Option<(CryptoHash, AggregateData)>; 2],
) -> TreeNode {
    let [left, right] = links.map(|link| {
        link.map(|(hash, aggregate_data)| Link::Reference {
            hash,
            child_heights: (0, 0),
            key: Vec::new(),
            aggregate_data,
        })
    });
    TreeNode::new_with_tree_inner(TreeNodeInner {
        marks: Default::default(),
        left,
        right,
        kv: KV::from_fields(Vec::new(), Vec::new(), kv_hash, NULL_HASH, feature),
    })
}

/// The hash of `node`, whose links hold their hashes: what the link of its
/// parent stores, as `TreeNode::commit` makes it.
fn hash_of(node: &TreeNode, cost: &mut OperationCost) -> Result<CryptoHash, Error> {
    use AggregateData::*;
    let (kv, left, right) = (
        node.kv_hash(),
        node.child_hash(true),
        node.child_hash(false),
    );
    let hash = match node.aggregate_data()? {
        ProvableCount(count) | ProvableCountAndSum(count, _) => {
            node_hash_with_count(kv, left, right, count)
        }
        ProvableSum(sum) => node_hash_with_sum(kv, left, right, sum),
        ProvableCountAndProvableSum(count, sum) => {
            node_hash_with_count_and_sum(kv, left, right, count, sum)
        }
        ProvableCountAndProvableBigSum(count, sum) => {
            node_hash_with_count_and_big_sum(kv, left, right, count, sum)
        }
        ProvableBigSum(sum) => node_hash_with_big_sum(kv, left, right, sum),
        NoAggregateData | Sum(_) | BigSum(_) | Count(_) | CountAndSum(..) | CountAndBigSum(..) => {
            node_hash(kv, left, right)
        }
    };
    Ok(hash.unwrap_add_cost(cost))
}

/// Merk's hash rules, for the hashes the tree layer makes.
///
/// It keeps the cost of the hashes it made, as Merk's hash functions count
/// them ([`Self::cost`]). Nothing is charged for it here: what a write is
/// charged for its hashes is its cost rules' to say.
#[derive(Debug, Clone)]
pub struct MerkHashRules<'v> {
    grove_version: &'v GroveVersion,
    /// Whether a value hash that only its writer can make is answered with
    /// the stored one.
    for_check: bool,
    cost: OperationCost,
}

impl<'v> MerkHashRules<'v> {
    /// The rules for an evaluation, a load or a commit, with elements read
    /// as `grove_version` reads them. A value hash that only its writer
    /// can make is refused.
    pub fn new(grove_version: &'v GroveVersion) -> Self {
        Self {
            grove_version,
            for_check: false,
            cost: OperationCost::default(),
        }
    }

    /// The rules for [`lignin_tree::hash::check`]: a value hash that only
    /// its writer can make is the one the cell stores, so that the check
    /// reports nothing for it and goes on to the cell's other hashes.
    ///
    /// Never for an evaluation: it makes no such hash.
    pub fn for_check(grove_version: &'v GroveVersion) -> Self {
        Self {
            for_check: true,
            ..Self::new(grove_version)
        }
    }

    /// The cost of the hashes these rules made: the calls of the hash
    /// function, as `merk::tree::hash` counts them.
    pub const fn cost(&self) -> &OperationCost {
        &self.cost
    }

    /// The value hash of an element with the bytes `value` that hosts the
    /// Merks whose root hashes are `hosted`, by position.
    pub fn element_value_hash(
        &mut self,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<CryptoHash, HashRefusal> {
        let how = ValueHash::of(value)?;
        // The cell hosts no more Merks than the element has.
        let hosts = |element: usize| match hosted.len() <= element {
            true => Ok(()),
            false => Err(HashRefusal::Hosted {
                element,
                cell: hosted.len(),
            }),
        };
        // The root hash of the Merk at a position: of an empty Merk where
        // it has no root, or the cell no pointer.
        let root = |position: usize| match hosted.get(position) {
            Some(Some(root)) => root,
            Some(None) | None => &NULL_HASH,
        };
        let mut cost = OperationCost::default();
        let own = |bytes: &[u8], cost: &mut OperationCost| value_hash(bytes).unwrap_add_cost(cost);
        let made = match how {
            ValueHash::Plain => {
                hosts(0)?;
                own(value, &mut cost)
            }
            ValueHash::Stripped => {
                hosts(0)?;
                let stripped = self.element(value)?.stripped_of_backward_references();
                let stripped = stripped
                    .serialize(self.grove_version)
                    .map_err(|error| HashRefusal::NotAnElement(error.to_string()))?;
                own(&stripped, &mut cost)
            }
            ValueHash::Tree | ValueHash::TreeInsertedPlain => {
                hosts(1)?;
                let own = own(value, &mut cost);
                combine_hash(&own, root(0)).unwrap_add_cost(&mut cost)
            }
            ValueHash::Indexed => {
                hosts(2)?;
                let own = own(value, &mut cost);
                combine_hash_three(&own, root(0), root(1)).unwrap_add_cost(&mut cost)
            }
            ValueHash::IndexedByAxes => {
                let element = self.element(value)?;
                let axes = element.axes().unwrap_or_default();
                hosts(1 + axes.len())?;
                let axes: Vec<(u8, CryptoHash)> = (axes.iter().enumerate())
                    .map(|(at, (tag, _))| (*tag, *root(1 + at)))
                    .collect();
                let own = own(value, &mut cost);
                let digest = axes_digest(&axes).unwrap_add_cost(&mut cost);
                combine_hash_three(&own, root(0), &digest).unwrap_add_cost(&mut cost)
            }
        };
        self.cost += cost;
        Ok(made)
    }

    /// The value hash an element with the bytes `value` has from its
    /// insert until something is written below it, where that is not the
    /// hash [`Self::element_value_hash`] makes: `value_hash(value)` for a
    /// tree of one of the four types that `GROVE_V1` and `GROVE_V2` insert
    /// as a plain value, and `None` for every other value.
    ///
    /// A cell of such a tree that hosts no root stores this hash or the
    /// combined one, and both are right.
    pub fn value_hash_at_insert(&mut self, value: &[u8]) -> Option<CryptoHash> {
        match ValueHash::of(value) {
            Ok(ValueHash::TreeInsertedPlain) => {
                Some(value_hash(value).unwrap_add_cost(&mut self.cost))
            }
            _ => None,
        }
    }

    fn element(&self, value: &[u8]) -> Result<Element, HashRefusal> {
        Element::deserialize(value, self.grove_version)
            .map_err(|error| HashRefusal::NotAnElement(error.to_string()))
    }

    /// The hash of the node a cell holds.
    fn cell_hash(&mut self, node: NodeRef<'_>) -> Result<CryptoHash, Error> {
        let feature = decode_feature(node.feature())?;
        let mut links = [None, None];
        for (side, link) in Side::BOTH.into_iter().zip(&mut links) {
            if let Some(stored) = node.link(side) {
                *link = Some((*stored.hash, decode_aggregate(stored.aggregate)?));
            }
        }
        hash_of(&bare_node(feature, *node.kv_hash(), links), &mut self.cost)
    }
}

impl HashRules for MerkHashRules<'_> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        let stored = node.value_hash();
        match self.element_value_hash(value, hosted) {
            Err(HashRefusal::StatedByWriter(_)) if self.for_check => Ok(*stored),
            Ok(made) if self.for_check && made != *stored => {
                // A tree with no root may have the hash of its insert.
                let no_root = hosted.iter().all(Option::is_none);
                let at_insert = no_root && self.value_hash_at_insert(value) == Some(*stored);
                Ok(if at_insert { *stored } else { made })
            }
            made => Ok(made?),
        }
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        Ok(kv_digest_to_kv_hash(key, value_hash).unwrap_add_cost(&mut self.cost))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        Ok(self
            .cell_hash(node)
            .map_err(|error| HashRefusal::Node(error.to_string()))?)
    }
}

/// A link of a node as Merk hands it to the store.
pub(super) struct HeldLink {
    /// The child's hash, or `None` where Merk holds none: the link is
    /// modified, and the hash is left to the engine.
    pub(super) hash: Option<CryptoHash>,
    /// The heights of the child's left and right subtree.
    pub(super) heights: (u8, u8),
    /// The child's aggregate, which is exact in every write.
    pub(super) aggregate: AggregateData,
}

impl HeldLink {
    /// The length of the link in Merk's record of its node, for a child
    /// with a key of `key_len` bytes: the hash takes its bytes whether or
    /// not Merk holds it yet.
    pub(super) fn record_len(&self, key_len: usize) -> Result<usize, Error> {
        use ed::Encode;

        let link = Link::Reference {
            hash: NULL_HASH,
            child_heights: self.heights,
            key: vec![0; key_len],
            aggregate_data: self.aggregate,
        };
        link.encoding_length().map_err(Error::EdError)
    }
}

/// What Merk holds of `link`.
pub(super) fn held_link(link: &Link) -> Result<HeldLink, Error> {
    Ok(match link {
        Link::Modified {
            tree,
            child_heights,
            ..
        } => HeldLink {
            hash: None,
            heights: *child_heights,
            aggregate: aggregate_of(tree)?,
        },
        Link::Reference {
            hash,
            child_heights,
            aggregate_data,
            ..
        }
        | Link::Uncommitted {
            hash,
            child_heights,
            aggregate_data,
            ..
        }
        | Link::Loaded {
            hash,
            child_heights,
            aggregate_data,
            ..
        } => HeldLink {
            hash: Some(*hash),
            heights: *child_heights,
            aggregate: *aggregate_data,
        },
    })
}

/// The aggregate of `node`, also where a child of it is modified and has
/// no aggregate in its link yet: that child's is made first, from the
/// child in memory.
fn aggregate_of(node: &TreeNode) -> Result<AggregateData, Error> {
    let modified = |left| matches!(node.link(left), Some(Link::Modified { .. }));
    if !modified(true) && !modified(false) {
        return node.aggregate_data();
    }
    let mut links = [None, None];
    for (left, link) in [true, false].into_iter().zip(&mut links) {
        if let Some(below) = node.link(left) {
            *link = Some((NULL_HASH, held_link(below)?.aggregate));
        }
    }
    bare_node(node.feature_type(), NULL_HASH, links).aggregate_data()
}

/// The hashes of `node` that Merk holds none of when it writes the node:
/// the hash of each child whose link is modified.
pub(super) fn stale_hashes(node: &TreeNode) -> Stale {
    let sides = [(true, Side::Left), (false, Side::Right)].into_iter();
    sides
        .filter(|(left, _)| matches!(node.link(*left), Some(Link::Modified { .. })))
        .map(|(_, side)| HashField::Child(side))
        .collect()
}

/// Refuses a node that was read with a stale hash: the bytes its cell
/// stores there are no hash, and Merk would take them for one.
pub(super) fn refuse_stale(stale: Stale) -> Result<(), Error> {
    match stale.is_empty() {
        true => Ok(()),
        false => Err(refused(&format!(
            "a node was read whose hashes are not all made yet: {stale:?}"
        ))),
    }
}
