//! Grove protocol version 6: stop points and finalize.
//!
//! [`GROVE_V5`](super::v5::GROVE_V5) apart from its `protocol_version` and
//! the slots below. It is the grove version of iteration two of LigninDB
//! (dashpay/grovedb#1052): a write stops at a stop point, and a finalize
//! carries every changed Merk to the root of the grove once. The rules are
//! in `docs/lignindb/cost-model.md`, section 16.
//!
//! - `merk_versions.cost_model.propagation: 1`: a write carries nothing
//!   above its stop points and is charged for no level there, opens a
//!   Merk, to change it or to read from it, without reading the host, and
//!   marks the host for nothing. The charge of a finalize is the cost of
//!   the finalize call. V1..V5 charge a write every level up to the root
//!   of the grove. Gated because it changes a tracked cost.
//!
//! - `operations.average_case.add_average_case_get_merk_at_path: 1` and
//!   `operations.worst_case.add_worst_case_get_merk_at_path: 1`: the
//!   estimate of the open of a Merk leaves out the seek and the loaded bytes
//!   of the host's read, which the open no longer makes. For the root Merk,
//!   which has no host, it has the bytes of the root key record. It is added
//!   to the cost it is given in full, or refused with nothing added. Gated
//!   because the estimate is the admission bound.
//!
//! - `merk_versions.average_case_costs.add_average_case_merk_propagate: 3`,
//!   `merk_versions.average_case_costs.sum_tree_estimated_size: 3`,
//!   `merk_versions.average_case_costs.value_with_feature_and_flags_size: 2`,
//!   `merk_versions.average_case_costs.subtree_with_feature_and_flags_size: 1`
//!   and `merk_versions.worst_case_costs.add_worst_case_merk_propagate: 1`: the
//!   estimate of one level of a write has the fetch of each node it has a
//!   put for, but the root, counts the levels of a Merk in whole numbers,
//!   sizes the aggregates of a tree as the cost of its element does, and
//!   in the worst case takes the most levels a Merk of the given size can
//!   have, with five nodes a level. The size of a value and of a subtree
//!   is refused where the longer aggregates would carry it past 32 bits.
//!   V1..V5 estimate a level without these, and
//!   keep the estimates they were admitted with. There a batch's estimate
//!   has every level up to the root of the grove; a write that stops at a
//!   stop point has its own level alone. Gated because the estimate is the
//!   admission bound.
//!
//! - `operations.average_case.average_case_merk_insert_element: 2`,
//!   `average_case_merk_replace_element: 1`,
//!   `average_case_merk_patch_element: 1`, and the three slots of the same
//!   names under `operations.worst_case`: the estimate of the put of an
//!   element counts its hash calls from the bytes of the element. A value
//!   is hashed with its length before it, a tree and a reference combine
//!   their hash with another, and an indexed tree with the roots of its
//!   secondaries. V1..V5 count the blocks of a length that is the element's
//!   cost, and estimate several kinds of tree as items. The estimate of a
//!   patch is sized from the bytes of the item and refuses a growth of more
//!   bytes than the item has. The worst case of a replacement has, for every
//!   kind of element, all of the node's bytes as replaced and all of the
//!   value's as added. V1..V5 have no added bytes for the elements they
//!   take to be put where one of their size was (a sum item, a tree with
//!   no aggregate or with a sum, and the three kinds of tree with a sum of
//!   128 bits that they size as trees), and added bytes for the others,
//!   which they size as items that may grow. Each refuses an element too
//!   long for a node to be sized by its bytes in 32 bits. Gated because the
//!   estimate is the admission bound.
//!
//! - `operations.average_case.average_case_merk_delete_tree: 1`: the
//!   estimate of the removal of a tree refuses the sizes of a layer that an
//!   estimate cannot count, where V1..V5 add the flags of the layer in 32
//!   bits unchecked. Gated because the estimate is the admission bound.
//!
//! - `operations.average_case.average_case_indexed_secondary_mirror: 1`: the
//!   estimate of the rows of an indexed tree takes the keys of its primary
//!   at the longest of the five kinds its layer gives a weight. V1..V5 look
//!   at trees, items and references alone in a layer of several kinds, so a
//!   primary of items with a sum has keys of no length and its rows are
//!   estimated shorter than they are written. Gated because the estimate is
//!   the admission bound.
//!
//! `GroveDb::average_case_finalize`, `GroveDb::worst_case_finalize` and
//! Merk's `add_average_case_merk_finalize` and `add_worst_case_merk_finalize`
//! are served from this version on: they refuse a version whose
//! `propagation` slot is 0, where no finalize exists.
//!
//! **This version is not in [`GROVE_VERSIONS`](super::GROVE_VERSIONS) yet**,
//! so `GroveVersion::latest()` is still version 5. The slots above decide
//! what is charged and estimated. What a write does is decided by the code
//! that applies it, and that code still carries every write to the root:
//! stop points and finalize are built by a later change, which lists this
//! version when a write stops where this version charges it.

use super::{
    grovedb_versions::{
        GroveDBOperationsAverageCaseVersions, GroveDBOperationsVersions,
        GroveDBOperationsWorstCaseVersions, GroveDBVersions,
    },
    merk_versions::{
        MerkAverageCaseCostsVersions, MerkCostModelVersions, MerkVersions,
        MerkWorstCaseCostsVersions,
    },
    v5::GROVE_V5,
    GroveVersion,
};

pub const GROVE_V6: GroveVersion = GroveVersion {
    protocol_version: 6,
    grovedb_versions: GroveDBVersions {
        operations: GroveDBOperationsVersions {
            average_case: GroveDBOperationsAverageCaseVersions {
                add_average_case_get_merk_at_path: 1, // changed: the host is not read
                // changed: the hash calls of a put, from the bytes of the element
                average_case_merk_insert_element: 2,
                average_case_merk_replace_element: 1,
                average_case_merk_patch_element: 1,
                // changed: sizes it cannot count are refused
                average_case_merk_delete_tree: 1,
                // changed: the keys of a primary, by any kind its layer names
                average_case_indexed_secondary_mirror: 1,
                ..GROVE_V5.grovedb_versions.operations.average_case
            },
            worst_case: GroveDBOperationsWorstCaseVersions {
                add_worst_case_get_merk_at_path: 1, // changed: the host is not read
                // changed: the hash calls of a put, from the bytes of the element
                worst_case_merk_insert_element: 2,
                worst_case_merk_replace_element: 1,
                worst_case_merk_patch_element: 1,
                ..GROVE_V5.grovedb_versions.operations.worst_case
            },
            ..GROVE_V5.grovedb_versions.operations
        },
        ..GROVE_V5.grovedb_versions
    },
    merk_versions: MerkVersions {
        // changed: a write stops at its stop points, and finalize carries
        cost_model: MerkCostModelVersions { propagation: 1 },
        average_case_costs: MerkAverageCaseCostsVersions {
            add_average_case_merk_propagate: 3, // changed: the fetch of each node
            sum_tree_estimated_size: 3,         // changed: aggregates as the element costs
            // changed: sizes they cannot count are refused
            value_with_feature_and_flags_size: 2,
            subtree_with_feature_and_flags_size: 1,
            ..GROVE_V5.merk_versions.average_case_costs
        },
        worst_case_costs: MerkWorstCaseCostsVersions {
            // changed: a bound of one level by itself
            add_worst_case_merk_propagate: 1,
            ..GROVE_V5.merk_versions.worst_case_costs
        },
        ..GROVE_V5.merk_versions
    },
    ..GROVE_V5
};
