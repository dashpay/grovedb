pub mod bulk_append_tree_versions;
pub mod grovedb_versions;
pub mod merk_versions;
pub mod mmr_versions;
pub mod v1;
pub mod v2;
pub mod v3;
pub mod v4;

pub use versioned_feature_core::*;

use crate::version::v3::GROVE_V3;
use crate::version::v4::GROVE_V4;
use crate::version::{
    bulk_append_tree_versions::BulkAppendTreeVersions, grovedb_versions::GroveDBVersions,
    merk_versions::MerkVersions, mmr_versions::MmrVersions, v1::GROVE_V1, v2::GROVE_V2,
};

#[derive(Clone, Debug, Default)]
pub struct GroveVersion {
    pub protocol_version: u32,
    pub grovedb_versions: GroveDBVersions,
    pub merk_versions: MerkVersions,
    pub mmr_versions: MmrVersions,
    pub bulk_append_tree_versions: BulkAppendTreeVersions,
}

impl GroveVersion {
    pub fn first<'a>() -> &'a Self {
        GROVE_VERSIONS
            .first()
            .expect("expected to have a platform version")
    }

    pub fn latest<'a>() -> &'a Self {
        GROVE_VERSIONS
            .last()
            .expect("expected to have a platform version")
    }
}

pub const GROVE_VERSIONS: &[GroveVersion] = &[GROVE_V1, GROVE_V2, GROVE_V3, GROVE_V4];
