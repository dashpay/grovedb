//! A node store that is not a storage context: nodes are kept as fields in
//! memory, and a commit stores of each node only the parts its
//! [`NodeChange`] names. It then checks that the stored node equals the node
//! Merk wrote, so a kind that claims too little fails the commit.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use ed::Encode;
use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
};
use grovedb_version::version::GroveVersion;

use super::{NodeChange, NodeChangeSet, NodeCursor, NodeStore, NodeWriteCost};
use crate::{
    tree::{
        kv::{ValueDefinedCostType, KV},
        AggregateData, TreeNode, TreeNodeInner,
    },
    CryptoHash, Error, Link, TreeFeatureType,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MemLink {
    key: Vec<u8>,
    hash: CryptoHash,
    child_heights: (u8, u8),
    aggregate_data: AggregateData,
}

impl MemLink {
    fn of(link: &Link) -> Self {
        let child_heights = match link {
            Link::Reference { child_heights, .. }
            | Link::Modified { child_heights, .. }
            | Link::Uncommitted { child_heights, .. }
            | Link::Loaded { child_heights, .. } => *child_heights,
        };
        MemLink {
            key: link.key().to_vec(),
            hash: *link.hash(),
            child_heights,
            aggregate_data: link.aggregate_data(),
        }
    }

    fn link(&self) -> Link {
        Link::Reference {
            hash: self.hash,
            child_heights: self.child_heights,
            key: self.key.clone(),
            aggregate_data: self.aggregate_data,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MemNode {
    value: Vec<u8>,
    feature_type: TreeFeatureType,
    value_hash: CryptoHash,
    kv_hash: CryptoHash,
    children: [Option<MemLink>; 2],
}

impl MemNode {
    fn of(node: &TreeNode) -> Self {
        MemNode {
            value: node.value_as_slice().to_vec(),
            feature_type: node.feature_type(),
            value_hash: *node.value_hash(),
            kv_hash: *node.kv_hash(),
            children: [true, false].map(|left| node.link(left).map(MemLink::of)),
        }
    }

    fn inner(&self, key: Vec<u8>) -> TreeNodeInner {
        TreeNodeInner {
            marks: Default::default(),
            left: self.children[0].as_ref().map(MemLink::link),
            right: self.children[1].as_ref().map(MemLink::link),
            kv: KV::from_fields(
                key,
                self.value.clone(),
                self.kv_hash,
                self.value_hash,
                self.feature_type,
            ),
        }
    }

    fn record(&self) -> Vec<u8> {
        self.inner(vec![]).encode().expect("a node encodes")
    }

    /// Stores of `written` only the parts `change` names.
    fn apply(&mut self, written: &MemNode, change: NodeChange) {
        match change {
            NodeChange::ValueWritten => *self = written.clone(),
            NodeChange::StructureChanged => {
                self.value_hash = written.value_hash;
                self.kv_hash = written.kv_hash;
                self.children = written.children.clone();
            }
            NodeChange::HashOnly => {
                self.value_hash = written.value_hash;
                self.kv_hash = written.kv_hash;
                for (stored, written) in self.children.iter_mut().zip(&written.children) {
                    if let (Some(stored), Some(written)) = (stored, written) {
                        stored.hash = written.hash;
                        stored.child_heights = written.child_heights;
                        stored.aggregate_data = written.aggregate_data;
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct State {
    nodes: BTreeMap<Vec<u8>, MemNode>,
    root_key: Option<Vec<u8>>,
    aux: BTreeMap<Vec<u8>, Vec<u8>>,
    meta: BTreeMap<Vec<u8>, Vec<u8>>,
    log: Vec<(Vec<u8>, Option<NodeChange>)>,
}

/// A handle of the in-memory store. Its clones share one subtree.
#[derive(Clone, Default)]
pub(crate) struct MemNodeStore {
    state: Arc<Mutex<State>>,
}

impl MemNodeStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Every node as `(key, record)`, in key order.
    pub(crate) fn records(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let state = self.state.lock().unwrap();
        state
            .nodes
            .iter()
            .map(|(key, node)| (key.clone(), node.record()))
            .collect()
    }

    /// The node changes committed since the last call, in commit order:
    /// the key, and the kind of a write or `None` for a removal.
    pub(crate) fn take_log(&self) -> Vec<(Vec<u8>, Option<NodeChange>)> {
        std::mem::take(&mut self.state.lock().unwrap().log)
    }
}

enum MemOp {
    PutNode(Vec<u8>, Box<MemNode>, NodeChange),
    DeleteNode(Vec<u8>),
    PutRootKey(Vec<u8>),
    DeleteRootKey,
    PutAux(Vec<u8>, Vec<u8>),
    DeleteAux(Vec<u8>),
}

#[derive(Default)]
pub(crate) struct MemChangeSet {
    ops: Vec<MemOp>,
}

impl NodeChangeSet for MemChangeSet {
    type Prepared = (MemNode, NodeChange);

    fn prepare_node(&mut self, node: &TreeNode, change: NodeChange) -> Self::Prepared {
        (MemNode::of(node), change)
    }

    fn put_node(
        &mut self,
        key: &[u8],
        (node, change): Self::Prepared,
        _cost: Option<NodeWriteCost>,
    ) -> Result<(), Error> {
        self.ops
            .push(MemOp::PutNode(key.to_vec(), Box::new(node), change));
        Ok(())
    }

    fn delete_node(&mut self, key: &[u8], _cost: Option<KeyValueStorageCost>) {
        self.ops.push(MemOp::DeleteNode(key.to_vec()));
    }

    fn put_root_key(
        &mut self,
        root_key: &[u8],
        _cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.ops.push(MemOp::PutRootKey(root_key.to_vec()));
        Ok(())
    }

    fn delete_root_key(&mut self, _cost: Option<KeyValueStorageCost>) {
        self.ops.push(MemOp::DeleteRootKey);
    }

    fn put_aux_record(
        &mut self,
        key: &[u8],
        value: &[u8],
        _cost: Option<KeyValueStorageCost>,
    ) -> Result<(), Error> {
        self.ops.push(MemOp::PutAux(key.to_vec(), value.to_vec()));
        Ok(())
    }

    fn delete_aux_record(&mut self, key: &[u8], _cost: Option<KeyValueStorageCost>) {
        self.ops.push(MemOp::DeleteAux(key.to_vec()));
    }
}

impl<'db> NodeStore<'db> for MemNodeStore {
    type ChangeSet = MemChangeSet;
    type Cursor = MemCursor;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        let state = self.state.lock().unwrap();
        let node = state.nodes.get(key).map(|node| {
            let mut inner = node.inner(key.to_vec());
            if let Some(value_defined_cost_fn) = &value_defined_cost_fn {
                inner.kv.value_defined_cost =
                    value_defined_cost_fn(inner.kv.value_as_slice(), grove_version);
            }
            TreeNode::new_with_tree_inner(inner)
        });
        Ok(node).wrap_with_cost(Default::default())
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        let state = self.state.lock().unwrap();
        Ok(state.nodes.get(key).map(|node| node.inner(vec![]))).wrap_with_cost(Default::default())
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        Ok(self.state.lock().unwrap().nodes.contains_key(key)).wrap_with_cost(Default::default())
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        Ok(self.state.lock().unwrap().root_key.clone()).wrap_with_cost(Default::default())
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        self.state.lock().unwrap().root_key = root_key.map(<[u8]>::to_vec);
        Ok(()).wrap_with_cost(Default::default())
    }

    fn cursor(&self) -> MemCursor {
        MemCursor {
            records: self.records(),
            position: None,
        }
    }

    fn change_set(&self) -> MemChangeSet {
        MemChangeSet::default()
    }

    fn commit(&self, change_set: MemChangeSet) -> CostResult<(), Error> {
        let mut state = self.state.lock().unwrap();
        for op in change_set.ops {
            match op {
                MemOp::PutNode(key, written, change) => {
                    let stored = match change {
                        NodeChange::ValueWritten => {
                            state.nodes.entry(key.clone()).or_insert((*written).clone())
                        }
                        _ => state
                            .nodes
                            .get_mut(&key)
                            .expect("only a written value may be new to the store"),
                    };
                    stored.apply(&written, change);
                    assert_eq!(
                        *stored, *written,
                        "{change:?} left the store with another node than Merk wrote"
                    );
                    state.log.push((key, Some(change)));
                }
                MemOp::DeleteNode(key) => {
                    state.nodes.remove(&key);
                    state.log.push((key, None));
                }
                MemOp::PutRootKey(root_key) => state.root_key = Some(root_key),
                MemOp::DeleteRootKey => state.root_key = None,
                MemOp::PutAux(key, value) => {
                    state.aux.insert(key, value);
                }
                MemOp::DeleteAux(key) => {
                    state.aux.remove(&key);
                }
            }
        }
        Ok(()).wrap_with_cost(Default::default())
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        Ok(self.state.lock().unwrap().aux.get(key).cloned()).wrap_with_cost(Default::default())
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        Ok(self.state.lock().unwrap().meta.get(key).cloned()).wrap_with_cost(Default::default())
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error> {
        let mut state = self.state.lock().unwrap();
        state.meta.insert(key.to_vec(), value.to_vec());
        Ok(()).wrap_with_cost(Default::default())
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error> {
        self.state.lock().unwrap().meta.remove(key);
        Ok(()).wrap_with_cost(Default::default())
    }
}

/// A cursor over the records the store held when the cursor was made.
pub(crate) struct MemCursor {
    records: Vec<(Vec<u8>, Vec<u8>)>,
    position: Option<usize>,
}

impl MemCursor {
    fn record(&self) -> Option<&(Vec<u8>, Vec<u8>)> {
        self.position
            .and_then(|position| self.records.get(position))
    }

    fn move_to(&mut self, position: Option<usize>) -> CostContext<()> {
        self.position = position.filter(|position| *position < self.records.len());
        ().wrap_with_cost(Default::default())
    }
}

impl NodeCursor for MemCursor {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.move_to(Some(0))
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        self.move_to(self.records.len().checked_sub(1))
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        let key = key.as_ref();
        self.move_to(Some(
            self.records.partition_point(|(k, _)| k.as_slice() < key),
        ))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        let key = key.as_ref();
        self.move_to(
            self.records
                .partition_point(|(k, _)| k.as_slice() <= key)
                .checked_sub(1),
        )
    }

    fn next(&mut self) -> CostContext<()> {
        self.move_to(self.position.map(|position| position + 1))
    }

    fn prev(&mut self) -> CostContext<()> {
        self.move_to(self.position.and_then(|position| position.checked_sub(1)))
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        self.record()
            .map(|(_, record)| record.as_slice())
            .wrap_with_cost(Default::default())
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        self.record()
            .map(|(key, _)| key.as_slice())
            .wrap_with_cost(Default::default())
    }

    fn valid(&self) -> CostContext<bool> {
        self.record().is_some().wrap_with_cost(Default::default())
    }
}
