//! `worst_case_merk_insert_element` — versioned dispatch.
//!
//! Adds the worst-case cost of inserting an element into a Merk, for one
//! level only: the batch estimator propagates the levels above.
//!
//! How a sum-bearing item (`SumItem` or `ItemWithSumItem`, wrapped or not) is
//! sized is gated on `worst_case.worst_case_sum_item_cost_size`, and how
//! creating a `PrivateDocumentStore` is priced on
//! `worst_case.worst_case_merk_insert_element`:
//!
//! * **[v0]** — released behaviour, `GROVE_V1`..`GROVE_V3`. Every element that
//!   is not one of the four sized tree types is sized by its serialized size.
//!   The apply writes a sum-bearing item with its specialized cost instead,
//!   which charges the sum value at its fixed 9-byte width
//!   (`SUM_ITEM_COST_SIZE`), so the estimate's `added_bytes` come in below
//!   the applied ones by up to 8 per item (8 for any sum value that encodes
//!   in one byte), and the worst case is not an upper bound.
//! * **[v1]** — `GROVE_V4`. A sum-bearing item is sized by the specialized
//!   cost the apply writes it with.
//! * **[v2]** — `GROVE_V5`+ (`worst_case_merk_insert_element: 1`). v1, and
//!   creating a `PrivateDocumentStore` is priced as the apply prices it: as
//!   the layered tree the apply writes (v1 prices it as an item), with its
//!   existence check and the two hashes of its empty root.
//!
//! The estimate is the admission bound historical blocks are replayed
//! against, so the released versions keep their sizing.
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

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

impl GroveDb {
    /// Add worst case for insertion into merk
    /// This only propagates on 1 level
    /// As higher level propagation is done in batching
    pub fn worst_case_merk_insert_element(
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
            .worst_case_merk_insert_element
        {
            0 => {}
            1 => {
                return Self::worst_case_merk_insert_element_v2(
                    key,
                    value,
                    in_parent_tree_type,
                    propagate_for_level,
                    grove_version,
                )
            }
            version => {
                return Err(Error::VersionError(
                    GroveVersionError::UnknownVersionMismatch {
                        method: "worst_case_merk_insert_element".to_string(),
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
            0 => Self::worst_case_merk_insert_element_v0(
                key,
                value,
                in_parent_tree_type,
                propagate_for_level,
                grove_version,
            ),
            1 => Self::worst_case_merk_insert_element_v1(
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
