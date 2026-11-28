//! Tests of [`Backup::copy`](super::Backup::copy) on database directories
//! that the facade made and closed, or was killed over, on the simulated
//! file system: what a copy holds, what it refuses, and what it leaves
//! when the disk under it fills or the power is cut at each of its
//! operations. The copies of checkpoints, beside a database that goes on
//! writing, are with the facade's tests (`db/tests/backup.rs`), where a
//! checkpoint can be made.

mod copy;
pub(crate) mod world;
