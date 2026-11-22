//! The engine this build of the suite runs on, and a database of it.

use std::ops::Deref;

use tempfile::TempDir;

#[cfg(merk_test_engine = "lignin")]
use super::LigninTestEngine;
#[cfg(merk_test_engine = "node-store-only")]
use super::NodeStoreOnlyEngine;
#[cfg(merk_test_engine = "rocksdb")]
use super::RocksDbTestEngine;
use super::TestEngine;

/// The engine this build of the suite runs on: the one `MERK_TEST_ENGINE`
/// names, or the first of `merk/test-engines.txt`.
#[cfg(merk_test_engine = "rocksdb")]
pub type SuiteEngine = RocksDbTestEngine;
/// The engine this build of the suite runs on: the one `MERK_TEST_ENGINE`
/// names, or the first of `merk/test-engines.txt`.
#[cfg(merk_test_engine = "node-store-only")]
pub type SuiteEngine = NodeStoreOnlyEngine;
/// The engine this build of the suite runs on: the one `MERK_TEST_ENGINE`
/// names, or the first of `merk/test-engines.txt`.
#[cfg(merk_test_engine = "lignin")]
pub type SuiteEngine = LigninTestEngine;

/// A transaction of the suite's engine.
pub type SuiteTransaction<'db> = <SuiteEngine as TestEngine>::Transaction<'db>;
/// The batch of the suite's engine.
pub type SuiteBatch = <SuiteEngine as TestEngine>::Batch;
/// A deferred subtree handle of the suite's engine.
pub type SuiteSubtree<'a> = <SuiteEngine as TestEngine>::Subtree<'a>;
/// An immediate subtree handle of the suite's engine.
pub type SuiteImmediateSubtree<'a> = <SuiteEngine as TestEngine>::ImmediateSubtree<'a>;

/// A database of the suite's engine in a temporary directory, removed when
/// this is dropped.
pub struct TempEngine {
    engine: SuiteEngine,
    _dir: TempDir,
}

impl TempEngine {
    /// A new, empty database.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("cannot create a temporary directory");
        TempEngine {
            engine: SuiteEngine::open(dir.path()),
            _dir: dir,
        }
    }
}

impl Default for TempEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for TempEngine {
    type Target = SuiteEngine;

    fn deref(&self) -> &SuiteEngine {
        &self.engine
    }
}
