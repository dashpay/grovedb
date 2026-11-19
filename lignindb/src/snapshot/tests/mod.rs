mod loom;
#[cfg(not(loom))]
mod model_check;
#[cfg(not(loom))]
mod reads;
#[cfg(not(loom))]
mod scaling;
#[cfg(not(loom))]
mod store;
#[cfg(not(loom))]
mod versions;

use lignin_format::CommitVersion;

use super::*;

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}
