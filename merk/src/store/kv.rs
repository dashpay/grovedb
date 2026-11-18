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
    use grovedb_costs::{
        storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree,
        OperationCost,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{
            test_utils::TempStorage, PrefixedRocksDbImmediateStorageContext,
            PrefixedRocksDbTransactionContext,
        },
        Batch, Storage, StorageBatch, StorageContext,
    };
    use grovedb_version::version::GroveVersion;

    use super::KvChangeSet;
    use crate::{
        store::{NodeChange, NodeChangeSet, NodeStore, NodeWriteCost},
        tree::{kv::ValueDefinedCostType, TreeNode},
        Error, Merk, TreeFeatureType,
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

    #[derive(Default)]
    struct RecordingBatch {
        calls: Vec<String>,
    }

    impl RecordingBatch {
        fn record(&mut self, call: &str, key: &[u8], value: &[u8], charged: bool) {
            self.calls
                .push(format!("{call} {key:?} {value:?} charged={charged}"));
        }
    }

    impl Batch for RecordingBatch {
        fn put<K: AsRef<[u8]>>(
            &mut self,
            key: K,
            value: &[u8],
            children_sizes: ChildrenSizesWithIsSumTree,
            cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            assert_eq!(children_sizes.is_some(), cost_info.is_some());
            if let Some((feature, left, right)) = children_sizes {
                assert!(feature.is_none());
                assert_eq!((left, right), (Some((1, 2)), None));
            }
            self.record("put", key.as_ref(), value, cost_info.is_some());
            Ok(())
        }

        fn put_aux<K: AsRef<[u8]>>(
            &mut self,
            key: K,
            value: &[u8],
            cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            self.record("put_aux", key.as_ref(), value, cost_info.is_some());
            Ok(())
        }

        fn put_root<K: AsRef<[u8]>>(
            &mut self,
            key: K,
            value: &[u8],
            cost_info: Option<KeyValueStorageCost>,
        ) -> Result<(), grovedb_costs::error::Error> {
            self.record("put_root", key.as_ref(), value, cost_info.is_some());
            Ok(())
        }

        fn delete<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
            self.record("delete", key.as_ref(), &[], cost_info.is_some());
        }

        fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
            self.record("delete_aux", key.as_ref(), &[], cost_info.is_some());
        }

        fn delete_root<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
            self.record("delete_root", key.as_ref(), &[], cost_info.is_some());
        }
    }

    #[test]
    fn every_change_is_one_batch_call_under_its_key_whatever_its_kind() {
        let written = node(b"n", b"v");
        let record = written.encode();
        let cost = || NodeWriteCost {
            feature: None,
            left_child: Some((1, 2)),
            right_child: None,
            storage_cost: KeyValueStorageCost::default(),
        };
        let mut change_set = KvChangeSet(RecordingBatch::default());

        change_set.put_root_key(b"n", None).unwrap();
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
        change_set.delete_node(b"e", Some(KeyValueStorageCost::default()));
        change_set.delete_node(b"f", None);
        change_set
            .put_aux_record(b"x", b"1", Some(KeyValueStorageCost::default()))
            .unwrap();
        change_set.delete_aux_record(b"y", None);
        change_set.delete_root_key(Some(KeyValueStorageCost::default()));

        let root_key = crate::ROOT_KEY_KEY;
        assert_eq!(
            change_set.0.calls,
            [
                format!("put_root {root_key:?} {:?} charged=false", b"n"),
                format!("put {:?} {record:?} charged=true", b"a"),
                format!("put {:?} {record:?} charged=true", b"b"),
                format!("put {:?} {record:?} charged=true", b"c"),
                format!("put {:?} {record:?} charged=false", b"d"),
                format!("delete {:?} [] charged=true", b"e"),
                format!("delete {:?} [] charged=false", b"f"),
                format!("put_aux {:?} {:?} charged=true", b"x", b"1"),
                format!("delete_aux {:?} [] charged=false", b"y"),
                format!("delete_root {root_key:?} [] charged=true"),
            ]
        );
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
