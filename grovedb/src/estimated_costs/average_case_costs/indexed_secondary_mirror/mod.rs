//! `average_case_indexed_secondary_mirror` — versioned dispatch.
//!
//! The average-case cost of the rows an indexed tree keeps in its
//! secondaries for one entry of its primary.
//!
//! * **[v0]** — `GROVE_V1`..`GROVE_V5`. In a primary described as a layer of
//!   several kinds, the keys are the longest of its trees, items and
//!   references, with a weight or without.
//! * **[v1]** — `GROVE_V6` (`average_case_indexed_secondary_mirror: 1`). The
//!   keys are the longest of the five kinds the layer gives a weight. A
//!   refusal has no cost.
//!
//! The estimate is the admission bound historical blocks are replayed
//! against, so the released versions keep theirs.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerInformation;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{batch::KeyInfoPath, estimated_costs::in_full_or_refused, Error, GroveDb};

impl GroveDb {
    /// Average-case cost of mirroring one primary-entry change into an
    /// indexed tree's per-axis secondary Merks.
    ///
    /// The secondary Merks are NOT described by the caller's
    /// `EstimatedLayerInformation` map — they live at derived prefixes
    /// (`Blake3(primary_prefix ‖ axis_tag)`) that are not GroveDB paths. But
    /// every input is derivable from the PRIMARY's layer information:
    ///
    /// - **Entry count** is the primary's, because the mirror is 1:1 — every
    ///   primary entry has exactly one row per axis (the indexed trees do no
    ///   sparse or conditional indexing).
    /// - **Key size** is the primary's key size plus the axis sort-key width
    ///   (8 bytes for count/sum, 16 for avg).
    /// - **Value size** is the canonical reference row's own serialized
    ///   size. Unlike the fixed-shape placeholder rows this replaced, a
    ///   reference row carries the primary key it points at, so its size
    ///   scales with the primary's key size — it is derived from the real
    ///   row rather than pinned to a constant.
    /// - **Tree type** is fixed per axis.
    ///
    /// Each axis is charged one Merk open, one delete of the old row and one
    /// insert of the new row, each propagating — which is what
    /// `mirror_*_to_secondary` actually performs.
    ///
    /// **Reference refresh is charged unconditionally.** A canonical row
    /// binds the primary node's commitment, so a value-only primary update
    /// — one that moves neither count nor sum — still rewrites every
    /// configured axis. The estimate must reflect that: it is an admission
    /// bound replayed against historical blocks, and an estimate that
    /// assumed "aggregates unchanged ⇒ no secondary write" would come in
    /// under actual `added_bytes`.
    pub fn average_case_indexed_secondary_mirror(
        primary_path: &KeyInfoPath,
        primary_layer_information: &EstimatedLayerInformation,
        axes: &[grovedb_element::indexed::IndexAxis],
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_indexed_secondary_mirror
        {
            0 => Self::average_case_indexed_secondary_mirror_v0(
                primary_path,
                primary_layer_information,
                axes,
                grove_version,
            ),
            1 => in_full_or_refused(Self::average_case_indexed_secondary_mirror_v1(
                primary_path,
                primary_layer_information,
                axes,
                grove_version,
            )),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_indexed_secondary_mirror".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
