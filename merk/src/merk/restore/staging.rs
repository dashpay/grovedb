//! Where the nodes of a restore are kept until the Merk is whole.
//!
//! The chunks of a Merk arrive from the root down, and a store that keeps a
//! link as the place of a child cannot write a node before the nodes its
//! links name ([`NodeStore::takes_links_to_absent_nodes`]). The restore of
//! a Merk on such a store is made on a [`Staging`]: a node store in memory
//! that keeps one record for a node under its key, as the store over a
//! storage context does. The restore makes on it the calls it makes there,
//! so it leaves in it the nodes it leaves there. When every chunk has
//! arrived, the Merk is handed to its own store, a child before its parent
//! ([`Staging::take_node`]).
//!
//! A staging is of one Merk and of one restore. It charges nothing: a
//! restore tracks no cost.
//!
//! It holds the whole Merk until the hand-over, in memory ([`Records`]).
//! Everything it asks of where the records are is in the five calls of that
//! type.

use std::{
    collections::BTreeMap,
    ops::Bound,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use ed::Decode;
use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
};
use grovedb_version::version::GroveVersion;

use crate::{
    store::{NodeChange, NodeChangeSet, NodeCursor, NodeStore, NodeWriteCost},
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Error,
};

/// The records of the staged nodes, as Merk encodes them, by the node's
/// key: where a staging keeps the Merk until it is handed over.
#[derive(Default)]
struct Records(BTreeMap<Vec<u8>, Vec<u8>>);

impl Records {
    /// Shows the record under `key` to `read`, if there is one.
    fn read<T>(&self, key: &[u8], read: impl FnOnce(&[u8]) -> T) -> Option<T> {
        self.0.get(key).map(|record| read(record))
    }

    /// Puts `record` under `key`, in place of any record that is there.
    fn put(&mut self, key: Vec<u8>, record: Vec<u8>) {
        self.0.insert(key, record);
    }

    /// Takes the record under `key` out.
    fn take(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.remove(key)
    }

    /// How many records there are.
    fn len(&self) -> usize {
        self.0.len()
    }

    /// The first record of `range` in key order, or its last, with its key.
    fn at_end_of(
        &self,
        range: (Bound<&[u8]>, Bound<&[u8]>),
        first: bool,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut records = self.0.range::<[u8], _>(range);
        let record = match first {
            true => records.next(),
            false => records.next_back(),
        };
        record.map(|(key, record)| (key.clone(), record.clone()))
    }
}

#[derive(Default)]
struct State {
    nodes: Records,
    root_key: Option<Vec<u8>>,
    aux: BTreeMap<Vec<u8>, Vec<u8>>,
    meta: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// The node store a restore writes until its Merk is whole. Its clones
/// share one Merk.
#[derive(Clone, Default)]
pub(super) struct Staging {
    state: Arc<Mutex<State>>,
}

fn free<T>(value: T) -> CostContext<T> {
    value.wrap_with_cost(Default::default())
}

impl Staging {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        locked(&self.state)
    }

    /// Takes the node under `key` out of the staging, to hand it to the
    /// Merk's own store.
    pub(super) fn take_node(
        &self,
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<Option<TreeNode>, Error> {
        let record = self.state().nodes.take(key);
        record
            .map(|record| {
                TreeNode::decode_raw(
                    &record,
                    key.to_vec(),
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version,
                )
            })
            .transpose()
    }

    /// How many nodes the staging holds.
    pub(super) fn len(&self) -> usize {
        self.state().nodes.len()
    }
}

enum Change {
    PutNode(Vec<u8>, Vec<u8>),
    DeleteNode(Vec<u8>),
    RootKey(Option<Vec<u8>>),
    PutAux(Vec<u8>, Vec<u8>),
    DeleteAux(Vec<u8>),
}

/// The change set of a [`Staging`]: every written node as its whole record.
#[derive(Default)]
pub(super) struct StagedChanges(Vec<Change>);

impl NodeChangeSet for StagedChanges {
    type Prepared = Vec<u8>;

    fn prepare_node(&mut self, node: &TreeNode, _change: NodeChange) -> Vec<u8> {
        node.encode()
    }

    fn put_node(
        &mut self,
        key: &[u8],
        node: Vec<u8>,
        _cost: Option<NodeWriteCost>,
    ) -> Result<(), Error> {
        self.0.push(Change::PutNode(key.to_vec(), node));
        Ok(())
    }

    fn delete_node(&mut self, key: &[u8], _cost: Option<KeyValueStorageCost>) {
        self.0.push(Change::DeleteNode(key.to_vec()));
    }

    fn put_root_key(
        &mut self,
        root_key: &[u8],
        _cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.0.push(Change::RootKey(Some(root_key.to_vec())));
        Ok(())
    }

    fn delete_root_key(&mut self, _cost: Option<KeyValueStorageCost>) {
        self.0.push(Change::RootKey(None));
    }

    fn put_aux_record(
        &mut self,
        key: &[u8],
        value: &[u8],
        _cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.0.push(Change::PutAux(key.to_vec(), value.to_vec()));
        Ok(())
    }

    fn delete_aux_record(&mut self, key: &[u8], _cost: Option<KeyValueStorageCost>) {
        self.0.push(Change::DeleteAux(key.to_vec()));
    }
}

impl<'db> NodeStore<'db> for Staging {
    type ChangeSet = StagedChanges;
    type Cursor = StagedCursor;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        let node = self.state().nodes.read(key, |record| {
            TreeNode::decode_raw(record, key.to_vec(), value_defined_cost_fn, grove_version)
        });
        free(node.transpose())
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        let node = self.state().nodes.read(key, |record| {
            TreeNodeInner::decode(record).map_err(Error::EdError)
        });
        free(node.transpose())
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        free(Ok(self.state().nodes.read(key, |_| ()).is_some()))
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        free(Ok(self.state().root_key.clone()))
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        self.state().root_key = root_key.map(<[u8]>::to_vec);
        free(Ok(()))
    }

    fn cursor(&self) -> StagedCursor {
        StagedCursor {
            state: Arc::clone(&self.state),
            at: None,
        }
    }

    fn change_set(&self) -> StagedChanges {
        StagedChanges::default()
    }

    fn commit(&self, change_set: StagedChanges) -> CostResult<(), Error> {
        let mut state = self.state();
        for change in change_set.0 {
            match change {
                Change::PutNode(key, record) => state.nodes.put(key, record),
                Change::DeleteNode(key) => {
                    state.nodes.take(&key);
                }
                Change::RootKey(root_key) => state.root_key = root_key,
                Change::PutAux(key, value) => {
                    state.aux.insert(key, value);
                }
                Change::DeleteAux(key) => {
                    state.aux.remove(&key);
                }
            }
        }
        free(Ok(()))
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        free(Ok(self.state().aux.get(key).cloned()))
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        free(Ok(self.state().meta.get(key).cloned()))
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error> {
        self.state().meta.insert(key.to_vec(), value.to_vec());
        free(Ok(()))
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error> {
        self.state().meta.remove(key);
        free(Ok(()))
    }
}

/// A cursor over the nodes of a [`Staging`] as they are at each move. It
/// keeps a copy of the record it stands on.
pub(super) struct StagedCursor {
    state: Arc<Mutex<State>>,
    at: Option<(Vec<u8>, Vec<u8>)>,
}

fn locked(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A record is put or removed whole: a panic leaves no half of one.
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl StagedCursor {
    /// Moves to the first record of `range`, or to its last.
    fn go(&mut self, range: (Bound<&[u8]>, Bound<&[u8]>), first: bool) -> CostContext<()> {
        self.at = locked(&self.state).nodes.at_end_of(range, first);
        free(())
    }

    /// Steps off the record the cursor stands on, to the one after it or
    /// the one before. A cursor that stands on none stays there.
    fn step(&mut self, forward: bool) -> CostContext<()> {
        let Some((key, _)) = self.at.take() else {
            return free(());
        };
        let key = key.as_slice();
        match forward {
            true => self.go((Bound::Excluded(key), Bound::Unbounded), true),
            false => self.go((Bound::Unbounded, Bound::Excluded(key)), false),
        }
    }
}

impl NodeCursor for StagedCursor {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.go((Bound::Unbounded, Bound::Unbounded), true)
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        self.go((Bound::Unbounded, Bound::Unbounded), false)
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.go((Bound::Included(key.as_ref()), Bound::Unbounded), true)
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.go((Bound::Unbounded, Bound::Included(key.as_ref())), false)
    }

    fn next(&mut self) -> CostContext<()> {
        self.step(true)
    }

    fn prev(&mut self) -> CostContext<()> {
        self.step(false)
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        free(self.at.as_ref().map(|(_, record)| record.as_slice()))
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        free(self.at.as_ref().map(|(key, _)| key.as_slice()))
    }

    fn valid(&self) -> CostContext<bool> {
        free(self.at.is_some())
    }
}

/// A restore on a staging moves between threads with it.
const _: fn() = || {
    fn moved<T: Send>() {}
    moved::<Staging>();
    moved::<StagedChanges>();
    moved::<StagedCursor>();
};

#[cfg(test)]
mod tests {
    use ed::Encode;

    use super::*;
    use crate::{store::mem::MemNodeStore, Merk, Op, TreeFeatureType, TreeType};

    const NO_VALUE_DEFINED_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> =
        None;

    fn version() -> &'static GroveVersion {
        GroveVersion::latest()
    }

    fn open<'db, S: NodeStore<'db>>(store: S) -> Merk<S> {
        Merk::open_base(
            store,
            TreeType::NormalTree,
            NO_VALUE_DEFINED_COST,
            version(),
        )
        .unwrap()
        .expect("the store opens")
    }

    fn key(n: u64) -> Vec<u8> {
        n.to_be_bytes().to_vec()
    }

    fn put(n: u64, value: u64) -> (Vec<u8>, Op) {
        let value = vec![value as u8; 3 + (value % 60) as usize];
        (key(n), Op::Put(value, TreeFeatureType::BasicMerkNode))
    }

    fn apply<'db, S: NodeStore<'db>>(merk: &mut Merk<S>, batch: &[(Vec<u8>, Op)]) {
        merk.apply::<_, Vec<u8>>(batch, &[], None, version())
            .unwrap()
            .expect("the batch applies");
    }

    fn records<'db, S: NodeStore<'db>>(store: &S) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut cursor = store.cursor();
        cursor.seek_to_first().unwrap();
        let mut records = vec![];
        while cursor.valid().unwrap() {
            records.push((
                cursor.key().unwrap().unwrap().to_vec(),
                cursor.value().unwrap().unwrap().to_vec(),
            ));
            cursor.next().unwrap();
        }
        records
    }

    #[test]
    fn a_merk_on_a_staging_is_the_merk_it_is_on_another_store() {
        let (staging, other) = (Staging::new(), MemNodeStore::new());
        let (mut staged, mut reference) = (open(staging.clone()), open(other.clone()));
        let batches: [Vec<(Vec<u8>, Op)>; 4] = [
            (0..200).map(|n| put(n, n)).collect(),
            (0..200)
                .filter(|n| n % 3 != 1)
                .map(|n| match n % 3 {
                    0 => (key(n), Op::Delete),
                    _ => put(n, n + 7),
                })
                .collect(),
            (150..400).map(|n| put(n, n * 3)).collect(),
            (0..400).map(|n| (key(n), Op::Delete)).collect(),
        ];
        for (round, batch) in batches.iter().enumerate() {
            apply(&mut staged, batch);
            apply(&mut reference, batch);
            assert_eq!(
                staged.root_hash().unwrap(),
                reference.root_hash().unwrap(),
                "round {round}"
            );
            assert_eq!(records(&staging), other.records(), "round {round}");
            assert_eq!(staging.len(), other.records().len());
            // The root key that the commit recorded, and the Merk as its
            // store opens it.
            assert_eq!(
                staging.root_key().unwrap().unwrap(),
                other.root_key().unwrap().unwrap()
            );
            let reopened = open(staging.clone());
            assert_eq!(reopened.root_hash().unwrap(), staged.root_hash().unwrap());
            assert_eq!(reopened.root_key(), reference.root_key());
        }
        // The last batch removed every node, and the root key with them.
        assert_eq!(staging.len(), 0);
        assert_eq!(staging.root_key().unwrap().unwrap(), None);
        assert!(staged.is_empty_tree().unwrap());
    }

    #[test]
    fn a_staging_reads_a_node_as_the_record_it_was_written_as() {
        let staging = Staging::new();
        let mut merk = open(staging.clone());
        apply(&mut merk, &(0..7).map(|n| put(n, n)).collect::<Vec<_>>());
        let root = merk.root_key().expect("a root");

        let node = staging
            .get_node(&root, NO_VALUE_DEFINED_COST, version())
            .unwrap()
            .unwrap()
            .expect("the root is there");
        assert_eq!(node.key(), root.as_slice());
        let record = records(&staging)
            .into_iter()
            .find_map(|(key, record)| (key == root).then_some(record))
            .expect("the cursor shows the root");
        assert_eq!(node.encode(), record);
        let read = staging.get_node_for_read(&root).unwrap().unwrap();
        assert_eq!(read.expect("the root is there").encode().unwrap(), record);
        assert!(staging.has_node(&root).unwrap().unwrap());

        // A cost that the value defines is the reader's to give.
        let defined = |_: &[u8], _: &GroveVersion| {
            Some(ValueDefinedCostType::SpecializedValueDefinedCost(11))
        };
        let costed = staging.get_node(&root, Some(defined), version()).unwrap();
        assert_ne!(costed.unwrap().expect("the root is there"), node);

        // A key with no node.
        let absent = key(99);
        let found = staging.get_node(&absent, NO_VALUE_DEFINED_COST, version());
        assert_eq!(found.unwrap().unwrap(), None);
        assert_eq!(staging.get_node_for_read(&absent).unwrap().unwrap(), None);
        assert!(!staging.has_node(&absent).unwrap().unwrap());
        assert_eq!(staging.take_node(&absent, version()).unwrap(), None);

        // A node that is taken is the node that was read, and is gone.
        let taken = staging.take_node(&root, version()).unwrap();
        assert_eq!(taken.expect("the root is there").encode(), record);
        assert_eq!(staging.len(), 6);
        assert!(!staging.has_node(&root).unwrap().unwrap());
        assert_eq!(staging.take_node(&root, version()).unwrap(), None);
    }

    #[test]
    fn the_cursor_of_a_staging_seeks_and_steps_in_both_directions() {
        let staging = Staging::new();
        let at = |cursor: &StagedCursor| {
            let key = cursor.key().unwrap().map(<[u8]>::to_vec);
            assert_eq!(cursor.valid().unwrap(), key.is_some());
            assert_eq!(cursor.value().unwrap().is_some(), key.is_some());
            key
        };

        // No node: every move comes to none.
        let mut cursor = staging.cursor();
        assert_eq!(at(&cursor), None);
        cursor.seek_to_first().unwrap();
        assert_eq!(at(&cursor), None);
        cursor.seek_to_last().unwrap();
        assert_eq!(at(&cursor), None);

        // A cursor shows the nodes as they are at each move.
        let mut merk = open(staging.clone());
        apply(&mut merk, &[put(10, 1), put(20, 2), put(30, 3)]);
        cursor.seek_to_first().unwrap();
        assert_eq!(at(&cursor), Some(key(10)));
        cursor.next().unwrap();
        assert_eq!(at(&cursor), Some(key(20)));
        cursor.next().unwrap();
        assert_eq!(at(&cursor), Some(key(30)));
        cursor.next().unwrap();
        assert_eq!(at(&cursor), None);
        // A step from no node goes nowhere.
        cursor.next().unwrap();
        assert_eq!(at(&cursor), None);
        cursor.prev().unwrap();
        assert_eq!(at(&cursor), None);

        cursor.seek_to_last().unwrap();
        assert_eq!(at(&cursor), Some(key(30)));
        cursor.prev().unwrap();
        assert_eq!(at(&cursor), Some(key(20)));
        cursor.prev().unwrap();
        cursor.prev().unwrap();
        assert_eq!(at(&cursor), None);

        for (sought, forward, back) in [
            (5, Some(10), None),
            (10, Some(10), Some(10)),
            (15, Some(20), Some(10)),
            (30, Some(30), Some(30)),
            (31, None, Some(30)),
        ] {
            cursor.seek(key(sought)).unwrap();
            assert_eq!(at(&cursor), forward.map(key), "seek {sought}");
            cursor.seek_for_prev(key(sought)).unwrap();
            assert_eq!(at(&cursor), back.map(key), "seek for prev {sought}");
        }

        // The record under the cursor is the node's.
        cursor.seek(key(20)).unwrap();
        let node = staging.get_node(&key(20), NO_VALUE_DEFINED_COST, version());
        let record = node.unwrap().unwrap().expect("the node is there").encode();
        assert_eq!(cursor.value().unwrap(), Some(record.as_slice()));
    }

    #[test]
    fn a_staging_keeps_the_side_records_of_its_merk() {
        let staging = Staging::new();
        let mut merk = open(staging.clone());
        let aux = |op| [(b"aux".to_vec(), op, None)];

        let put_aux = aux(Op::Put(b"a".to_vec(), TreeFeatureType::BasicMerkNode));
        merk.apply::<_, Vec<u8>>(&[put(1, 1)], &put_aux, None, version())
            .unwrap()
            .unwrap();
        assert_eq!(merk.get_aux(b"aux").unwrap().unwrap(), Some(b"a".to_vec()));
        merk.apply::<Vec<u8>, _>(&[], &aux(Op::Delete), None, version())
            .unwrap()
            .unwrap();
        assert_eq!(merk.get_aux(b"aux").unwrap().unwrap(), None);

        let meta = |staging: &Staging| staging.get_meta_record(b"meta").unwrap().unwrap();
        merk.put_meta(b"meta".to_vec(), b"m".to_vec())
            .unwrap()
            .unwrap();
        assert_eq!(meta(&staging), Some(b"m".to_vec()));
        merk.delete_meta(b"meta").unwrap().unwrap();
        assert_eq!(meta(&staging), None);

        merk.set_base_root_key(None).unwrap().unwrap();
        assert_eq!(staging.root_key().unwrap().unwrap(), None);
        merk.set_base_root_key(Some(key(1))).unwrap().unwrap();
        assert_eq!(staging.root_key().unwrap().unwrap(), Some(key(1)));
    }
}
