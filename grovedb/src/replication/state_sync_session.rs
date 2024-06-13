use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomPinned,
    pin::Pin,
};
use std::fs::Metadata;
use grovedb_costs::CostsExt;

use grovedb_merk::{CryptoHash, Restorer};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::PrefixedRocksDbImmediateStorageContext;

use super::{util_decode_vec_ops, util_split_global_chunk_id, CURRENT_STATE_SYNC_VERSION, util_create_global_chunk_id_2};
use crate::{replication::util_path_to_string, Error, GroveDb, Transaction, replication};
use crate::util::storage_context_optional_tx;

pub(crate) type SubtreePrefix = [u8; blake3::OUT_LEN];

pub(crate) type SubtreeMetadata = (SubtreePrefix, Vec<Vec<u8>>, /*Option<Vec<u8>>, bool,*/ CryptoHash, CryptoHash);

struct SubtreeStateSyncInfo<'db> {
    /// Current Chunk restorer
    restorer: Restorer<PrefixedRocksDbImmediateStorageContext<'db>>,
    /// Set of global chunk ids requested to be fetched and pending for
    /// processing. For the description of global chunk id check
    /// fetch_chunk().
    root_key: Option<Vec<u8>>,
    is_sum_tree: bool,
    pending_chunks: BTreeSet<Vec<u8>>,
    current_path: Vec<Vec<u8>>,
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
            root_key: None,
            is_sum_tree: false,
            pending_chunks: Default::default(),
            current_path: vec![],
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
    // Root app_hash
    app_hash: [u8; 32],
    // Version of state sync protocol,
    pub(crate) version: u16,
    // Transaction goes last to be dropped last as well
    transaction: Transaction<'db>,
    _pin: PhantomPinned,
}

impl<'db> MultiStateSyncSession<'db> {
    /// Initializes a new state sync session.
    pub fn new(transaction: Transaction<'db>, app_hash: [u8; 32]) -> Pin<Box<Self>> {
        Box::pin(MultiStateSyncSession {
            transaction,
            current_prefixes: Default::default(),
            processed_prefixes: Default::default(),
            app_hash,
            version: CURRENT_STATE_SYNC_VERSION,
            _pin: PhantomPinned,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.current_prefixes.is_empty()
    }

    pub fn is_sync_completed(&self) -> bool {
        for (_, subtree_state_info) in self.current_prefixes.iter() {
            if !subtree_state_info.pending_chunks.is_empty() {
                return false;
            }
        }
        return true;
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
        parent_path: Vec<Vec<u8>>,
        current_path: Vec<u8>,
    ) -> Result<(Vec<u8>), Error> {
        // SAFETY: we get an immutable reference of a transaction that stays behind
        // `Pin` so this reference shall remain valid for the whole session
        // object lifetime.
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &mut Transaction<'db> =
                &mut Pin::into_inner_unchecked(self.as_mut()).transaction;
            &*(tx as *mut _)
        };

        if let Ok((merk, root_key, is_sum_tree)) = db.open_merk_for_replication(path.clone(), transaction_ref) {
            let restorer = Restorer::new(merk, hash, actual_hash);
            let mut sync_info = SubtreeStateSyncInfo::new(restorer);
            sync_info.pending_chunks.insert(vec![]);
            sync_info.root_key = root_key.clone();
            sync_info.is_sum_tree = is_sum_tree;
            println!("{}", format!("adding:{} {:?} {} {:?}", hex::encode(chunk_prefix), root_key.clone(), is_sum_tree, util_path_to_string(path.to_vec().as_slice())));
            self.as_mut()
                .current_prefixes()
                .insert(chunk_prefix, sync_info);
            let x = util_create_global_chunk_id_2(chunk_prefix, root_key, is_sum_tree, vec![]);
            Ok((x))
        } else {
            Err(Error::InternalError("Unable to open merk for replication"))
        }
    }

    pub fn add_subtree_sync_info_2<'b, B: AsRef<[u8]>>(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        db: &'db GroveDb,
        metadata: SubtreeMetadata
    ) -> Result<(Vec<u8>), Error> {
        let (prefix, path, hash, actual_hash) = metadata;
        Ok(vec![])
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
        global_chunk_id: &[u8],
        chunk: Vec<u8>,
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

        // [OLD_WAY]
        //let (chunk_prefix, chunk_id) = util_split_global_chunk_id(global_chunk_id, self.app_hash)?;
        // [NEW_WAY]
        let (chunk_prefix, _, _, chunk_id) = replication::util_split_global_chunk_id_2(global_chunk_id, &self.app_hash)?;

        if self.is_empty() {
            return Err(Error::InternalError("GroveDB is not in syncing mode"));
        }

        let current_prefixes = self.as_mut().current_prefixes();
        let Some(subtree_state_sync) = current_prefixes.get_mut(&chunk_prefix) else {
            return Err(Error::InternalError("Unable to process incoming chunk"));
        };
        let Ok(res) = subtree_state_sync.apply_inner_chunk(&chunk_id, chunk) else {
            return Err(Error::InternalError("Invalid incoming prefix"));
        };

        if !res.is_empty() {
            for local_chunk_id in res.iter() {
                // [NEW_WAY]
                let x = util_create_global_chunk_id_2(chunk_prefix, subtree_state_sync.root_key.clone(), subtree_state_sync.is_sum_tree.clone(), local_chunk_id.clone());
                next_chunk_ids.push(x);
                // [OLD_WAY]
                //let mut next_global_chunk_id = chunk_prefix.to_vec();
                //next_global_chunk_id.extend(local_chunk_id.to_vec());
                //next_chunk_ids.push(next_global_chunk_id);
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

                let x = self.add_subtree_sync_info(
                    db,
                    path.into(),
                    elem_value_hash.clone(),
                    Some(actual_value_hash.clone()),
                    prefix.clone(),
                    vec![],
                    vec![],
                )?;

                // [NEW_WAY]
                res.push(x);
                // [OLD_WAY]
                //let root_chunk_prefix = prefix.to_vec();
                //res.push(root_chunk_prefix.to_vec());
                //res.push(prefix.to_vec());
            }
        }

        Ok(res)
    }
}

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