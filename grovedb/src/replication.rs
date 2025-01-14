mod state_sync_session;

use std::pin::Pin;

use grovedb_merk::{tree::hash::CryptoHash, ChunkProducer};
use grovedb_path::SubtreePath;
use grovedb_version::{check_grovedb_v0, error::GroveVersionError, version::GroveVersion};

pub use self::state_sync_session::MultiStateSyncSession;
use crate::{Error, GroveDb, TransactionArg};

/// Type alias representing a chunk identifier in the state synchronization
/// process.
///
/// - `SubtreePrefix`: The prefix of the subtree (32 bytes).
/// - `Option<Vec<u8>>`: The root key, which may be `None` if not present.
/// - `bool`: Indicates whether the tree is a sum tree.
/// - `Vec<u8>`: The chunk ID representing traversal instructions.
pub type ChunkIdentifier = (crate::SubtreePrefix, Option<Vec<u8>>, bool, Vec<u8>);

pub const CURRENT_STATE_SYNC_VERSION: u16 = 1;

#[cfg(feature = "minimal")]
impl GroveDb {
    pub fn start_syncing_session(&self, app_hash: [u8; 32]) -> Pin<Box<MultiStateSyncSession>> {
        MultiStateSyncSession::new(self.start_transaction(), app_hash)
    }

    pub fn commit_session(&self, session: Pin<Box<MultiStateSyncSession>>) -> Result<(), Error> {
        match self.commit_transaction(session.into_transaction()).value {
            Ok(_) => Ok(()),
            Err(e) => {
                // Log the error or handle it as needed
                eprintln!("Failed to commit session: {:?}", e);
                Err(e)
            }
        }
    }

    /// Fetch a chunk by global chunk ID (should be called by ABCI when the
    /// `LoadSnapshotChunk` method is invoked).
    ///
    /// # Parameters
    /// - `global_chunk_id`: Global chunk ID in the following format:
    ///   `[SUBTREE_PREFIX:SIZE_ROOT_KEY:ROOT_KEY:IS_SUM_TREE:CHUNK_ID]`
    ///   - **SUBTREE_PREFIX**: 32 bytes (mandatory) - All zeros indicate the
    ///     Root subtree.
    ///   - **SIZE_ROOT_KEY**: 1 byte - Size of `ROOT_KEY` in bytes.
    ///   - **ROOT_KEY**: `SIZE_ROOT_KEY` bytes (optional).
    ///   - **IS_SUM_TREE**: 1 byte (mandatory) - Marks if the tree is a sum
    ///     tree or not.
    ///   - **CHUNK_ID**: 0 or more bytes (optional) - Traversal instructions to
    ///     the root of the given chunk. Traversal instructions are represented
    ///     as "1" for left and "0" for right.
    ///     - TODO: Compact `CHUNK_ID` into a bitset for size optimization as a
    ///       subtree can be large, and traversal instructions for the deepest
    ///       chunks could consume significant space.
    ///
    /// - `transaction`: The transaction used to fetch the chunk.
    /// - `version`: The version of the state sync protocol.
    /// - `grove_version`: The version of GroveDB.
    ///
    /// # Returns
    /// Returns the chunk proof operators for the requested chunk, encoded as
    /// bytes.
    pub fn fetch_chunk(
        &self,
        global_chunk_id: &[u8],
        transaction: TransactionArg,
        version: u16,
        grove_version: &GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        check_grovedb_v0!(
            "fetch_chunk",
            grove_version.grovedb_versions.replication.fetch_chunk
        );
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let root_app_hash = self.root_hash(transaction, grove_version).value?;
        let (chunk_prefix, root_key, is_sum_tree, chunk_id) =
            utils::decode_global_chunk_id(global_chunk_id, &root_app_hash)?;

        // TODO: Refactor this by writing fetch_chunk_inner (as only merk constructor
        // and type are different)
        if let Some(tx) = transaction {
            let merk = self
                .open_transactional_merk_by_prefix(
                    chunk_prefix,
                    root_key,
                    is_sum_tree,
                    tx,
                    None,
                    grove_version,
                )
                .value
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to open merk by prefix tx:{} with:{}",
                        hex::encode(chunk_prefix),
                        e
                    ))
                })?;
            if merk.is_empty_tree().unwrap() {
                return Ok(vec![]);
            }

            let mut chunk_producer = ChunkProducer::new(&merk).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to create chunk producer by prefix tx:{} with:{}",
                    hex::encode(chunk_prefix),
                    e
                ))
            })?;
            let (chunk, _) = chunk_producer
                .chunk(&chunk_id, grove_version)
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to apply chunk:{} with:{}",
                        hex::encode(chunk_prefix),
                        e
                    ))
                })?;
            let op_bytes = utils::encode_vec_ops(chunk).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to encode chunk ops:{} with:{}",
                    hex::encode(chunk_prefix),
                    e
                ))
            })?;
            Ok(op_bytes)
        } else {
            let merk = self
                .open_non_transactional_merk_by_prefix(
                    chunk_prefix,
                    root_key,
                    is_sum_tree,
                    None,
                    grove_version,
                )
                .value
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to open merk by prefix non-tx:{} with:{}",
                        e,
                        hex::encode(chunk_prefix)
                    ))
                })?;
            if merk.is_empty_tree().unwrap() {
                return Ok(vec![]);
            }

            let mut chunk_producer = ChunkProducer::new(&merk).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to create chunk producer by prefix non-tx:{} with:{}",
                    hex::encode(chunk_prefix),
                    e
                ))
            })?;
            let (chunk, _) = chunk_producer
                .chunk(&chunk_id, grove_version)
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to apply chunk:{} with:{}",
                        hex::encode(chunk_prefix),
                        e
                    ))
                })?;
            let op_bytes = utils::encode_vec_ops(chunk).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to encode chunk ops:{} with:{}",
                    hex::encode(chunk_prefix),
                    e
                ))
            })?;
            Ok(op_bytes)
        }
    }

    /// Starts a state synchronization process for a snapshot with the given
    /// `app_hash` root hash. This method should be called by ABCI when the
    /// `OfferSnapshot` method is invoked.
    ///
    /// # Parameters
    /// - `app_hash`: The root hash of the application state to synchronize.
    /// - `version`: The version of the state sync protocol to use.
    /// - `grove_version`: The version of GroveDB being used.
    ///
    /// # Returns
    /// - `Ok(Pin<Box<MultiStateSyncSession>>)`: A pinned, boxed
    ///   `MultiStateSyncSession` representing the new sync session. This
    ///   session allows for managing the synchronization process.
    /// - `Err(Error)`: An error indicating why the state sync process could not
    ///   be started.
    ///
    /// # Behavior
    /// - Initiates the state synchronization process by preparing the necessary
    ///   data and resources.
    /// - Returns the first set of global chunk IDs that can be fetched from
    ///   available sources.
    /// - A new sync session is created and managed internally, facilitating
    ///   further synchronization.
    ///
    /// # Usage
    /// This method is typically called as part of the ABCI `OfferSnapshot`
    /// workflow when a new snapshot synchronization process is required to
    /// bring the application state up to date.
    ///
    /// # Notes
    /// - The returned `MultiStateSyncSession` is pinned because its lifetime
    ///   may depend on asynchronous operations or other system resources that
    ///   require it to remain immovable in memory.
    /// - Ensure that `app_hash` corresponds to a valid snapshot to avoid
    ///   errors.
    pub fn start_snapshot_syncing(
        &self,
        app_hash: CryptoHash,
        version: u16,
        grove_version: &GroveVersion,
    ) -> Result<Pin<Box<MultiStateSyncSession>>, Error> {
        check_grovedb_v0!(
            "start_snapshot_syncing",
            grove_version
                .grovedb_versions
                .replication
                .start_snapshot_syncing
        );
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let root_prefix = [0u8; 32];

        let mut session = self.start_syncing_session(app_hash);

        session.add_subtree_sync_info(
            self,
            SubtreePath::empty(),
            app_hash,
            None,
            root_prefix,
            grove_version,
        )?;

        Ok(session)
    }
}

pub(crate) mod utils {
    use grovedb_merk::{
        ed::Encode,
        proofs::{Decoder, Op},
    };

    use crate::{replication::ChunkIdentifier, Error};

    /// Converts a path, represented as a slice of byte vectors (`&[Vec<u8>]`),
    /// into a human-readable string representation for debugging purposes.
    ///
    /// # Parameters
    /// - `path`: A slice of byte vectors where each vector represents a segment
    ///   of the path.
    ///
    /// # Returns
    /// - `Vec<String>`: A vector of strings where each string is a
    ///   human-readable representation of a corresponding segment in the input
    ///   path. If a segment contains invalid UTF-8, it is replaced with the
    ///   placeholder string `"<NON_UTF8_PATH>"`.
    ///
    /// # Behavior
    /// - Each byte vector in the path is interpreted as a UTF-8 string. If the
    ///   conversion fails, the placeholder `"<NON_UTF8_PATH>"` is used instead.
    /// - This function is primarily intended for debugging and logging.
    ///
    /// # Notes
    /// - This function does not handle or normalize paths; it only provides a
    ///   human-readable representation.
    /// - Be cautious when using this for paths that might contain sensitive
    ///   data, as the output could be logged.
    pub fn path_to_string(path: &[Vec<u8>]) -> Vec<String> {
        let mut subtree_path_str: Vec<String> = vec![];
        for subtree in path {
            let string = std::str::from_utf8(subtree).unwrap_or("<NON_UTF8_PATH>");
            subtree_path_str.push(string.to_string());
        }
        subtree_path_str
    }

    /// Decodes a given global chunk ID into its components:
    /// `[SUBTREE_PREFIX:SIZE_ROOT_KEY:ROOT_KEY:IS_SUM_TREE:CHUNK_ID]`.
    ///
    /// # Parameters
    /// - `global_chunk_id`: A byte slice representing the global chunk ID to
    ///   decode.
    /// - `app_hash`: The application hash, which may be required for validation
    ///   or context.
    ///
    /// # Returns
    /// - `Ok(ChunkIdentifier)`: A tuple containing the decoded components:
    ///   - `SUBTREE_PREFIX`: A 32-byte prefix of the subtree.
    ///   - `SIZE_ROOT_KEY`: Size of the root key (derived from `ROOT_KEY`
    ///     length).
    ///   - `ROOT_KEY`: Optional root key as a byte vector.
    ///   - `IS_SUM_TREE`: A boolean indicating whether the tree is a sum tree.
    ///   - `CHUNK_ID`: Traversal instructions as a byte vector.
    /// - `Err(Error)`: An error if the global chunk ID could not be decoded.
    pub fn decode_global_chunk_id(
        global_chunk_id: &[u8],
        app_hash: &[u8],
    ) -> Result<ChunkIdentifier, Error> {
        let chunk_prefix_length: usize = 32;
        if global_chunk_id.len() < chunk_prefix_length {
            return Err(Error::CorruptedData(
                "expected global chunk id of at least 32 length".to_string(),
            ));
        }

        if global_chunk_id == app_hash {
            let root_chunk_prefix_key: crate::SubtreePrefix = [0u8; 32];
            return Ok((root_chunk_prefix_key, None, false, vec![]));
        }

        let (chunk_prefix_key, remaining) = global_chunk_id.split_at(chunk_prefix_length);

        let root_key_size_length: usize = 1;
        if remaining.len() < root_key_size_length {
            return Err(Error::CorruptedData(
                "unable to decode root key size".to_string(),
            ));
        }
        let (root_key_size, remaining) = remaining.split_at(root_key_size_length);
        if remaining.len() < root_key_size[0] as usize {
            return Err(Error::CorruptedData(
                "unable to decode root key".to_string(),
            ));
        }
        let (root_key, remaining) = remaining.split_at(root_key_size[0] as usize);
        let is_sum_tree_length: usize = 1;
        if remaining.len() < is_sum_tree_length {
            return Err(Error::CorruptedData(
                "unable to decode root key".to_string(),
            ));
        }
        let (is_sum_tree, chunk_id) = remaining.split_at(is_sum_tree_length);

        let subtree_prefix: crate::SubtreePrefix = chunk_prefix_key
            .try_into()
            .map_err(|_| Error::CorruptedData("unable to construct subtree".to_string()))?;

        if !root_key.is_empty() {
            Ok((
                subtree_prefix,
                Some(root_key.to_vec()),
                is_sum_tree[0] != 0,
                chunk_id.to_vec(),
            ))
        } else {
            Ok((subtree_prefix, None, is_sum_tree[0] != 0, chunk_id.to_vec()))
        }
    }

    /// Encodes the given components into a global chunk ID in the format:
    /// `[SUBTREE_PREFIX:SIZE_ROOT_KEY:ROOT_KEY:IS_SUM_TREE:CHUNK_ID]`.
    ///
    /// # Parameters
    /// - `subtree_prefix`: A 32-byte array representing the prefix of the
    ///   subtree.
    /// - `root_key_opt`: An optional root key as a byte vector.
    /// - `is_sum_tree`: A boolean indicating whether the tree is a sum tree.
    /// - `chunk_id`: A byte vector representing the traversal instructions.
    ///
    /// # Returns
    /// - A `Vec<u8>` containing the encoded global chunk ID.
    pub fn encode_global_chunk_id(
        subtree_prefix: [u8; blake3::OUT_LEN],
        root_key_opt: Option<Vec<u8>>,
        is_sum_tree: bool,
        chunk_id: Vec<u8>,
    ) -> Vec<u8> {
        let mut res = vec![];

        res.extend(subtree_prefix);

        if let Some(root_key) = root_key_opt {
            res.push(root_key.len() as u8);
            res.extend(root_key);
        } else {
            res.push(0u8);
        }

        let mut is_sum_tree_v = 0u8;
        if is_sum_tree {
            is_sum_tree_v = 1u8;
        }
        res.push(is_sum_tree_v);

        res.extend(chunk_id.to_vec());

        res
    }

    /// Encodes a vector of operations (`Vec<Op>`) into a byte vector.
    ///
    /// # Parameters
    /// - `chunk`: A vector of `Op` operations to be encoded.
    ///
    /// # Returns
    /// - `Ok(Vec<u8>)`: A byte vector representing the encoded operations.
    /// - `Err(Error)`: An error if the encoding process fails.
    pub fn encode_vec_ops(chunk: Vec<Op>) -> Result<Vec<u8>, Error> {
        let mut res = vec![];
        for op in chunk {
            op.encode_into(&mut res)
                .map_err(|e| Error::CorruptedData(format!("unable to encode chunk: {}", e)))?;
        }
        Ok(res)
    }

    /// Decodes a byte vector into a vector of operations (`Vec<Op>`).
    ///
    /// # Parameters
    /// - `chunk`: A byte vector representing encoded operations.
    ///
    /// # Returns
    /// - `Ok(Vec<Op>)`: A vector of decoded `Op` operations.
    /// - `Err(Error)`: An error if the decoding process fails.
    pub fn decode_vec_ops(chunk: Vec<u8>) -> Result<Vec<Op>, Error> {
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
}
