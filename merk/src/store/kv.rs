//! [`NodeStore`] over a storage context of `grovedb-storage`.
//!
//! Every storage context is a `NodeStore`: a node is one record under its
//! key, encoded and decoded whole, and a change set is the context's batch.
//! The kind of a node change is not used: every written node is put as a
//! whole record.

use ed::Decode;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt, OperationCost,
};
use grovedb_storage::{Batch, StorageContext};
use grovedb_version::version::GroveVersion;

use super::{NodeChange, NodeChangeSet, NodeStore, NodeWriteCost};
use crate::{
    merk::defaults::ROOT_KEY_KEY,
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Error,
    Error::{CostsError, EdError, StorageError},
};

/// The change set of a storage context: its batch.
pub struct KvChangeSet<B>(B);

impl<B: Batch> NodeChangeSet for KvChangeSet<B> {
    type Prepared = Vec<u8>;

    fn prepare_node(&mut self, node: &TreeNode, _change: NodeChange) -> Vec<u8> {
        let mut record = Vec::with_capacity(node.encoding_length());
        node.encode_into(&mut record);
        record
    }

    fn put_node(
        &mut self,
        key: &[u8],
        node: Vec<u8>,
        cost: Option<NodeWriteCost>,
    ) -> Result<(), Error> {
        let (children_sizes, cost_info) = match cost {
            Some(cost) => (
                Some((cost.feature, cost.left_child, cost.right_child)),
                Some(cost.storage_cost),
            ),
            None => (None, None),
        };
        self.0
            .put(key, &node, children_sizes, cost_info)
            .map_err(CostsError)
    }

    fn delete_node(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>) {
        self.0.delete(key, cost)
    }

    fn put_root_key(
        &mut self,
        root_key: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.0
            .put_root(ROOT_KEY_KEY, root_key, cost)
            .map_err(CostsError)
    }

    fn delete_root_key(&mut self, cost: Option<KeyValueStorageCost>) {
        self.0.delete_root(ROOT_KEY_KEY, cost)
    }

    fn put_aux_record(
        &mut self,
        key: &[u8],
        value: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.0.put_aux(key, value, cost).map_err(CostsError)
    }

    fn delete_aux_record(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>) {
        self.0.delete_aux(key, cost)
    }
}

impl<'db, S> NodeStore<'db> for S
where
    S: StorageContext<'db>,
{
    type ChangeSet = KvChangeSet<S::Batch>;
    type Cursor = S::RawIterator;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        let mut cost = OperationCost::default();
        let record = cost_return_on_error!(&mut cost, self.get(key).map_err(StorageError));

        let node = cost_return_on_error_no_add!(
            cost,
            record
                .map(|record| TreeNode::decode_raw(
                    &record,
                    key.to_vec(),
                    value_defined_cost_fn,
                    grove_version
                ))
                .transpose()
        );

        Ok(node).wrap_with_cost(cost)
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        let mut cost = OperationCost::default();
        let record = cost_return_on_error!(&mut cost, self.get(key).map_err(StorageError));
        record
            .map(|record| TreeNodeInner::decode(record.as_slice()).map_err(EdError))
            .transpose()
            .wrap_with_cost(cost)
    }

    fn write_node(&self, node: &TreeNode, _change: NodeChange) -> CostResult<(), Error> {
        self.put(node.key(), &node.encode(), None, None)
            .map_err(StorageError)
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_root(ROOT_KEY_KEY).map_err(StorageError)
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        match root_key {
            Some(root_key) => self.put_root(ROOT_KEY_KEY, root_key, None),
            None => self.delete_root(ROOT_KEY_KEY, None),
        }
        .map_err(StorageError)
    }

    fn cursor(&self) -> S::RawIterator {
        self.raw_iter()
    }

    fn change_set(&self) -> KvChangeSet<S::Batch> {
        KvChangeSet(self.new_batch())
    }

    fn commit(&self, change_set: KvChangeSet<S::Batch>) -> CostResult<(), Error> {
        self.commit_batch(change_set.0).map_err(StorageError)
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_aux(key).map_err(StorageError)
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_meta(key).map_err(StorageError)
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error> {
        self.put_meta(key, value, None).map_err(StorageError)
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error> {
        self.delete_meta(key, None).map_err(StorageError)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use grovedb_costs::{
        storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost},
        CostResult, OperationCost, TreeCostType,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{
            test_utils::TempStorage, PrefixedRocksDbImmediateStorageContext,
            PrefixedRocksDbTransactionContext,
        },
        Storage, StorageBatch, StorageContext,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        merk::{chunks::ChunkProducer, restore::Restorer},
        store::{
            recording::{call, failed_call, Call, Recording, Tape},
            stored_record, NodeChange, NodeChangeSet, NodeCursor, NodeStore, NodeWriteCost,
        },
        tree::{kv::ValueDefinedCostType, AggregateData, TreeNode},
        Error, Link, Merk, Op, TreeFeatureType,
        TreeFeatureType::BasicMerkNode,
        TreeType, ROOT_KEY_KEY,
    };

    const NO_VALUE_DEFINED_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> =
        None;

    fn assert_send<T: Send>() {}

    fn one_seek(storage_loaded_bytes: u64) -> OperationCost {
        OperationCost {
            seek_count: 1,
            storage_loaded_bytes,
            ..Default::default()
        }
    }

    #[allow(dead_code)]
    fn the_adapter_is_send_when_the_storage_context_is<'db, S>()
    where
        S: StorageContext<'db> + Send,
        S::Batch: Send,
        S::RawIterator: Send,
    {
        assert_send::<Merk<S>>();
        assert_send::<<S as NodeStore<'db>>::ChangeSet>();
        assert_send::<<S as NodeStore<'db>>::Cursor>();
    }

    #[test]
    fn the_change_sets_and_cursors_of_the_rocksdb_contexts_are_send() {
        type Transactional = PrefixedRocksDbTransactionContext<'static>;
        type Immediate = PrefixedRocksDbImmediateStorageContext<'static>;

        assert_send::<<Transactional as NodeStore<'static>>::ChangeSet>();
        assert_send::<<Transactional as NodeStore<'static>>::Cursor>();
        assert_send::<<Immediate as NodeStore<'static>>::ChangeSet>();
        assert_send::<<Immediate as NodeStore<'static>>::Cursor>();
    }

    fn charge(added_bytes: u32) -> KeyValueStorageCost {
        KeyValueStorageCost {
            value_storage_cost: StorageCost {
                added_bytes,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn every_change_is_one_batch_call_under_its_key_whatever_its_kind() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        let tape = Tape::default();
        let store = tape.recording(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), &tx)
                .unwrap(),
        );
        let written = node(b"n", b"v");
        let record = written.encode();
        let cost = || NodeWriteCost {
            feature: Some((TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes, 3)),
            left_child: Some((1, 2)),
            right_child: None,
            storage_cost: charge(40),
        };
        let mut change_set = store.change_set();

        change_set.put_root_key(b"n", Some(charge(5))).unwrap();
        for (key, kind) in [
            (b"a", NodeChange::HashOnly),
            (b"b", NodeChange::StructureChanged),
            (b"c", NodeChange::ValueWritten),
        ] {
            let prepared = change_set.prepare_node(&written, kind);
            change_set.put_node(key, prepared, Some(cost())).unwrap();
        }
        let prepared = change_set.prepare_node(&written, NodeChange::HashOnly);
        change_set.put_node(b"d", prepared, None).unwrap();
        change_set.delete_node(b"e", Some(charge(1)));
        change_set.delete_node(b"f", None);
        change_set
            .put_aux_record(b"x", b"1", Some(charge(2)))
            .unwrap();
        change_set.delete_aux_record(b"y", Some(charge(4)));
        change_set.delete_root_key(Some(charge(3)));

        let charged_put = |key: &[u8]| Call {
            sizes: Some((Some(("varint as 8", 3)), Some((1, 2)), None)),
            cost: Some(charge(40)),
            ..call("batch.put", key, &record)
        };
        let charged = |cost: u32, call: Call| Call {
            cost: Some(charge(cost)),
            ..call
        };
        assert_eq!(
            tape.take(),
            [
                call("new_batch", b"", b""),
                charged(5, call("batch.put_root", ROOT_KEY_KEY, b"n")),
                charged_put(b"a"),
                charged_put(b"b"),
                charged_put(b"c"),
                call("batch.put", b"d", &record),
                charged(1, call("batch.delete", b"e", b"")),
                call("batch.delete", b"f", b""),
                charged(2, call("batch.put_aux", b"x", b"1")),
                charged(4, call("batch.delete_aux", b"y", b"")),
                charged(3, call("batch.delete_root", ROOT_KEY_KEY, b"")),
            ]
        );
    }

    fn link(key: &[u8]) -> Link {
        Link::Reference {
            hash: [0; 32],
            child_heights: (0, 0),
            key: key.to_vec(),
            aggregate_data: AggregateData::NoAggregateData,
        }
    }

    /// The result of an operation that made exactly `calls` and cost what
    /// those calls returned.
    fn made<T>(tape: &Tape, operation: CostResult<T, Error>, calls: &[Call]) -> Result<T, Error> {
        assert_eq!(tape.take(), calls);
        assert_eq!(operation.cost, tape.take_cost());
        operation.value
    }

    fn each_operation_is_one_storage_call<'db, S: StorageContext<'db>>(
        tape: &Tape,
        store: &Recording<S>,
    ) {
        let grove_version = GroveVersion::latest();
        let record = node(b"key", b"value").encode();
        let other = node(b"other", b"value");
        let get = |key: &[u8]| [call("get", key, b"")];
        let cost_of_value = |value: &[u8], _: &GroveVersion| {
            Some(ValueDefinedCostType::SpecializedValueDefinedCost(
                value.len() as u32,
            ))
        };
        let cost_of_five_bytes = Some(ValueDefinedCostType::SpecializedValueDefinedCost(5));

        let found = store.get_node(b"key", NO_VALUE_DEFINED_COST, grove_version);
        let found = made(tape, found, &get(b"key")).unwrap().unwrap();
        assert_eq!(found.encode(), record);
        assert_eq!(found.key(), b"key");
        assert_eq!(found.inner.kv.value_defined_cost, None);
        let found = store.get_node(b"key", Some(cost_of_value), grove_version);
        let found = made(tape, found, &get(b"key")).unwrap().unwrap();
        assert_eq!(found.inner.kv.value_defined_cost, cost_of_five_bytes);
        let absent = store.get_node(b"none", NO_VALUE_DEFINED_COST, grove_version);
        assert!(made(tape, absent, &get(b"none")).unwrap().is_none());

        let read = made(tape, store.get_node_for_read(b"key"), &get(b"key"));
        assert_eq!(read.unwrap().unwrap().value_as_slice(), b"value");
        let absent = made(tape, store.get_node_for_read(b"none"), &get(b"none"));
        assert!(absent.unwrap().is_none());

        let fetched = store.fetch_node(&link(b"key"), Some(&cost_of_value), grove_version);
        let fetched = made(tape, fetched, &get(b"key")).unwrap();
        assert_eq!(fetched.encode(), record);
        assert_eq!(fetched.inner.kv.value_defined_cost, cost_of_five_bytes);
        let missing = store.fetch_node(
            &link(b"none"),
            NO_VALUE_DEFINED_COST.as_ref(),
            grove_version,
        );
        assert!(matches!(
            made(tape, missing, &get(b"none")),
            Err(Error::KeyNotFoundError("Key not found for fetch"))
        ));

        let write = store.write_node(&other, NodeChange::StructureChanged);
        made(tape, write, &[call("put", b"other", &other.encode())]).unwrap();

        let root = made(
            tape,
            store.root_key(),
            &[call("get_root", ROOT_KEY_KEY, b"")],
        );
        assert_eq!(root.unwrap(), Some(b"key".to_vec()));
        made(
            tape,
            store.set_root_key(Some(b"other")),
            &[call("put_root", ROOT_KEY_KEY, b"other")],
        )
        .unwrap();
        made(
            tape,
            store.set_root_key(None),
            &[call("delete_root", ROOT_KEY_KEY, b"")],
        )
        .unwrap();

        let aux = made(
            tape,
            store.get_aux_record(b"aux"),
            &[call("get_aux", b"aux", b"")],
        );
        assert_eq!(aux.unwrap(), Some(b"aaa".to_vec()));
        let meta = made(
            tape,
            store.get_meta_record(b"meta"),
            &[call("get_meta", b"meta", b"")],
        );
        assert_eq!(meta.unwrap(), Some(b"mm".to_vec()));
        made(
            tape,
            store.put_meta_record(b"meta", b"m"),
            &[call("put_meta", b"meta", b"m")],
        )
        .unwrap();
        made(
            tape,
            store.delete_meta_record(b"meta"),
            &[call("delete_meta", b"meta", b"")],
        )
        .unwrap();

        let cursor = store.cursor();
        assert_eq!(tape.take(), [call("raw_iter", b"", b"")]);
        drop(cursor);
        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(&other, NodeChange::ValueWritten);
        change_set.put_node(b"batched", prepared, None).unwrap();
        assert_eq!(
            tape.take(),
            [
                call("new_batch", b"", b""),
                call("batch.put", b"batched", &other.encode())
            ]
        );
        made(
            tape,
            store.commit(change_set),
            &[call("commit_batch", b"", b"")],
        )
        .unwrap();
    }

    #[test]
    fn every_operation_is_one_storage_call_and_costs_what_that_call_costs() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let stored = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();
        stored
            .write_node(&node(b"key", b"value"), NodeChange::ValueWritten)
            .unwrap()
            .unwrap();
        stored.set_root_key(Some(b"key")).unwrap().unwrap();
        stored.put_meta_record(b"meta", b"mm").unwrap().unwrap();
        let mut change_set = stored.change_set();
        change_set.put_aux_record(b"aux", b"aaa", None).unwrap();
        stored.commit(change_set).unwrap().unwrap();

        let batch = StorageBatch::new();
        let tape = Tape::default();
        each_operation_is_one_storage_call(
            &tape,
            &tape.recording(
                storage
                    .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), &tx)
                    .unwrap(),
            ),
        );
        each_operation_is_one_storage_call(&tape, &tape.recording(stored));
    }

    #[test]
    fn a_storage_call_that_fails_is_a_storage_error_with_the_cost_of_that_call() {
        fn failing<T>(
            tape: &Tape,
            name: &'static str,
            operation: impl FnOnce() -> CostResult<T, Error>,
        ) {
            tape.take();
            tape.fail(Some(name));
            let failed = operation();
            tape.fail(None);
            assert_eq!(failed.cost, failed_call(), "{name}");
            assert!(
                matches!(
                    failed.value,
                    Err(Error::StorageError(grovedb_storage::Error::StorageError(text)))
                        if text == format!("{name} failed")
                ),
                "{name}"
            );
            let calls = tape.take();
            assert_eq!(
                calls.len(),
                1,
                "{name}: no call follows the one that failed"
            );
            assert_eq!(calls[0].name, name);
        }

        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        let tape = Tape::default();
        let store = tape.recording(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), &tx)
                .unwrap(),
        );
        let written = node(b"key", b"value");
        let no_cost = NO_VALUE_DEFINED_COST;

        failing(&tape, "get", || {
            store.get_node(b"key", no_cost, grove_version)
        });
        failing(&tape, "get", || store.get_node_for_read(b"key"));
        failing(&tape, "get", || {
            store.fetch_node(&link(b"key"), no_cost.as_ref(), grove_version)
        });
        failing(&tape, "put", || {
            store.write_node(&written, NodeChange::ValueWritten)
        });
        failing(&tape, "get_root", || store.root_key());
        failing(&tape, "put_root", || store.set_root_key(Some(b"key")));
        failing(&tape, "delete_root", || store.set_root_key(None));
        failing(&tape, "get_aux", || store.get_aux_record(b"aux"));
        failing(&tape, "get_meta", || store.get_meta_record(b"meta"));
        failing(&tape, "put_meta", || store.put_meta_record(b"meta", b"m"));
        failing(&tape, "delete_meta", || store.delete_meta_record(b"meta"));

        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(&written, NodeChange::ValueWritten);
        change_set.put_node(b"key", prepared, None).unwrap();
        failing(&tape, "commit_batch", || store.commit(change_set));
    }

    #[test]
    fn a_put_the_batch_refuses_is_a_costs_error() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let store = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();
        let written = node(b"key", b"value");
        let unverifiable = || KeyValueStorageCost {
            needs_value_verification: true,
            ..Default::default()
        };
        fn refused(put: Result<(), Error>) {
            assert!(matches!(
                put,
                Err(Error::CostsError(
                    grovedb_costs::error::Error::StorageCostMismatch { .. }
                ))
            ));
        }

        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(&written, NodeChange::ValueWritten);
        refused(change_set.put_node(
            b"key",
            prepared,
            Some(NodeWriteCost {
                feature: None,
                left_child: None,
                right_child: None,
                storage_cost: unverifiable(),
            }),
        ));
        refused(change_set.put_root_key(b"key", Some(unverifiable())));
        refused(change_set.put_aux_record(b"aux", b"a", Some(unverifiable())));

        store.commit(change_set).unwrap().unwrap();
        assert!(stored_record(&store, b"key").is_none());
        assert_eq!(store.root_key().unwrap().unwrap(), None);
        assert_eq!(store.get_aux_record(b"aux").unwrap().unwrap(), None);
    }

    #[test]
    fn a_merk_makes_its_storage_calls_in_the_order_it_always_made_them() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let tape = Tape::default();
        let open = || {
            Merk::open_base(
                tape.recording(
                    storage
                        .get_immediate_storage_context(SubtreePath::empty(), &tx)
                        .unwrap(),
                ),
                TreeType::NormalTree,
                NO_VALUE_DEFINED_COST,
                grove_version,
            )
            .unwrap()
            .expect("the store opens")
        };
        let put = |key: &[u8], value: &[u8]| (key.to_vec(), Op::Put(value.to_vec(), BasicMerkNode));
        let aux_put = (b"x".to_vec(), Op::Put(b"1".to_vec(), BasicMerkNode), None);

        let mut merk = open();
        assert_eq!(tape.take_brief(), ["get_root r"]);
        merk.apply::<_, Vec<u8>>(
            &[b"a", b"b", b"c", b"d"].map(|key| put(key, b"1")),
            &[aux_put],
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            tape.take_brief(),
            [
                "new_batch",
                "batch.put_root r c",
                "batch.put a charged",
                "batch.put b charged",
                "batch.put c charged",
                "batch.put d charged",
                "batch.put_aux x 1",
                "commit_batch",
            ]
        );

        let mut merk = open();
        assert_eq!(tape.take_brief(), ["get_root r", "get c"]);
        merk.get(b"b", false, NO_VALUE_DEFINED_COST, grove_version)
            .unwrap()
            .unwrap();
        merk.get(b"a", true, NO_VALUE_DEFINED_COST, grove_version)
            .unwrap()
            .unwrap();
        assert_eq!(tape.take_brief(), ["get b", "get a"]);
        merk.apply::<_, Vec<u8>>(
            &[put(b"a", b"2"), (b"d".to_vec(), Op::Delete)],
            &[(b"x".to_vec(), Op::Delete, None)],
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            tape.take_brief(),
            [
                "get b",
                "get a",
                "get d",
                "new_batch",
                "batch.put_root r b",
                "batch.put a charged",
                "batch.put b charged",
                "batch.put c charged",
                "batch.delete d charged",
                "batch.delete_aux x",
                "commit_batch",
            ]
        );
        merk.apply::<Vec<u8>, Vec<u8>>(&[], &[], None, grove_version)
            .unwrap()
            .unwrap();
        assert_eq!(
            tape.take_brief(),
            ["new_batch", "batch.put b charged", "commit_batch"]
        );

        merk.put_meta(b"m".to_vec(), b"1".to_vec())
            .unwrap()
            .unwrap();
        merk.get_meta(b"m".to_vec()).unwrap().unwrap();
        merk.get_meta(b"n".to_vec()).unwrap().unwrap();
        merk.delete_meta(b"m").unwrap().unwrap();
        merk.get_aux(b"x").unwrap().unwrap();
        assert_eq!(
            tape.take_brief(),
            ["put_meta m 1", "get_meta n", "delete_meta m", "get_aux x"]
        );
        merk.set_base_root_key(None).unwrap().unwrap();
        merk.set_base_root_key(Some(b"b".to_vec()))
            .unwrap()
            .unwrap();
        assert_eq!(tape.take_brief(), ["delete_root r", "put_root r b"]);

        assert!(!merk.is_empty_tree().unwrap());
        assert_eq!(
            tape.take_brief(),
            ["raw_iter", "cursor.seek_to_first", "cursor.valid"]
        );
        merk.clear().unwrap().unwrap();
        let mut cleared = vec!["raw_iter", "cursor.seek_to_first", "new_batch"];
        for delete in ["batch.delete a", "batch.delete b", "batch.delete c"] {
            cleared.extend(["cursor.valid", "cursor.key", delete, "cursor.next"]);
        }
        cleared.extend(["cursor.valid", "commit_batch"]);
        assert_eq!(tape.take_brief(), cleared);

        let mut merk = open();
        assert_eq!(tape.take_brief(), ["get_root r", "get b"]);
        merk.apply::<Vec<u8>, Vec<u8>>(&[], &[], None, grove_version)
            .unwrap()
            .unwrap();
        assert_eq!(
            tape.take_brief(),
            ["new_batch", "batch.delete_root r charged", "commit_batch"]
        );

        let open_layered = |root_key: Option<Vec<u8>>| {
            Merk::open_layered_with_root_key(
                tape.recording(
                    storage
                        .get_immediate_storage_context(SubtreePath::empty(), &tx)
                        .unwrap(),
                ),
                root_key,
                TreeType::NormalTree,
                NO_VALUE_DEFINED_COST,
                grove_version,
            )
            .unwrap()
            .expect("the store opens")
        };
        let mut layered = open_layered(None);
        assert_eq!(tape.take_brief(), [] as [&str; 0]);
        layered
            .apply::<_, Vec<u8>>(
                &[b"e", b"f", b"g"].map(|key| put(key, b"1")),
                &[],
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            tape.take_brief(),
            [
                "new_batch",
                "batch.put e charged",
                "batch.put f charged",
                "batch.put g charged",
                "commit_batch",
            ]
        );
        let layered = open_layered(layered.root_key());
        assert_eq!(tape.take_brief(), ["get f"]);

        let mut walk = vec!["raw_iter", "cursor.seek_to_first", "cursor.key"];
        walk.extend(["cursor.next", "cursor.key"]);
        assert!(!layered
            .is_empty_tree_except(BTreeSet::from([b"e".as_slice()]))
            .unwrap());
        assert_eq!(tape.take_brief(), walk);
        walk.extend(["cursor.next", "cursor.key"].repeat(2));
        assert!(layered
            .is_empty_tree_except(BTreeSet::from([b"e".as_slice(), b"f", b"g"]))
            .unwrap());
        assert_eq!(tape.take_brief(), walk);
    }

    #[test]
    fn a_chunk_restore_writes_uncharged_and_corrects_a_parent_with_a_direct_put() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let open = |path: &[u8]| {
            storage
                .get_immediate_storage_context([path].as_ref().into(), &tx)
                .unwrap()
        };
        let keys: Vec<Vec<u8>> = (0..40u8)
            .map(|n| vec![b'a' + n / 26, b'a' + n % 26])
            .collect();
        let mut source = Merk::open_base(
            open(b"source"),
            TreeType::NormalTree,
            NO_VALUE_DEFINED_COST,
            grove_version,
        )
        .unwrap()
        .unwrap();
        let batch: Vec<_> = keys
            .iter()
            .map(|key| (key.clone(), Op::Put(vec![123, key[1]], BasicMerkNode)))
            .collect();
        source
            .apply::<_, Vec<u8>>(&batch, &[], None, grove_version)
            .unwrap()
            .unwrap();

        let tape = Tape::default();
        let target = Merk::open_base(
            tape.recording(open(b"target")),
            TreeType::NormalTree,
            NO_VALUE_DEFINED_COST,
            grove_version,
        )
        .unwrap()
        .unwrap();
        assert_eq!(tape.take_brief(), ["get_root r"]);

        let mut producer = ChunkProducer::new(&source).expect("the source produces chunks");
        let mut restorer = Restorer::new(target, source.root_hash().unwrap(), None);
        let mut restore = |chunk_id: &[u8]| {
            let (chunk, _) = producer.chunk(chunk_id, grove_version).unwrap();
            restorer
                .process_chunk(chunk_id, chunk, grove_version)
                .expect("the chunk restores")
        };

        let below = restore(&[]);
        assert_eq!(
            tape.take_brief(),
            [
                "new_batch",
                "batch.put af",
                "batch.put ak",
                "batch.put ap",
                "batch.put au",
                "batch.put az",
                "batch.put be",
                "batch.put bj",
                "commit_batch",
                "put_root r au",
            ]
        );
        restore(&[1, 1, 1]);
        assert_eq!(
            tape.take_brief(),
            [
                "new_batch",
                "batch.put aa",
                "batch.put ab",
                "batch.put ac",
                "batch.put ad",
                "batch.put ae",
                "commit_batch",
                "get af",
                "put af",
            ]
        );
        for chunk_id in below.iter().filter(|chunk_id| *chunk_id != &[1, 1, 1]) {
            restore(chunk_id);
        }
        tape.take();

        let restored = restorer.finalize(grove_version).expect("restore finalizes");
        assert_eq!(restored.root_hash().unwrap(), source.root_hash().unwrap());
        let finalize = tape.take();
        let written = |name: &str| -> Vec<Vec<u8>> {
            finalize
                .iter()
                .filter(|call| call.name == name)
                .map(|call| call.key.clone())
                .collect()
        };
        let mut rewritten = written("batch.put");
        rewritten.sort();
        assert_eq!(
            rewritten, keys,
            "the heights of every node are written again"
        );
        assert_eq!(written("new_batch").len(), 1);
        assert_eq!(written("commit_batch").len(), 1);
        assert!(written("put").is_empty());
        assert!(finalize.iter().all(|call| call.cost.is_none()));
    }

    #[test]
    fn what_a_storage_batch_holds_is_read_once_the_batch_is_applied() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        let in_a_change_set = node(b"key", b"value");
        let direct = node(b"direct", b"value");
        let holds = |store: &PrefixedRocksDbTransactionContext| {
            let node = |key: &[u8]| {
                store
                    .get_node(key, NO_VALUE_DEFINED_COST, grove_version)
                    .unwrap()
                    .unwrap()
                    .is_some()
            };
            let mut cursor = store.cursor();
            cursor.seek_to_first().unwrap();
            (
                node(b"key"),
                node(b"direct"),
                cursor.valid().unwrap(),
                store.root_key().unwrap().unwrap(),
                store.get_aux_record(b"aux").unwrap().unwrap(),
                store.get_meta_record(b"meta").unwrap().unwrap(),
            )
        };

        let store = storage
            .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), &tx)
            .unwrap();
        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(&in_a_change_set, NodeChange::ValueWritten);
        change_set.put_node(b"key", prepared, None).unwrap();
        change_set.put_aux_record(b"aux", b"a", None).unwrap();
        store.commit(change_set).unwrap().unwrap();
        store
            .write_node(&direct, NodeChange::ValueWritten)
            .unwrap()
            .unwrap();
        store.set_root_key(Some(b"key")).unwrap().unwrap();
        store.put_meta_record(b"meta", b"m").unwrap().unwrap();
        assert_eq!(holds(&store), (false, false, false, None, None, None));
        assert_eq!(batch.len(), 5);

        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .unwrap();
        let store = storage
            .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
            .unwrap();
        assert_eq!(
            holds(&store),
            (
                true,
                true,
                true,
                Some(b"key".to_vec()),
                Some(b"a".to_vec()),
                Some(b"m".to_vec())
            )
        );
    }

    #[test]
    fn a_read_is_answered_by_the_transaction_when_it_is_made() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let ours = storage.start_transaction();
        let store = storage
            .get_immediate_storage_context(SubtreePath::empty(), &ours)
            .unwrap();
        let value_of = |key: &[u8]| {
            store
                .fetch_node(&link(key), NO_VALUE_DEFINED_COST.as_ref(), grove_version)
                .unwrap()
                .ok()
                .map(|node| node.value_as_slice().to_vec())
        };
        fn keys_under(mut cursor: impl NodeCursor) -> Vec<Vec<u8>> {
            let mut keys = vec![];
            cursor.seek_to_first().unwrap();
            while let Some(key) = cursor.key().unwrap() {
                keys.push(key.to_vec());
                cursor.next().unwrap();
            }
            keys
        }

        store
            .write_node(&node(b"ours", b"1"), NodeChange::ValueWritten)
            .unwrap()
            .unwrap();
        let opened_before = store.cursor();

        let theirs = storage.start_transaction();
        storage
            .get_immediate_storage_context(SubtreePath::empty(), &theirs)
            .unwrap()
            .write_node(&node(b"theirs", b"2"), NodeChange::ValueWritten)
            .unwrap()
            .unwrap();
        assert_eq!(value_of(b"theirs"), None);
        storage.commit_transaction(theirs).unwrap().unwrap();

        assert_eq!(value_of(b"theirs"), Some(b"2".to_vec()));
        assert_eq!(value_of(b"ours"), Some(b"1".to_vec()));
        assert_eq!(keys_under(opened_before), [b"ours".to_vec()]);
        assert_eq!(
            keys_under(store.cursor()),
            [b"ours".to_vec(), b"theirs".to_vec()]
        );
    }

    #[allow(dead_code)]
    fn the_cursor_and_the_batch_are_the_storage_contexts_own<'db, S: StorageContext<'db>>(
        cursor: <S as NodeStore<'db>>::Cursor,
        change_set: <S as NodeStore<'db>>::ChangeSet,
    ) -> (S::RawIterator, S::Batch) {
        (cursor, change_set.0)
    }

    fn node(key: &[u8], value: &[u8]) -> TreeNode {
        TreeNode::new(
            key.to_vec(),
            value.to_vec(),
            None,
            TreeFeatureType::BasicMerkNode,
        )
        .unwrap()
    }

    #[test]
    fn a_node_is_one_whole_record_under_its_key() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let store = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();
        let written = node(b"key", b"value");

        let mut change_set = store.change_set();
        let record = change_set.prepare_node(&written, NodeChange::HashOnly);
        assert_eq!(record, written.encode());
        change_set.put_node(b"key", record, None).unwrap();
        store.commit(change_set).unwrap().unwrap();

        assert_eq!(
            StorageContext::get(&store, b"key").unwrap().unwrap(),
            Some(written.encode())
        );
        let charged = storage
            .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
            .unwrap();
        let fetched = charged.get_node(b"key", NO_VALUE_DEFINED_COST, grove_version);
        assert_eq!(fetched.cost, one_seek(written.encoding_length() as u64));
        let fetched = fetched.value.unwrap().expect("the node is stored");
        assert_eq!(fetched.key(), b"key");
        assert_eq!(fetched.encode(), written.encode());
        let read = store.get_node_for_read(b"key").unwrap().unwrap().unwrap();
        assert_eq!(read.value_as_slice(), b"value");
        assert_eq!(read.key_as_slice(), b"");

        let mut change_set = store.change_set();
        change_set.delete_node(b"key", None);
        store.commit(change_set).unwrap().unwrap();
        assert!(store
            .get_node(b"key", NO_VALUE_DEFINED_COST, grove_version)
            .unwrap()
            .unwrap()
            .is_none());
        assert!(store.get_node_for_read(b"key").unwrap().unwrap().is_none());
    }

    #[test]
    fn a_single_node_write_is_a_direct_put() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let written = node(b"key", b"value");

        let immediate = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();
        let mut change_set = immediate.change_set();
        let record = change_set.prepare_node(&written, NodeChange::ValueWritten);
        change_set.put_node(b"batched", record, None).unwrap();
        let through_a_batch = immediate.commit(change_set);
        assert_eq!(through_a_batch.cost.seek_count, 1);
        through_a_batch.value.unwrap();

        let cost = immediate.write_node(&written, NodeChange::ValueWritten);
        assert!(cost.cost.is_nothing());
        cost.value.unwrap();
        assert_eq!(
            immediate
                .get_node(b"key", NO_VALUE_DEFINED_COST, grove_version)
                .unwrap()
                .unwrap()
                .expect("the node is stored")
                .encode(),
            written.encode()
        );

        let batch = StorageBatch::new();
        let batched = storage
            .get_transactional_storage_context(
                [b"other".as_slice()].as_ref().into(),
                Some(&batch),
                &tx,
            )
            .unwrap();
        batched
            .write_node(&written, NodeChange::ValueWritten)
            .unwrap()
            .unwrap();
        assert_eq!(batch.len(), 1);

        let unbatched = storage
            .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
            .unwrap();
        assert!(matches!(
            unbatched
                .write_node(&written, NodeChange::ValueWritten)
                .unwrap(),
            Err(Error::StorageError(_))
        ));
    }

    #[test]
    fn an_undecodable_record_is_an_encoding_error() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let store = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();
        StorageContext::put(&store, b"key", &[9, 9, 9], None, None)
            .unwrap()
            .unwrap();
        let store = storage
            .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
            .unwrap();

        let read = store.get_node_for_read(b"key");
        assert_eq!(read.cost, one_seek(3));
        assert!(matches!(read.value, Err(Error::EdError(_))));
        let fetched = store.get_node(b"key", NO_VALUE_DEFINED_COST, grove_version);
        assert_eq!(fetched.cost.seek_count, 1);
        assert!(matches!(fetched.value, Err(Error::EdError(_))));
    }

    #[test]
    fn a_direct_element_read_reports_an_undecodable_record_as_before() {
        use ed::Decode;
        use grovedb_element::Element;
        use grovedb_version::version::v1::GROVE_V1;

        use crate::{element::get::ElementFetchFromStorageExtensions, tree::TreeNodeInner};

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let store = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();
        StorageContext::put(&store, b"key", &[9, 9, 9], None, None)
            .unwrap()
            .unwrap();
        let store = storage
            .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
            .unwrap();
        let decode_error = TreeNodeInner::decode([9, 9, 9].as_slice())
            .expect_err("the record does not decode")
            .to_string();

        for grove_version in [&GROVE_V1, GroveVersion::latest()] {
            let read = Element::get_optional_from_storage(&store, b"key", grove_version);
            assert_eq!(read.cost, one_seek(3));
            assert!(matches!(
                read.value,
                Err(Error::CorruptedData(text)) if text == decode_error
            ));
        }
    }

    #[test]
    fn a_direct_element_read_reports_a_failed_read_as_a_storage_error() {
        use grovedb_element::Element;
        use grovedb_version::version::v1::GROVE_V1;

        use crate::element::get::ElementFetchFromStorageExtensions;

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let tape = Tape::default();
        let store = tape.recording(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
                .unwrap(),
        );

        for grove_version in [&GROVE_V1, GroveVersion::latest()] {
            tape.take();
            tape.fail(Some("get"));
            let optional = Element::get_optional_from_storage(&store, b"key", grove_version);
            let required = Element::get_from_storage(&store, b"key", grove_version);
            tape.fail(None);

            for read in [optional.map_ok(|_| ()), required.map_ok(|_| ())] {
                assert_eq!(read.cost, failed_call());
                assert!(matches!(
                    read.value,
                    Err(Error::StorageError(grovedb_storage::Error::StorageError(text)))
                        if text == "get failed"
                ));
            }
            assert_eq!(
                tape.take(),
                [call("get", b"key", b""), call("get", b"key", b"")]
            );

            let absent = Element::get_optional_from_storage(&store, b"key", grove_version);
            assert!(matches!(absent.value, Ok(None)));
        }
    }

    #[test]
    fn the_root_key_and_the_side_records_are_records_of_the_context() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let store = storage
            .get_immediate_storage_context(SubtreePath::empty(), &tx)
            .unwrap();

        assert_eq!(store.root_key().unwrap().unwrap(), None);
        store.set_root_key(Some(b"root")).unwrap().unwrap();
        assert_eq!(store.root_key().unwrap().unwrap(), Some(b"root".to_vec()));
        assert_eq!(
            store.get_root(crate::ROOT_KEY_KEY).unwrap().unwrap(),
            Some(b"root".to_vec())
        );
        store.set_root_key(None).unwrap().unwrap();
        assert_eq!(store.root_key().unwrap().unwrap(), None);

        let mut change_set = store.change_set();
        change_set.put_root_key(b"next", None).unwrap();
        change_set.put_aux_record(b"aux", b"a", None).unwrap();
        store.commit(change_set).unwrap().unwrap();
        assert_eq!(store.root_key().unwrap().unwrap(), Some(b"next".to_vec()));
        assert_eq!(
            store.get_aux_record(b"aux").unwrap().unwrap(),
            Some(b"a".to_vec())
        );
        let mut change_set = store.change_set();
        change_set.delete_root_key(None);
        change_set.delete_aux_record(b"aux", None);
        store.commit(change_set).unwrap().unwrap();
        assert_eq!(store.root_key().unwrap().unwrap(), None);
        assert_eq!(store.get_aux_record(b"aux").unwrap().unwrap(), None);

        store.put_meta_record(b"meta", b"m").unwrap().unwrap();
        assert_eq!(
            store.get_meta_record(b"meta").unwrap().unwrap(),
            Some(b"m".to_vec())
        );
        store.delete_meta_record(b"meta").unwrap().unwrap();
        assert_eq!(store.get_meta_record(b"meta").unwrap().unwrap(), None);
    }
}
