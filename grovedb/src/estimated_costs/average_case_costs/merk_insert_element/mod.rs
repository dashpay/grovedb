//! `average_case_merk_insert_element` — versioned dispatch.
//!
//! Adds the average-case cost of inserting an element into a Merk, for one
//! level only: the batch estimator propagates the levels above.
//!
//! How a sum-bearing item (`SumItem` or `ItemWithSumItem`, wrapped or not) is
//! sized is gated on `average_case.average_case_sum_item_cost_size`, and how
//! a wrapped tree is sized and creating a `PrivateDocumentStore` is priced on
//! `average_case.average_case_merk_insert_element`:
//!
//! * **[v0]** — released behaviour, `GROVE_V1`..`GROVE_V3`. Every element that
//!   is not a tree is sized by its serialized size. The apply writes a
//!   sum-bearing item with its specialized cost instead, which charges the
//!   sum value at its fixed 9-byte width (`SUM_ITEM_COST_SIZE`), so the
//!   estimate's `added_bytes` come in below the applied ones by up to 8 per
//!   item (8 for any sum value that encodes in one byte).
//! * **[v1]** — `GROVE_V4`. A sum-bearing item is sized by the specialized
//!   cost the apply writes it with. A tree wrapped in `NonCounted`,
//!   `NotSummed` or `NotCountedOrSummed` is sized as the tree alone, in v0 as
//!   well, so its `added_bytes` come in 1 below the applied ones, which
//!   charge the wrapper's byte.
//! * **[v2]** — `GROVE_V5`+ (`average_case_merk_insert_element: 1`). v1, and
//!   a wrapped tree is sized with its wrapper's byte, and creating a
//!   `PrivateDocumentStore` is priced as the apply prices it: its existence
//!   check and the two hashes of its empty root.
//!
//! * **[v3]** — `GROVE_V6` (`average_case_merk_insert_element: 2`). v2, with
//!   the hash calls of the put counted from the bytes of the element.
//!
//! The estimate is the admission bound historical blocks are replayed
//! against, so the released versions keep their sizing.
//!
//! [v0]: self::v0
//! [v1]: self::v1
//! [v2]: self::v2
//! [v3]: self::v3

mod v0;
mod v1;
mod v2;
mod v3;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::average_case_costs::EstimatedLayerInformation, tree_type::TreeType,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    batch::key_info::KeyInfo, estimated_costs::in_full_or_refused, Element, Error, GroveDb,
};

impl GroveDb {
    /// Add average case for insertion into merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn average_case_merk_insert_element(
        key: &KeyInfo,
        value: &Element,
        in_tree_type: TreeType,
        propagate_for_level: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_merk_insert_element
        {
            0 => {}
            1 => {
                return Self::average_case_merk_insert_element_v2(
                    key,
                    value,
                    in_tree_type,
                    propagate_for_level,
                    grove_version,
                )
            }
            2 => {
                return in_full_or_refused(Self::average_case_merk_insert_element_v3(
                    key,
                    value,
                    in_tree_type,
                    propagate_for_level,
                    grove_version,
                ))
            }
            version => {
                return Err(Error::VersionError(
                    GroveVersionError::UnknownVersionMismatch {
                        method: "average_case_merk_insert_element".to_string(),
                        known_versions: vec![0, 1, 2],
                        received: version,
                    },
                ))
                .wrap_with_cost(OperationCost::default())
            }
        }

        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_sum_item_cost_size
        {
            0 => Self::average_case_merk_insert_element_v0(
                key,
                value,
                in_tree_type,
                propagate_for_level,
                grove_version,
            ),
            1 => Self::average_case_merk_insert_element_v1(
                key,
                value,
                in_tree_type,
                propagate_for_level,
                grove_version,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_sum_item_cost_size".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
