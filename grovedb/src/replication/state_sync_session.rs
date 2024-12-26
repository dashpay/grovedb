use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomPinned,
    pin::Pin,
};

use grovedb_merk::{CryptoHash, Restorer};
use grovedb_merk::tree::kv::ValueDefinedCostType;
use grovedb_merk::tree::value_hash;
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::{PrefixedRocksDbImmediateStorageContext, RocksDbStorage};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;
use super::{util_decode_vec_ops, CURRENT_STATE_SYNC_VERSION, util_create_global_chunk_id};
use crate::{replication::util_path_to_string, Error, GroveDb, Transaction, replication, Element};

pub(crate) type SubtreePrefix = [u8; blake3::OUT_LEN];

pub(crate) type SubtreeMetadata = (SubtreePrefix, Vec<Vec<u8>>, CryptoHash, CryptoHash);

struct SubtreeStateSyncInfo<'db> {
    /// Current Chunk restorer
    restorer: Restorer<PrefixedRocksDbImmediateStorageContext<'db>>,
    /// Set of global chunk ids requested to be fetched and pending for
    /// processing. For the description of global chunk id check
    /// fetch_chunk().
    pending_chunks: BTreeSet<Vec<u8>>,
    /// Tree root key
    root_key: Option<Vec<u8>>,
    /// Is Sum tree?
    is_sum_tree: bool,
    /// Path of current tree
    current_path: Vec<Vec<u8>>,
    /// Number of processed chunks in current prefix (Path digest)
    num_processed_chunks: usize,
}

impl<'db> SubtreeStateSyncInfo<'db> {
    pub fn get_current_path(&self) -> Vec<Vec<u8>> {
        self.current_path.clone()
    }

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
        grove_version: &GroveVersion,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let mut res = vec![];

        if !self.pending_chunks.contains(chunk_id) {
            return Err(Error::InternalError(
                "Incoming global_chunk_id not expected".to_string(),
            ));
        }
        self.pending_chunks.remove(chunk_id);
        if !chunk_data.is_empty() {
            match util_decode_vec_ops(chunk_data) {
                Ok(ops) => {
                    match self.restorer.process_chunk(chunk_id, ops, grove_version) {
                        Ok(next_chunk_ids) => {
                            self.num_processed_chunks += 1;
                            for next_chunk_id in next_chunk_ids {
                                self.pending_chunks.insert(next_chunk_id.clone());
                                res.push(next_chunk_id);
                            }
                        }
                        _ => {
                            return Err(Error::InternalError("Unable to process incoming chunk".to_string()));
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

        true
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
        grove_version: &GroveVersion,
    ) -> Result<(Vec<u8>), Error> {
        // SAFETY: we get an immutable reference of a transaction that stays behind
        // `Pin` so this reference shall remain valid for the whole session
        // object lifetime.
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &mut Transaction<'db> =
                &mut Pin::into_inner_unchecked(self.as_mut()).transaction;
            &*(tx as *mut _)
        };

        if let Ok((merk, root_key, is_sum_tree)) = db.open_merk_for_replication(path.clone(), transaction_ref, grove_version) {
            let restorer = Restorer::new(merk, hash, actual_hash);
            let mut sync_info = SubtreeStateSyncInfo::new(restorer);
            sync_info.pending_chunks.insert(vec![]);
            sync_info.root_key = root_key.clone();
            sync_info.is_sum_tree = is_sum_tree;
            sync_info.current_path = path.to_vec();
            self.as_mut()
                .current_prefixes()
                .insert(chunk_prefix, sync_info);
            Ok((util_create_global_chunk_id(chunk_prefix, root_key, is_sum_tree, vec![])))
        } else {
            Err(Error::InternalError("Unable to open merk for replication".to_string()))
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
        global_chunk_id: &[u8],
        chunk: Vec<u8>,
        version: u16,
        grove_version: &GroveVersion,
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

        let (chunk_prefix, _, _, chunk_id) = replication::util_split_global_chunk_id(global_chunk_id, &self.app_hash)?;

        if self.is_empty() {
            return Err(Error::InternalError("GroveDB is not in syncing mode".to_string()));
        }

        let current_prefixes = self.as_mut().current_prefixes();
        let Some(subtree_state_sync) = current_prefixes.get_mut(&chunk_prefix) else {
            return Err(Error::InternalError("Unable to process incoming chunk".to_string()));
        };
        let Ok(res) = subtree_state_sync.apply_inner_chunk(&chunk_id, chunk, grove_version) else {
            return Err(Error::InternalError("Invalid incoming prefix".to_string()));
        };

        if !res.is_empty() {
            for local_chunk_id in res.iter() {
                next_chunk_ids.push(util_create_global_chunk_id(chunk_prefix, subtree_state_sync.root_key.clone(), subtree_state_sync.is_sum_tree.clone(), local_chunk_id.clone()));
            }

            Ok(next_chunk_ids)
        } else {
            if !subtree_state_sync.pending_chunks.is_empty() {
                return Ok(vec![]);
            }

            let completed_path = subtree_state_sync.get_current_path();

            // Subtree is finished. We can save it.
            if subtree_state_sync.num_processed_chunks > 0 {
                if let Some(prefix_data) = current_prefixes.remove(&chunk_prefix) {
                    if let Err(err) = prefix_data.restorer.finalize(grove_version) {
                        return Err(Error::InternalError(format!(
                            "Unable to finalize Merk: {:?}",
                            err
                        )));
                    }
                } else {
                    return Err(Error::InternalError(format!(
                        "Prefix {:?} does not exist in current_prefixes",
                        chunk_prefix
                    )));
                }
            }

            self.as_mut().processed_prefixes().insert(chunk_prefix);

            println!("    finished tree: {:?}", util_path_to_string(completed_path.as_slice()));
            let new_subtrees_metadata = self.discover_new_subtrees_metadata(db, completed_path.to_vec(), grove_version)?;

            if let Ok(res) = self.prepare_sync_state_sessions(db, new_subtrees_metadata, grove_version) {
                next_chunk_ids.extend(res);
                Ok(next_chunk_ids)
            } else {
                Err(Error::InternalError("Unable to discover Subtrees".to_string()))
            }
        }
    }

    fn discover_new_subtrees_metadata(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        db: &'db GroveDb,
        path_vec: Vec<Vec<u8>>,
        grove_version: &GroveVersion,
    ) -> Result<SubtreesMetadata, Error> {
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &mut Transaction<'db> =
                &mut Pin::into_inner_unchecked(self.as_mut()).transaction;
            &*(tx as *mut _)
        };
        let subtree_path: Vec<&[u8]> = path_vec.iter().map(|vec| vec.as_slice()).collect();
        let path: &[&[u8]] = &subtree_path;
        let merk = db.open_transactional_merk_at_path(path.into(), transaction_ref, None, grove_version)
            .value
            .map_err(|e| Error::CorruptedData(
                format!("failed to open merk by path-tx:{}", e),
            ))?;
        if merk.is_empty_tree().unwrap() {
            return Ok(SubtreesMetadata::default());
        }
        let mut subtree_keys = BTreeSet::new();

        let mut raw_iter = Element::iterator(merk.storage.raw_iter()).unwrap();
        while let Some((key, value)) = raw_iter.next_element(grove_version).unwrap().unwrap() {
            if value.is_any_tree() {
                subtree_keys.insert(key.to_vec());
            }
        }

        let mut subtrees_metadata = SubtreesMetadata::new();
        for subtree_key in &subtree_keys {
            if let Ok(Some((elem_value, elem_value_hash))) = merk
                .get_value_and_value_hash(
                    subtree_key.as_slice(),
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version,
                )
                .value
            {
                let actual_value_hash = value_hash(&elem_value).unwrap();
                let mut new_path = path_vec.to_vec();
                new_path.push(subtree_key.to_vec());

                let subtree_path: Vec<&[u8]> = new_path.iter().map(|vec| vec.as_slice()).collect();
                let path: &[&[u8]] = &subtree_path;
                let prefix = RocksDbStorage::build_prefix(path.as_ref().into()).unwrap();

                println!("    discovered {:?} prefix:{}", util_path_to_string(&new_path), hex::encode(prefix));

                subtrees_metadata.data.insert(
                    prefix,
                    (new_path.to_vec(), actual_value_hash, elem_value_hash),
                );
            }
        }

        Ok((subtrees_metadata))
    }

    /// Prepares sync session for the freshly discovered subtrees and returns
    /// global chunk ids of those new subtrees.
    fn prepare_sync_state_sessions(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        db: &'db GroveDb,
        subtrees_metadata: SubtreesMetadata,
        grove_version: &GroveVersion,
    ) -> Result<Vec<Vec<u8>>, Error> {
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

                let next_chunks_ids = self.add_subtree_sync_info(
                    db,
                    path.into(),
                    elem_value_hash.clone(),
                    Some(actual_value_hash.clone()),
                    prefix.clone(),
                    grove_version
                )?;

                res.push(next_chunks_ids);
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
                metadata_path_str,
            )?;
        }
        Ok(())
    }
}