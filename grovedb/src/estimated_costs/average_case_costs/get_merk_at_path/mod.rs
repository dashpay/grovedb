//! `add_average_case_get_merk_at_path`: versioned dispatch.
//!
//! Adds the average-case cost of the open of a Merk that a write changes.
//! What differs between the versions is the read of the host, the element
//! that names the Merk's root:
//!
//! * **[v0]**: `GROVE_V1`..`GROVE_V5`. The open reads the host, so the
//!   estimate has a seek and the loaded bytes of that read.
//! * **[v1]**: `GROVE_V6`. A write opens a Merk by its own path and root and
//!   is charged nothing for the host (`docs/lignindb/cost-model.md`, SP2 and
//!   ES1), so the estimate has neither.
//!
//! The estimate is the admission bound historical blocks are replayed
//! against, so the released versions keep theirs.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::OperationCost;
use grovedb_merk::tree_type::TreeType;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{batch::KeyInfoPath, Error, GroveDb};

impl GroveDb {
    /// Add average case for getting a merk tree
    pub fn add_average_case_get_merk_at_path_engine_free(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        merk_should_be_empty: bool,
        in_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .add_average_case_get_merk_at_path
        {
            0 => Self::add_average_case_get_merk_at_path_v0(
                cost,
                path,
                merk_should_be_empty,
                in_tree_type,
                grove_version,
            ),
            1 => Self::add_average_case_get_merk_at_path_v1(
                cost,
                path,
                merk_should_be_empty,
                grove_version,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "add_average_case_get_merk_at_path".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
