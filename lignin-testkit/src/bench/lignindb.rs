//! The harness's engine over LigninDB: `GroveDb` on `LigninEngine`, as
//! `Engine::open` sets it up, with the options a node gets by default.
//!
//! Every count comes from one reading of the store's stats
//! ([`Counters::from`] of `lignin_store::stats::StoreStats`), so nothing is
//! collected between commits and [`Engine::settle`] does nothing. The
//! background work is idle once no page version waits to be written back
//! and the flusher and the cleaner have written nothing for a second.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use grovedb::{GroveDb, LigninEngine, PathQuery, Transaction};
use grovedb_costs::{CostContext, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{
    engine::{Block, Counters, Engine},
    BenchError,
};
use crate::workload::{apply, Op, Outcome, TransactionOp};

/// GroveDB on LigninDB, as `GroveDb::open_with_engine` sets it up.
pub struct LigninDbEngine {
    db: GroveDb<LigninEngine>,
    dir: PathBuf,
}

/// How long [`Engine::wait_for_background`] waits at most.
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(1800);
/// Readings of the stats, this far apart, that must all find nothing
/// waiting and nothing written since the reading before.
const IDLE_POLLS: u32 = 5;
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(200);

impl LigninDbEngine {
    /// The database underneath.
    pub fn grove(&self) -> &GroveDb<LigninEngine> {
        &self.db
    }

    /// What waits to be written back, and the bytes the background work
    /// has written.
    fn background(&self) -> (usize, u64) {
        let store = self.db.engine().database().stats().store;
        (
            store.watch.unwritten,
            store.flush_bytes + store.cleaner_bytes,
        )
    }
}

impl Engine for LigninDbEngine {
    type Block<'a> = LigninDbBlock<'a>;

    const NAME: &'static str = "lignindb";

    fn open(dir: &Path) -> Result<Self, BenchError> {
        Ok(LigninDbEngine {
            db: GroveDb::open_with_engine(dir)?,
            dir: dir.to_path_buf(),
        })
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn begin_block(&self) -> LigninDbBlock<'_> {
        LigninDbBlock {
            db: &self.db,
            transaction: self.db.start_transaction(),
            snapshot: None,
        }
    }

    fn settle(&mut self) -> Result<(), BenchError> {
        Ok(())
    }

    fn read(&self, op: &Op, grove_version: &GroveVersion) -> Result<OperationCost, BenchError> {
        let CostContext { value, cost } = apply(&self.db, op, None, grove_version);
        value?;
        Ok(cost)
    }

    fn prove(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(Vec<u8>, OperationCost), BenchError> {
        let CostContext { value, cost } = self.db.prove_query(query, None, grove_version);
        Ok((value?, cost))
    }

    fn root_hash(&self, grove_version: &GroveVersion) -> Result<[u8; 32], BenchError> {
        Ok(self.db.root_hash(None, grove_version).value?)
    }

    fn checkpoint(&self, target: &Path) -> Result<(), BenchError> {
        Ok(self.db.create_checkpoint(target)?)
    }

    fn flush(&self) -> Result<(), BenchError> {
        Ok(self.db.flush()?)
    }

    fn wait_for_background(&mut self) -> Result<(), BenchError> {
        let deadline = Instant::now() + BACKGROUND_TIMEOUT;
        let mut last = self.background();
        let mut idle_polls = 0;
        while idle_polls < IDLE_POLLS {
            if Instant::now() > deadline {
                return Err(BenchError::Check(format!(
                    "LigninDB's background work did not go idle in {BACKGROUND_TIMEOUT:?}"
                )));
            }
            std::thread::sleep(IDLE_POLL_INTERVAL);
            let now = self.background();
            idle_polls = if now.0 == 0 && now == last {
                idle_polls + 1
            } else {
                0
            };
            last = now;
        }
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters, BenchError> {
        Ok(Counters::from(&self.db.engine().database().stats().store))
    }
}

/// The write transaction of one block on [`LigninDbEngine`], and a snapshot
/// read transaction while the block holds one open, as
/// [`RocksDbBlock`](super::rocksdb::RocksDbBlock) keeps them.
pub struct LigninDbBlock<'a> {
    db: &'a GroveDb<LigninEngine>,
    transaction: Transaction<'a, LigninEngine>,
    snapshot: Option<Transaction<'a, LigninEngine>>,
}

impl Block for LigninDbBlock<'_> {
    fn apply(
        &mut self,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, BenchError> {
        let transaction = match op {
            Op::Transaction(TransactionOp::SnapshotRead(_)) => {
                self.snapshot.as_ref().ok_or_else(|| {
                    BenchError::Check("a snapshot read needs an open snapshot".to_string())
                })?
            }
            _ => &self.transaction,
        };
        let CostContext { value, cost } = apply(self.db, op, Some(transaction), grove_version);
        match value? {
            Outcome::Abandoned => self.transaction = self.db.start_transaction(),
            Outcome::SnapshotOpens => {
                self.snapshot = Some(self.db.start_snapshot_read_transaction())
            }
            Outcome::SnapshotCloses => self.snapshot = None,
            _ => {}
        }
        Ok(cost)
    }

    fn commit(self) -> Result<(), BenchError> {
        if self.snapshot.is_some() {
            return Err(BenchError::Check(
                "a snapshot read transaction is open at the end of the block; the harness \
                 keeps one only within a block"
                    .to_string(),
            ));
        }
        Ok(self.db.commit_transaction(self.transaction).value?)
    }
}

#[cfg(test)]
mod tests {
    use grovedb::Element;

    use super::*;
    use crate::workload::{Path as GrovePath, Read, Write};

    fn insert(path: GrovePath, key: &[u8], element: Element) -> Op {
        Op::Write(Write::Insert {
            path,
            key: key.to_vec(),
            element,
        })
    }

    #[test]
    fn counters_come_from_the_stats_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let mut engine = LigninDbEngine::open(dir.path()).unwrap();
        engine.settle().unwrap();
        let opened = engine.counters().unwrap();

        let mut block = engine.begin_block();
        block
            .apply(
                &insert(vec![], b"tree", Element::empty_tree()),
                grove_version,
            )
            .unwrap();
        block
            .apply(
                &insert(
                    vec![b"tree".to_vec()],
                    b"key",
                    Element::new_item(b"value".to_vec()),
                ),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        engine.flush().unwrap();
        engine.wait_for_background().unwrap();

        let written = engine.counters().unwrap().since(&opened);
        assert_eq!(written.log_groups, 1);
        assert!(written.node_records > 0 && written.log_bytes > 0);
        assert!(written.keyed_lookups.is_some() && written.hashes.is_some());
        assert_eq!(engine.background().0, 0);
        assert_eq!(engine.dir(), dir.path());

        let root = engine.root_hash(grove_version).unwrap();
        let read = engine.grove().root_hash(None, grove_version).unwrap();
        assert_eq!(read.unwrap(), root);
        let query = PathQuery::new_single_key(vec![b"tree".to_vec()], b"key".to_vec());
        let (proof, _) = engine.prove(&query, grove_version).unwrap();
        let (proved, _) = GroveDb::verify_query(&proof, &query, grove_version).unwrap();
        assert_eq!(proved, root);
        let target = dir.path().join("checkpoint");
        engine.checkpoint(&target).unwrap();
        assert!(target.is_dir());
    }

    #[test]
    fn a_block_keeps_a_snapshot_only_within_itself() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let engine = LigninDbEngine::open(dir.path()).unwrap();
        let mut block = engine.begin_block();
        let read = Read::Get {
            path: vec![],
            key: b"key".to_vec(),
        };
        let refused = block.apply(
            &Op::Transaction(TransactionOp::SnapshotRead(read)),
            grove_version,
        );
        assert!(matches!(refused, Err(BenchError::Check(_))));
        block
            .apply(&Op::Transaction(TransactionOp::OpenSnapshot), grove_version)
            .unwrap();
        assert!(matches!(block.commit(), Err(BenchError::Check(_))));
    }
}
