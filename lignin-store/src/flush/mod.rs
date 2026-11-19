//! Write-back: page versions go from the buffer pool to segments.

mod checkpoint;
mod config;
mod error;
mod page;
mod place;
mod run;
mod state;
mod write_back;

#[cfg(test)]
mod tests;

pub use checkpoint::Checkpointed;
pub use config::{
    max_run_len, DeadSpace, FlushConfig, FlushStats, DEFAULT_CHAIN_DELTAS, DEFAULT_SEGMENT_LEN,
};
pub use error::FlushError;
pub use run::{Room, Tick};
pub use state::FlushState;
pub use write_back::{PageValues, Pass, Published, RegroupError, Stores, WriteBack};
