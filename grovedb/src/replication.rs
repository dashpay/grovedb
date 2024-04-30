use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use grovedb_merk::{merk::restore::Restorer, tree::hash::CryptoHash};
#[rustfmt::skip]
use grovedb_storage::rocksdb_storage::storage_context::context_immediate::PrefixedRocksDbImmediateStorageContext;

use crate::Error;

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
