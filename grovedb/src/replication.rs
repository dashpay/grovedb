use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use grovedb_merk::{
    merk::restore::Restorer,
    proofs::Op,
    tree::{hash::CryptoHash, kv::ValueDefinedCostType, value_hash},
    ChunkProducer,
};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
#[rustfmt::skip]
use grovedb_storage::rocksdb_storage::storage_context::context_immediate::PrefixedRocksDbImmediateStorageContext;

use crate::{replication, Error, GroveDb, Transaction, TransactionArg};

pub(crate) type SubtreePrefix = [u8; blake3::OUT_LEN];

pub const CURRENT_STATE_SYNC_VERSION: u16 = 1;

struct SubtreeStateSyncInfo<'db> {
    // Current Chunk restorer
    pub restorer: Option<Restorer<PrefixedRocksDbImmediateStorageContext<'db>>>,
    // Set of global chunk ids requested to be fetched and pending for processing. For the
    // description of global chunk id check fetch_chunk().
    pub pending_chunks: BTreeSet<Vec<u8>>,
    // Number of processed chunks in current prefix (Path digest)
    pub num_processed_chunks: usize,
}

// Struct governing state sync
pub struct MultiStateSyncInfo<'db> {
    // Map of current processing subtrees
    // SubtreePrefix (Path digest) -> SubtreeStateSyncInfo
    pub current_prefixes: BTreeMap<SubtreePrefix, SubtreeStateSyncInfo<'db>>,
    // Set of processed prefixes (Path digests)
    pub processed_prefixes: BTreeSet<SubtreePrefix>,
    // Version of state sync protocol,
    pub version: u16,
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
            );
        }
        Ok(())
    }
}

// Converts a path into a human-readable string (for debugging)
pub fn util_path_to_string(path: &[Vec<u8>]) -> Vec<String> {
    let mut subtree_path_str: Vec<String> = vec![];
    for subtree in path {
        let string = std::str::from_utf8(subtree).expect("should be able to convert path");
        subtree_path_str.push(
            string
                .parse()
                .expect("should be able to parse path to string"),
        );
    }
    subtree_path_str
}

// Splits the given global chunk id into [SUBTREE_PREFIX:CHUNK_ID]
pub fn util_split_global_chunk_id(
    global_chunk_id: &[u8],
) -> Result<(crate::SubtreePrefix, Vec<u8>), Error> {
    let chunk_prefix_length: usize = 32;
    if global_chunk_id.len() < chunk_prefix_length {
        return Err(Error::CorruptedData(
            "expected global chunk id of at least 32 length".to_string(),
        ));
    }

    let (chunk_prefix, chunk_id) = global_chunk_id.split_at(chunk_prefix_length);
    let mut array = [0u8; 32];
    array.copy_from_slice(chunk_prefix);
    let chunk_prefix_key: crate::SubtreePrefix = array;
    Ok((chunk_prefix_key, chunk_id.to_vec()))
}

#[cfg(feature = "full")]
impl GroveDb {
    pub fn create_subtree_state_sync_info(&self) -> SubtreeStateSyncInfo {
        let pending_chunks = BTreeSet::new();
        SubtreeStateSyncInfo {
            restorer: None,
            pending_chunks,
            num_processed_chunks: 0,
        }
    }

    pub fn create_multi_state_sync_info(&self) -> MultiStateSyncInfo {
        let processed_prefixes = BTreeSet::new();
        let current_prefixes = BTreeMap::default();
        MultiStateSyncInfo {
            current_prefixes,
            processed_prefixes,
            version: CURRENT_STATE_SYNC_VERSION,
        }
    }

    // Returns the discovered subtrees found recursively along with their associated
    // metadata Params:
    // tx: Transaction. Function returns the data by opening merks at given tx.
    // TODO: Add a SubTreePath as param and start searching from that path instead
    // of root (as it is now)
    pub fn get_subtrees_metadata(&self, tx: TransactionArg) -> Result<SubtreesMetadata, Error> {
        let mut subtrees_metadata = crate::replication::SubtreesMetadata::new();

        let subtrees_root = self.find_subtrees(&SubtreePath::empty(), tx).value?;
        for subtree in subtrees_root.into_iter() {
            let subtree_path: Vec<&[u8]> = subtree.iter().map(|vec| vec.as_slice()).collect();
            let path: &[&[u8]] = &subtree_path;
            let prefix = RocksDbStorage::build_prefix(path.as_ref().into()).unwrap();

            let current_path = SubtreePath::from(path);

            match (current_path.derive_parent(), subtree.last()) {
                (Some((parent_path, _)), Some(parent_key)) => match tx {
                    None => {
                        let parent_merk = self
                            .open_non_transactional_merk_at_path(parent_path, None)
                            .value?;
                        if let Ok(Some((elem_value, elem_value_hash))) = parent_merk
                            .get_value_and_value_hash(
                                parent_key,
                                true,
                                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                            )
                            .value
                        {
                            let actual_value_hash = value_hash(&elem_value).unwrap();
                            subtrees_metadata.data.insert(
                                prefix,
                                (current_path.to_vec(), actual_value_hash, elem_value_hash),
                            );
                        }
                    }
                    Some(t) => {
                        let parent_merk = self
                            .open_transactional_merk_at_path(parent_path, t, None)
                            .value?;
                        if let Ok(Some((elem_value, elem_value_hash))) = parent_merk
                            .get_value_and_value_hash(
                                parent_key,
                                true,
                                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                            )
                            .value
                        {
                            let actual_value_hash = value_hash(&elem_value).unwrap();
                            subtrees_metadata.data.insert(
                                prefix,
                                (current_path.to_vec(), actual_value_hash, elem_value_hash),
                            );
                        }
                    }
                },
                _ => {
                    subtrees_metadata.data.insert(
                        prefix,
                        (
                            current_path.to_vec(),
                            CryptoHash::default(),
                            CryptoHash::default(),
                        ),
                    );
                }
            }
        }
        Ok(subtrees_metadata)
    }

    // Fetch a chunk by global chunk id (should be called by ABCI when
    // LoadSnapshotChunk method is called) Params:
    // global_chunk_id: Global chunk id in the following format:
    // [SUBTREE_PREFIX:CHUNK_ID] SUBTREE_PREFIX: 32 bytes (mandatory) (All zeros
    // = Root subtree) CHUNK_ID: 0.. bytes (optional) Traversal instructions to
    // the root of the given chunk. Traversal instructions are "1" for left, and
    // "0" for right. TODO: Compact CHUNK_ID into bitset for size optimization
    // as a subtree can be big hence traversal instructions for the deepest chunks
    // tx: Transaction. Function returns the data by opening merks at given tx.
    // Returns the Chunk proof operators for the requested chunk
    pub fn fetch_chunk(
        &self,
        global_chunk_id: &[u8],
        tx: TransactionArg,
        version: u16,
    ) -> Result<Vec<Op>, Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let chunk_prefix_length: usize = 32;
        if global_chunk_id.len() < chunk_prefix_length {
            return Err(Error::CorruptedData(
                "expected global chunk id of at least 32 length".to_string(),
            ));
        }

        let (chunk_prefix, chunk_id) = global_chunk_id.split_at(chunk_prefix_length);

        let mut array = [0u8; 32];
        array.copy_from_slice(chunk_prefix);
        let chunk_prefix_key: crate::SubtreePrefix = array;

        let subtrees_metadata = self.get_subtrees_metadata(tx)?;

        match subtrees_metadata.data.get(&chunk_prefix_key) {
            Some(path_data) => {
                let subtree = &path_data.0;
                let subtree_path: Vec<&[u8]> = subtree.iter().map(|vec| vec.as_slice()).collect();
                let path: &[&[u8]] = &subtree_path;

                match tx {
                    None => {
                        let merk = self
                            .open_non_transactional_merk_at_path(path.into(), None)
                            .value?;

                        if merk.is_empty_tree().unwrap() {
                            return Ok(vec![]);
                        }

                        let chunk_producer_res = ChunkProducer::new(&merk);
                        match chunk_producer_res {
                            Ok(mut chunk_producer) => {
                                let chunk_res = chunk_producer.chunk(chunk_id);
                                match chunk_res {
                                    Ok((chunk, _)) => Ok(chunk),
                                    Err(_) => Err(Error::CorruptedData(
                                        "Unable to create to load chunk".to_string(),
                                    )),
                                }
                            }
                            Err(_) => Err(Error::CorruptedData(
                                "Unable to create Chunk producer".to_string(),
                            )),
                        }
                    }
                    Some(t) => {
                        let merk = self
                            .open_transactional_merk_at_path(path.into(), t, None)
                            .value?;

                        if merk.is_empty_tree().unwrap() {
                            return Ok(vec![]);
                        }

                        let chunk_producer_res = ChunkProducer::new(&merk);
                        match chunk_producer_res {
                            Ok(mut chunk_producer) => {
                                let chunk_res = chunk_producer.chunk(chunk_id);
                                match chunk_res {
                                    Ok((chunk, _)) => Ok(chunk),
                                    Err(_) => Err(Error::CorruptedData(
                                        "Unable to create to load chunk".to_string(),
                                    )),
                                }
                            }
                            Err(_) => Err(Error::CorruptedData(
                                "Unable to create Chunk producer".to_string(),
                            )),
                        }
                    }
                }
            }
            None => Err(Error::CorruptedData("Prefix not found".to_string())),
        }
    }

    // Starts a state sync process (should be called by ABCI when OfferSnapshot
    // method is called) Params:
    // state_sync_info: Consumed StateSyncInfo
    // app_hash: Snapshot's AppHash
    // tx: Transaction for the state sync
    // Returns the first set of global chunk ids that can be fetched from sources (+
    // the StateSyncInfo transferring ownership back to the caller)
    pub fn start_snapshot_syncing<'db>(
        &'db self,
        mut state_sync_info: MultiStateSyncInfo<'db>,
        app_hash: CryptoHash,
        tx: &'db Transaction,
        version: u16,
    ) -> Result<(Vec<Vec<u8>>, MultiStateSyncInfo), Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }
        if version != state_sync_info.version {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let mut res = vec![];

        if !state_sync_info.current_prefixes.is_empty() || !state_sync_info.processed_prefixes.is_empty() {
            return Err(Error::InternalError(
                "GroveDB has already started a snapshot syncing",
            ));
        }

        println!(
            "    starting:{:?}...",
            replication::util_path_to_string(&vec![])
        );

        let mut root_prefix_state_sync_info = self.create_subtree_state_sync_info();
        let root_prefix = [0u8; 32];
        if let Ok(merk) = self.open_merk_for_replication(SubtreePath::empty(), tx) {
            let restorer = Restorer::new(merk, app_hash, None);
            root_prefix_state_sync_info.restorer = Some(restorer);
            root_prefix_state_sync_info.pending_chunks.insert(vec![]);
            state_sync_info.current_prefixes.insert(root_prefix, root_prefix_state_sync_info);

            res.push(root_prefix.to_vec());
        } else {
            return Err(Error::InternalError("Unable to open merk for replication"));
        }

        Ok((res, state_sync_info))
    }


    // Apply a chunk (should be called by ABCI when ApplySnapshotChunk method is
    // called) Params:
    // state_sync_info: Consumed MultiStateSyncInfo
    // chunk: (Global chunk id, Chunk proof operators)
    // tx: Transaction for the state sync
    // Returns the next set of global chunk ids that can be fetched from sources (+
    // the MultiStateSyncInfo transferring ownership back to the caller)
    pub fn apply_chunk<'db>(
        &'db self,
        mut state_sync_info: MultiStateSyncInfo<'db>,
        chunk: (&[u8], Vec<Op>),
        tx: &'db Transaction,
        version: u16,
    ) -> Result<(Vec<Vec<u8>>, MultiStateSyncInfo), Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }
        if version != state_sync_info.version {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let mut next_chunk_ids = vec![];

        let (global_chunk_id, chunk_data) = chunk;
        let (chunk_prefix, chunk_id) = replication::util_split_global_chunk_id(global_chunk_id)?;

        if state_sync_info.current_prefixes.is_empty() {
            return Err(Error::InternalError("GroveDB is not in syncing mode"));
        }
        if let Some(mut subtree_state_sync) = state_sync_info.current_prefixes.remove(&chunk_prefix) {
            if let Ok((res, mut new_subtree_state_sync)) = self.apply_inner_chunk(subtree_state_sync, &chunk_id, chunk_data) {
                if !res.is_empty() {
                    for local_chunk_id in res.iter() {
                        let mut next_global_chunk_id = chunk_prefix.to_vec();
                        next_global_chunk_id.extend(local_chunk_id.to_vec());
                        next_chunk_ids.push(next_global_chunk_id);
                    }

                    // re-insert subtree_state_sync in state_sync_info
                    state_sync_info.current_prefixes.insert(chunk_prefix, new_subtree_state_sync);
                    Ok((next_chunk_ids, state_sync_info))
                }
                else {
                    if !new_subtree_state_sync.pending_chunks.is_empty() {
                        // re-insert subtree_state_sync in state_sync_info
                        state_sync_info.current_prefixes.insert(chunk_prefix, new_subtree_state_sync);
                        return Ok((vec![], state_sync_info))
                    }

                    // Subtree is finished. We can save it.
                    match new_subtree_state_sync.restorer.take() {
                        None => {
                            return Err(Error::InternalError("Unable to finalize subtree"));
                        }
                        Some(restorer) => {
                            if (new_subtree_state_sync.num_processed_chunks > 0) && (restorer.finalize().is_err()) {
                                return Err(Error::InternalError("Unable to finalize Merk"));
                            }
                            state_sync_info.processed_prefixes.insert(chunk_prefix);


                            // Subtree was successfully save. Time to discover new subtrees that need to be processed
                            let subtrees_metadata = self.get_subtrees_metadata(Some(tx))?;
                            if let Some(value) = subtrees_metadata.data.get(&chunk_prefix) {
                                println!(
                                    "    path:{:?} done (num_processed_chunks:{:?})",
                                    replication::util_path_to_string(&value.0),
                                    new_subtree_state_sync.num_processed_chunks
                                );
                            }

                            if let Ok((res,new_state_sync_info)) = self.discover_subtrees(state_sync_info, subtrees_metadata, tx) {
                                next_chunk_ids.extend(res);
                                Ok((next_chunk_ids, new_state_sync_info))
                            }
                            else {
                                return Err(Error::InternalError("Unable to discover Subtrees"));
                            }
                        }
                    }
                }
            }
            else {
                return Err(Error::InternalError("Unable to process incoming chunk"));
            }
        }
        else {
            return Err(Error::InternalError("Invalid incoming prefix"));
        }
    }

    // Apply a chunk using the given SubtreeStateSyncInfo
    // state_sync_info: Consumed SubtreeStateSyncInfo
    // chunk_id: Local chunk id
    // chunk_data: Chunk proof operators
    // Returns the next set of global chunk ids that can be fetched from sources (+
    // the SubtreeStateSyncInfo transferring ownership back to the caller)
    fn apply_inner_chunk<'db>(
        &'db self,
        mut state_sync_info: SubtreeStateSyncInfo<'db>,
        chunk_id: &[u8],
        chunk_data: Vec<Op>,
    ) -> Result<(Vec<Vec<u8>>, SubtreeStateSyncInfo), Error> {
        let mut res = vec![];

        match &mut state_sync_info.restorer {
            Some(restorer) => {
                if !state_sync_info.pending_chunks.contains(chunk_id) {
                    return Err(Error::InternalError(
                        "Incoming global_chunk_id not expected",
                    ));
                }
                state_sync_info.pending_chunks.remove(chunk_id);
                if !chunk_data.is_empty() {
                    match restorer.process_chunk(&chunk_id, chunk_data) {
                        Ok(next_chunk_ids) => {
                            state_sync_info.num_processed_chunks += 1;
                            for next_chunk_id in next_chunk_ids {
                                state_sync_info
                                    .pending_chunks
                                    .insert(next_chunk_id.clone());
                                res.push(next_chunk_id);
                            }
                        }
                        _ => {
                            return Err(Error::InternalError("Unable to process incoming chunk"));
                        }
                    };
                }
            },
            _ => {
                return Err(Error::InternalError("Invalid internal state (restorer"));
            }
        }

        Ok((res, state_sync_info))
    }

    // Prepares SubtreeStateSyncInfos for the freshly discovered subtrees in subtrees_metadata
    // and returns the root global chunk ids for all of those new subtrees.
    // state_sync_info: Consumed MultiStateSyncInfo
    // subtrees_metadata: Metadata about discovered subtrees
    // chunk_data: Chunk proof operators
    // Returns the next set of global chunk ids that can be fetched from sources (+
    // the MultiStateSyncInfo transferring ownership back to the caller)
    fn discover_subtrees<'db>(
        &'db self,
        mut state_sync_info: MultiStateSyncInfo<'db>,
        subtrees_metadata: SubtreesMetadata,
        tx: &'db Transaction,
    ) -> Result<(Vec<Vec<u8>>, MultiStateSyncInfo), Error> {
        let mut res = vec![];

        for (prefix, prefix_metadata) in &subtrees_metadata.data {
            if !state_sync_info.processed_prefixes.contains(prefix) && !state_sync_info.current_prefixes.contains_key(prefix){
                let (current_path, s_actual_value_hash, s_elem_value_hash) =
                    &prefix_metadata;

                let subtree_path: Vec<&[u8]> =
                    current_path.iter().map(|vec| vec.as_slice()).collect();
                let path: &[&[u8]] = &subtree_path;
                println!(
                    "    path:{:?} starting...",
                    replication::util_path_to_string(&prefix_metadata.0)
                );

                let mut subtree_state_sync_info = self.create_subtree_state_sync_info();
                if let Ok(merk) = self.open_merk_for_replication(path.into(), tx) {
                    let restorer = Restorer::new(
                        merk,
                        *s_elem_value_hash,
                        Some(*s_actual_value_hash),
                    );
                    subtree_state_sync_info.restorer = Some(restorer);
                    subtree_state_sync_info.pending_chunks.insert(vec![]);

                    state_sync_info.current_prefixes.insert(*prefix, subtree_state_sync_info);

                    let root_chunk_prefix = prefix.to_vec();
                    res.push(root_chunk_prefix.to_vec());
                } else {
                    return Err(Error::InternalError(
                        "Unable to open Merk for replication",
                    ));
                }
            }
        }

        Ok((res, state_sync_info))
    }
}
