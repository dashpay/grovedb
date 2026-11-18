//! The harness's engine over today's RocksDB storage.
//!
//! GroveDB opens RocksDB itself, with statistics off, and keeps the handle
//! private, so the counters are read from what RocksDB leaves in the
//! database directory:
//!
//! - `log_bytes`, `log_groups`, `node_records`, `node_record_bytes`,
//!   `other_records` and `other_record_bytes`: the write-ahead log files
//!   (`*.log`), read after every commit by [`wal`]. Every commit is one
//!   write batch. Merk nodes are the puts to the default column family
//!   (id 0); the other families are aux (1), roots (2) and meta (3), in the
//!   order GroveDB creates them.
//! - `flush_bytes` and `compaction_bytes`: the `EVENT_LOG_v1` lines of the
//!   info log (`LOG`), read by [`info_log`].
//! - `page_reads` and `cache_hits`: not available without RocksDB's
//!   statistics.

pub mod info_log;
pub mod wal;

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use grovedb::{GroveDb, PathQuery, Transaction};
use grovedb_costs::{CostContext, OperationCost};
use grovedb_version::version::GroveVersion;

use self::{
    info_log::{EventTotals, InfoLogTail},
    wal::{LogTail, WalTotals},
};
use super::{
    engine::{Block, Counters, Engine},
    BenchError,
};
use crate::workload::{apply, Op};

/// GroveDB on RocksDB, as `GroveDb::open` sets it up.
pub struct RocksDbEngine {
    db: GroveDb,
    dir: PathBuf,
    logs: BTreeMap<u64, LogTail>,
    wal: WalTotals,
    info_log: InfoLogTail,
    events: EventTotals,
}

impl RocksDbEngine {
    /// What the write-ahead log held since the engine was opened.
    pub fn wal_totals(&self) -> &WalTotals {
        &self.wal
    }

    /// What the info log reported since the engine was opened.
    pub fn event_totals(&self) -> EventTotals {
        self.events
    }

    /// The write-ahead log files in the directory, by number.
    fn log_files(&self) -> Result<BTreeMap<u64, PathBuf>, BenchError> {
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|error| BenchError::io("listing the database directory", error))?;
        let mut files = BTreeMap::new();
        for entry in entries {
            let entry =
                entry.map_err(|error| BenchError::io("listing the database directory", error))?;
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "log") {
                if let Some(number) = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u64>().ok())
                {
                    files.insert(number, path);
                }
            }
        }
        Ok(files)
    }

    fn read_logs(&mut self) -> Result<(), BenchError> {
        let files = self.log_files()?;
        self.logs.retain(|number, _| files.contains_key(number));
        for (number, path) in files {
            let tail = self.logs.entry(number).or_default();
            let data = read_from(&path, tail.offset())?;
            tail.read(&data, &mut self.wal)
                .map_err(|error| BenchError::Wal {
                    file: path.display().to_string(),
                    error,
                })?;
        }
        Ok(())
    }

    fn read_info_log(&mut self) -> Result<(), BenchError> {
        let data = read_from(&self.dir.join("LOG"), self.info_log.offset())?;
        self.info_log.read(&data, &mut self.events);
        Ok(())
    }
}

/// The bytes of `path` from `offset` on; nothing if the file is gone.
fn read_from(path: &Path, offset: u64) -> Result<Vec<u8>, BenchError> {
    let context = || format!("reading {}", path.display());
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(BenchError::io(context(), error)),
    };
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| BenchError::io(context(), error))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|error| BenchError::io(context(), error))?;
    Ok(data)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

impl Engine for RocksDbEngine {
    type Block<'a> = RocksDbBlock<'a>;

    const NAME: &'static str = "rocksdb";

    fn open(dir: &Path) -> Result<Self, BenchError> {
        let db = GroveDb::open(dir)?;
        let mut engine = RocksDbEngine {
            db,
            dir: dir.to_path_buf(),
            logs: BTreeMap::new(),
            wal: WalTotals::default(),
            info_log: InfoLogTail::default(),
            events: EventTotals::default(),
        };
        for (number, path) in engine.log_files()? {
            engine
                .logs
                .insert(number, LogTail::starting_at(file_len(&path)));
        }
        engine.read_info_log()?;
        engine.events = EventTotals::default();
        Ok(engine)
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn begin_block(&self) -> RocksDbBlock<'_> {
        RocksDbBlock {
            db: &self.db,
            transaction: self.db.start_transaction(),
        }
    }

    fn settle(&mut self) -> Result<(), BenchError> {
        self.read_logs()
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

    fn counters(&mut self) -> Result<Counters, BenchError> {
        self.read_logs()?;
        self.read_info_log()?;
        let nodes = self.wal.default_family();
        let others = self.wal.other_families();
        Ok(Counters {
            log_bytes: self.wal.file_bytes,
            log_groups: self.wal.batches,
            node_records: nodes.puts,
            node_record_bytes: nodes.put_bytes,
            other_records: nodes.others + others.puts + others.others,
            other_record_bytes: nodes.other_bytes + others.put_bytes + others.other_bytes,
            flush_bytes: self.events.flush_bytes(),
            compaction_bytes: self.events.compaction_bytes,
            page_reads: None,
            cache_hits: None,
        })
    }
}

/// The write transaction of one block on [`RocksDbEngine`].
pub struct RocksDbBlock<'a> {
    db: &'a GroveDb,
    transaction: Transaction<'a>,
}

impl Block for RocksDbBlock<'_> {
    fn apply(
        &mut self,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, BenchError> {
        let CostContext { value, cost } = apply(self.db, op, Some(&self.transaction), grove_version);
        value?;
        Ok(cost)
    }

    fn commit(self) -> Result<(), BenchError> {
        Ok(self.db.commit_transaction(self.transaction).value?)
    }
}

#[cfg(test)]
mod tests {
    use grovedb::Element;

    use super::*;
    use crate::workload::{Path as GrovePath, Write};

    fn insert(path: GrovePath, key: &[u8], element: Element) -> Op {
        Op::Write(Write::Insert {
            path,
            key: key.to_vec(),
            element,
        })
    }

    #[test]
    fn counters_come_from_the_log_and_the_info_log() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let mut engine = RocksDbEngine::open(dir.path()).unwrap();
        assert_eq!(engine.counters().unwrap(), Counters::default());

        let mut block = engine.begin_block();
        block
            .apply(&insert(vec![], b"tree", Element::empty_tree()), grove_version)
            .unwrap();
        let cost = block
            .apply(
                &insert(vec![b"tree".to_vec()], b"key", Element::new_item(vec![7; 100])),
                grove_version,
            )
            .unwrap();
        assert!(cost.seek_count > 0);
        block.commit().unwrap();
        engine.settle().unwrap();
        let after_block = engine.counters().unwrap();
        assert_eq!(after_block.log_groups, 1);
        assert!(after_block.node_records >= 3, "{after_block:?}");
        assert!(after_block.node_record_bytes > 100);
        assert!(after_block.log_bytes > after_block.node_record_bytes);
        assert_eq!(after_block.page_reads, None);

        engine.db.put_aux(b"aux-key", b"aux-value", None, None).unwrap().unwrap();
        engine.settle().unwrap();
        let aux = engine.wal_totals().families.get(&1).copied().unwrap();
        // The key carries the 32-byte prefix of the root path.
        assert_eq!((aux.puts, aux.put_bytes), (1, 32 + 7 + 9), "aux is family 1");

        engine.flush().unwrap();
        let flushed = engine.counters().unwrap().since(&after_block);
        assert!(flushed.flush_bytes > 0, "{:?}", engine.event_totals());
        assert_eq!(flushed.compaction_bytes, 0);
        assert_eq!(
            flushed.physical_bytes(),
            flushed.log_bytes + flushed.flush_bytes
        );

        let root_hash = engine.root_hash(grove_version).unwrap();
        let checkpoint = dir.path().join("..").join(format!(
            "{}-checkpoint",
            dir.path().file_name().unwrap().to_str().unwrap()
        ));
        engine.checkpoint(&checkpoint).unwrap();
        assert!(engine.checkpoint(&checkpoint).is_err());
        std::fs::remove_dir_all(&checkpoint).unwrap();
        assert_eq!(engine.dir(), dir.path());
        drop(engine);

        let mut reopened = RocksDbEngine::open(dir.path()).unwrap();
        assert_eq!(reopened.root_hash(grove_version).unwrap(), root_hash);
        assert_eq!(reopened.counters().unwrap().log_bytes, 0);
        let get = Op::Read(crate::workload::Read::Get {
            path: vec![b"tree".to_vec()],
            key: b"key".to_vec(),
        });
        assert!(reopened.read(&get, grove_version).unwrap().seek_count > 0);
        let missing = Op::Read(crate::workload::Read::Get {
            path: vec![b"tree".to_vec()],
            key: b"missing".to_vec(),
        });
        assert!(matches!(
            reopened.read(&missing, grove_version),
            Err(BenchError::Grove(_))
        ));
        let path_query = PathQuery::new_single_key(vec![b"tree".to_vec()], b"key".to_vec());
        let (proof, _) = reopened.prove(&path_query, grove_version).unwrap();
        let (proved, rows) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
        assert_eq!((proved, rows.len()), (root_hash, 1));

        let mut refused = reopened.begin_block();
        assert!(refused
            .apply(
                &insert(vec![b"absent".to_vec()], b"key", Element::new_item(vec![1])),
                grove_version
            )
            .is_err());
    }

    #[test]
    fn an_unreadable_log_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = RocksDbEngine::open(dir.path()).unwrap();
        let (number, _) = engine.log_files().unwrap().into_iter().last().unwrap();
        let fake = dir.path().join(format!("{:06}.log", number + 1000));
        std::fs::write(&fake, [1, 2, 3, 4, 5, 0, 9, 0, 0]).unwrap();
        let error = engine.settle().unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "write-ahead log {}: unknown log record type 9",
                fake.display()
            )
        );
        std::fs::remove_file(&fake).unwrap();
        std::fs::create_dir(&fake).unwrap();
        assert!(engine
            .settle()
            .unwrap_err()
            .to_string()
            .starts_with("reading"));
    }
}
