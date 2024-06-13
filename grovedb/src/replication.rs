mod state_sync_session;

use std::pin::Pin;
use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

use grovedb_merk::{ed::Encode, proofs::{Decoder, Op}, tree::{hash::CryptoHash, kv::ValueDefinedCostType, value_hash}, ChunkProducer, Merk};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_storage::{Storage, StorageContext};

pub use self::state_sync_session::MultiStateSyncSession;
use self::state_sync_session::SubtreesMetadata;
use crate::{Element, Error, error, GroveDb, replication, TransactionArg};
use crate::replication::state_sync_session::{SubtreeMetadata, SubtreePrefix};

pub const CURRENT_STATE_SYNC_VERSION: u16 = 1;

#[cfg(feature = "full")]
impl GroveDb {
    pub fn start_syncing_session(&self, app_hash: [u8; 32]) -> Pin<Box<MultiStateSyncSession>> {
        MultiStateSyncSession::new(self.start_transaction(), app_hash)
    }

    pub fn commit_session(&self, session: Pin<Box<MultiStateSyncSession>>) {
        // we do not care about the cost
        let _ = self.commit_transaction(session.into_transaction());
    }

    pub fn get_subtree_metadata_by_prefix(
        &self,
        transaction: TransactionArg,
        path: Vec<Vec<u8>>,
    ) -> Result<Vec<SubtreeMetadata>, Error> {
        let mut res = vec![];

        if let Some(tx) = transaction {
            let current_path = SubtreePath::from(path.as_slice());
            let storage = self.db.get_transactional_storage_context(current_path, None, tx)
                .value;
            let mut raw_iter = Element::iterator(storage.raw_iter()).value;
            while let Ok(Some((key, value))) = raw_iter.next_element().value
            {
                match value {
                    Element::Tree(ref root_key, _) => {}
                    Element::SumTree(ref root_key, _, _) => {}
                    _ => {}
                }
                if value.is_tree() {

                }
            }
        }
        /*
        if let Some(tx) = transaction {
            let storage = self.db.get_transactional_storage_context_by_subtree_prefix(prefix, None, tx)
                .unwrap_add_cost(&mut cost);
            let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
            while let Some((key, value)) =
                cost_return_on_error!(&mut cost, raw_iter.next_element())
            {
                match value {

                }
                if value.is_tree() {

                }
            }
        }
*/
        //let storage = self.get_transactional_storage_context_by_subtree_prefix()
        /*
                while let Some(q) = queue.pop() {
                    let subtree_path: SubtreePath<Vec<u8>> = q.as_slice().into();
                    // Get the correct subtree with q_ref as path
                    storage_context_optional_tx!(self.db, subtree_path, None, transaction, storage, {
                        let storage = storage.unwrap_add_cost(&mut cost);
                        let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
                        while let Some((key, value)) =
                            cost_return_on_error!(&mut cost, raw_iter.next_element())
                        {
                            if value.is_tree() {
                                let mut sub_path = q.clone();
                                sub_path.push(key.to_vec());
                                queue.push(sub_path.clone());
                                result.push(sub_path);
                            }
                        }
                    })
                }
                Ok(result).wrap_with_cost(cost)
        */
        Ok(res)
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

    pub fn fetch_chunk(
        &self,
        global_chunk_id: &[u8],
        transaction: TransactionArg,
        version: u16,
    ) -> Result<Vec<u8>, Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        let root_app_hash = self.root_hash(transaction).value?;
        let (chunk_prefix, root_key, is_sum_tree, chunk_id) =
            replication::util_split_global_chunk_id_2(global_chunk_id, &root_app_hash)?;

        // TODO: Refactor this by writing fetch_chunk_inner (as only merk constructor and type are different)
        match transaction {
            None => {
                let merk = self.open_non_transactional_merk_by_prefix(chunk_prefix,
                root_key,
                is_sum_tree, None)
                    .value
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to open merk by prefix non-tx:{} with:{}", e, hex::encode(chunk_prefix)),
                    ))?;
                if merk.is_empty_tree().unwrap() {
                    return Ok(vec![]);
                }

                let mut chunk_producer = ChunkProducer::new(&merk)
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to create chunk producer by prefix non-tx:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                let ((chunk,_)) = chunk_producer.chunk(&chunk_id)
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to apply chunk:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                let op_bytes = util_encode_vec_ops(chunk)
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to encode chunk ops:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                Ok(op_bytes)
            }
            Some(tx) => {
                let merk = self.open_transactional_merk_by_prefix(chunk_prefix,
                                                                      root_key,
                                                                      is_sum_tree, tx, None)
                    .value
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to open merk by prefix tx:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                if merk.is_empty_tree().unwrap() {
                    return Ok(vec![]);
                }

                let mut chunk_producer = ChunkProducer::new(&merk)
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to create chunk producer by prefix tx:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                let ((chunk,_)) = chunk_producer.chunk(&chunk_id)
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to apply chunk:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                let op_bytes = util_encode_vec_ops(chunk)
                    .map_err(|e| Error::CorruptedData(
                        format!("failed to encode chunk ops:{} with:{}", hex::encode(chunk_prefix), e),
                    ))?;
                Ok(op_bytes)
            }
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
    ) -> Result<Pin<Box<MultiStateSyncSession<'db>>>, Error> {
        // For now, only CURRENT_STATE_SYNC_VERSION is supported
        if version != CURRENT_STATE_SYNC_VERSION {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }

        println!("    starting:{:?}...", util_path_to_string(&[]));

        let root_prefix = [0u8; 32];

        let mut session = self.start_syncing_session(app_hash);

        session.add_subtree_sync_info(self, SubtreePath::empty(), app_hash, None, root_prefix, vec![], vec![])?;

        Ok(session)
    }
}

// Converts a path into a human-readable string (for debugging)
pub fn util_path_to_string(path: &[Vec<u8>]) -> Vec<String> {
    let mut subtree_path_str: Vec<String> = vec![];
    for subtree in path {
        let string = std::str::from_utf8(&subtree).unwrap_or_else(|_| "<NON_UTF8_PATH>");
        subtree_path_str.push(
            string.to_string()
        );
    }
    subtree_path_str
}

// Splits the given global chunk id into [SUBTREE_PREFIX:CHUNK_ID]
pub fn util_split_global_chunk_id(
    global_chunk_id: &[u8],
    app_hash: [u8; 32],
) -> Result<(crate::SubtreePrefix, Vec<u8>), Error> {
    let chunk_prefix_length: usize = 32;
    if global_chunk_id.len() < chunk_prefix_length {
        return Err(Error::CorruptedData(
            "expected global chunk id of at least 32 length".to_string(),
        ));
    }

    if global_chunk_id == app_hash {
        let array_of_zeros: [u8; 32] = [0; 32];
        let root_chunk_prefix_key: crate::SubtreePrefix = array_of_zeros;
        return Ok((root_chunk_prefix_key, vec![]));
    }

    let (chunk_prefix, chunk_id) = global_chunk_id.split_at(chunk_prefix_length);
    let mut array = [0u8; 32];
    array.copy_from_slice(chunk_prefix);
    let chunk_prefix_key: crate::SubtreePrefix = array;
    Ok((chunk_prefix_key, chunk_id.to_vec()))
}


pub fn util_split_global_chunk_id_2(
    global_chunk_id: &[u8],
    app_hash: &[u8],
) -> Result<(crate::SubtreePrefix, Option<Vec<u8>>, bool, Vec<u8>), Error> {
    //println!("got>{}", hex::encode(global_chunk_id));
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

    let subtree_prefix: crate::SubtreePrefix = chunk_prefix_key.try_into()
        .map_err(|_| {
            error::Error::CorruptedData(
                "unable to construct subtree".to_string(),
            )
        })?;

    if !root_key.is_empty() {
        Ok((subtree_prefix, Some(root_key.to_vec()), is_sum_tree[0] != 0, chunk_id.to_vec()))
    }
    else {
        Ok((subtree_prefix, None, is_sum_tree[0] != 0, chunk_id.to_vec()))
    }
}

// Create the given global chunk id into [SUBTREE_PREFIX:SIZE_ROOT_KEY:ROOT_KEY:IS_SUM_TREE:CHUNK_ID]
pub fn util_create_global_chunk_id_2(
    subtree_prefix: [u8; blake3::OUT_LEN],
    root_key_opt: Option<Vec<u8>>,
    is_sum_tree:bool,
    chunk_id: Vec<u8>
) -> (Vec<u8>){
    let mut res = vec![];

    res.extend(subtree_prefix);

    let mut root_key_len = 0u8;
    let mut root_key_vec = vec![];
    if let Some(root_key) = root_key_opt {
        res.push(root_key.len() as u8);
        res.extend(root_key.clone());
        root_key_len = root_key.len() as u8;
        root_key_vec = root_key;
    }
    else {
        res.push(0u8);
    }

    let mut is_sum_tree_v = 0u8;
    if is_sum_tree {
        is_sum_tree_v = 1u8;
    }
    res.push(is_sum_tree_v);


    res.extend(chunk_id.to_vec());
    //println!("snd>{}|{}|{}|{}|{:?}", hex::encode(res.clone()), root_key_len, hex::encode(root_key_vec), is_sum_tree_v, chunk_id);
    res
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
