//! The flat key-value seam.
//!
//! A flat keyspace is an ordered map from byte keys to byte values. GroveDB
//! keeps everything that is not a Merk node in one:
//!
//! - the records of the append-only trees (MMR, bulk-append, dense,
//!   commitment, private-document), in the data keyspace of their subtree;
//! - auxiliary records, in a subtree's aux keyspace;
//! - metadata, in a subtree's meta keyspace, which includes the
//!   pending-prefix-drop redo records under a reserved namespace.
//!
//! [`FlatStore`] is the handle to one such keyspace: get, put, delete, an
//! ordered cursor and a range delete over the whole keyspace. It names no
//! engine. The append-only tree crates are written against it, and each
//! storage engine supplies the handles.
//!
//! [`PendingPrefixDrops`] is the database-level half of the surface: the
//! three steps that reclaim the storage of a dropped subtree outside any
//! transaction.
//!
//! The full contract, with what is atomic with what and what holds after a
//! crash, is in `docs/lignindb/flat-store.md`.

use grovedb_costs::{
    cost_return_on_error, storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt,
    OperationCost,
};

pub use crate::storage::SubtreePrefix;
use crate::{Error, RawIterator};

/// One ordered key-value keyspace.
///
/// Keys are the caller's own bytes: a handle never shows the subtree prefix
/// an engine may store in front of them, in `get`, in `put` or through the
/// cursor. Keys order as byte strings.
///
/// # Visibility of writes
///
/// A handle either defers its writes or applies them at once, and the engine
/// decides which when it hands the handle out.
///
/// - A *deferred* handle stages `put` and `delete` in the multi-context batch
///   it was opened with. `get` and the cursor do not see a staged write. The
///   batch is applied to the transaction as one unit, so every write staged
///   through any handle of that batch becomes visible together.
/// - An *immediate* handle writes to the transaction at once, and its own
///   reads see the write.
///
/// Either way a read sees the transaction's applied writes and, beneath
/// them, the state the transaction reads from.
///
/// # Costs
///
/// Every method reports an `OperationCost`. What a call costs is set by the
/// cost model of the GroveVersion in force, not by this trait, and both
/// engines must report the same cost for the same call on the same state.
/// `cost_info` is the caller's own account of a write's storage cost, which
/// the model uses in place of measuring the record. For a new record in a
/// data keyspace the caller leaves the key's cost out of it, because the
/// caller does not know how the engine stores the key.
///
/// A deferred handle reports nothing for a write when it is staged; the
/// cost is reported when the batch is applied.
pub trait FlatStore {
    /// Ordered cursor over the keyspace.
    type Cursor<'a>: RawIterator
    where
        Self: 'a;

    /// Read the value stored under `key`.
    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error>;

    /// Store `value` under `key`, replacing any value already there.
    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Remove the record under `key`. Removing a key that holds no record
    /// succeeds.
    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Open a cursor over the keyspace. It is unpositioned until its first
    /// seek, walks the records visible to this handle when it was opened,
    /// and never leaves the keyspace.
    ///
    /// Deleting the record the cursor stands on and then stepping forward is
    /// defined. After any other write to the keyspace the cursor's results
    /// are not.
    fn cursor(&self) -> Self::Cursor<'_>;

    /// Range delete: remove every record of the keyspace, and of no other
    /// keyspace of the subtree.
    ///
    /// The removal belongs to the transaction like any other write of the
    /// handle. It deletes the records the cursor sees. On a deferred handle
    /// those deletes are staged, and a key the batch already holds a write
    /// for is settled by the batch's own rule.
    ///
    /// The cost is that of walking the keyspace with the cursor and deleting
    /// each record with no `cost_info`. An engine may do less work than
    /// that, but it must report this cost.
    fn clear(&self) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut cursor = self.cursor();
        cursor.seek_to_first().unwrap_add_cost(&mut cost);

        while cursor.valid().unwrap_add_cost(&mut cost) {
            if let Some(key) = cursor.key().unwrap_add_cost(&mut cost) {
                cost_return_on_error!(&mut cost, self.delete(key, None));
            }
            cursor.next().unwrap_add_cost(&mut cost);
        }
        Ok(()).wrap_with_cost(cost)
    }
}

/// Database-level reclamation of dropped subtree namespaces.
///
/// A flat drop removes a subtree's element from its parent and stages a
/// [`PendingPrefixDropRecord`] in the same commit
/// ([`stage_pending_prefix_drop`]). The storage the subtree owned is
/// reclaimed later by a drain that runs these three steps for each record,
/// in this order: list, delete the ranges, remove the record.
///
/// None of the three runs inside a transaction and none is charged. Each
/// takes effect on its own and is safe to repeat, so a drain interrupted at
/// any point is finished by running it again: a record is removed only after
/// its ranges are gone, and deleting a range that is already empty changes
/// nothing.
pub trait PendingPrefixDrops {
    /// List every committed pending-prefix-drop record, in ascending order
    /// of primary prefix.
    ///
    /// A record staged in a transaction that has not committed is not
    /// listed, so a drop that may still roll back is never reclaimed.
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error>;

    /// Remove every record stored under each of `prefixes`, in every
    /// keyspace of those subtrees: tree data, root pointer, aux and meta.
    ///
    /// The caller passes only prefixes that nothing reachable refers to any
    /// more, and nothing writes under them. A read that pinned an older
    /// committed state still sees the records.
    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error>;

    /// Remove the pending-prefix-drop record keyed by `primary_prefix`.
    /// Removing a record that is not there succeeds.
    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), Error>;
}

/// Stage `record` through `meta`, the meta keyspace of the subtree prefix
/// [`pending_prefix_drops_namespace`].
///
/// The record is keyed by its primary prefix and written with no
/// `cost_info`. Staged through the batch that also carries the delete of the
/// subtree's element, it commits with that delete or not at all.
pub fn stage_pending_prefix_drop<S: FlatStore>(
    meta: &S,
    record: &PendingPrefixDropRecord,
) -> CostResult<(), Error> {
    match record.encode() {
        Ok(value) => meta.put(record.primary_prefix, &value, None),
        Err(e) => Err(e).wrap_with_cost(OperationCost::default()),
    }
}

/// `Blake3("grovedb_pending_prefix_drops_v0")`.
const PENDING_PREFIX_DROPS_NAMESPACE: SubtreePrefix = [
    0xd5, 0x6a, 0x87, 0xf5, 0xb6, 0x7f, 0xed, 0xf2, 0x89, 0x90, 0xfa, 0x33, 0x85, 0x91, 0x24, 0x7e,
    0x60, 0xe2, 0xc1, 0xeb, 0x23, 0x5e, 0x39, 0xd9, 0xde, 0x90, 0xb3, 0xaf, 0x4e, 0xc5, 0x74, 0x88,
];

/// The reserved 32-byte namespace under which pending-prefix-drop redo
/// records live in the meta keyspace. Records are keyed
/// `namespace ‖ primary_prefix`.
///
/// The value is `Blake3("grovedb_pending_prefix_drops_v0")`. It cannot
/// collide with any path-derived prefix (`build_prefix`), axis-derived
/// secondary prefix (`secondary_prefix_for`), or the root prefix
/// (`[0u8; 32]`) without a Blake3 preimage/collision, and the meta keyspace
/// carries no production data outside this namespace.
pub fn pending_prefix_drops_namespace() -> &'static SubtreePrefix {
    &PENDING_PREFIX_DROPS_NAMESPACE
}

/// A durable redo record for one flat-subtree drop: the storage prefixes
/// that became unreachable when the subtree's element was deleted from its
/// parent Merk, plus the subtree's full path for the drain-time liveness
/// guard.
///
/// The record is written in the same atomic commit as the parent-Merk
/// delete and erased once every doomed prefix has been range-deleted,
/// making reclamation crash-safe and idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPrefixDropRecord {
    /// The dropped subtree's own path-derived prefix; also the record's key
    /// suffix within the namespace.
    pub primary_prefix: SubtreePrefix,
    /// Full path of the dropped subtree (parent path segments plus its key
    /// as the last segment). Used by the drain to verify the path does not
    /// resolve to a live element before reclaiming.
    pub path: Vec<Vec<u8>>,
    /// Every prefix to range-delete: the primary prefix plus, for indexed
    /// primaries, the per-axis secondary prefixes.
    pub doomed_prefixes: Vec<SubtreePrefix>,
}

impl PendingPrefixDropRecord {
    /// Wire version byte of the record value encoding.
    const ENCODING_VERSION: u8 = 0;

    /// Encode the record value:
    /// `[version(1)] [doomed_count(1)] [doomed_count × 32] [segment_count(1)]
    /// ([segment_len(1)] [segment_bytes])*`.
    ///
    /// Both counts and every segment length must fit in a `u8`; path
    /// segments are already capped at 255 bytes by `build_prefix`, and the
    /// doomed set is at most 1 + one prefix per index axis.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let doomed_count: u8 = self.doomed_prefixes.len().try_into().map_err(|_| {
            Error::StorageError("pending-prefix-drop record: too many doomed prefixes".to_string())
        })?;
        let segment_count: u8 = self.path.len().try_into().map_err(|_| {
            Error::StorageError("pending-prefix-drop record: path too deep".to_string())
        })?;
        let mut out = Vec::with_capacity(
            3 + 32 * self.doomed_prefixes.len()
                + self.path.iter().map(|s| 1 + s.len()).sum::<usize>(),
        );
        out.push(Self::ENCODING_VERSION);
        out.push(doomed_count);
        for prefix in &self.doomed_prefixes {
            out.extend_from_slice(prefix);
        }
        out.push(segment_count);
        for segment in &self.path {
            let len: u8 = segment.len().try_into().map_err(|_| {
                Error::StorageError(
                    "pending-prefix-drop record: path segment over 255 bytes".to_string(),
                )
            })?;
            out.push(len);
            out.extend_from_slice(segment);
        }
        Ok(out)
    }

    /// Decode a record from its key suffix (the primary prefix) and value.
    pub fn decode(key_suffix: &[u8], value: &[u8]) -> Result<Self, Error> {
        let corrupt =
            |what: &str| Error::StorageError(format!("corrupt pending-prefix-drop record: {what}"));
        let primary_prefix: SubtreePrefix = key_suffix
            .try_into()
            .map_err(|_| corrupt("key suffix is not 32 bytes"))?;
        fn take<'v>(rest: &mut &'v [u8], n: usize, what: &str) -> Result<&'v [u8], Error> {
            if rest.len() < n {
                return Err(Error::StorageError(format!(
                    "corrupt pending-prefix-drop record: {what}"
                )));
            }
            let (head, tail) = rest.split_at(n);
            *rest = tail;
            Ok(head)
        }
        let mut rest = value;
        let version = take(&mut rest, 1, "missing version byte")?[0];
        if version != Self::ENCODING_VERSION {
            return Err(corrupt("unknown encoding version"));
        }
        let doomed_count = take(&mut rest, 1, "missing doomed count")?[0] as usize;
        let mut doomed_prefixes = Vec::with_capacity(doomed_count);
        for _ in 0..doomed_count {
            let prefix: SubtreePrefix = take(&mut rest, 32, "truncated doomed prefix")?
                .try_into()
                .expect("split_at returned exactly 32 bytes");
            doomed_prefixes.push(prefix);
        }
        let segment_count = take(&mut rest, 1, "missing segment count")?[0] as usize;
        let mut path = Vec::with_capacity(segment_count);
        for _ in 0..segment_count {
            let len = take(&mut rest, 1, "missing segment length")?[0] as usize;
            path.push(take(&mut rest, len, "truncated path segment")?.to_vec());
        }
        if !rest.is_empty() {
            return Err(corrupt("trailing bytes"));
        }
        Ok(Self {
            primary_prefix,
            path,
            doomed_prefixes,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap};

    use grovedb_costs::CostContext;

    use super::*;

    /// A `BTreeMap` behind the trait: immediate writes, no charge.
    #[derive(Default)]
    struct MapStore {
        records: RefCell<BTreeMap<Vec<u8>, Vec<u8>>>,
        fail_delete: bool,
    }

    struct MapCursor {
        records: Vec<(Vec<u8>, Vec<u8>)>,
        position: Option<usize>,
    }

    impl MapCursor {
        fn current(&self) -> Option<&(Vec<u8>, Vec<u8>)> {
            self.position.and_then(|i| self.records.get(i))
        }
    }

    impl RawIterator for MapCursor {
        fn seek_to_first(&mut self) -> CostContext<()> {
            self.position = Some(0);
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek_to_last(&mut self) -> CostContext<()> {
            self.position = self.records.len().checked_sub(1);
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
            self.position = Some(
                self.records
                    .partition_point(|(k, _)| k.as_slice() < key.as_ref()),
            );
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
            self.position = self
                .records
                .partition_point(|(k, _)| k.as_slice() <= key.as_ref())
                .checked_sub(1);
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn next(&mut self) -> CostContext<()> {
            self.position = self.position.map(|i| i + 1);
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn prev(&mut self) -> CostContext<()> {
            self.position = self.position.and_then(|i| i.checked_sub(1));
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn value(&self) -> CostContext<Option<&[u8]>> {
            self.current()
                .map(|(_, v)| v.as_slice())
                .wrap_with_cost(OperationCost::default())
        }

        fn key(&self) -> CostContext<Option<&[u8]>> {
            self.current()
                .map(|(k, _)| k.as_slice())
                .wrap_with_cost(OperationCost::default())
        }

        fn valid(&self) -> CostContext<bool> {
            self.current()
                .is_some()
                .wrap_with_cost(OperationCost::default())
        }
    }

    impl FlatStore for MapStore {
        type Cursor<'a>
            = MapCursor
        where
            Self: 'a;

        fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
            Ok(self.records.borrow().get(key.as_ref()).cloned())
                .wrap_with_cost(OperationCost::default())
        }

        fn put<K: AsRef<[u8]>>(
            &self,
            key: K,
            value: &[u8],
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), Error> {
            self.records
                .borrow_mut()
                .insert(key.as_ref().to_vec(), value.to_vec());
            Ok(()).wrap_with_cost(OperationCost::default())
        }

        fn delete<K: AsRef<[u8]>>(
            &self,
            key: K,
            _cost_info: Option<KeyValueStorageCost>,
        ) -> CostResult<(), Error> {
            if self.fail_delete {
                return Err(Error::StorageError("injected delete failure".to_string()))
                    .wrap_with_cost(OperationCost::with_seek_count(7));
            }
            self.records.borrow_mut().remove(key.as_ref());
            Ok(()).wrap_with_cost(OperationCost::with_seek_count(2))
        }

        fn cursor(&self) -> Self::Cursor<'_> {
            MapCursor {
                records: self
                    .records
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                position: None,
            }
        }
    }

    fn record(primary: u8, path: Vec<Vec<u8>>) -> PendingPrefixDropRecord {
        PendingPrefixDropRecord {
            primary_prefix: [primary; 32],
            path,
            doomed_prefixes: vec![[primary; 32], [primary.wrapping_add(1); 32]],
        }
    }

    #[test]
    fn cursor_walks_the_keyspace_in_key_order_both_ways() {
        let store = MapStore::default();
        for key in [b"b", b"d", b"a", b"c"] {
            store.put(key, key, None).unwrap().unwrap();
        }

        let mut cursor = store.cursor();
        assert!(!cursor.valid().unwrap());
        cursor.seek_to_first().unwrap();
        let mut forward = Vec::new();
        while cursor.valid().unwrap() {
            forward.push(cursor.key().unwrap().unwrap().to_vec());
            assert_eq!(cursor.value().unwrap(), cursor.key().unwrap());
            cursor.next().unwrap();
        }
        assert_eq!(forward, [b"a", b"b", b"c", b"d"]);

        cursor.seek_to_last().unwrap();
        let mut backward = Vec::new();
        while cursor.valid().unwrap() {
            backward.push(cursor.key().unwrap().unwrap().to_vec());
            cursor.prev().unwrap();
        }
        assert_eq!(backward, [b"d", b"c", b"b", b"a"]);

        cursor.seek(b"bb").unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"c".as_slice()));
        cursor.seek_for_prev(b"bb").unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"b".as_slice()));
        cursor.seek_for_prev(b"").unwrap();
        assert!(!cursor.valid().unwrap());
        assert!(cursor.status().is_ok());
    }

    #[test]
    fn clear_removes_every_record_and_charges_the_walk() {
        let store = MapStore::default();
        for key in [b"a", b"b", b"c"] {
            store.put(key, b"value", None).unwrap().unwrap();
        }

        let cleared = store.clear();
        cleared.value.expect("clear succeeds");
        assert!(store.records.borrow().is_empty());
        // One seek to the first record, then per record one delete (2 seeks
        // in this store) and one step.
        assert_eq!(
            cleared.cost,
            OperationCost::with_seek_count(1 + 3 * (2 + 1))
        );

        let cleared_again = store.clear();
        cleared_again
            .value
            .expect("clearing an empty keyspace succeeds");
        assert_eq!(cleared_again.cost, OperationCost::with_seek_count(1));
    }

    #[test]
    fn clear_stops_at_the_first_failed_delete_and_keeps_its_cost() {
        let store = MapStore {
            fail_delete: true,
            ..Default::default()
        };
        store.put(b"a", b"value", None).unwrap().unwrap();
        store.put(b"b", b"value", None).unwrap().unwrap();

        let cleared = store.clear();
        assert!(matches!(cleared.value, Err(Error::StorageError(_))));
        assert_eq!(cleared.cost, OperationCost::with_seek_count(1 + 7));
        assert_eq!(store.records.borrow().len(), 2);
    }

    #[test]
    fn delete_of_an_absent_key_succeeds() {
        let store = MapStore::default();
        store.delete(b"missing", None).unwrap().expect("no record");
        assert_eq!(store.get(b"missing").unwrap().unwrap(), None);
    }

    #[test]
    fn a_staged_drop_record_is_keyed_by_its_primary_prefix() {
        let meta = MapStore::default();
        let record = record(7, vec![b"documents".to_vec(), b"expired".to_vec()]);

        let staged = stage_pending_prefix_drop(&meta, &record);
        staged.value.expect("record is staged");
        assert_eq!(staged.cost, OperationCost::default());

        let stored = meta.records.borrow();
        assert_eq!(stored.len(), 1);
        let (key, value) = stored.iter().next().expect("one record");
        assert_eq!(key.as_slice(), [7u8; 32]);
        assert_eq!(
            PendingPrefixDropRecord::decode(key, value).expect("decodes"),
            record
        );
    }

    #[test]
    fn a_record_that_cannot_be_encoded_is_not_staged() {
        let meta = MapStore::default();
        let record = record(7, vec![vec![0u8; 256]]);

        let staged = stage_pending_prefix_drop(&meta, &record);
        assert!(matches!(
            staged.value,
            Err(Error::StorageError(message)) if message.contains("path segment over 255 bytes")
        ));
        assert_eq!(staged.cost, OperationCost::default());
        assert!(meta.records.borrow().is_empty());
    }

    #[test]
    fn encode_rejects_counts_that_do_not_fit_a_byte() {
        let mut too_many_prefixes = record(1, vec![]);
        too_many_prefixes.doomed_prefixes = vec![[0u8; 32]; 256];
        assert!(matches!(
            too_many_prefixes.encode(),
            Err(Error::StorageError(message)) if message.contains("too many doomed prefixes")
        ));

        let too_deep = record(1, vec![vec![1u8]; 256]);
        assert!(matches!(
            too_deep.encode(),
            Err(Error::StorageError(message)) if message.contains("path too deep")
        ));
    }

    #[test]
    fn decode_rejects_every_malformed_record() {
        let record = record(3, vec![b"a".to_vec(), b"bc".to_vec()]);
        let value = record.encode().expect("encodes");
        let key = record.primary_prefix;
        assert_eq!(
            PendingPrefixDropRecord::decode(&key, &value).expect("decodes"),
            record
        );

        let decode_error =
            |key: &[u8], value: &[u8]| match PendingPrefixDropRecord::decode(key, value) {
                Err(Error::StorageError(message)) => message,
                other => panic!("expected a storage error, got {other:?}"),
            };

        assert!(decode_error(&key[..31], &value).contains("key suffix is not 32 bytes"));
        assert!(decode_error(&key, &[]).contains("missing version byte"));
        assert!(decode_error(&key, &[1]).contains("unknown encoding version"));
        assert!(decode_error(&key, &[0]).contains("missing doomed count"));
        assert!(decode_error(&key, &[0, 1, 9]).contains("truncated doomed prefix"));
        assert!(decode_error(&key, &[0, 0]).contains("missing segment count"));
        assert!(decode_error(&key, &[0, 0, 1]).contains("missing segment length"));
        assert!(decode_error(&key, &[0, 0, 1, 2, 9]).contains("truncated path segment"));
        let mut trailing = value.clone();
        trailing.push(0);
        assert!(decode_error(&key, &trailing).contains("trailing bytes"));
    }

    #[cfg(feature = "rocksdb_storage")]
    #[test]
    fn the_namespace_is_the_hash_of_its_label() {
        assert_eq!(
            pending_prefix_drops_namespace(),
            blake3::hash(b"grovedb_pending_prefix_drops_v0").as_bytes()
        );
    }
}
