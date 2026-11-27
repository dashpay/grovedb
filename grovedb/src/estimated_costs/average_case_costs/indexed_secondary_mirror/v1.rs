//! `average_case_indexed_secondary_mirror` — **v1** (`GROVE_V6`).
//!
//! [`super::v0`], with the keys of the primary taken at the longest of the
//! five kinds its layer gives a weight (`layer_key_size`, which the estimate
//! of a finalize sizes a root key by). v0 sizes them by
//! `average_case_layer_key_size`, which in a layer of several kinds looks at
//! trees, items and references alone, with a weight or without: a primary of
//! items with a sum has keys of no length there, and its rows, which are
//! keyed by those keys and name them, are estimated shorter than they are
//! written.
//!
//! v0 takes two things from the layer of the primary: how many entries it
//! has, and how long its keys are. So v1 finds the length of the keys and
//! gives v0 the same count with a layer of one kind whose keys are of that
//! length, which v0 reads as it is given.

use grovedb_costs::CostResult;
use grovedb_merk::estimated_costs::average_case_costs::{
    EstimatedLayerInformation, EstimatedLayerSizes,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::KeyInfoPath, estimated_costs::average_case_costs::layer_key_size, Error, GroveDb,
};

impl GroveDb {
    /// `average_case_indexed_secondary_mirror` v1 — see the module
    /// documentation.
    pub(super) fn average_case_indexed_secondary_mirror_v1(
        primary_path: &KeyInfoPath,
        primary_layer_information: &EstimatedLayerInformation,
        axes: &[grovedb_element::indexed::IndexAxis],
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        // A layer gives the keys of each kind in one byte, so the longest
        // of them is one.
        let key_size = u8::try_from(layer_key_size(
            &primary_layer_information.estimated_layer_sizes,
        ))
        .unwrap_or(u8::MAX);
        let keyed_as_found = EstimatedLayerInformation {
            tree_type: primary_layer_information.tree_type,
            estimated_layer_count: primary_layer_information.estimated_layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(key_size, 0, None),
        };
        Self::average_case_indexed_secondary_mirror_v0(
            primary_path,
            &keyed_as_found,
            axes,
            grove_version,
        )
    }
}
