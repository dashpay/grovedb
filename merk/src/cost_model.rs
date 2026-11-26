//! The engine-neutral cost model, as Merk and the stores under it reach it.
//!
//! What the storage layer charges is defined by the functions of
//! [`grovedb_costs::cost_model`], which are re-exported here. Each takes
//! logical sizes and returns an `OperationCost`. A store that keeps Merk's
//! nodes for an engine charges through them and works out no charge of its
//! own. The storage contexts under [`store::kv`](crate::store::kv) do so,
//! and so does the store of LigninDB (`store::lignin`), which keeps no
//! record of a node.
//!
//! This module adds what only Merk knows: which version of those rules a
//! grove version and a handle charge by, and the sizes of a node as the
//! rules take them, for a node Merk holds ([`node_record_len`]) and for one
//! a store holds as its parts ([`node_record_len_from_sizes`]). Merk's own
//! cost rules, which decide what a node's key and value cost and which
//! nodes are fetched, stay where they are, in [`tree`](crate::tree).
//!
//! It also has the rules that say which operation carries a changed Merk
//! into the element that hosts it, and is charged for it
//! ([`PropagationCostModel`]). They move no charge of the storage layer or
//! of a node: they say which operation makes it.
//!
//! The rules are specified in `docs/lignindb/cost-model.md`.

mod v0;
mod v1;

use ed::Encode;
pub use grovedb_costs::cost_model::{
    BatchCharge, ImmediateBatchCharge, Keyspace, StorageCostModel, PREFIX_LEN,
};
use grovedb_costs::OperationCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    tree::{AggregateData, TreeNode},
    tree_type::TreeType,
    Error, Link, TreeFeatureType,
};

/// The version of the storage layer's cost rules that a subtree handle
/// charges by.
///
/// A handle is opened with no grove version, on every engine: neither the
/// storage contexts of `grovedb-storage` nor the handles of `store::lignin`
/// are given one, and most operations of a
/// [`NodeStore`](crate::store::NodeStore) take none. So a handle charges by
/// one version, this one, and that is right while
/// [`storage_cost_model`] gives it for every grove version. A grove version
/// that charges by other storage rules has to reach the handles first.
pub const HANDLE_COST_MODEL: StorageCostModel = StorageCostModel::V0;

/// The version of the storage layer's cost rules that `grove_version`
/// charges by.
///
/// Grove versions 1 to 6 charge by the first, which is the one a handle
/// charges by ([`HANDLE_COST_MODEL`]). Grove version 6 changes which
/// operation makes a charge ([`PropagationCostModel`]) and no charge of
/// the storage layer.
pub fn storage_cost_model(_grove_version: &GroveVersion) -> StorageCostModel {
    StorageCostModel::V0
}

/// The version of the rules that say which operation carries a changed
/// Merk into the element that hosts it, and is charged for it
/// (`cost-model.md`, section 16).
///
/// To carry a Merk is to write its root key, its root hash and its
/// aggregate into its host. Every rule is a method, and a caller holds a
/// `PropagationCostModel` and names no version at a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropagationCostModel {
    /// Grove versions 1 to 5. A write carries every Merk it changed to the
    /// root of the grove before it returns, and is charged every level on
    /// the way. There is no mark and no finalize.
    V0,
    /// Grove version 6. A write stops at its stop points and carries
    /// nothing above them, so it is charged for no level there. A finalize
    /// carries every marked Merk once, and its charge is the cost of the
    /// finalize call.
    V1,
}

/// The operation that carries a changed Merk into its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// The write that changed the Merk, before it returns.
    Write,
    /// A finalize, after the write.
    Finalize,
}

/// The version of the propagation rules that `grove_version` charges by.
pub fn propagation_cost_model(grove_version: &GroveVersion) -> Result<PropagationCostModel, Error> {
    match grove_version.merk_versions.cost_model.propagation {
        0 => Ok(PropagationCostModel::V0),
        1 => Ok(PropagationCostModel::V1),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "propagation_cost_model".to_string(),
                known_versions: vec![0, 1],
                received: version,
            },
        )),
    }
}

impl PropagationCostModel {
    /// The operation that carries a subtree above a stop point, and is
    /// charged for it (SP1, FZ1).
    pub fn carrier(self) -> Carrier {
        match self {
            Self::V0 => v0::carrier(),
            Self::V1 => v1::carrier(),
        }
    }

    /// SP2: what a write is charged for the host of a Merk it opens to
    /// change it. `read` is what the read of the host is charged where it is
    /// made: the prefix of the host's path, and the read of the element.
    ///
    /// The first version charges it as it is made. The second charges
    /// nothing, whether or not the code makes the read.
    pub fn host_read_at_open(self, read: OperationCost) -> OperationCost {
        match self {
            Self::V0 => v0::host_read_at_open(read),
            Self::V1 => v1::host_read_at_open(read),
        }
    }

    /// SP5: the stop point of a subtree that a write changed.
    ///
    /// `merks_on_the_path` has the tree type of every subtree on the way to
    /// the changed one, the root Merk first and the changed subtree last.
    /// The answer is the place in it of the subtree the write carries the
    /// change into and stops at. On the first version that is the root
    /// Merk. On the second it is the changed subtree itself, or, where an
    /// indexed tree is on the way, the primary of the outermost one.
    pub fn stop_point(self, merks_on_the_path: &[TreeType]) -> usize {
        match self {
            Self::V0 => v0::stop_point(merks_on_the_path),
            Self::V1 => v1::stop_point(merks_on_the_path),
        }
    }

    /// SP3: what a write is charged for the mark it leaves on the host of a
    /// stop point. The first version has no mark.
    pub fn mark(self) -> Result<OperationCost, Error> {
        match self {
            Self::V0 => v0::mark(),
            Self::V1 => v1::mark(),
        }
    }

    /// SP6: what a write that removes or replaces a host is charged for
    /// taking away the mark on it and every mark under it. The first
    /// version has no mark.
    pub fn marks_under_a_removed_host(self) -> Result<OperationCost, Error> {
        match self {
            Self::V0 => v0::marks_under_a_removed_host(),
            Self::V1 => v1::marks_under_a_removed_host(),
        }
    }

    /// FZ1: the charge of a finalize, with nothing in it yet. The first
    /// version has no finalize.
    pub fn finalize(self) -> Result<FinalizeCharge, Error> {
        match self {
            Self::V0 => v0::finalize(),
            Self::V1 => v1::finalize(),
        }
    }
}

/// The charge of one finalize, as it adds up: the cost of the finalize call
/// and of no other operation (FZ1).
#[derive(Debug)]
pub struct FinalizeCharge {
    cost: OperationCost,
}

impl FinalizeCharge {
    fn new() -> Self {
        FinalizeCharge {
            cost: OperationCost::default(),
        }
    }

    /// FZ2 and FZ4: adds the cost of an event the finalize performed, as the
    /// rules of the storage layer and of Merk charge it.
    ///
    /// A finalize rewrites elements whose value cost it does not change, so
    /// an event that adds or removes bytes is refused, and nothing of it is
    /// added: such bytes would be charged to no write and carry no flags.
    pub fn event(&mut self, cost: OperationCost) -> Result<(), Error> {
        if cost.storage_cost.added_bytes != 0 || cost.storage_cost.removed_bytes.has_removal() {
            return Err(Error::CorruptedCodeExecution(
                "an event of a finalize added or removed bytes",
            ));
        }
        self.cost += cost;
        Ok(())
    }

    /// FZ1: the cost of the finalize call.
    pub fn finish(self) -> OperationCost {
        self.cost
    }
}

/// SP4: what a write is charged for reaching `node` where an earlier write
/// left it loaded: the fetch it would have made, one read of the node's
/// record.
pub fn fetch_as_not_loaded(node: &TreeNode) -> Result<OperationCost, Error> {
    Ok(HANDLE_COST_MODEL.read(Some(node_record_len(HANDLE_COST_MODEL, node)?)))
}

/// The record length of `node`: the number of bytes its encoding takes,
/// from the lengths of its parts. It is what a fetch of the node loads,
/// whatever a store keeps of it.
///
/// The feature of the node and each of its links are measured by their own
/// encoders. A kind of feature or of aggregate that Merk gains is therefore
/// measured as Merk encodes it, with no list of kinds to keep here.
///
/// A node that cannot be encoded has no record length: one with a link
/// that is modified and not yet committed.
pub fn node_record_len(model: StorageCostModel, node: &TreeNode) -> Result<usize, Error> {
    let feature_len = node
        .feature_type()
        .encoding_length()
        .map_err(Error::EdError)?;
    let link_len = |left: bool| {
        node.link(left)
            .map(Encode::encoding_length)
            .transpose()
            .map_err(Error::EdError)
    };
    Ok(model.node_record_len(
        node.value_as_slice().len(),
        feature_len,
        link_len(true)?,
        link_len(false)?,
    ))
}

/// The record length of a node that a store holds as its parts and not as
/// a record: the number of bytes Merk's record of the node takes, with no
/// record encoded (`cost-model.md` N1).
///
/// `value_len` is the length of the node's value. `left_child` and
/// `right_child` are, for each child, the length of its key and the
/// aggregate of the subtree under it, which is all of a link that its
/// length depends on.
///
/// The feature and the aggregates are measured by Merk's own encoders, as
/// in [`node_record_len`], so a kind Merk gains is measured as Merk encodes
/// it. A link encodes the key of its child after one length byte, so its
/// length is that of a link to a child with an empty key and the length of
/// the key.
pub fn node_record_len_from_sizes(
    model: StorageCostModel,
    value_len: usize,
    feature: &TreeFeatureType,
    left_child: Option<(u8, AggregateData)>,
    right_child: Option<(u8, AggregateData)>,
) -> Result<usize, Error> {
    let feature_len = feature.encoding_length().map_err(Error::EdError)?;
    let link_len = |child: Option<(u8, AggregateData)>| {
        child
            .map(|(key_len, aggregate_data)| {
                let keyless = Link::Reference {
                    hash: [0; 32],
                    child_heights: (0, 0),
                    key: Vec::new(),
                    aggregate_data,
                };
                keyless
                    .encoding_length()
                    .map(|keyless_len| usize::from(key_len) + keyless_len)
                    .map_err(Error::EdError)
            })
            .transpose()
    };
    Ok(model.node_record_len(
        value_len,
        feature_len,
        link_len(left_child)?,
        link_len(right_child)?,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ed::Decode;
    use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
    use grovedb_version::version::{v5::GROVE_V5, v6::GROVE_V6, GROVE_VERSIONS};

    /// The rules a grove version charges by.
    fn rules_of(grove_version: &GroveVersion) -> PropagationCostModel {
        propagation_cost_model(grove_version).expect("a known version of the rules")
    }

    use super::*;
    use crate::{
        tree::AggregateData,
        Link,
        TreeFeatureType::{self, *},
    };

    const MODEL: StorageCostModel = StorageCostModel::V0;
    const COUNTS: [u64; 5] = [0, 127, 128, 70_000, u64::MAX];
    const SUMS: [i64; 6] = [0, -1, 63, -65, i64::MAX, i64::MIN];
    const BIG_SUMS: [i128; 3] = [0, -7, i128::MAX];

    fn with_counts_and<S: Copy>(
        sums: &[S],
        make: fn(u64, S) -> TreeFeatureType,
    ) -> Vec<TreeFeatureType> {
        COUNTS
            .iter()
            .flat_map(|count| sums.iter().map(move |sum| make(*count, *sum)))
            .collect()
    }

    /// The values of one kind of feature to measure, and the kind to go on
    /// with. Every kind is named, so a kind Merk gains does not compile
    /// until it is given values here.
    fn values_and_next_kind(
        kind: TreeFeatureType,
    ) -> (Vec<TreeFeatureType>, Option<TreeFeatureType>) {
        match kind {
            BasicMerkNode => (vec![BasicMerkNode], Some(SummedMerkNode(0))),
            SummedMerkNode(_) => (
                SUMS.map(SummedMerkNode).to_vec(),
                Some(BigSummedMerkNode(0)),
            ),
            BigSummedMerkNode(_) => (
                BIG_SUMS.map(BigSummedMerkNode).to_vec(),
                Some(CountedMerkNode(0)),
            ),
            CountedMerkNode(_) => (
                COUNTS.map(CountedMerkNode).to_vec(),
                Some(CountedSummedMerkNode(0, 0)),
            ),
            CountedSummedMerkNode(..) => (
                with_counts_and(&SUMS, CountedSummedMerkNode),
                Some(ProvableCountedMerkNode(0)),
            ),
            ProvableCountedMerkNode(_) => (
                COUNTS.map(ProvableCountedMerkNode).to_vec(),
                Some(ProvableCountedSummedMerkNode(0, 0)),
            ),
            ProvableCountedSummedMerkNode(..) => (
                with_counts_and(&SUMS, ProvableCountedSummedMerkNode),
                Some(ProvableSummedMerkNode(0)),
            ),
            ProvableSummedMerkNode(_) => (
                SUMS.map(ProvableSummedMerkNode).to_vec(),
                Some(ProvableCountedAndProvableSummedMerkNode(0, 0)),
            ),
            ProvableCountedAndProvableSummedMerkNode(..) => (
                with_counts_and(&SUMS, ProvableCountedAndProvableSummedMerkNode),
                Some(ProvableCountedAndProvableBigSummedMerkNode(0, 0)),
            ),
            ProvableCountedAndProvableBigSummedMerkNode(..) => (
                with_counts_and(&BIG_SUMS, ProvableCountedAndProvableBigSummedMerkNode),
                Some(CountedBigSummedMerkNode(0, 0)),
            ),
            CountedBigSummedMerkNode(..) => (
                with_counts_and(&BIG_SUMS, CountedBigSummedMerkNode),
                Some(ProvableBigSummedMerkNode(0)),
            ),
            ProvableBigSummedMerkNode(_) => {
                (BIG_SUMS.map(ProvableBigSummedMerkNode).to_vec(), None)
            }
        }
    }

    fn features() -> Vec<TreeFeatureType> {
        std::iter::successors(Some(values_and_next_kind(BasicMerkNode)), |(_, next)| {
            next.map(values_and_next_kind)
        })
        .flat_map(|(values, _)| values)
        .collect()
    }

    fn link(key_len: usize, aggregate_of: TreeFeatureType) -> Link {
        Link::Reference {
            hash: [3; 32],
            child_heights: (1, 2),
            key: vec![0xCD; key_len],
            aggregate_data: AggregateData::from(aggregate_of),
        }
    }

    fn node(
        value_len: usize,
        feature: TreeFeatureType,
        left: Option<Link>,
        right: Option<Link>,
    ) -> TreeNode {
        TreeNode::from_fields(
            b"key".to_vec(),
            vec![0x5A; value_len],
            [7; 32],
            left,
            right,
            feature,
        )
        .unwrap()
    }

    /// The grove versions in force, and the one that is defined for stop
    /// points.
    fn defined_versions() -> impl Iterator<Item = &'static GroveVersion> {
        GROVE_VERSIONS.iter().chain([&GROVE_V6])
    }

    fn seeks(seek_count: u32) -> OperationCost {
        OperationCost {
            seek_count,
            ..Default::default()
        }
    }

    #[test]
    fn every_grove_version_charges_by_the_first_storage_rules() {
        for grove_version in defined_versions() {
            assert_eq!(storage_cost_model(grove_version), StorageCostModel::V0);
        }
    }

    #[test]
    fn a_handle_charges_by_the_rules_of_every_grove_version() {
        for grove_version in defined_versions() {
            assert_eq!(storage_cost_model(grove_version), HANDLE_COST_MODEL);
        }
    }

    #[test]
    fn sp1_a_write_carries_to_the_root_on_grove_versions_1_to_5_and_stops_on_6() {
        for grove_version in GROVE_VERSIONS {
            let model = propagation_cost_model(grove_version).expect("a known version");
            assert_eq!(model, PropagationCostModel::V0);
            assert_eq!(model.carrier(), Carrier::Write);
        }
        let model = propagation_cost_model(&GROVE_V6).expect("a known version");
        assert_eq!(model, PropagationCostModel::V1);
        assert_eq!(model.carrier(), Carrier::Finalize);
    }

    #[test]
    fn a_propagation_version_that_is_not_known_is_refused() {
        let mut unknown = GROVE_V6.clone();
        unknown.merk_versions.cost_model.propagation = 2;
        assert!(matches!(
            propagation_cost_model(&unknown),
            Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    received: 2,
                    ref known_versions,
                    ..
                }
            )) if known_versions == &[0, 1]
        ));
    }

    #[test]
    fn sp2_a_write_is_charged_nothing_for_the_host_of_a_merk_it_opens() {
        let host_read = OperationCost {
            seek_count: 1,
            storage_loaded_bytes: 78,
            hash_node_calls: 1,
            ..Default::default()
        };
        assert_eq!(
            rules_of(&GROVE_V6).host_read_at_open(host_read.clone()),
            OperationCost::default()
        );
        assert_eq!(
            rules_of(&GROVE_V5).host_read_at_open(host_read.clone()),
            host_read
        );
    }

    #[test]
    fn sp3_a_mark_is_free_and_a_version_without_stop_points_has_none() {
        assert_eq!(
            rules_of(&GROVE_V6).mark().expect("a version with marks"),
            OperationCost::default()
        );
        assert!(matches!(
            rules_of(&GROVE_V5).mark(),
            Err(Error::NotSupported(_))
        ));
    }

    const INDEXED: [TreeType; 5] = [
        TreeType::ProvableSumIndexedTree,
        TreeType::ProvableCountIndexedTree,
        TreeType::ProvableBigSumIndexedTree,
        TreeType::ProvableCountProvableSumIndexedTree,
        TreeType::ProvableCountProvableBigSumIndexedTree,
    ];
    const NOT_INDEXED: [TreeType; 17] = [
        TreeType::NormalTree,
        TreeType::SumTree,
        TreeType::BigSumTree,
        TreeType::CountTree,
        TreeType::CountSumTree,
        TreeType::ProvableCountTree,
        TreeType::ProvableCountSumTree,
        TreeType::ProvableSumTree,
        TreeType::ProvableBigSumTree,
        TreeType::ProvableCountProvableSumTree,
        TreeType::ProvableCountProvableBigSumTree,
        TreeType::CountBigSumTree,
        TreeType::MmrTree,
        TreeType::BulkAppendTree(4),
        TreeType::DenseAppendOnlyFixedSizeTree(4),
        TreeType::CommitmentTree(4),
        TreeType::PrivateDocumentStore(4),
    ];

    #[test]
    fn sp5_a_write_stops_at_the_subtree_it_changed_where_no_indexed_tree_is_above_it() {
        let v1 = rules_of(&GROVE_V6);
        let root = TreeType::NormalTree;
        for changed in NOT_INDEXED {
            for above in NOT_INDEXED {
                assert!(!above.is_indexed_primary(), "{above}");
                // One, two and three levels below the root Merk.
                assert_eq!(v1.stop_point(&[root, changed]), 1, "{changed}");
                assert_eq!(v1.stop_point(&[root, above, changed]), 2, "{above}");
                assert_eq!(v1.stop_point(&[root, above, above, changed]), 3, "{above}");
            }
        }
        // The root Merk is its own stop point.
        assert_eq!(v1.stop_point(&[root]), 0);
        assert_eq!(v1.stop_point(&[]), 0);
    }

    #[test]
    fn sp5_a_write_under_an_indexed_tree_stops_at_the_primary_of_the_outermost() {
        let v1 = rules_of(&GROVE_V6);
        let (root, count) = (TreeType::NormalTree, TreeType::CountTree);
        for indexed in INDEXED {
            assert!(indexed.is_indexed_primary(), "{indexed}");
            // A write of the primary itself stops there.
            assert_eq!(v1.stop_point(&[root, indexed]), 1, "{indexed}");
            // A write of a tree that is an entry, and of trees one and two
            // levels under an entry: every change under an entry reaches
            // the entry's rows, so the write carries to the primary.
            assert_eq!(v1.stop_point(&[root, indexed, count]), 1, "{indexed}");
            assert_eq!(
                v1.stop_point(&[root, indexed, count, count]),
                1,
                "{indexed}"
            );
            assert_eq!(
                v1.stop_point(&[root, indexed, count, count, count]),
                1,
                "{indexed}"
            );
            // The primary two levels down, with ordinary trees above it.
            assert_eq!(
                v1.stop_point(&[root, count, indexed, count, count]),
                2,
                "{indexed}"
            );
            // An indexed tree inside an indexed tree: the outer one.
            for inner in INDEXED {
                assert_eq!(
                    v1.stop_point(&[root, count, indexed, count, inner, count]),
                    2,
                    "{indexed} over {inner}"
                );
            }
        }
    }

    #[test]
    fn sp5_a_write_of_a_version_that_carries_to_the_root_stops_at_the_root_merk() {
        let v0 = rules_of(&GROVE_V5);
        for tree_type in INDEXED.into_iter().chain(NOT_INDEXED) {
            assert_eq!(v0.stop_point(&[TreeType::NormalTree, tree_type]), 0);
            assert_eq!(
                v0.stop_point(&[TreeType::NormalTree, tree_type, TreeType::CountTree]),
                0
            );
        }
    }

    #[test]
    fn sp6_the_marks_under_a_removed_host_go_for_nothing() {
        assert_eq!(
            rules_of(&GROVE_V6)
                .marks_under_a_removed_host()
                .expect("a version with marks"),
            OperationCost::default()
        );
        assert!(matches!(
            rules_of(&GROVE_V5).marks_under_a_removed_host(),
            Err(Error::NotSupported(_))
        ));
    }

    #[test]
    fn fz1_the_charge_of_a_finalize_is_the_sum_of_its_events_and_nothing_else() {
        let mut charge = rules_of(&GROVE_V6)
            .finalize()
            .expect("a version with a finalize");
        let events = [
            OperationCost {
                seek_count: 1,
                storage_loaded_bytes: 270,
                hash_node_calls: 1,
                ..Default::default()
            },
            OperationCost {
                seek_count: 2,
                storage_cost: StorageCost {
                    replaced_bytes: 288,
                    ..Default::default()
                },
                hash_node_calls: 9,
                ..Default::default()
            },
            seeks(1),
        ];
        let mut sum = OperationCost::default();
        for event in events {
            sum += event.clone();
            charge.event(event).expect("an event of a finalize");
        }
        assert_eq!(charge.finish(), sum);

        let empty = rules_of(&GROVE_V6)
            .finalize()
            .expect("a version with a finalize");
        assert_eq!(empty.finish(), OperationCost::default());
        assert!(matches!(
            rules_of(&GROVE_V5).finalize(),
            Err(Error::NotSupported(_))
        ));
    }

    #[test]
    fn fz4_an_event_of_a_finalize_that_adds_or_removes_bytes_is_refused() {
        let storage = |added_bytes, removed_bytes| OperationCost {
            seek_count: 1,
            storage_cost: StorageCost {
                added_bytes,
                replaced_bytes: 77,
                removed_bytes,
            },
            ..Default::default()
        };
        let by_owner = StorageRemovedBytes::SectionedStorageRemoval(
            [([7; 32], [(0, 1)].into_iter().collect())]
                .into_iter()
                .collect(),
        );
        let mut charge = rules_of(&GROVE_V6)
            .finalize()
            .expect("a version with a finalize");
        let replaces = storage(0, StorageRemovedBytes::NoStorageRemoval);
        charge
            .event(replaces.clone())
            .expect("an event of a finalize");
        for refused in [
            storage(1, StorageRemovedBytes::NoStorageRemoval),
            storage(0, StorageRemovedBytes::BasicStorageRemoval(1)),
            storage(0, by_owner),
        ] {
            assert!(matches!(
                charge.event(refused),
                Err(Error::CorruptedCodeExecution(_))
            ));
        }
        // Nothing of a refused event is in the charge.
        assert_eq!(charge.finish(), replaces);
    }

    #[test]
    fn sp4_a_node_an_earlier_write_left_loaded_is_charged_the_read_of_its_record() {
        let all = features();
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 3 + 2) % all.len()];
            let node = node(
                120,
                *feature,
                Some(link(4, other)),
                Some(link(32, *feature)),
            );
            let record_len = node.encode().len();
            assert_eq!(
                fetch_as_not_loaded(&node).expect("a node Merk encodes"),
                OperationCost {
                    seek_count: 1,
                    storage_loaded_bytes: record_len as u64,
                    ..Default::default()
                },
                "{feature:?}"
            );
            assert_eq!(
                fetch_as_not_loaded(&node).expect("a node Merk encodes"),
                HANDLE_COST_MODEL.read(Some(record_len)),
                "{feature:?}"
            );
        }
    }

    fn sizes_of(link: &Option<Link>) -> Option<(u8, AggregateData)> {
        link.as_ref()
            .map(|link| (link.key().len() as u8, link.aggregate_data()))
    }

    #[test]
    fn the_record_length_from_sizes_is_the_length_of_the_record_merk_writes() {
        let all = features();
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 7 + 3) % all.len()];
            for (left, right) in [
                (None, None),
                (Some(link(0, other)), None),
                (None, Some(link(255, *feature))),
                (Some(link(32, *feature)), Some(link(1, other))),
            ] {
                for value_len in [0, 1, 200, 70_000] {
                    let node = node(value_len, *feature, left.clone(), right.clone());
                    let from_sizes = node_record_len_from_sizes(
                        MODEL,
                        value_len,
                        feature,
                        sizes_of(&left),
                        sizes_of(&right),
                    )
                    .expect("sizes Merk encodes");
                    assert_eq!(from_sizes, node.encode().len(), "{feature:?}");
                    assert_eq!(
                        from_sizes,
                        node_record_len(MODEL, &node).expect("a node Merk encodes"),
                        "{feature:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_link_is_as_long_as_its_childs_key_whatever_the_key_holds() {
        for feature in features() {
            for key_len in 0..=u8::MAX {
                let child = link(usize::from(key_len), feature);
                let from_sizes = node_record_len_from_sizes(
                    MODEL,
                    3,
                    &BasicMerkNode,
                    sizes_of(&Some(child.clone())),
                    None,
                )
                .expect("sizes Merk encodes");
                let node = node(3, BasicMerkNode, Some(child), None);
                assert_eq!(from_sizes, node.encode().len(), "{feature:?}, {key_len}");
            }
        }
    }

    #[test]
    fn the_record_length_is_the_length_of_the_record_merk_writes() {
        let all = features();
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 7 + 3) % all.len()];
            for (left, right) in [
                (None, None),
                (Some(link(0, other)), None),
                (None, Some(link(255, *feature))),
                (Some(link(32, *feature)), Some(link(1, other))),
            ] {
                for value_len in [0, 1, 200, 70_000] {
                    let node = node(value_len, *feature, left.clone(), right.clone());
                    let record_len = node_record_len(MODEL, &node).expect("a node Merk encodes");
                    assert_eq!(record_len, node.encode().len(), "{feature:?}");
                    assert_eq!(record_len, node.encoding_length(), "{feature:?}");
                }
            }
        }
    }

    #[test]
    fn the_sizes_merk_gives_with_a_node_write_add_up_to_its_record_length() {
        let all = features();
        for (index, feature) in all.iter().enumerate() {
            let other = all[(index * 5 + 1) % all.len()];
            let node = node(9, *feature, Some(link(4, other)), Some(link(32, *feature)));
            let feature_len = feature
                .tree_feature_specialized_type_and_length()
                .map_or(0, |(_, len)| len as usize);
            let link_len = |left: bool| {
                node.child_ref_and_sum_size(left)
                    .map(|(key_and_fixed_len, aggregate_len)| {
                        (key_and_fixed_len + aggregate_len) as usize
                    })
            };
            assert_eq!(
                MODEL.node_record_len(9, 1 + feature_len, link_len(true), link_len(false)),
                node.encode().len(),
                "{feature:?}"
            );
        }
    }

    #[test]
    fn every_kind_merk_decodes_is_measured() {
        let all = features();
        let feature_tags: BTreeSet<u8> = all
            .iter()
            .map(|feature| feature.encode().unwrap()[0])
            .collect();
        let aggregate_tag_at = 1 + 4 + 32 + 2;
        let aggregate_tags: BTreeSet<u8> = all
            .iter()
            .map(|feature| link(4, *feature).encode().unwrap()[aggregate_tag_at])
            .collect();
        assert_eq!(feature_tags, aggregate_tags);

        for tag in 0..=u8::MAX {
            let mut feature = vec![tag];
            feature.extend([0; 40]);
            assert_eq!(
                TreeFeatureType::decode(feature.as_slice()).is_ok(),
                feature_tags.contains(&tag),
                "feature tag {tag}"
            );

            let mut link = link(4, BasicMerkNode).encode().unwrap();
            link[aggregate_tag_at] = tag;
            link.extend([0; 40]);
            assert_eq!(
                Link::decode(link.as_slice()).is_ok(),
                aggregate_tags.contains(&tag),
                "aggregate tag {tag}"
            );
        }
    }

    #[test]
    fn a_node_that_cannot_be_encoded_has_no_record_length() {
        let unencodable = node(1, BasicMerkNode, None, Some(link(256, BasicMerkNode)));
        assert!(matches!(
            node_record_len(MODEL, &unencodable),
            Err(Error::EdError(_))
        ));
        let unencodable = node(1, BasicMerkNode, Some(link(256, BasicMerkNode)), None);
        assert!(matches!(
            node_record_len(MODEL, &unencodable),
            Err(Error::EdError(_))
        ));
    }
}
