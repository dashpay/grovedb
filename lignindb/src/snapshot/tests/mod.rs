mod loom;
#[cfg(not(loom))]
mod model_check;
#[cfg(not(loom))]
mod versions;

use lignin_format::CommitVersion;

use super::*;

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}
