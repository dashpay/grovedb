use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomPinned,
    pin::Pin,
};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::costs::ElementCostExtensions,
    tree::{kv::ValueDefinedCostType, value_hash},
    tree_type::TreeType,
    CryptoHash, Merk, Restorer,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbImmediateStorageContext, RocksDbStorage},
    Storage, StorageContext,
};
use grovedb_version::version::GroveVersion;

use super::{
    indexed_sync::{
        verify_indexed_binding, IndexedHeader, IndexedHeaderRequest, INDEXED_SYNC_MIN_VERSION,
    },
    is_supported_state_sync_version,
    non_merk_sync::{supports_entry_replay, NonMerkRestorer},
    utils::{decode_vec_ops, encode_global_chunk_id, path_to_string},
};
use crate::{
    element::elements_iterator::ElementIteratorExtensions,
    operations::indexed_tree::{axis_secondary_tree_type, indexed_element_axes},
    replication,
    replication::utils::{pack_nested_bytes, unpack_nested_bytes},
    Element, Error, GroveDb, Transaction,
};

/// Number of elements packed together
pub const CONST_GROUP_PACKING_SIZE: usize = 32;

pub(crate) type SubtreePrefix = [u8; 32];

/// The restore backend for one subtree: Merk chunk restore for ordinary
/// subtrees, entry replay for the non-Merk append-only tree family
/// (CommitmentTree / MmrTree / BulkAppendTree /
/// DenseAppendOnlyFixedSizeTree / PrivateDocumentStore) — see issues #785
/// and #783 / #784.
enum SubtreeRestorer<'db> {
    Merk(Restorer<PrefixedRocksDbImmediateStorageContext<'db>>),
    NonMerk(NonMerkRestorer),
    /// An indexed primary (protocol version 2) waiting for its header
    /// page: the actual `Restorer` cannot be constructed until the
    /// [`IndexedHeader`] delivers the expected primary root hash. Holds
    /// the opened Merk; `None` only transiently while the header page is
    /// being processed.
    IndexedPending(Option<Merk<PrefixedRocksDbImmediateStorageContext<'db>>>),
}

/// Struct governing the state synchronization of one subtree.
struct SubtreeStateSyncInfo<'db> {
    /// Current chunk restorer (Merk chunks or non-Merk entry replay)
    restorer: SubtreeRestorer<'db>,

    /// Set of global chunk ids requested to be fetched and pending for
    /// processing. For the description of global chunk id check
    /// fetch_chunk().
    pending_chunks: BTreeSet<Vec<u8>>,

    /// Tree root key
    root_key: Option<Vec<u8>>,

    /// The type of tree
    tree_type: TreeType,

    /// Path of current tree
    current_path: Vec<Vec<u8>>,

    /// Number of processed chunks in current prefix (Path digest)
    num_processed_chunks: usize,
}

impl SubtreeStateSyncInfo<'_> {
    /// Applies a chunk using the given `SubtreeStateSyncInfo`.
    ///
    /// # Parameters
    /// - `chunk_id`: A byte slice representing the local chunk ID to be
    ///   applied.
    /// - `chunk_data`: A byte slice containing the chunk proof operators,
    ///   encoded as bytes.
    /// - `grove_version`: A reference to the `GroveVersion` being used for
    ///   synchronization.
    ///
    /// # Returns
    /// - `Ok((Vec<Vec<u8>>, Option<IndexedHeader>))`: the next local chunk
    ///   IDs to fetch for this subtree, plus — exactly once per indexed
    ///   primary — the decoded [`IndexedHeader`] the session must use to
    ///   activate the group's axis secondaries.
    /// - `Err(Error)`: An error if the chunk cannot be applied.
    ///
    /// # Behavior
    /// - The function consumes the provided `SubtreeStateSyncInfo` to apply the
    ///   given chunk.
    /// - Once the chunk is applied, the function calculates and returns the
    ///   next set of global chunk IDs required for further state
    ///   synchronization.
    ///
    /// # Usage
    /// This function is called as part of the state sync process to apply
    /// received chunks and advance the synchronization state.
    ///
    /// # Notes
    /// - Ensure that the `chunk_data` is correctly encoded and matches the
    ///   expected format.
    /// - The function modifies the state of the synchronization process, so it
    ///   must be used carefully to maintain correctness.
    fn apply_inner_chunk<'tx, 'db: 'tx>(
        &mut self,
        db: &'db GroveDb,
        tx: &'tx Transaction<'db>,
        chunk_id: &[u8],
        chunk_data: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<(Vec<Vec<u8>>, Option<IndexedHeader>), Error> {
        let mut res = vec![];

        if !self.pending_chunks.contains(chunk_id) {
            return Err(Error::InternalError(
                "Incoming global_chunk_id not expected".to_string(),
            ));
        }
        self.pending_chunks.remove(chunk_id);

        // An indexed primary's first response is its header page:
        // `pack([header, root_chunk_ops])`. Construct the real Merk
        // restorer against the header's primary root hash (direct
        // root-hash comparison — the three-input parent binding is
        // verified at group finalize), process the bundled root chunk,
        // and hand the header up so the session can activate the axis
        // secondaries.
        if matches!(self.restorer, SubtreeRestorer::IndexedPending(_)) {
            let SubtreeRestorer::IndexedPending(pending_merk) =
                std::mem::replace(&mut self.restorer, SubtreeRestorer::IndexedPending(None))
            else {
                unreachable!("matched IndexedPending above");
            };
            let merk = pending_merk.ok_or_else(|| {
                Error::InternalError("indexed primary header page processed twice".to_string())
            })?;
            let sections = unpack_nested_bytes(chunk_data)?;
            let [header_bytes, root_chunk_ops]: [Vec<u8>; 2] =
                sections.try_into().map_err(|_| {
                    Error::CorruptedData(
                        "indexed header page must carry exactly a header and a root chunk"
                            .to_string(),
                    )
                })?;
            let header = IndexedHeader::decode(&header_bytes)?;
            let mut restorer = Restorer::new(merk, header.primary_root_hash, None);
            if !root_chunk_ops.is_empty() {
                let ops = decode_vec_ops(&root_chunk_ops)?;
                match restorer.process_chunk(&[], ops, grove_version) {
                    Ok(next_chunk_ids) => {
                        self.num_processed_chunks += 1;
                        for next_chunk_id in next_chunk_ids {
                            self.pending_chunks.insert(next_chunk_id.clone());
                            res.push(next_chunk_id);
                        }
                    }
                    Err(e) => {
                        return Err(Error::InternalError(format!(
                            "Unable to process indexed primary root chunk: {e}"
                        )));
                    }
                }
            }
            self.restorer = SubtreeRestorer::Merk(restorer);
            return Ok((res, Some(header)));
        }

        match &mut self.restorer {
            SubtreeRestorer::Merk(restorer) => {
                if !chunk_data.is_empty() {
                    match decode_vec_ops(chunk_data) {
                        Ok(ops) => {
                            match restorer.process_chunk(chunk_id, ops, grove_version) {
                                Ok(next_chunk_ids) => {
                                    self.num_processed_chunks += 1;
                                    for next_chunk_id in next_chunk_ids {
                                        self.pending_chunks.insert(next_chunk_id.clone());
                                        res.push(next_chunk_id);
                                    }
                                }
                                Err(e) => {
                                    return Err(Error::InternalError(format!(
                                        "Unable to process incoming chunk: {e}"
                                    )));
                                }
                            };
                        }
                        Err(e) => {
                            return Err(Error::CorruptedData(format!(
                                "Unable to decode incoming chunk: {e}"
                            )));
                        }
                    }
                }
            }
            SubtreeRestorer::NonMerk(non_merk_restorer) => {
                let next_chunk_ids = non_merk_restorer.apply_page(
                    db,
                    tx,
                    &self.current_path,
                    chunk_id,
                    chunk_data,
                    grove_version,
                )?;
                self.num_processed_chunks += 1;
                for next_chunk_id in next_chunk_ids {
                    self.pending_chunks.insert(next_chunk_id.clone());
                    res.push(next_chunk_id);
                }
            }
            SubtreeRestorer::IndexedPending(_) => {
                unreachable!("handled before the match above");
            }
        }

        Ok((res, None))
    }
}

impl<'tx> SubtreeStateSyncInfo<'tx> {
    pub fn new(restorer: Restorer<PrefixedRocksDbImmediateStorageContext<'tx>>) -> Self {
        SubtreeStateSyncInfo {
            restorer: SubtreeRestorer::Merk(restorer),
            root_key: None,
            tree_type: TreeType::NormalTree,
            pending_chunks: Default::default(),
            current_path: vec![],
            num_processed_chunks: 0,
        }
    }
}

/// Struct governing the state synchronization process.
pub struct MultiStateSyncSession<'db> {
    /// GroveDb instance to apply changes to
    db: &'db GroveDb,

    /// Map of currently processing subtrees.
    /// Keys are `SubtreePrefix` (path digests), and values are
    /// `SubtreeStateSyncInfo` for each subtree.
    current_prefixes: BTreeMap<SubtreePrefix, SubtreeStateSyncInfo<'db>>,

    /// Set of processed prefixes, represented as `SubtreePrefix` (path
    /// digests).
    processed_prefixes: BTreeSet<SubtreePrefix>,

    /// Root application hash (`app_hash`).
    app_hash: [u8; 32],

    /// Version of the state synchronization protocol.
    pub(crate) version: u16,

    /// Maximum number of subtrees that can be processed in a single batch.
    subtrees_batch_size: usize,

    /// Counter tracking the number of subtrees processed in the current batch.
    num_processed_subtrees_in_batch: usize,

    /// Metadata for newly discovered subtrees that are pending processing.
    pending_discovered_subtrees: Option<SubtreesMetadata>,

    /// In-flight indexed-tree groups (protocol version 2), keyed by the
    /// primary's prefix. A group is removed — after passing the joint
    /// verification — once its primary and every axis secondary have been
    /// fully restored.
    indexed_groups: BTreeMap<SubtreePrefix, IndexedSyncGroup>,

    /// Maps each in-flight axis secondary's derived prefix to its owning
    /// `(primary_prefix, axis_tag)`.
    secondary_owner: BTreeMap<SubtreePrefix, (SubtreePrefix, u8)>,

    /// Transaction used for the synchronization process.
    /// This is placed last to ensure it is dropped last.
    transaction: Transaction<'db>,

    /// Marker to ensure this struct is not moved in memory.
    _pin: PhantomPinned,
}

/// Target-side tracking of one indexed subtree's transfer (protocol
/// version 2): the parent-bound hashes to verify against, the configured
/// axes, and the actual root hashes of members restored so far.
struct IndexedSyncGroup {
    /// Path of the primary subtree (for error reporting).
    path: Vec<Vec<u8>>,
    /// The indexed element as decoded from the restored parent.
    element: Element,
    /// `value_hash(element_bytes)` from the parent.
    actual_value_hash: CryptoHash,
    /// The three-input combined element value hash bound into the parent.
    elem_value_hash: CryptoHash,
    /// `(axis_tag, secondary_prefix, secondary_root_key)` in canonical
    /// element order.
    axes: Vec<(u8, SubtreePrefix, Option<Vec<u8>>)>,
    /// The wire header, once received. A hint for per-chunk verification
    /// only — the joint check uses the actual restored root hashes.
    header: Option<IndexedHeader>,
    /// Actual root hash of the fully restored primary.
    primary_root: Option<CryptoHash>,
    /// Actual root hashes of fully restored secondaries, by axis tag.
    secondary_roots: BTreeMap<u8, CryptoHash>,
}

impl<'db> MultiStateSyncSession<'db> {
    /// Initializes a new state sync session speaking the given state sync
    /// protocol version.
    pub fn new(
        db: &'db GroveDb,
        app_hash: [u8; 32],
        subtrees_batch_size: usize,
        version: u16,
    ) -> Pin<Box<Self>> {
        Box::pin(MultiStateSyncSession {
            db,
            transaction: db.start_transaction(),
            current_prefixes: Default::default(),
            processed_prefixes: Default::default(),
            app_hash,
            version,
            subtrees_batch_size,
            num_processed_subtrees_in_batch: 0,
            pending_discovered_subtrees: None,
            indexed_groups: Default::default(),
            secondary_owner: Default::default(),
            _pin: PhantomPinned,
        })
    }

    /// Returns true if there are no prefixes currently being synced.
    pub fn is_empty(&self) -> bool {
        self.current_prefixes.is_empty()
    }

    /// Returns true if all subtrees have been fully synchronized.
    /// Returns false if sync has never started (no prefixes processed).
    pub fn is_sync_completed(&self) -> bool {
        if self.current_prefixes.is_empty() && self.processed_prefixes.is_empty() {
            return false;
        }

        for subtree_state_info in self.current_prefixes.values() {
            if !subtree_state_info.pending_chunks.is_empty() {
                return false;
            }
        }

        if self.pending_discovered_subtrees.is_some() {
            return false;
        }

        // An indexed group still tracked here has members whose joint
        // verification has not run yet (e.g. secondaries not activated).
        if !self.indexed_groups.is_empty() {
            return false;
        }

        true
    }

    /// Commits the sync session by finalizing the underlying transaction.
    ///
    /// Before committing, verifies that the GroveDB root hash matches the
    /// expected `app_hash` to ensure the overall composition of all restored
    /// subtrees is correct.
    pub fn commit(self: Pin<Box<Self>>, grove_version: &GroveVersion) -> Result<(), Error> {
        if !self.is_sync_completed() {
            return Err(Error::CorruptedData(
                "cannot commit an incomplete state sync session".to_string(),
            ));
        }

        // SAFETY: the struct isn't used anymore and no storage contexts would access
        // transaction — is_sync_completed() guarantees all restorers are finished
        // and current_prefixes has no active storage contexts.
        let session = unsafe { Pin::into_inner_unchecked(self) };

        // Verify the final root hash matches the expected app_hash before committing.
        // Individual subtree chunks are hash-verified during restore, but we must also
        // verify the overall GroveDB root to ensure the composition is correct.
        //
        // INVARIANT (https://github.com/dashpay/grovedb/issues/775): every write of
        // the sync — all restored subtrees across every discovery batch — stays
        // inside `session.transaction` until this check passes. Nothing is
        // persisted early; a mismatch here (or dropping the session at any point
        // before commit) rolls the destination back to its pre-sync state.
        // `subtrees_batch_size` only paces subtree discovery; it must never
        // reintroduce intermediate commits.
        let actual_root_hash = session
            .db
            .root_hash(Some(&session.transaction), grove_version)
            .unwrap()
            .map_err(|e| {
                Error::CorruptedData(format!("failed to compute root hash before commit: {e}"))
            })?;
        if actual_root_hash != session.app_hash {
            return Err(Error::CorruptedData(format!(
                "state sync root hash mismatch: expected {}, got {}",
                hex::encode(session.app_hash),
                hex::encode(actual_root_hash),
            )));
        }

        session
            .db
            .commit_transaction(session.transaction)
            .value
            .map_err(|e| Error::InternalError(format!("failed to commit sync transaction: {e}")))?;
        Ok(())
    }

    /// Adds synchronization information for a subtree into the current
    /// synchronization session.
    ///
    /// This function interacts with a `GroveDb` database to open a Merk tree at
    /// the specified path, calculate and verify its cryptographic hashes,
    /// and update the session state with the relevant synchronization
    /// information. The function generates and returns the global chunk ID for
    /// the subtree.
    ///
    /// # Parameters
    /// - `self`: A pinned, boxed instance of the `MultiStateSyncSession`.
    /// - `db`: A reference to the `GroveDb` instance.
    /// - `path`: The path to the subtree as a `SubtreePath`.
    /// - `hash`: The expected cryptographic hash of the subtree.
    /// - `actual_hash`: An optional actual cryptographic hash to compare
    ///   against the expected hash.
    /// - `chunk_prefix`: A 32-byte prefix used for identifying chunks in the
    ///   synchronization process.
    /// - `grove_version`: The GroveDB version to use for processing.
    ///
    /// # Returns
    /// - `Ok(Vec<u8>)`: On success, returns the encoded global chunk ID for the
    ///   subtree.
    /// - `Err(Error)`: If the Merk tree cannot be opened or synchronization
    ///   information cannot be added.
    ///
    /// # Errors
    /// This function returns an error if:
    /// - The Merk tree at the specified path cannot be opened.
    /// - Any synchronization-related operations fail.
    /// - Internal errors occur during processing.
    ///
    /// # Safety
    /// - This function uses unsafe code to create a reference to the
    ///   transaction. Ensure that the transaction is properly managed and the
    ///   lifetime guarantees are respected.
    pub fn add_subtree_sync_info<'b, B: AsRef<[u8]>>(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        path: SubtreePath<'b, B>,
        hash: CryptoHash,
        actual_hash: Option<CryptoHash>,
        chunk_prefix: [u8; 32],
        grove_version: &GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &Transaction<'db> = &self.as_ref().transaction;
            &*(tx as *const _)
        };

        if let Ok((merk, root_key, tree_type, element)) =
            self.db
                .open_merk_for_replication(path.clone(), transaction_ref, grove_version)
        {
            if supports_entry_replay(tree_type) {
                // Non-Merk append-only subtree: restored by replaying leaf
                // entries rather than Merk chunks (see issues #785 and
                // #783 / #784). The Merk opened above is structurally
                // empty for these types and is not needed.
                drop(merk);
                let element = element.ok_or_else(|| {
                    Error::InternalError(
                        "append-only subtree must have a parent element".to_string(),
                    )
                })?;
                let actual_hash = actual_hash.ok_or_else(|| {
                    Error::InternalError(
                        "append-only subtree sync requires the parent element value hash"
                            .to_string(),
                    )
                })?;
                let non_merk_restorer = NonMerkRestorer::new(element, hash, actual_hash)?;
                let first_chunk_id = non_merk_restorer.initial_chunk_id();
                let mut sync_info = SubtreeStateSyncInfo {
                    restorer: SubtreeRestorer::NonMerk(non_merk_restorer),
                    root_key: root_key.clone(),
                    tree_type,
                    pending_chunks: Default::default(),
                    current_path: path.to_vec(),
                    num_processed_chunks: 0,
                };
                sync_info.pending_chunks.insert(first_chunk_id.clone());
                self.as_mut()
                    .current_prefixes()
                    .insert(chunk_prefix, sync_info);
                return encode_global_chunk_id(
                    chunk_prefix,
                    root_key,
                    tree_type,
                    vec![first_chunk_id],
                );
            }
            let restorer = Restorer::new(merk, hash, actual_hash);
            let mut sync_info = SubtreeStateSyncInfo::new(restorer);
            sync_info.pending_chunks.insert(vec![]);
            sync_info.root_key = root_key.clone();
            sync_info.tree_type = tree_type;
            sync_info.current_path = path.to_vec();
            self.as_mut()
                .current_prefixes()
                .insert(chunk_prefix, sync_info);
            encode_global_chunk_id(chunk_prefix, root_key, tree_type, vec![])
        } else {
            Err(Error::InternalError(
                "Unable to open merk for replication".to_string(),
            ))
        }
    }

    fn current_prefixes(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut BTreeMap<SubtreePrefix, SubtreeStateSyncInfo<'db>> {
        // SAFETY: we only access a single field and do not move the struct;
        // the pin invariant only protects `transaction` from being moved.
        &mut unsafe { self.get_unchecked_mut() }.current_prefixes
    }

    fn processed_prefixes(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut BTreeSet<SubtreePrefix> {
        // SAFETY: we only access a single field and do not move the struct;
        // the pin invariant only protects `transaction` from being moved.
        &mut unsafe { self.get_unchecked_mut() }.processed_prefixes
    }

    fn num_processed_subtrees_in_batch(self: Pin<&mut MultiStateSyncSession<'db>>) -> &mut usize {
        // SAFETY: we only access a single field and do not move the struct;
        // the pin invariant only protects `transaction` from being moved.
        &mut unsafe { self.get_unchecked_mut() }.num_processed_subtrees_in_batch
    }

    fn pending_discovered_subtrees(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut Option<SubtreesMetadata> {
        // SAFETY: we only access a single field and do not move the struct;
        // the pin invariant only protects `transaction` from being moved.
        &mut unsafe { self.get_unchecked_mut() }.pending_discovered_subtrees
    }

    fn indexed_groups(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut BTreeMap<SubtreePrefix, IndexedSyncGroup> {
        // SAFETY: we only access a single field and do not move the struct;
        // the pin invariant only protects `transaction` from being moved.
        &mut unsafe { self.get_unchecked_mut() }.indexed_groups
    }

    fn secondary_owner(
        self: Pin<&mut MultiStateSyncSession<'db>>,
    ) -> &mut BTreeMap<SubtreePrefix, (SubtreePrefix, u8)> {
        // SAFETY: we only access a single field and do not move the struct;
        // the pin invariant only protects `transaction` from being moved.
        &mut unsafe { self.get_unchecked_mut() }.secondary_owner
    }

    /// Registers an indexed subtree group (protocol version 2) and opens
    /// its primary for restore.
    ///
    /// The primary starts in the header-pending state: its single pending
    /// chunk is the [`IndexedHeaderRequest`] carrying the axis tags and
    /// secondary root keys from the hash-verified element; the responding
    /// header page delivers the root hashes needed to construct the
    /// actual restorer. Axis secondaries are activated when that header
    /// arrives (see [`Self::register_indexed_header`]).
    fn add_indexed_primary_sync_info(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        path: Vec<Vec<u8>>,
        elem_value_hash: CryptoHash,
        actual_value_hash: CryptoHash,
        element: Element,
        chunk_prefix: SubtreePrefix,
        grove_version: &GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &Transaction<'db> = &self.as_ref().transaction;
            &*(tx as *const _)
        };

        let subtree_path: Vec<&[u8]> = path.iter().map(|vec| vec.as_slice()).collect();
        let path_ref: &[&[u8]] = &subtree_path;
        let (merk, root_key, tree_type, _element) = self
            .db
            .open_merk_for_replication(path_ref.into(), transaction_ref, grove_version)
            .map_err(|e| {
                Error::InternalError(format!(
                    "Unable to open indexed primary for replication: {e}"
                ))
            })?;
        if !tree_type.is_indexed_primary() {
            return Err(Error::InternalError(format!(
                "expected an indexed primary at {:?}, got {tree_type:?}",
                path_to_string(&path)
            )));
        }

        let axes_pairs = indexed_element_axes(&element)?;
        let mut axes = Vec::with_capacity(axes_pairs.len());
        let mut request_axes = Vec::with_capacity(axes_pairs.len());
        for (axis, secondary_root_key) in axes_pairs {
            let secondary_prefix =
                RocksDbStorage::secondary_prefix_for(&chunk_prefix, axis.tag()).unwrap();
            axes.push((axis.tag(), secondary_prefix, secondary_root_key.clone()));
            request_axes.push((axis.tag(), secondary_root_key));
        }
        let header_request = IndexedHeaderRequest { axes: request_axes }.encode();

        for (tag, secondary_prefix, _) in &axes {
            self.as_mut()
                .secondary_owner()
                .insert(*secondary_prefix, (chunk_prefix, *tag));
        }
        self.as_mut().indexed_groups().insert(
            chunk_prefix,
            IndexedSyncGroup {
                path: path.clone(),
                element,
                actual_value_hash,
                elem_value_hash,
                axes,
                header: None,
                primary_root: None,
                secondary_roots: BTreeMap::new(),
            },
        );

        let mut sync_info = SubtreeStateSyncInfo {
            restorer: SubtreeRestorer::IndexedPending(Some(merk)),
            root_key: root_key.clone(),
            tree_type,
            pending_chunks: Default::default(),
            current_path: path,
            num_processed_chunks: 0,
        };
        sync_info.pending_chunks.insert(header_request.clone());
        self.as_mut()
            .current_prefixes()
            .insert(chunk_prefix, sync_info);
        encode_global_chunk_id(chunk_prefix, root_key, tree_type, vec![header_request])
    }

    /// Activates the Merk chunk restore of one axis secondary, verified
    /// per-chunk against the group header's hash for that axis. Called
    /// only after the group's header arrived.
    fn add_indexed_secondary_sync_info(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        secondary_prefix: SubtreePrefix,
        primary_prefix: SubtreePrefix,
        axis_tag: u8,
        grove_version: &GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        let group = self.indexed_groups.get(&primary_prefix).ok_or_else(|| {
            Error::InternalError("indexed secondary has no registered group".to_string())
        })?;
        let header = group.header.as_ref().ok_or_else(|| {
            Error::InternalError("indexed secondary activated before the group header".to_string())
        })?;
        let expected_root_hash = header
            .axes
            .iter()
            .find(|(tag, _)| *tag == axis_tag)
            .map(|(_, hash)| *hash)
            .ok_or_else(|| {
                Error::InternalError("group header is missing the requested axis".to_string())
            })?;
        let root_key = group
            .axes
            .iter()
            .find(|(tag, ..)| *tag == axis_tag)
            .map(|(_, _, root_key)| root_key.clone())
            .ok_or_else(|| {
                Error::InternalError("group axes are missing the requested axis".to_string())
            })?;
        let axis = IndexAxis::try_from_tag(axis_tag)
            .map_err(|e| Error::CorruptedData(format!("invalid axis tag in indexed group: {e}")))?;
        let tree_type = axis_secondary_tree_type(axis);

        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &Transaction<'db> = &self.as_ref().transaction;
            &*(tx as *const _)
        };
        let storage = self
            .db
            .db
            .get_immediate_storage_context_by_subtree_prefix(secondary_prefix, transaction_ref)
            .unwrap();
        let merk = if root_key.is_some() {
            Merk::open_layered_with_root_key(
                storage,
                root_key.clone(),
                tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!("cannot open indexed secondary for restore: {e}"))
            })
            .unwrap()?
        } else {
            Merk::open_base(
                storage,
                tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "cannot open empty indexed secondary for restore: {e}"
                ))
            })
            .unwrap()?
        };
        let restorer = Restorer::new(merk, expected_root_hash, None);
        let mut sync_info = SubtreeStateSyncInfo::new(restorer);
        sync_info.pending_chunks.insert(vec![]);
        sync_info.root_key = root_key.clone();
        sync_info.tree_type = tree_type;
        // Secondaries live at a derived prefix, not a path; current_path
        // stays empty and completion skips subtree discovery for them.
        self.as_mut()
            .current_prefixes()
            .insert(secondary_prefix, sync_info);
        encode_global_chunk_id(secondary_prefix, root_key, tree_type, vec![])
    }

    /// Stores a received indexed header on its group after validating
    /// that its axis tags exactly match the element's configured axes,
    /// and returns the metadata entries that activate the group's
    /// secondaries.
    fn register_indexed_header(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        primary_prefix: SubtreePrefix,
        header: IndexedHeader,
    ) -> Result<SubtreesMetadata, Error> {
        let group = self
            .as_mut()
            .indexed_groups()
            .get_mut(&primary_prefix)
            .ok_or_else(|| {
                Error::InternalError("received an indexed header for an unknown group".to_string())
            })?;
        if group.header.is_some() {
            return Err(Error::InternalError(
                "received a second indexed header for the same group".to_string(),
            ));
        }
        if header.axes.len() != group.axes.len()
            || header
                .axes
                .iter()
                .zip(group.axes.iter())
                .any(|((header_tag, _), (group_tag, ..))| header_tag != group_tag)
        {
            return Err(Error::CorruptedData(
                "indexed header axes do not match the element's configured axes".to_string(),
            ));
        }
        let mut metadata = SubtreesMetadata::new();
        for (tag, secondary_prefix, _) in &group.axes {
            metadata.data.insert(
                *secondary_prefix,
                SubtreeMetadata::IndexedSecondary {
                    primary_prefix,
                    axis_tag: *tag,
                },
            );
        }
        group.header = Some(header);
        Ok(metadata)
    }

    /// Records the actual restored root hash of a completed subtree that
    /// is a member of an indexed group (no-op otherwise) and, once the
    /// whole group is restored, runs the unconditional joint verification
    /// against the parent binding. The recorded hashes come from the
    /// restored Merks themselves — the wire header plays no part here.
    fn note_indexed_member_complete(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        chunk_prefix: SubtreePrefix,
        actual_root_hash: CryptoHash,
    ) -> Result<(), Error> {
        let (primary_prefix, axis_tag) = if self.indexed_groups.contains_key(&chunk_prefix) {
            (chunk_prefix, None)
        } else if let Some((primary_prefix, axis_tag)) = self.secondary_owner.get(&chunk_prefix) {
            (*primary_prefix, Some(*axis_tag))
        } else {
            return Ok(());
        };

        let groups = self.as_mut().indexed_groups();
        let group = groups
            .get_mut(&primary_prefix)
            .expect("membership checked above");
        match axis_tag {
            None => group.primary_root = Some(actual_root_hash),
            Some(tag) => {
                group.secondary_roots.insert(tag, actual_root_hash);
            }
        }
        let group_complete = group.header.is_some()
            && group.primary_root.is_some()
            && group.secondary_roots.len() == group.axes.len();
        if !group_complete {
            return Ok(());
        }

        let group = groups.remove(&primary_prefix).expect("present above");
        for (_, secondary_prefix, _) in &group.axes {
            self.as_mut().secondary_owner().remove(secondary_prefix);
        }
        let secondary_roots: Vec<(u8, CryptoHash)> = group
            .axes
            .iter()
            .map(|(tag, ..)| (*tag, group.secondary_roots[tag]))
            .collect();
        verify_indexed_binding(
            &group.element,
            &group.actual_value_hash,
            &group.elem_value_hash,
            &group.primary_root.expect("checked complete above"),
            &secondary_roots,
        )
        .map_err(|e| {
            Error::CorruptedData(format!(
                "indexed subtree at {:?} failed joint verification: {e}",
                path_to_string(&group.path)
            ))
        })
    }

    /// Applies a chunk during the state synchronization process.
    /// This method should be called by ABCI when the `ApplySnapshotChunk`
    /// method is invoked.
    ///
    /// # Parameters
    /// - `self`: A pinned mutable reference to the `MultiStateSyncSession`.
    /// - `db`: A reference to the `GroveDb` instance used for synchronization.
    /// - `packed_global_chunk_ids`: A byte slice representing the packed global
    ///   chunk IDs being applied.
    /// - `packed_global_chunks`: A byte slice containing packed encoded proof
    ///   for the chunk.
    /// - `version`: The state synchronization protocol version being used.
    /// - `grove_version`: A reference to the `GroveVersion` specifying the
    ///   GroveDB version.
    ///
    /// # Returns
    /// - `Ok(Vec<Vec<u8>>)`: A tuple of: vector of global chunk IDs (each
    ///   represented as a vector of bytes) that can be fetched from sources for
    ///   further synchronization.
    /// - `Err(Error)`: An error if the chunk application fails or if the chunk
    ///   proof is invalid.
    ///
    /// # Behavior
    /// - This method applies the given chunk using the provided
    ///   `global_chunk_id` and its corresponding proof data (`chunk`).
    /// - Once the chunk is applied successfully, it calculates and returns the
    ///   next set of global chunk IDs required for further synchronization.
    ///
    /// # Notes
    /// - Ensure the `chunk` is correctly encoded and matches the expected proof
    ///   format.
    /// - This function modifies the state of the synchronization session, so it
    ///   must be used carefully to maintain correctness and avoid errors.
    /// - The pinned `self` ensures that the session cannot be moved in memory,
    ///   preserving consistency during the synchronization process.
    pub fn apply_chunk(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        packed_global_chunk_ids: &[u8],
        packed_global_chunks: &[u8],
        version: u16,
        grove_version: &GroveVersion,
    ) -> Result<Vec<Vec<u8>>, Error> {
        if !is_supported_state_sync_version(version) {
            return Err(Error::CorruptedData(
                "Unsupported state sync protocol version".to_string(),
            ));
        }
        if version != self.version {
            return Err(Error::CorruptedData(
                "state sync protocol version does not match the session's version".to_string(),
            ));
        }

        let mut nested_global_chunk_ids: Vec<Vec<u8>> = vec![];
        let mut nested_global_chunks: Vec<Vec<u8>> = vec![];
        if self.app_hash == packed_global_chunk_ids {
            nested_global_chunk_ids = vec![packed_global_chunk_ids.to_vec()];
            nested_global_chunks = unpack_nested_bytes(packed_global_chunks)?;
        } else {
            nested_global_chunk_ids.extend(unpack_nested_bytes(packed_global_chunk_ids)?);
            nested_global_chunks.extend(unpack_nested_bytes(packed_global_chunks)?);
        }

        if nested_global_chunk_ids.len() != nested_global_chunks.len() {
            return Err(Error::InternalError(
                "Packed num of global chunkIDs and chunks are not matching".to_string(),
            ));
        }
        if self.is_empty() {
            return Err(Error::InternalError(
                "GroveDB is not in syncing mode".to_string(),
            ));
        }

        let db = self.db;
        // SAFETY: the transaction lives as long as the pinned session and is
        // dropped last; the reference is only used within this call while
        // the session is alive. This mirrors the pattern used by
        // `add_subtree_sync_info` and `discover_new_subtrees_metadata`.
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &Transaction<'db> = &self.as_ref().transaction;
            &*(tx as *const _)
        };

        let mut next_global_chunk_ids: Vec<Vec<u8>> = vec![];
        let mut received_headers: Vec<(SubtreePrefix, IndexedHeader)> = vec![];

        for (iter_global_chunk_id, iter_packed_chunks) in nested_global_chunk_ids
            .iter()
            .zip(nested_global_chunks.iter())
        {
            let mut next_chunk_ids = vec![];

            let (chunk_prefix, _, _, nested_local_chunk_ids) =
                replication::utils::decode_global_chunk_id(
                    iter_global_chunk_id.as_slice(),
                    &self.app_hash,
                )?;

            let it_chunk_ids = if nested_local_chunk_ids.is_empty() {
                vec![vec![]]
            } else {
                nested_local_chunk_ids
            };

            let current_nested_chunk_data = unpack_nested_bytes(iter_packed_chunks.as_slice())?;

            if it_chunk_ids.len() != current_nested_chunk_data.len() {
                return Err(Error::InternalError(
                    "Packed num of chunkIDs and chunks are not matching #2".to_string(),
                ));
            }

            let current_prefixes = self.as_mut().current_prefixes();
            let Some(subtree_state_sync) = current_prefixes.get_mut(&chunk_prefix) else {
                return Err(Error::InternalError(
                    "Unable to process incoming chunk".to_string(),
                ));
            };

            let mut next_local_chunk_ids = vec![];
            for (current_local_chunk_id, current_local_chunks) in
                it_chunk_ids.iter().zip(current_nested_chunk_data.iter())
            {
                let (local_ids, header) = subtree_state_sync.apply_inner_chunk(
                    db,
                    transaction_ref,
                    current_local_chunk_id.as_slice(),
                    current_local_chunks.as_slice(),
                    grove_version,
                )?;
                next_local_chunk_ids.extend(local_ids);
                if let Some(header) = header {
                    received_headers.push((chunk_prefix, header));
                }
            }

            if !next_local_chunk_ids.is_empty() {
                for grouped_ids in next_local_chunk_ids.chunks(CONST_GROUP_PACKING_SIZE) {
                    next_chunk_ids.push(encode_global_chunk_id(
                        chunk_prefix,
                        subtree_state_sync.root_key.clone(),
                        subtree_state_sync.tree_type,
                        grouped_ids.to_vec(),
                    )?);
                }
                next_global_chunk_ids.extend(next_chunk_ids);
            } else if subtree_state_sync.pending_chunks.is_empty() {
                let completed_path = subtree_state_sync.current_path.clone();

                // Subtree is finished. We can save it.
                let is_subtree_empty = subtree_state_sync.num_processed_chunks == 0;
                let mut is_non_merk_subtree = false;
                // Actual root hash of a completed Merk restore, recorded
                // for indexed-group members (the joint verification uses
                // these, never the wire header's claims).
                let mut completed_member_root: Option<CryptoHash> = None;
                if let Some(prefix_data) = current_prefixes.remove(&chunk_prefix) {
                    match prefix_data.restorer {
                        SubtreeRestorer::Merk(restorer) => {
                            if is_subtree_empty {
                                // For empty subtrees, verify the restorer's underlying merk
                                // has a NULL root hash. A malicious peer that sends empty
                                // data for a non-empty subtree will be caught here (and
                                // also at commit time via H3 root hash verification).
                                let merk = restorer.into_merk();
                                let merk_root = merk.root_hash().unwrap();
                                if merk_root != grovedb_merk::tree::hash::NULL_HASH {
                                    return Err(Error::InternalError(
                                        "empty subtree has non-null root hash".to_string(),
                                    ));
                                }
                                completed_member_root = Some(merk_root);
                            } else {
                                match restorer.finalize(grove_version) {
                                    Ok(merk) => {
                                        completed_member_root = Some(merk.root_hash().unwrap());
                                    }
                                    Err(err) => {
                                        return Err(Error::InternalError(format!(
                                            "Unable to finalize Merk: {:?}",
                                            err
                                        )));
                                    }
                                }
                            }
                        }
                        SubtreeRestorer::NonMerk(non_merk_restorer) => {
                            // Entry replay finished: recompute the state
                            // root from the replayed payload and verify it
                            // against the parent binding. A byzantine
                            // source that tampered with any wire byte is
                            // rejected here.
                            is_non_merk_subtree = true;
                            non_merk_restorer.finalize(
                                db,
                                transaction_ref,
                                &completed_path,
                                grove_version,
                            )?;
                        }
                        SubtreeRestorer::IndexedPending(_) => {
                            return Err(Error::InternalError(
                                "indexed primary completed before its header page".to_string(),
                            ));
                        }
                    }
                } else {
                    return Err(Error::InternalError(format!(
                        "Prefix {:?} does not exist in current_prefixes",
                        chunk_prefix
                    )));
                }

                // Whether this prefix is an axis secondary must be read
                // BEFORE the group bookkeeping below, which un-registers
                // the group's members once the group resolves.
                let is_indexed_secondary = self.secondary_owner.contains_key(&chunk_prefix);

                self.as_mut().processed_prefixes().insert(chunk_prefix);

                *self.as_mut().num_processed_subtrees_in_batch() += 1;

                if let Some(actual_root_hash) = completed_member_root {
                    self.note_indexed_member_complete(chunk_prefix, actual_root_hash)?;
                }

                // Non-Merk append-only subtrees never contain child
                // subtrees, and their data namespace holds raw payload
                // entries (not Elements) — running element discovery over
                // it would fail. Skip it. Indexed-axis secondaries hold
                // only reference rows and live at a derived prefix with no
                // path, so discovery is skipped for them too.
                let new_subtrees_metadata = if is_non_merk_subtree || is_indexed_secondary {
                    SubtreesMetadata::default()
                } else {
                    self.discover_new_subtrees_metadata(&completed_path, grove_version)?
                };

                if self.num_processed_subtrees_in_batch >= self.subtrees_batch_size {
                    match self.as_mut().pending_discovered_subtrees() {
                        None => {
                            *self.as_mut().pending_discovered_subtrees() =
                                Some(new_subtrees_metadata);
                        }
                        Some(existing_subtrees_metadata) => {
                            existing_subtrees_metadata
                                .data
                                .extend(new_subtrees_metadata.data);
                        }
                    }
                } else {
                    let res = self
                        .prepare_sync_state_sessions(new_subtrees_metadata, grove_version)
                        .map_err(|e| {
                            Error::InternalError(format!("Unable to discover Subtrees: {e}"))
                        })?;
                    next_chunk_ids.extend(res);
                    next_global_chunk_ids.extend(next_chunk_ids);
                }
            }
        }

        // Indexed headers received in this call activate their groups'
        // axis secondaries, through the same discovery pacing as newly
        // discovered subtrees.
        for (primary_prefix, header) in received_headers {
            let secondaries_metadata = self.register_indexed_header(primary_prefix, header)?;
            if self.num_processed_subtrees_in_batch >= self.subtrees_batch_size {
                match self.as_mut().pending_discovered_subtrees() {
                    None => {
                        *self.as_mut().pending_discovered_subtrees() = Some(secondaries_metadata);
                    }
                    Some(existing_subtrees_metadata) => {
                        existing_subtrees_metadata
                            .data
                            .extend(secondaries_metadata.data);
                    }
                }
            } else {
                let res = self
                    .prepare_sync_state_sessions(secondaries_metadata, grove_version)
                    .map_err(|e| {
                        Error::InternalError(format!("Unable to activate indexed secondaries: {e}"))
                    })?;
                next_global_chunk_ids.extend(res);
            }
        }

        if self.num_processed_subtrees_in_batch >= self.subtrees_batch_size
            && self.current_prefixes.is_empty()
        {
            // Batch boundary: everything restored so far stays inside the
            // session transaction (see the atomicity invariant in
            // `commit()`); `subtrees_batch_size` only paces how many
            // subtrees are discovered and in flight at once.
            let new_subtrees_metadata =
                self.as_mut()
                    .pending_discovered_subtrees()
                    .take()
                    .ok_or(Error::CorruptedData(
                        "No pending subtrees available for resume_sync".to_string(),
                    ))?;
            *self.as_mut().num_processed_subtrees_in_batch() = 0;

            let mut next_chunk_ids = vec![];

            let discovered_chunk_ids = self
                .prepare_sync_state_sessions(new_subtrees_metadata, grove_version)
                .map_err(|e| Error::InternalError(format!("Unable to discover Subtrees: {e}")))?;
            next_chunk_ids.extend(discovered_chunk_ids);
            next_global_chunk_ids.extend(next_chunk_ids);
        }

        let mut res: Vec<Vec<u8>> = vec![];
        for grouped_next_global_chunk_ids in next_global_chunk_ids.chunks(CONST_GROUP_PACKING_SIZE)
        {
            res.push(pack_nested_bytes(grouped_next_global_chunk_ids.to_vec())?);
        }

        Ok(res)
    }

    /// Discovers new subtrees at the given path that need to be synchronized.
    ///
    /// # Parameters
    /// - `self`: A pinned mutable reference to the `MultiStateSyncSession`.
    /// - `db`: A reference to the `GroveDb` instance being used for
    ///   synchronization.
    /// - `path_vec`: A vector of byte vectors representing the path where
    ///   subtrees should be discovered.
    /// - `grove_version`: A reference to the `GroveVersion` specifying the
    ///   GroveDB version.
    ///
    /// # Returns
    /// - `Ok(SubtreesMetadata)`: Metadata about the discovered subtrees,
    ///   including information necessary for their synchronization.
    /// - `Err(Error)`: An error if the discovery process fails.
    ///
    /// # Behavior
    /// - This function traverses the specified `path_vec` in the database and
    ///   identifies subtrees that are not yet synchronized.
    /// - Returns metadata about these subtrees, which can be used to initiate
    ///   or manage the synchronization process.
    ///
    /// # Notes
    /// - The `path_vec` should represent a valid path in the GroveDB where
    ///   subtrees are expected to exist.
    /// - Ensure that the GroveDB instance (`db`) and Grove version
    ///   (`grove_version`) are compatible and up-to-date to avoid errors during
    ///   discovery.
    /// - The function modifies the state of the synchronization session, so it
    ///   should be used carefully to maintain session integrity.
    fn discover_new_subtrees_metadata(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        path_vec: &[Vec<u8>],
        grove_version: &GroveVersion,
    ) -> Result<SubtreesMetadata, Error> {
        let transaction_ref: &'db Transaction<'db> = unsafe {
            let tx: &Transaction<'db> = &self.as_ref().transaction;
            &*(tx as *const _)
        };
        let subtree_path: Vec<&[u8]> = path_vec.iter().map(|vec| vec.as_slice()).collect();
        let path: &[&[u8]] = &subtree_path;
        let merk = self
            .db
            .open_transactional_merk_at_path(path.into(), transaction_ref, None, grove_version)
            .value
            .map_err(|e| Error::CorruptedData(format!("failed to open merk by path-tx:{}", e)))?;
        if merk.is_empty_tree().unwrap() {
            return Ok(SubtreesMetadata::default());
        }
        let mut subtree_elements: BTreeMap<Vec<u8>, Element> = BTreeMap::new();

        let mut raw_iter = Element::iterator(merk.storage.raw_iter()).unwrap();
        while let Some((key, value)) = raw_iter.next_element(grove_version).unwrap()? {
            if value.is_any_tree() {
                // Indexed trees are transferable starting with state sync
                // protocol version 2 (their primaries commit a three-input
                // `combine_hash_three` and carry secondary storage
                // namespaces at `Blake3(prefix ‖ axis_tag)` — see
                // `indexed_sync`). A version 1 session keeps the up-front
                // reject: its peer cannot serve them and its restorer
                // cannot verify them, so a chunk-based restore would fail
                // midway with an opaque "chunk doesn't match expected
                // root hash".
                if value.is_indexed_tree() && self.version < INDEXED_SYNC_MIN_VERSION {
                    return Err(Error::NotSupported(
                        "state sync does not support indexed trees \
                         (ProvableCountIndexedTree / ProvableSumIndexedTree / \
                         ProvableCountProvableSumIndexedTree) before protocol version 2"
                            .to_string(),
                    ));
                }
                // Non-Merk append-only trees (CommitmentTree / MmrTree /
                // BulkAppendTree / DenseAppendOnlyFixedSizeTree /
                // PrivateDocumentStore) are discovered like any subtree;
                // `add_subtree_sync_info` routes them to the entry-replay
                // restore path instead of Merk chunk restore (see issues
                // #785 and #783 / #784).
                subtree_elements.insert(key.to_vec(), value);
            }
        }

        let mut subtrees_metadata = SubtreesMetadata::new();
        for (subtree_key, element) in subtree_elements {
            let (elem_value, elem_value_hash) = merk
                .get_value_and_value_hash(
                    subtree_key.as_slice(),
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version,
                )
                .value
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to get value and hash for subtree key during discovery: {e}"
                    ))
                })?
                .ok_or_else(|| {
                    Error::CorruptedData(
                        "subtree key found in iterator but missing from merk".to_string(),
                    )
                })?;

            let actual_value_hash = value_hash(&elem_value).unwrap();
            let mut new_path = path_vec.to_vec();
            new_path.push(subtree_key);

            let subtree_path: Vec<&[u8]> = new_path.iter().map(|vec| vec.as_slice()).collect();
            let path: &[&[u8]] = &subtree_path;
            let prefix = RocksDbStorage::build_prefix(path.as_ref().into()).unwrap();

            let entry = if element.is_indexed_tree() {
                SubtreeMetadata::IndexedPrimary {
                    path: new_path,
                    actual_value_hash,
                    elem_value_hash,
                    element,
                }
            } else {
                SubtreeMetadata::Ordinary {
                    path: new_path,
                    actual_value_hash,
                    elem_value_hash,
                }
            };
            subtrees_metadata.data.insert(prefix, entry);
        }

        Ok(subtrees_metadata)
    }

    /// Prepares a synchronization session for the newly discovered subtrees and
    /// returns the global chunk IDs of those subtrees.
    ///
    /// # Parameters
    /// - `self`: A pinned mutable reference to the `MultiStateSyncSession`.
    /// - `db`: A reference to the `GroveDb` instance used for managing the
    ///   synchronization process.
    /// - `subtrees_metadata`: Metadata about the discovered subtrees that
    ///   require synchronization.
    /// - `grove_version`: A reference to the `GroveVersion` specifying the
    ///   GroveDB version.
    ///
    /// # Returns
    /// - `Ok(Vec<Vec<u8>>)`: A vector of global chunk IDs (each represented as
    ///   a vector of bytes) corresponding to the newly discovered subtrees.
    ///   These IDs can be fetched from sources to continue the synchronization
    ///   process.
    /// - `Err(Error)`: An error if the synchronization session could not be
    ///   prepared or if processing the metadata fails.
    ///
    /// # Behavior
    /// - Initializes the synchronization state for the newly discovered
    ///   subtrees based on the provided metadata.
    /// - Calculates and returns the global chunk IDs of these subtrees,
    ///   enabling further state synchronization.
    ///
    /// # Notes
    /// - Ensure that the `subtrees_metadata` accurately reflects the subtrees
    ///   requiring synchronization.
    /// - This function modifies the state of the synchronization session to
    ///   include the new subtrees.
    /// - Proper handling of the returned global chunk IDs is essential to
    ///   ensure seamless state synchronization.
    fn prepare_sync_state_sessions(
        self: &mut Pin<Box<MultiStateSyncSession<'db>>>,
        subtrees_metadata: SubtreesMetadata,
        grove_version: &GroveVersion,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let mut res = vec![];

        for (prefix, prefix_metadata) in subtrees_metadata.data {
            if self.processed_prefixes.contains(&prefix)
                || self.current_prefixes.contains_key(&prefix)
            {
                continue;
            }
            let next_chunks_ids = match prefix_metadata {
                SubtreeMetadata::Ordinary {
                    path,
                    actual_value_hash,
                    elem_value_hash,
                } => {
                    let subtree_path: Vec<&[u8]> = path.iter().map(|vec| vec.as_slice()).collect();
                    let path_ref: &[&[u8]] = &subtree_path;

                    self.add_subtree_sync_info(
                        path_ref.into(),
                        elem_value_hash,
                        Some(actual_value_hash),
                        prefix,
                        grove_version,
                    )?
                }
                SubtreeMetadata::IndexedPrimary {
                    path,
                    actual_value_hash,
                    elem_value_hash,
                    element,
                } => self.add_indexed_primary_sync_info(
                    path,
                    elem_value_hash,
                    actual_value_hash,
                    element,
                    prefix,
                    grove_version,
                )?,
                SubtreeMetadata::IndexedSecondary {
                    primary_prefix,
                    axis_tag,
                } => self.add_indexed_secondary_sync_info(
                    prefix,
                    primary_prefix,
                    axis_tag,
                    grove_version,
                )?,
            };

            res.push(next_chunks_ids);
        }

        Ok(res)
    }
}

/// Metadata for one discovered subtree awaiting synchronization.
///
/// The `actual_value_hash` (`value_hash(element_bytes)`) and
/// `elem_value_hash` (the element value hash the parent Merk committed to)
/// are required to verify the integrity of the newly constructed subtree
/// after synchronization.
pub enum SubtreeMetadata {
    /// An ordinary subtree — including the non-Merk entry-replay family.
    Ordinary {
        /// The path of the subtree in GroveDB.
        path: Vec<Vec<u8>>,
        /// The subtree's actual value hash in the parent.
        actual_value_hash: CryptoHash,
        /// The subtree's element value hash in the parent.
        elem_value_hash: CryptoHash,
    },
    /// An indexed-tree primary (protocol version 2). Carries the decoded
    /// element so the axis tags and secondary root keys survive to
    /// session setup.
    IndexedPrimary {
        /// The path of the primary subtree in GroveDB.
        path: Vec<Vec<u8>>,
        /// The subtree's actual value hash in the parent.
        actual_value_hash: CryptoHash,
        /// The subtree's (three-input) element value hash in the parent.
        elem_value_hash: CryptoHash,
        /// The decoded indexed-tree element.
        element: Element,
    },
    /// One axis secondary of an indexed group, emitted when the group's
    /// header arrives; everything else needed lives on the registered
    /// group.
    IndexedSecondary {
        /// Prefix of the owning primary.
        primary_prefix: SubtreePrefix,
        /// The axis this secondary indexes.
        axis_tag: u8,
    },
}

/// Struct containing metadata about the current subtrees found in GroveDB.
/// This metadata is used during the state synchronization process to track
/// discovered subtrees and verify their integrity after they are constructed.
pub struct SubtreesMetadata {
    /// Discovered subtrees pending sync, keyed by their `SubtreePrefix`
    /// (the path digest of the subtree, or the derived secondary prefix
    /// for indexed-axis secondaries).
    pub data: BTreeMap<SubtreePrefix, SubtreeMetadata>,
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
            match metadata {
                SubtreeMetadata::Ordinary { path, .. } => {
                    writeln!(
                        f,
                        " prefix:{:?} -> path:{:?}",
                        hex::encode(prefix),
                        path_to_string(path),
                    )?;
                }
                SubtreeMetadata::IndexedPrimary { path, .. } => {
                    writeln!(
                        f,
                        " prefix:{:?} -> indexed primary path:{:?}",
                        hex::encode(prefix),
                        path_to_string(path),
                    )?;
                }
                SubtreeMetadata::IndexedSecondary {
                    primary_prefix,
                    axis_tag,
                } => {
                    writeln!(
                        f,
                        " prefix:{:?} -> indexed secondary (axis {axis_tag}) of primary:{:?}",
                        hex::encode(prefix),
                        hex::encode(primary_prefix),
                    )?;
                }
            }
        }
        Ok(())
    }
}
