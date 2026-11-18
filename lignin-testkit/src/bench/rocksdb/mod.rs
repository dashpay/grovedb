//! The harness's engine over today's RocksDB storage.
//!
//! GroveDB opens RocksDB itself, with statistics off, and keeps the handle
//! private, so the counters are read from what RocksDB leaves in the
//! database directory:
//!
//! - `log_bytes`, `log_groups`, `node_records`, `node_record_bytes`,
//!   `other_records` and `other_record_bytes`: the write-ahead log files
//!   (`*.log`), read after every commit by [`wal`]. What the logs held
//!   when the engine was opened is skipped at the first `settle`. Every commit is one
//!   write batch. Merk nodes are the puts to the default column family
//!   (id 0); the other families are aux (1), roots (2) and meta (3), in the
//!   order GroveDB creates them. RocksDB deletes a log only once a later
//!   one is in use, which a commit by the harness causes, and the log is
//!   read before every commit; consecutive write batches carry consecutive
//!   sequence numbers, so a batch the reader missed fails the run instead of
//!   going uncounted.
//! - `flush_bytes` and `compaction_bytes`: the `EVENT_LOG_v1` lines of the
//!   info log (`LOG`), read by [`info_log`].
//! - `page_reads` and `cache_hits`: not available without RocksDB's
//!   statistics.
//!
//! The node anatomy decodes every default-family put in the log as a Merk
//! node record: the key is the subtree's 32-byte prefix and the node's key.

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
use grovedb_merk::tree::{kv::ValueDefinedCostType, TreeNode};
use grovedb_version::version::GroveVersion;

use self::{
    info_log::{EventTotals, InfoLogTail},
    wal::{LogTail, WalTotals},
};
use super::{
    engine::{Block, Counters, Engine, NodeAnatomy},
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
    anatomy: NodeAnatomy,
    /// Whether the logs that existed at open have been read past.
    positioned: bool,
}

/// How long [`Engine::wait_for_background`] waits at most.
const BACKGROUND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);
/// Polls of the info log, this far apart, that must all find no flush or
/// compaction running: a finished job can start the next one.
const IDLE_POLLS: u32 = 5;
const IDLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// The length of a subtree prefix at the front of every node record's key.
const PREFIX_LEN: usize = 32;
const HASH_LEN: u64 = 32;

impl NodeAnatomy {
    /// Adds one default-family record, decoding it as a Merk node.
    fn add_record(&mut self, key: &[u8], value: &[u8]) {
        self.records += 1;
        let total = (key.len() + value.len()) as u64;
        let node = key.get(PREFIX_LEN..).and_then(|node_key| {
            TreeNode::decode_raw(
                value,
                node_key.to_vec(),
                None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                GroveVersion::latest(),
            )
            .ok()
        });
        let Some(node) = node else {
            self.undecoded += 1;
            self.other_bytes += total;
            return;
        };
        let links: Vec<_> = [true, false]
            .into_iter()
            .filter_map(|left| node.link(left))
            .collect();
        let keys = key.len() as u64
            + links
                .iter()
                .map(|link| link.key().len() as u64)
                .sum::<u64>();
        let values = node.value_as_slice().len() as u64;
        let link_hashes = HASH_LEN * links.len() as u64;
        let node_hashes = 2 * HASH_LEN;
        self.key_bytes += keys;
        self.value_bytes += values;
        self.node_hash_bytes += node_hashes;
        self.link_hash_bytes += link_hashes;
        self.other_bytes += total.saturating_sub(keys + values + link_hashes + node_hashes);
    }
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
            if path.extension().is_some_and(|extension| extension == "log")
                && let Some(number) = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u64>().ok())
            {
                files.insert(number, path);
            }
        }
        Ok(files)
    }

    /// Skips what the logs held when the engine was opened: each log is read
    /// to its last record and nothing it held is counted. The sequence
    /// number the last batch read leads to is kept, so a batch missed after
    /// the open is still a gap.
    fn position_logs(&mut self) -> Result<(), BenchError> {
        let mut skipped = WalTotals::default();
        for (number, path) in self.log_files()? {
            let mut tail = LogTail::default();
            tail.read(&read_from(&path, 0)?, &mut skipped)
                .map_err(|error| BenchError::Wal {
                    file: path.display().to_string(),
                    error,
                })?;
            self.logs.insert(number, tail);
        }
        self.wal.next_sequence = skipped.next_sequence;
        self.positioned = true;
        Ok(())
    }

    fn read_logs(&mut self) -> Result<(), BenchError> {
        if !self.positioned {
            return self.position_logs();
        }
        let files = self.log_files()?;
        self.logs.retain(|number, _| files.contains_key(number));
        for (number, path) in files {
            // RocksDB deletes a log it no longer needs at any time, also
            // between the listing and this read.
            let length = match std::fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.logs.remove(&number);
                    continue;
                }
                Err(error) => {
                    return Err(BenchError::io(format!("reading {}", path.display()), error));
                }
            };
            let tail = self.logs.entry(number).or_default();
            if length < tail.offset() {
                return Err(BenchError::Check(format!(
                    "write-ahead log {} is shorter than what was read of it",
                    path.display()
                )));
            }
            let data = read_from(&path, tail.offset())?;
            let anatomy = &mut self.anatomy;
            tail.read_visiting(&data, &mut self.wal, &mut |family, key, value| {
                if family == 0 {
                    anatomy.add_record(key, value);
                }
            })
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
            anatomy: NodeAnatomy::default(),
            positioned: false,
        };
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

    fn wait_for_background(&mut self) -> Result<(), BenchError> {
        let deadline = std::time::Instant::now() + BACKGROUND_TIMEOUT;
        let mut idle_polls = 0;
        while idle_polls < IDLE_POLLS {
            if std::time::Instant::now() > deadline {
                return Err(BenchError::Check(format!(
                    "RocksDB's background work did not go idle in {BACKGROUND_TIMEOUT:?}"
                )));
            }
            std::thread::sleep(IDLE_POLL_INTERVAL);
            self.read_info_log()?;
            idle_polls = if self.events.idle() {
                idle_polls + 1
            } else {
                0
            };
        }
        Ok(())
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

    fn node_anatomy(&self) -> Option<NodeAnatomy> {
        Some(self.anatomy)
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
        let CostContext { value, cost } =
            apply(self.db, op, Some(&self.transaction), grove_version);
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
            .apply(
                &insert(vec![], b"tree", Element::empty_tree()),
                grove_version,
            )
            .unwrap();
        let cost = block
            .apply(
                &insert(
                    vec![b"tree".to_vec()],
                    b"key",
                    Element::new_item(vec![7; 100]),
                ),
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
        let anatomy = engine.node_anatomy().unwrap();
        assert_eq!(anatomy.records, after_block.node_records);
        assert_eq!(anatomy.undecoded, 0);
        assert_eq!(anatomy.total(), after_block.node_record_bytes);
        assert!(anatomy.value_bytes > 100);
        assert_eq!(anatomy.node_hash_bytes, 64 * anatomy.records);
        let mut odd = NodeAnatomy::default();
        odd.add_record(b"short", b"not a node");
        odd.add_record(&[0; 40], b"");
        assert_eq!((odd.records, odd.undecoded, odd.other_bytes), (2, 2, 55));
        assert_eq!(anatomy.since(&anatomy), NodeAnatomy::default());

        engine
            .db
            .put_aux(b"aux-key", b"aux-value", None, None)
            .unwrap()
            .unwrap();
        engine.settle().unwrap();
        let aux = engine.wal_totals().families.get(&1).copied().unwrap();
        // The key carries the 32-byte prefix of the root path.
        assert_eq!(
            (aux.puts, aux.put_bytes),
            (1, 32 + 7 + 9),
            "aux is family 1"
        );

        engine.flush().unwrap();
        engine.wait_for_background().unwrap();
        assert!(engine.event_totals().idle());
        let flushed = engine.counters().unwrap().since(&after_block);
        assert!(flushed.flush_bytes > 0, "{:?}", engine.event_totals());
        assert_eq!(flushed.compaction_bytes, 0);
        assert_eq!(
            flushed.physical_bytes(),
            flushed.log_bytes + flushed.flush_bytes
        );

        let checkpoint = dir.path().join("..").join(format!(
            "{}-checkpoint",
            dir.path().file_name().unwrap().to_str().unwrap()
        ));
        engine.checkpoint(&checkpoint).unwrap();
        assert!(engine.checkpoint(&checkpoint).is_err());
        std::fs::remove_dir_all(&checkpoint).unwrap();
        assert_eq!(engine.dir(), dir.path());
        let mut block = engine.begin_block();
        block
            .apply(
                &insert(
                    vec![b"tree".to_vec()],
                    b"unflushed",
                    Element::new_item(vec![2; 10]),
                ),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        engine.settle().unwrap();
        let root_hash = engine.root_hash(grove_version).unwrap();
        assert!(engine.wal_totals().next_sequence.is_some());
        drop(engine);

        let mut reopened = RocksDbEngine::open(dir.path()).unwrap();
        assert_eq!(reopened.counters().unwrap().log_bytes, 0);
        assert_eq!(reopened.root_hash(grove_version).unwrap(), root_hash);
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

        let mut block = reopened.begin_block();
        block
            .apply(
                &insert(
                    vec![b"tree".to_vec()],
                    b"after",
                    Element::new_item(vec![1; 10]),
                ),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        reopened.settle().unwrap();
        let counted = reopened.counters().unwrap();
        assert_eq!(counted.log_groups, 1, "only the block after the reopen");
        assert!(counted.node_records >= 2);

        let mut refused = reopened.begin_block();
        assert!(refused
            .apply(
                &insert(vec![b"absent".to_vec()], b"key", Element::new_item(vec![1])),
                grove_version
            )
            .is_err());
    }

    #[test]
    fn positioning_skips_the_logs_and_keeps_their_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = RocksDbEngine::open(dir.path()).unwrap();
        let (number, _) = engine.log_files().unwrap().into_iter().next_back().unwrap();
        // One write batch of one put, at sequence 41, in one full record.
        let mut batch = 41u64.to_le_bytes().to_vec();
        batch.extend_from_slice(&1u32.to_le_bytes());
        batch.extend_from_slice(&[0x1, 1, b'k', 1, b'v']);
        let mut record = vec![0, 0, 0, 0];
        record.extend_from_slice(&(batch.len() as u16).to_le_bytes());
        record.push(1);
        record.extend_from_slice(&batch);
        let planted = dir.path().join(format!("{:06}.log", number + 1000));
        std::fs::write(&planted, &record).unwrap();

        engine.settle().unwrap();
        assert_eq!(engine.wal_totals().next_sequence, Some(42));
        assert_eq!(engine.wal_totals().batches, 0, "nothing before is counted");
        assert_eq!(
            engine.logs.get(&(number + 1000)).unwrap().offset(),
            record.len() as u64
        );
        std::fs::remove_file(&planted).unwrap();
        engine.settle().unwrap();
        assert!(!engine.logs.contains_key(&(number + 1000)));
    }

    #[test]
    fn a_log_shorter_than_what_was_read_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let mut engine = RocksDbEngine::open(dir.path()).unwrap();
        engine.settle().unwrap();
        let mut block = engine.begin_block();
        block
            .apply(
                &insert(vec![], b"tree", Element::empty_tree()),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        engine.settle().unwrap();
        let (_, live) = engine.log_files().unwrap().into_iter().next_back().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&live)
            .unwrap()
            .set_len(0)
            .unwrap();
        assert_eq!(
            engine.settle().unwrap_err().to_string(),
            format!(
                "write-ahead log {} is shorter than what was read of it",
                live.display()
            )
        );
    }

    #[test]
    fn an_unreadable_log_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = RocksDbEngine::open(dir.path()).unwrap();
        let (number, _) = engine.log_files().unwrap().into_iter().next_back().unwrap();
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
