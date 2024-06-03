mod state_sync_session;

use std::pin::Pin;

use grovedb_merk::{
    ed::Encode,
    proofs::{Decoder, Op},
    tree::{hash::CryptoHash, kv::ValueDefinedCostType, value_hash},
    ChunkProducer,
};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;

pub use self::state_sync_session::MultiStateSyncSession;
use self::state_sync_session::SubtreesMetadata;
use crate::{Error, GroveDb, TransactionArg};

pub const CURRENT_STATE_SYNC_VERSION: u16 = 1;

#[cfg(feature = "full")]
impl GroveDb {
    pub fn start_syncing_session(&self) -> Pin<Box<MultiStateSyncSession>> {
        MultiStateSyncSession::new(self.start_transaction())
    }

    pub fn commit_session(&self, session: Pin<Box<MultiStateSyncSession>>) {
        // we do not care about the cost
        let _ = self.commit_transaction(session.into_transaction());
    }

    // Returns the discovered subtrees found recursively along with their associated
    // metadata Params:
    // tx: Transaction. Function returns the data by opening merks at given tx.
    // TODO: Add a SubTreePath as param and start searching from that path instead
    // of root (as it is now)
    pub fn get_subtrees_metadata(&self, tx: TransactionArg) -> Result<SubtreesMetadata, Error> {
        let mut subtrees_metadata = SubtreesMetadata::new();

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
    // Returns the Chunk proof operators for the requested chunk encoded in bytes
    pub fn fetch_chunk(
        &self,
        global_chunk_id: &[u8],
        tx: TransactionArg,
        version: u16,
    ) -> Result<Vec<u8>, Error> {
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
                                    Ok((chunk, _)) => match util_encode_vec_ops(chunk) {
                                        Ok(op_bytes) => Ok(op_bytes),
                                        Err(_) => Err(Error::CorruptedData(
                                            "Unable to create to load chunk".to_string(),
                                        )),
                                    },
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
                                    Ok((chunk, _)) => match util_encode_vec_ops(chunk) {
                                        Ok(op_bytes) => Ok(op_bytes),
                                        Err(_) => Err(Error::CorruptedData(
                                            "Unable to create to load chunk".to_string(),
                                        )),
                                    },
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

    /// Starts a state sync process of a snapshot with `app_hash` root hash,
    /// should be called by ABCI when OfferSnapshot  method is called.
    /// Returns the first set of global chunk ids that can be fetched from
    /// sources and a new sync session.
    pub fn start_snapshot_syncing<'db>(
        &'db self,
        app_hash: CryptoHash,
        version: u16,
    ) -> Result<(Vec<Vec<u8>>, Pin<Box<MultiStateSyncSession<'db>>>), Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        println!("    starting:{:?}...", util_path_to_string(&[]));

        let root_prefix = [0u8; 32];

        let mut session = self.start_syncing_session();
        session.add_subtree_sync_info(self, SubtreePath::empty(), app_hash, None, root_prefix)?;

        Ok((vec![root_prefix.to_vec()], session))
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

pub fn util_encode_vec_ops(chunk: Vec<Op>) -> Result<Vec<u8>, Error> {
    let mut res = vec![];
    for op in chunk {
        op.encode_into(&mut res)
            .map_err(|e| Error::CorruptedData(format!("unable to encode chunk: {}", e)))?;
    }
    Ok(res)
}

pub fn util_decode_vec_ops(chunk: Vec<u8>) -> Result<Vec<Op>, Error> {
    let decoder = Decoder::new(&chunk);
    let mut res = vec![];
    for op in decoder {
        match op {
            Ok(op) => res.push(op),
            Err(e) => {
                return Err(Error::CorruptedData(format!(
                    "unable to decode chunk: {}",
                    e
                )));
            }
        }
    }
    Ok(res)
}
