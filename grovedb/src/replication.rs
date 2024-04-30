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

// Struct governing state sync
pub struct StateSyncInfo<'db> {
    // Current Chunk restorer
    pub restorer: Option<Restorer<PrefixedRocksDbImmediateStorageContext<'db>>>,
    // Set of processed prefixes (Path digests)
    pub processed_prefixes: BTreeSet<SubtreePrefix>,
    // Current processed prefix (Path digest)
    pub current_prefix: Option<SubtreePrefix>,
    // Set of global chunk ids requested to be fetched and pending for processing. For the
    // description of global chunk id check fetch_chunk().
    pub pending_chunks: BTreeSet<Vec<u8>>,
    // Number of processed chunks in current prefix (Path digest)
    pub num_processed_chunks: usize,
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
                " prefix:{:?} -> path:{:?}\n",
                hex::encode(prefix),
                metadata_path_str
            );
        }
        Ok(())
    }
}

// Converts a path into a human-readable string (for debuting)
pub fn util_path_to_string(path: &[Vec<u8>]) -> Vec<String> {
    let mut subtree_path_str: Vec<String> = vec![];
    for subtree in path {
        let string = std::str::from_utf8(subtree).unwrap();
        subtree_path_str.push(string.parse().unwrap());
    }
    subtree_path_str
}

// Splits the given global chunk id into [SUBTREE_PREFIX:CHUNK_ID]
pub fn util_split_global_chunk_id(
    global_chunk_id: &[u8],
) -> Result<(crate::SubtreePrefix, String), Error> {
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
    let str_chunk_id = String::from_utf8(chunk_id.to_vec());
    match str_chunk_id {
        Ok(s) => Ok((chunk_prefix_key, s)),
        Err(_) => Err(Error::CorruptedData(
            "unable to convert chunk id to string".to_string(),
        )),
    }
}

#[cfg(feature = "full")]
impl GroveDb {
    pub fn create_state_sync_info(&self) -> StateSyncInfo {
        let pending_chunks = BTreeSet::new();
        let processed_prefixes = BTreeSet::new();
        StateSyncInfo {
            restorer: None,
            processed_prefixes,
            current_prefix: None,
            pending_chunks,
            num_processed_chunks: 0,
        }
    }

    // Returns the discovered subtrees found recursively along with their associated
    // metadata Params:
    // tx: Transaction. Function returns the data by opening merks at given tx.
    // TODO: Add a SubTreePath as param and start searching from that path instead
    // of root (as it is now)
    pub fn get_subtrees_metadata<'db>(
        &'db self,
        tx: TransactionArg,
    ) -> Result<SubtreesMetadata, Error> {
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
                        if let Ok((Some((elem_value, elem_value_hash)))) = parent_merk
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
                        if let Ok((Some((elem_value, elem_value_hash)))) = parent_merk
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
    pub fn fetch_chunk<'db>(
        &'db self,
        global_chunk_id: &[u8],
        tx: TransactionArg,
    ) -> Result<Vec<Op>, Error> {
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

                let merk = self
                    .open_non_transactional_merk_at_path(path.into(), None)
                    .value?;

                if merk.is_empty_tree().unwrap() {
                    return Ok(vec![]);
                }

                let chunk_producer_res = ChunkProducer::new(&merk);
                match chunk_producer_res {
                    Ok(mut chunk_producer) => {
                        let chunk_res = chunk_producer
                            .chunk(String::from_utf8(chunk_id.to_vec()).unwrap().as_str());
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
        mut state_sync_info: StateSyncInfo<'db>,
        app_hash: CryptoHash,
        tx: &'db Transaction,
    ) -> Result<(Vec<Vec<u8>>, StateSyncInfo), Error> {
        let mut res = vec![];

        match (
            &mut state_sync_info.restorer,
            &state_sync_info.current_prefix,
        ) {
            (None, None) => {
                if state_sync_info.pending_chunks.is_empty()
                    && state_sync_info.processed_prefixes.is_empty()
                {
                    let root_prefix = [0u8; 32];
                    let merk = self
                        .open_merk_for_replication(SubtreePath::empty(), tx)
                        .unwrap();
                    let restorer = Restorer::new(merk, app_hash, None);
                    state_sync_info.restorer = Some(restorer);
                    state_sync_info.current_prefix = Some(root_prefix);
                    state_sync_info.pending_chunks.insert(root_prefix.to_vec());

                    res.push(root_prefix.to_vec());
                } else {
                    return Err(Error::InternalError("Invalid internal state sync info"));
                }
            }
            _ => {
                return Err(Error::InternalError(
                    "GroveDB has already started a snapshot syncing",
                ));
            }
        }

        Ok((res, state_sync_info))
    }

    // Apply a chunk (should be called by ABCI when ApplySnapshotChunk method is
    // called) Params:
    // state_sync_info: Consumed StateSyncInfo
    // chunk: (Global chunk id, Chunk proof operators)
    // tx: Transaction for the state sync
    // Returns the next set of global chunk ids that can be fetched from sources (+
    // the StateSyncInfo transferring ownership back to the caller)
    pub fn apply_chunk<'db>(
        &'db self,
        mut state_sync_info: StateSyncInfo<'db>,
        chunk: (&[u8], Vec<Op>),
        tx: &'db Transaction,
    ) -> Result<(Vec<Vec<u8>>, StateSyncInfo), Error> {
        let mut res = vec![];

        let (global_chunk_id, chunk_data) = chunk;
        let (chunk_prefix, chunk_id) = replication::util_split_global_chunk_id(global_chunk_id)?;

        match (
            &mut state_sync_info.restorer,
            &state_sync_info.current_prefix,
        ) {
            (Some(restorer), Some(ref current_prefix)) => {
                if *current_prefix != chunk_prefix {
                    return Err(Error::InternalError("Invalid incoming prefix"));
                }
                if !state_sync_info.pending_chunks.contains(global_chunk_id) {
                    return Err(Error::InternalError(
                        "Incoming global_chunk_id not expected",
                    ));
                }
                state_sync_info.pending_chunks.remove(global_chunk_id);
                if !chunk_data.is_empty() {
                    match restorer.process_chunk(chunk_id.to_string(), chunk_data) {
                        Ok(next_chunk_ids) => {
                            state_sync_info.num_processed_chunks += 1;
                            for next_chunk_id in next_chunk_ids {
                                let mut next_global_chunk_id = chunk_prefix.to_vec();
                                next_global_chunk_id.extend(next_chunk_id.as_bytes().to_vec());
                                state_sync_info
                                    .pending_chunks
                                    .insert(next_global_chunk_id.clone());
                                res.push(next_global_chunk_id);
                            }
                        }
                        _ => {
                            return Err(Error::InternalError("Unable to process incoming chunk"));
                        }
                    };
                }
            }
            _ => {
                return Err(Error::InternalError("GroveDB is not in syncing mode"));
            }
        }

        if res.is_empty() {
            if !state_sync_info.pending_chunks.is_empty() {
                return Ok((res, state_sync_info));
            }
            match (
                state_sync_info.restorer.take(),
                state_sync_info.current_prefix.take(),
            ) {
                (Some(restorer), Some(current_prefix)) => {
                    if (state_sync_info.num_processed_chunks > 0) && (restorer.finalize().is_err())
                    {
                        return Err(Error::InternalError("Unable to finalize merk"));
                    }
                    state_sync_info.processed_prefixes.insert(current_prefix);

                    let subtrees_metadata = self.get_subtrees_metadata(Some(tx))?;
                    if let Some(value) = subtrees_metadata.data.get(&current_prefix) {
                        println!(
                            "    path:{:?} done",
                            replication::util_path_to_string(&value.0)
                        );
                    }

                    for (prefix, prefix_metadata) in &subtrees_metadata.data {
                        if !state_sync_info.processed_prefixes.contains(prefix) {
                            let (current_path, s_actual_value_hash, s_elem_value_hash) =
                                &prefix_metadata;

                            let subtree_path: Vec<&[u8]> =
                                current_path.iter().map(|vec| vec.as_slice()).collect();
                            let path: &[&[u8]] = &subtree_path;

                            let merk = self.open_merk_for_replication(path.into(), tx).unwrap();
                            let restorer =
                                Restorer::new(merk, *s_elem_value_hash, Some(*s_actual_value_hash));
                            state_sync_info.restorer = Some(restorer);
                            state_sync_info.current_prefix = Some(*prefix);
                            state_sync_info.num_processed_chunks = 0;

                            let root_chunk_prefix = prefix.to_vec();
                            state_sync_info
                                .pending_chunks
                                .insert(root_chunk_prefix.clone());
                            res.push(root_chunk_prefix);
                            break;
                        }
                    }
                }
                _ => {
                    return Err(Error::InternalError("Unable to finalize tree"));
                }
            }
        }

        Ok((res, state_sync_info))
    }
}
