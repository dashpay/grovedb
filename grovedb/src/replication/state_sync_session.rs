use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomPinned,
    pin::Pin,
};

use grovedb_merk::{CryptoHash, Restorer};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::PrefixedRocksDbImmediateStorageContext;

use super::{util_decode_vec_ops, util_split_global_chunk_id, CURRENT_STATE_SYNC_VERSION};
use crate::{replication::util_path_to_string, Error, GroveDb, Transaction};

pub(crate) type SubtreePrefix = [u8; blake3::OUT_LEN];

struct SubtreeStateSyncInfo<'db> {
    /// Current Chunk restorer
    restorer: Restorer<PrefixedRocksDbImmediateStorageContext<'db>>,
    /// Set of global chunk ids requested to be fetched and pending for
    /// processing. For the description of global chunk id check
    /// fetch_chunk().
    pending_chunks: BTreeSet<Vec<u8>>,
    /// Number of processed chunks in current prefix (Path digest)
    num_processed_chunks: usize,
}

impl<'db> SubtreeStateSyncInfo<'db> {
    // Apply a chunk using the given SubtreeStateSyncInfo
    // state_sync_info: Consumed SubtreeStateSyncInfo
    // chunk_id: Local chunk id
    // chunk_data: Chunk proof operators encoded in bytes
    // Returns the next set of global chunk ids that can be fetched from sources (+
    // the SubtreeStateSyncInfo transferring ownership back to the caller)
    fn apply_inner_chunk(
        &mut self,
        chunk_id: &[u8],
        chunk_data: Vec<u8>,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let mut res = vec![];

        if !self.pending_chunks.contains(chunk_id) {
            return Err(Error::InternalError(
                "Incoming global_chunk_id not expected",
            ));
        }
        self.pending_chunks.remove(chunk_id);
        if !chunk_data.is_empty() {
            match util_decode_vec_ops(chunk_data) {
                Ok(ops) => {
                    match self.restorer.process_chunk(chunk_id, ops) {
                        Ok(next_chunk_ids) => {
                            self.num_processed_chunks += 1;
                            for next_chunk_id in next_chunk_ids {
                                self.pending_chunks.insert(next_chunk_id.clone());
                                res.push(next_chunk_id);
                            }
                        }
                        _ => {
                            return Err(Error::InternalError("Unable to process incoming chunk"));
                        }
                    };
                }
                Err(_) => {
                    return Err(Error::CorruptedData(
                        "Unable to decode incoming chunk".to_string(),
                    ));
                }
            }
        }

        Ok(res)
    }
}

impl<'tx> SubtreeStateSyncInfo<'tx> {
    pub fn new(restorer: Restorer<PrefixedRocksDbImmediateStorageContext<'tx>>) -> Self {
        SubtreeStateSyncInfo {
            restorer,
            pending_chunks: Default::default(),
            num_processed_chunks: 0,
        }
    }
}

// Struct governing state sync
pub struct MultiStateSyncSession<'db> {
    // Map of current processing subtrees
    // SubtreePrefix (Path digest) -> SubtreeStateSyncInfo
    current_prefixes: BTreeMap<SubtreePrefix, SubtreeStateSyncInfo<'db>>,
    // Set of processed prefixes (Path digests)
    processed_prefixes: BTreeSet<SubtreePrefix>,
    // Version of state sync protocol,
    pub(crate) version: u16,
    // Transaction goes last to be dropped last as well
    transaction: Transaction<'db>,
    _pin: PhantomPinned,
}

impl<'db> MultiStateSyncSession<'db> {
    /// Initializes a new state sync session.
    pub fn new(transaction: Transaction<'db>) -> Pin<Box<Self>> {
        Box::pin(MultiStateSyncSession {
            transaction,
            current_prefixes: Default::default(),
            processed_prefixes: Default::default(),
            version: CURRENT_STATE_SYNC_VERSION,
            _pin: PhantomPinned,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.current_prefixes.is_empty()
    }

    pub fn into_transaction(self: Pin<Box<Self>>) -> Transaction<'db> {
        // SAFETY: the struct isn't used anymore and no one will refer to transaction
        // address again
        unsafe { Pin::into_inner_unchecked(self) }.transaction
    }

    pub fn add_subtree_sync_info<'b, B: AsRef<[u8]>>(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        db: &'db GroveDb,
        path: SubtreePath<'b, B>,
        hash: CryptoHash,
        actual_hash: Option<CryptoHash>,
        chunk_prefix: [u8; 32],
    ) -> Result<(), Error> {
        // SAFETY: we get an immutable reference of a transaction that stays behind
        // `Pin` so this reference shall remain valid for the whole session
        // object lifetime.
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &mut Transaction<'db> =
                &mut Pin::into_inner_unchecked(self.as_mut()).transaction;
            &*(tx as *mut _)
        };

        if let Ok(merk) = db.open_merk_for_replication(path, transaction_ref) {
            let restorer = Restorer::new(merk, hash, actual_hash);
            let mut sync_info = SubtreeStateSyncInfo::new(restorer);
            sync_info.pending_chunks.insert(vec![]);
            self.as_mut()
                .current_prefixes()
                .insert(chunk_prefix, sync_info);
            Ok(())
        } else {
            Err(Error::InternalError("Unable to open merk for replication"))
        }
    }

    fn current_prefixes(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut BTreeMap<SubtreePrefix, SubtreeStateSyncInfo<'db>> {
        // SAFETY: no memory-sensitive assumptions are made about fields except the
        // `transaciton` so it will be safe to modify them
        &mut unsafe { self.get_unchecked_mut() }.current_prefixes
    }

    fn processed_prefixes(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut BTreeSet<SubtreePrefix> {
        // SAFETY: no memory-sensitive assumptions are made about fields except the
        // `transaciton` so it will be safe to modify them
        &mut unsafe { self.get_unchecked_mut() }.processed_prefixes
    }

    /// Applies a chunk, shuold be called by ABCI when `ApplySnapshotChunk`
    /// method is called. `chunk` is a pair of global chunk id and an
    /// encoded proof.
    pub fn apply_chunk(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        db: &'db GroveDb,
        chunk: (&[u8], Vec<u8>),
        version: u16,
    ) -> Result<Vec<Vec<u8>>, Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }
        if version != self.version {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let mut next_chunk_ids = vec![];

        let (global_chunk_id, chunk_data) = chunk;
        let (chunk_prefix, chunk_id) = util_split_global_chunk_id(global_chunk_id)?;

        if self.is_empty() {
            return Err(Error::InternalError("GroveDB is not in syncing mode"));
        }

        let current_prefixes = self.as_mut().current_prefixes();
        let Some(subtree_state_sync) = current_prefixes.get_mut(&chunk_prefix) else {
            return Err(Error::InternalError("Unable to process incoming chunk"));
        };
        let Ok(res) = subtree_state_sync.apply_inner_chunk(&chunk_id, chunk_data) else {
            return Err(Error::InternalError("Invalid incoming prefix"));
        };

        if !res.is_empty() {
            for local_chunk_id in res.iter() {
                let mut next_global_chunk_id = chunk_prefix.to_vec();
                next_global_chunk_id.extend(local_chunk_id.to_vec());
                next_chunk_ids.push(next_global_chunk_id);
            }

            Ok(next_chunk_ids)
        } else {
            if !subtree_state_sync.pending_chunks.is_empty() {
                return Ok(vec![]);
            }

            // Subtree is finished. We can save it.
            if (subtree_state_sync.num_processed_chunks > 0)
                && (current_prefixes
                    .remove(&chunk_prefix)
                    .expect("prefix exists")
                    .restorer
                    .finalize()
                    .is_err())
            {
                return Err(Error::InternalError("Unable to finalize Merk"));
            }
            self.as_mut().processed_prefixes().insert(chunk_prefix);

            // // Subtree was successfully save. Time to discover new subtrees that
            // // need to be processed
            // if let Some(value) = subtrees_metadata.data.get(&chunk_prefix) {
            //     println!(
            //         "    path:{:?} done (num_processed_chunks:{:?})",
            //         util_path_to_string(&value.0),
            //         subtree_state_sync.num_processed_chunks
            //     );
            // }

            if let Ok(res) = self.discover_subtrees(db) {
                next_chunk_ids.extend(res);
                Ok(next_chunk_ids)
            } else {
                Err(Error::InternalError("Unable to discover Subtrees"))
            }
        }
    }

    /// Prepares sync session for the freshly discovered subtrees and returns
    /// global chunk ids of those new subtrees.
    fn discover_subtrees(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        db: &'db GroveDb,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let subtrees_metadata = db.get_subtrees_metadata(Some(&self.transaction))?;

        let mut res = vec![];

        for (prefix, prefix_metadata) in &subtrees_metadata.data {
            if !self.processed_prefixes.contains(prefix)
                && !self.current_prefixes.contains_key(prefix)
            {
                let (current_path, actual_value_hash, elem_value_hash) = &prefix_metadata;

                let subtree_path: Vec<&[u8]> =
                    current_path.iter().map(|vec| vec.as_slice()).collect();
                let path: &[&[u8]] = &subtree_path;
                println!(
                    "    path:{:?} starting...",
                    util_path_to_string(&prefix_metadata.0)
                );

                self.add_subtree_sync_info(
                    db,
                    path.into(),
                    elem_value_hash.clone(),
                    Some(actual_value_hash.clone()),
                    prefix.clone(),
                )?;
                res.push(prefix.to_vec());
            }
        }

        Ok(res)
    }
}

// impl<'db> Default for MultiStateSyncInfo<'db> {
//     fn default() -> Self {
//         Self {
//             current_prefixes: BTreeMap::new(),
//             processed_prefixes: BTreeSet::new(),
//             version: CURRENT_STATE_SYNC_VERSION,
//         }
//     }
// }

// fn lol(db: &GroveDb) -> MultiStateSyncSession {
//     let mut sync = MultiStateSyncSession {
//         transaction: db.start_transaction(),
//         current_prefixes: Default::default(),
//         processed_prefixes: Default::default(),
//         version: 0,
//     };

//     sync.current_prefixes.insert(
//         b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
//         SubtreeStateSyncInfo {
//             restorer: Some(Restorer::new(
//                 db.open_merk_for_replication(SubtreePath::empty(),
// &sync.transaction)                     .unwrap(),
//                 b"11111111111111111111111111111111".to_owned(),
//                 None,
//             )),
//             pending_chunks: Default::default(),
//             num_processed_chunks: 0,
//         },
//     );

//     let ass: Option<&mut SubtreeStateSyncInfo> =
// sync.current_prefixes.values_mut().next();

//     let ass2: &mut SubtreeStateSyncInfo = ass.unwrap();

//     ass2.apply_inner_chunk(b"a", vec![]).unwrap();

//     sync
// }

// Struct containing information about current subtrees found in GroveDB
pub struct SubtreesMetadata {
    // Map of Prefix (Path digest) -> (Actual path, Parent Subtree actual_value_hash, Parent
    // Subtree elem_value_hash) Note: Parent Subtree actual_value_hash, Parent Subtree
    // elem_value_hash are needed when verifying the new constructed subtree after wards.
    pub data: BTreeMap<SubtreePrefix, (Vec<Vec<u8>>, CryptoHash, CryptoHash)>,
}

impl SubtreesMetadata {
    pub fn new() -> SubtreesMetadata {
        SubtreesMetadata {
            data: BTreeMap::new(),
        }
    }
}

impl Default for SubtreesMetadata {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SubtreesMetadata {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (prefix, metadata) in self.data.iter() {
            let metadata_path = &metadata.0;
            let metadata_path_str = util_path_to_string(metadata_path);
            writeln!(
                f,
                " prefix:{:?} -> path:{:?}",
                hex::encode(prefix),
                metadata_path_str
            )?;
        }
        Ok(())
    }
}
