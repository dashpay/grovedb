pub mod grovedb_versions;
pub mod v1;

pub use versioned_feature_core::*;

use crate::version::{grovedb_versions::GroveDBVersions, v1::GROVE_V1};

#[derive(Clone, Debug, Default)]
pub struct GroveVersion {
    pub protocol_version: u32,
    pub grovedb_versions: GroveDBVersions,
}

impl GroveVersion {
    pub fn latest<'a>() -> &'a Self {
        GROVE_VERSIONS
            .last()
            .expect("expected to have a platform version")
    }
}

pub const GROVE_VERSIONS: &[GroveVersion] = &[GROVE_V1];
