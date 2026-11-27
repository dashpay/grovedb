//! `worst_case_merk_replace_element` — versioned dispatch.
//!
//! Adds the worst-case cost of replacing an element in a Merk, for one level
//! only: the batch estimator propagates the levels above.
//!
//! How a sum-bearing item (`SumItem` or `ItemWithSumItem`, wrapped or not) is
//! sized is gated on `worst_case.worst_case_sum_item_cost_size`:
//!
//! * **[v0]** — released behaviour, `GROVE_V1`..`GROVE_V3`. A `SumItem` is
//!   sized by `SUM_ITEM_COST_SIZE` and its flags, without the byte of a
//!   wrapper, and an `ItemWithSumItem` by its serialized size. The apply
//!   writes either with its specialized cost, which charges the sum value at
//!   its fixed 9-byte width and counts the wrapper byte, so a wrapped
//!   `SumItem` is under-sized by 1 and an `ItemWithSumItem` by up to 8.
//! * **[v1]** — `GROVE_V4`+. Both sum-bearing items are sized by the
//!   specialized cost the apply writes them with.
//!
//! * **[v2]** — `GROVE_V6` (`worst_case_merk_replace_element: 1`). v1, with
//!   the storage of a replacement of anything by anything (all of the node's
//!   bytes replaced, and all of the value's added, for every kind of
//!   element), with the hash calls of the put counted from the bytes of the
//!   element, and with an element too long to size refused.
//!
//! The estimate is the admission bound historical blocks are replayed
//! against, so the released versions keep the v0 sizing.
//!
//! [v0]: self::v0
//! [v1]: self::v1
//! [v2]: self::v2

mod v0;
mod v1;
mod v2;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::worst_case_costs::WorstCaseLayerInformation, tree_type::TreeType,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{
    batch::key_info::KeyInfo, estimated_costs::in_full_or_refused, Element, Error, GroveDb,
};

impl GroveDb {
    /// Add worst case for replacement in merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn worst_case_merk_replace_element(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_type: TreeType,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .worst_case
            .worst_case_merk_replace_element
        {
            0 => {}
            1 => {
                return in_full_or_refused(Self::worst_case_merk_replace_element_v2(
                    key,
                    value,
                    in_parent_tree_type,
                    propagate_for_level,
                    grove_version,
                ))
            }
            version => {
                return Err(Error::VersionError(
                    GroveVersionError::UnknownVersionMismatch {
                        method: "worst_case_merk_replace_element".to_string(),
                        known_versions: vec![0, 1],
                        received: version,
                    },
                ))
                .wrap_with_cost(OperationCost::default())
            }
        }

        match grove_version
            .grovedb_versions
            .operations
            .worst_case
            .worst_case_sum_item_cost_size
        {
            0 => Self::worst_case_merk_replace_element_v0(
                key,
                value,
                in_parent_tree_type,
                propagate_for_level,
                grove_version,
            ),
            1 => Self::worst_case_merk_replace_element_v1(
                key,
                value,
                in_parent_tree_type,
                propagate_for_level,
                grove_version,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "worst_case_sum_item_cost_size".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
