//! GroveDB batch operations support

#[cfg(feature = "full")]
mod batch_structure;
#[cfg(feature = "full")]
pub mod estimated_costs;
#[cfg(feature = "full")]
pub mod key_info;
#[cfg(feature = "full")]
mod mode;
#[cfg(test)]
mod multi_insert_cost_tests;
#[cfg(feature = "full")]
mod options;
#[cfg(test)]
mod single_deletion_cost_tests;
#[cfg(test)]
mod single_insert_cost_tests;

#[cfg(feature = "full")]
use core::fmt;
#[cfg(feature = "full")]
use std::{
    cmp::Ordering,
    collections::{btree_map::Entry, BTreeMap, HashMap},
    hash::{Hash, Hasher},
    ops::AddAssign,
    slice::Iter,
    vec::IntoIter,
};

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use estimated_costs::{
    average_case_costs::AverageCaseTreeCacheKnownPaths,
    worst_case_costs::WorstCaseTreeCacheKnownPaths,
};
#[cfg(feature = "full")]
use integer_encoding::VarInt;
#[cfg(feature = "full")]
use itertools::Itertools;
#[cfg(feature = "full")]
use key_info::{KeyInfo, KeyInfo::KnownKey};
#[cfg(feature = "full")]
use merk::{
    tree::{kv::KV, value_hash, NULL_HASH},
    CryptoHash, Error as MerkError, Merk, MerkType,
};
#[cfg(feature = "full")]
pub use options::BatchApplyOptions;
#[cfg(feature = "full")]
use storage::{
    rocksdb_storage::{PrefixedRocksDbBatchStorageContext, PrefixedRocksDbBatchTransactionContext},
    Storage, StorageBatch, StorageContext,
};
#[cfg(feature = "full")]
use visualize::{Drawer, Visualize};

#[cfg(feature = "full")]
use crate::{
    batch::{
        batch_structure::BatchStructure,
        estimated_costs::EstimatedCostsType,
        mode::{BatchRunMode, BatchRunMode::ExecuteMode},
    },
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{path_from_reference_path_type, path_from_reference_qualified_path_type},
    subtree::{SUM_TREE_COST_SIZE, TREE_COST_SIZE},
    Element, ElementFlags, Error, GroveDb, Transaction, TransactionArg,
};

#[cfg(feature = "full")]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Op {
    ReplaceTreeRootKey {
        hash: [u8; 32],
        root_key: Option<Vec<u8>>,
        sum: Option<i64>,
    },
    Insert {
        element: Element,
    },
    Replace {
        element: Element,
    },
    InsertTreeWithRootHash {
        hash: [u8; 32],
        root_key: Option<Vec<u8>>,
        flags: Option<ElementFlags>,
        sum: Option<i64>,
    },
    Delete,
    DeleteTree,
    DeleteSumTree,
}

#[cfg(feature = "full")]
impl PartialOrd for Op {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Op::Delete, Op::Insert { .. }) => Some(Ordering::Less),
            (Op::Delete, Op::Replace { .. }) => Some(Ordering::Less),
            (Op::Insert { .. }, Op::Delete) => Some(Ordering::Greater),
            (Op::Replace { .. }, Op::Delete) => Some(Ordering::Greater),
            _ => Some(Ordering::Equal),
        }
    }
}

#[cfg(feature = "full")]
impl Ord for Op {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).expect("all ops have order")
    }
}

#[cfg(feature = "full")]
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct KnownKeysPath(Vec<Vec<u8>>);

#[cfg(feature = "full")]
impl Hash for KnownKeysPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

#[cfg(feature = "full")]
impl PartialEq<KeyInfoPath> for KnownKeysPath {
    fn eq(&self, other: &KeyInfoPath) -> bool {
        self.0 == other.to_path_refs()
    }
}

#[cfg(feature = "full")]
impl PartialEq<Vec<Vec<u8>>> for KnownKeysPath {
    fn eq(&self, other: &Vec<Vec<u8>>) -> bool {
        self.0 == other.as_slice()
    }
}

#[cfg(feature = "full")]
#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Debug, Default)]
pub struct KeyInfoPath(pub Vec<KeyInfo>);

#[cfg(feature = "full")]
impl Hash for KeyInfoPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

#[cfg(feature = "full")]
impl Visualize for KeyInfoPath {
    fn visualize<W: std::io::Write>(&self, mut drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
        drawer.write(b"path: ")?;
        let mut path_out = Vec::new();
        let mut path_drawer = Drawer::new(&mut path_out);
        for k in &self.0 {
            path_drawer = k.visualize(path_drawer).unwrap();
            path_drawer.write(b" ").unwrap();
        }
        drawer.write(path_out.as_slice()).unwrap();
        Ok(drawer)
    }
}

#[cfg(feature = "full")]
impl KeyInfoPath {
    pub fn from_vec(vec: Vec<KeyInfo>) -> Self {
        KeyInfoPath(vec)
    }

    pub fn from_known_path<'p, P>(path: P) -> Self
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        KeyInfoPath(path.into_iter().map(|k| KnownKey(k.to_vec())).collect())
    }

    pub fn from_known_owned_path<'p, P>(path: P) -> Self
    where
        P: IntoIterator<Item = Vec<u8>>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        KeyInfoPath(path.into_iter().map(KnownKey).collect())
    }

    pub fn to_path_consume(self) -> Vec<Vec<u8>> {
        self.0.into_iter().map(|k| k.get_key()).collect()
    }

    pub fn to_path(&self) -> Vec<Vec<u8>> {
        self.0.iter().map(|k| k.get_key_clone()).collect()
    }

    pub fn to_path_refs(&self) -> Vec<&[u8]> {
        self.0.iter().map(|k| k.as_slice()).collect()
    }

    pub fn split_last(&self) -> Option<(&KeyInfo, &[KeyInfo])> {
        self.0.split_last()
    }

    pub fn last(&self) -> Option<&KeyInfo> {
        self.0.last()
    }

    pub fn as_vec(&self) -> &Vec<KeyInfo> {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> u32 {
        self.0.len() as u32
    }

    pub fn push(&mut self, k: KeyInfo) {
        self.0.push(k);
    }

    pub fn iter(&self) -> Iter<'_, KeyInfo> {
        self.0.iter()
    }

    pub fn into_iter(self) -> IntoIter<KeyInfo> {
        self.0.into_iter()
    }
}

#[cfg(feature = "full")]
/// Batch operation
#[derive(Clone, PartialEq)]
pub struct GroveDbOp {
    /// Path to a subtree - subject to an operation
    pub path: KeyInfoPath,
    /// Key of an element in the subtree
    pub key: KeyInfo,
    /// Operation to perform on the key
    pub op: Op,
}

#[cfg(feature = "full")]
impl fmt::Debug for GroveDbOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut path_out = Vec::new();
        let path_drawer = Drawer::new(&mut path_out);
        self.path.visualize(path_drawer).unwrap();
        let mut key_out = Vec::new();
        let key_drawer = Drawer::new(&mut key_out);
        self.key.visualize(key_drawer).unwrap();

        let op_dbg = match &self.op {
            Op::Insert { element } => match element {
                Element::Item(..) => "Insert Item",
                Element::Reference(..) => "Insert Ref",
                Element::Tree(..) => "Insert Tree",
                Element::SumTree(..) => "Insert Sum Tree",
                Element::SumItem(..) => "Insert Sum Item",
            },
            Op::Replace { element } => match element {
                Element::Item(..) => "Replace Item",
                Element::Reference(..) => "Replace Ref",
                Element::Tree(..) => "Replace Tree",
                Element::SumTree(..) => "Replace Sum Tree",
                Element::SumItem(..) => "Replace Sum Item",
            },
            Op::Delete => "Delete",
            Op::DeleteTree => "Delete Tree",
            Op::DeleteSumTree => "Delete Sum Tree",
            Op::ReplaceTreeRootKey { .. } => "Replace Tree Hash and Root Key",
            Op::InsertTreeWithRootHash { .. } => "Insert Tree Hash and Root Key",
        };

        f.debug_struct("GroveDbOp")
            .field("path", &String::from_utf8_lossy(&path_out))
            .field("key", &String::from_utf8_lossy(&key_out))
            .field("op", &op_dbg)
            .finish()
    }
}

#[cfg(feature = "full")]
impl GroveDbOp {
    pub fn insert_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Insert { element },
        }
    }

    pub fn insert_estimated_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Insert { element },
        }
    }

    pub fn replace_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Replace { element },
        }
    }

    pub fn replace_estimated_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Replace { element },
        }
    }

    pub fn delete_op(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Delete,
        }
    }

    pub fn delete_tree_op(path: Vec<Vec<u8>>, key: Vec<u8>, is_sum_tree: bool) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: if is_sum_tree {
                Op::DeleteSumTree
            } else {
                Op::DeleteTree
            },
        }
    }

    pub fn delete_estimated_op(path: KeyInfoPath, key: KeyInfo) -> Self {
        Self {
            path,
            key,
            op: Op::Delete,
        }
    }

    pub fn delete_estimated_tree_op(path: KeyInfoPath, key: KeyInfo, is_sum_tree: bool) -> Self {
        Self {
            path,
            key,
            op: if is_sum_tree {
                Op::DeleteSumTree
            } else {
                Op::DeleteTree
            },
        }
    }

    pub fn verify_consistency_of_operations(ops: &Vec<GroveDbOp>) -> GroveDbOpConsistencyResults {
        let ops_len = ops.len();
        // operations should not have any duplicates
        let mut repeated_ops = vec![];
        for (i, op) in ops.iter().enumerate() {
            if i == ops_len {
                continue;
            } // Don't do last one
            let count = ops
                .split_at(i + 1)
                .1
                .iter()
                .filter(|&current_op| current_op == op)
                .count() as u16;
            if count > 1 {
                repeated_ops.push((op.clone(), count));
            }
        }

        let mut same_path_key_ops = vec![];

        // No double insert or delete of same key in same path
        for (i, op) in ops.iter().enumerate() {
            if i == ops_len {
                continue;
            } // Don't do last one
            let mut doubled_ops = ops
                .split_at(i + 1)
                .1
                .iter()
                .filter_map(|current_op| {
                    if current_op.path == op.path && current_op.key == op.key {
                        Some(current_op.op.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<Op>>();
            if doubled_ops.len() > 0 {
                doubled_ops.push(op.op.clone());
                same_path_key_ops.push((op.path.clone(), op.key.clone(), doubled_ops));
            }
        }

        let inserts = ops
            .iter()
            .filter_map(|current_op| match current_op.op {
                Op::Insert { .. } | Op::Replace { .. } => Some(current_op.clone()),
                _ => None,
            })
            .collect::<Vec<GroveDbOp>>();

        let deletes = ops
            .iter()
            .filter_map(|current_op| {
                if let Op::Delete = current_op.op {
                    Some(current_op.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<GroveDbOp>>();

        let mut insert_ops_below_deleted_ops = vec![];

        // No inserts under a deleted path
        for deleted_op in deletes.iter() {
            let mut deleted_qualified_path = deleted_op.path.clone();
            deleted_qualified_path.push(deleted_op.key.clone());
            let inserts_with_deleted_ops_above = inserts
                .iter()
                .filter_map(|inserted_op| {
                    if deleted_op.path.len() < inserted_op.path.len()
                        && deleted_qualified_path
                            .iter()
                            .zip(inserted_op.path.iter())
                            .all(|(a, b)| a == b)
                    {
                        Some(inserted_op.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<GroveDbOp>>();
            if inserts_with_deleted_ops_above.len() > 0 {
                insert_ops_below_deleted_ops
                    .push((deleted_op.clone(), inserts_with_deleted_ops_above));
            }
        }

        GroveDbOpConsistencyResults {
            repeated_ops,
            same_path_key_ops,
            insert_ops_below_deleted_ops,
        }
    }
}

#[cfg(feature = "full")]
#[derive(Debug)]
pub struct GroveDbOpConsistencyResults {
    repeated_ops: Vec<(GroveDbOp, u16)>, // the u16 is count
    same_path_key_ops: Vec<(KeyInfoPath, KeyInfo, Vec<Op>)>,
    insert_ops_below_deleted_ops: Vec<(GroveDbOp, Vec<GroveDbOp>)>, /* the deleted op first,
                                                                     * then inserts under */
}

#[cfg(feature = "full")]
impl GroveDbOpConsistencyResults {
    pub fn is_empty(&self) -> bool {
        self.repeated_ops.is_empty()
            && self.same_path_key_ops.is_empty()
            && self.insert_ops_below_deleted_ops.is_empty()
    }
}

#[cfg(feature = "full")]
/// Cache for Merk trees by their paths.
struct TreeCacheMerkByPath<S, F> {
    merks: HashMap<Vec<Vec<u8>>, Merk<S>>,
    get_merk_fn: F,
}

#[cfg(feature = "full")]
impl<S, F> fmt::Debug for TreeCacheMerkByPath<S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheMerkByPath").finish()
    }
}

#[cfg(feature = "full")]
trait TreeCache<G, SR> {
    fn insert(&mut self, op: &GroveDbOp, is_sum_tree: bool) -> CostResult<(), Error>;

    fn get_batch_run_mode(&self) -> BatchRunMode;

    /// We will also be returning an op mode, this is to be used in propagation
    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<(CryptoHash, Option<Vec<u8>>, Option<i64>), Error>;

    fn update_base_merk_root_key(&mut self, root_key: Option<Vec<u8>>) -> CostResult<(), Error>;
}

#[cfg(feature = "full")]
impl<'db, S, F> TreeCacheMerkByPath<S, F>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    /// A reference assumes the value hash of the base item it points to.
    /// In a reference chain base_item -> ref_1 -> ref_2 e.t.c.
    /// all references in that chain (ref_1, ref_2) assume the value hash of the
    /// base_item. The goal of this function is to figure out what the
    /// value_hash of a reference chain is. If we want to insert ref_3 to the
    /// chain above and nothing else changes, we can get the value_hash from
    /// ref_2. But when dealing with batches, you can have an operation to
    /// insert ref_3 and another operation to change something in the
    /// reference chain in the same batch.
    /// All these has to be taken into account.
    fn follow_reference_get_value_hash<'a>(
        &'a mut self,
        qualified_path: &[Vec<u8>],
        ops_by_qualified_paths: &'a BTreeMap<Vec<Vec<u8>>, Op>,
        recursions_allowed: u8,
    ) -> CostResult<CryptoHash, Error> {
        let mut cost = OperationCost::default();
        if recursions_allowed == 0 {
            return Err(Error::ReferenceLimit).wrap_with_cost(cost);
        }
        // If the element being referenced changes in the same batch
        // we need to set the value_hash based on the new change and not the old state.
        if let Some(op) = ops_by_qualified_paths.get(qualified_path) {
            // the path is being modified, inserted or deleted in the batch of operations
            match op {
                Op::ReplaceTreeRootKey { .. } | Op::InsertTreeWithRootHash { .. } => {
                    return Err(Error::InvalidBatchOperation(
                        "references can not point to trees being updated",
                    ))
                    .wrap_with_cost(cost);
                }
                Op::Insert { element } | Op::Replace { element } => match element {
                    Element::Item(..) | Element::SumItem(..) => {
                        let serialized = cost_return_on_error_no_add!(&cost, element.serialize());
                        let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
                        Ok(val_hash).wrap_with_cost(cost)
                    }
                    Element::Reference(path, ..) => {
                        let path = cost_return_on_error_no_add!(
                            &cost,
                            path_from_reference_qualified_path_type(path.clone(), qualified_path)
                        );
                        self.follow_reference_get_value_hash(
                            path.as_slice(),
                            ops_by_qualified_paths,
                            recursions_allowed - 1,
                        )
                    }
                    Element::Tree(..) | Element::SumTree(..) => {
                        return Err(Error::InvalidBatchOperation(
                            "references can not point to trees being updated",
                        ))
                        .wrap_with_cost(cost);
                    }
                },
                Op::Delete | Op::DeleteTree | Op::DeleteSumTree => {
                    return Err(Error::InvalidBatchOperation(
                        "references can not point to something currently being deleted",
                    ))
                    .wrap_with_cost(cost);
                }
            }
        } else {
            let (key, reference_path) = qualified_path.split_last().unwrap(); // already checked
            let reference_merk_wrapped = self
                .merks
                .remove(reference_path)
                .map(|x| Ok(x).wrap_with_cost(Default::default()))
                .unwrap_or_else(|| (self.get_merk_fn)(reference_path, false));
            let merk = cost_return_on_error!(&mut cost, reference_merk_wrapped);

            // Here the element being referenced doesn't change in the same batch
            // and the max hop count is 1, meaning it should point directly to the base
            // element at this point we can extract the value hash from the
            // reference element directly
            if recursions_allowed == 1 {
                let referenced_element_value_hash_opt = cost_return_on_error!(
                    &mut cost,
                    merk.get_value_hash(key.as_ref())
                        .map_err(|e| Error::CorruptedData(e.to_string()))
                );

                let referenced_element_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_element_value_hash_opt
                        .ok_or({
                            let reference_string = reference_path
                                .iter()
                                .map(|a| hex::encode(a))
                                .collect::<Vec<String>>()
                                .join("/");
                            Error::MissingReference(format!(
                                "direct reference to path:`{}` key:`{}` in batch is missing",
                                reference_string,
                                hex::encode(key)
                            ))
                        })
                        .wrap_with_cost(OperationCost::default())
                );

                return Ok(referenced_element_value_hash).wrap_with_cost(cost);
            } else {
                // Here the element being referenced doesn't change in the same batch
                // but the hop count is greater than 1, we can't just take the value hash from
                // the referenced element as an element further down in the chain might still
                // change in the batch.
                let referenced_element = cost_return_on_error!(
                    &mut cost,
                    merk.get(key.as_ref())
                        .map_err(|e| Error::CorruptedData(e.to_string()))
                );

                let referenced_element = cost_return_on_error_no_add!(
                    &cost,
                    referenced_element.ok_or({
                        let reference_string = reference_path
                            .iter()
                            .map(|a| hex::encode(a))
                            .collect::<Vec<String>>()
                            .join("/");
                        Error::MissingReference(format!(
                            "reference to path:`{}` key:`{}` in batch is missing",
                            reference_string,
                            hex::encode(key)
                        ))
                    })
                );

                let element = cost_return_on_error_no_add!(
                    &cost,
                    Element::deserialize(referenced_element.as_slice()).map_err(|_| {
                        Error::CorruptedData(String::from("unable to deserialize element"))
                    })
                );

                match element {
                    Element::Item(..) | Element::SumItem(..) => {
                        let serialized = cost_return_on_error_no_add!(&cost, element.serialize());
                        let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
                        Ok(val_hash).wrap_with_cost(cost)
                    }
                    Element::Reference(path, ..) => {
                        let path = cost_return_on_error_no_add!(
                            &cost,
                            path_from_reference_qualified_path_type(path.clone(), qualified_path)
                        );
                        self.follow_reference_get_value_hash(
                            path.as_slice(),
                            ops_by_qualified_paths,
                            recursions_allowed - 1,
                        )
                    }
                    Element::Tree(..) | Element::SumTree(..) => {
                        return Err(Error::InvalidBatchOperation(
                            "references can not point to trees being updated",
                        ))
                        .wrap_with_cost(cost);
                    }
                }
            }
        }
    }
}

#[cfg(feature = "full")]
impl<'db, S, F, G, SR> TreeCache<G, SR> for TreeCacheMerkByPath<S, F>
where
    G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
    SR: FnMut(
        &mut ElementFlags,
        u32,
        u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    fn insert(&mut self, op: &GroveDbOp, is_sum_tree: bool) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut inserted_path = op.path.to_path();
        inserted_path.push(op.key.get_key_clone());
        if !self.merks.contains_key(&inserted_path) {
            let mut merk =
                cost_return_on_error!(&mut cost, (self.get_merk_fn)(&inserted_path, true));
            merk.is_sum_tree = is_sum_tree;
            self.merks.insert(inserted_path, merk);
        }

        Ok(()).wrap_with_cost(cost)
    }

    fn update_base_merk_root_key(&mut self, root_key: Option<Vec<u8>>) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let base_path = vec![];
        let merk_wrapped = self
            .merks
            .remove(&base_path)
            .map(|x| Ok(x).wrap_with_cost(Default::default()))
            .unwrap_or_else(|| (self.get_merk_fn)(&[], false));
        let mut merk = cost_return_on_error!(&mut cost, merk_wrapped);
        merk.set_base_root_key(root_key)
            .add_cost(cost)
            .map_err(|_| Error::InternalError("unable to set base root key"))
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<(CryptoHash, Option<Vec<u8>>, Option<i64>), Error> {
        let mut cost = OperationCost::default();
        // todo: fix this
        let p = path.to_path();
        let path = &p;

        let merk_wrapped = self
            .merks
            .remove(path)
            .map(|x| Ok(x).wrap_with_cost(Default::default()))
            .unwrap_or_else(|| (self.get_merk_fn)(path, false));
        let mut merk = cost_return_on_error!(&mut cost, merk_wrapped);
        let is_sum_tree = merk.is_sum_tree;

        let mut batch_operations: Vec<(Vec<u8>, _)> = vec![];
        for (key_info, op) in ops_at_path_by_key.into_iter() {
            match op {
                Op::Insert { element } | Op::Replace { element } => match &element {
                    Element::Reference(path_reference, element_max_reference_hop, _) => {
                        let merk_feature_type = cost_return_on_error!(
                            &mut cost,
                            element
                                .get_feature_type(is_sum_tree)
                                .wrap_with_cost(OperationCost::default())
                        );
                        let path_iter = path.iter().map(|x| x.as_slice());
                        let path_reference = cost_return_on_error!(
                            &mut cost,
                            path_from_reference_path_type(
                                path_reference.clone(),
                                path_iter,
                                Some(key_info.as_slice())
                            )
                            .wrap_with_cost(OperationCost::default())
                        );
                        if path_reference.len() == 0 {
                            return Err(Error::InvalidBatchOperation(
                                "attempting to insert an empty reference",
                            ))
                            .wrap_with_cost(cost);
                        }

                        let referenced_element_value_hash = cost_return_on_error!(
                            &mut cost,
                            self.follow_reference_get_value_hash(
                                path_reference.as_slice(),
                                ops_by_qualified_paths,
                                element_max_reference_hop.unwrap_or(MAX_REFERENCE_HOPS as u8)
                            )
                        );

                        cost_return_on_error!(
                            &mut cost,
                            element.insert_reference_into_batch_operations(
                                key_info.get_key_clone(),
                                referenced_element_value_hash,
                                &mut batch_operations,
                                merk_feature_type
                            )
                        );
                    }
                    Element::Tree(..) | Element::SumTree(..) => {
                        let merk_feature_type = cost_return_on_error!(
                            &mut cost,
                            element
                                .get_feature_type(is_sum_tree)
                                .wrap_with_cost(OperationCost::default())
                        );
                        cost_return_on_error!(
                            &mut cost,
                            element.insert_subtree_into_batch_operations(
                                key_info.get_key_clone(),
                                NULL_HASH,
                                false,
                                &mut batch_operations,
                                merk_feature_type
                            )
                        );
                    }
                    Element::Item(..) | Element::SumItem(..) => {
                        let merk_feature_type = cost_return_on_error!(
                            &mut cost,
                            element
                                .get_feature_type(is_sum_tree)
                                .wrap_with_cost(OperationCost::default())
                        );
                        if batch_apply_options.validate_insertion_does_not_override {
                            let inserted = cost_return_on_error!(
                                &mut cost,
                                element.insert_if_not_exists_into_batch_operations(
                                    &mut merk,
                                    key_info.get_key(),
                                    &mut batch_operations,
                                    merk_feature_type
                                )
                            );
                            if !inserted {
                                return Err(Error::InvalidBatchOperation(
                                    "attempting to overwrite a tree",
                                ))
                                .wrap_with_cost(cost);
                            }
                        } else {
                            cost_return_on_error!(
                                &mut cost,
                                element.insert_into_batch_operations(
                                    key_info.get_key(),
                                    &mut batch_operations,
                                    merk_feature_type
                                )
                            );
                        }
                    }
                },
                Op::Delete => {
                    cost_return_on_error!(
                        &mut cost,
                        Element::delete_into_batch_operations(
                            key_info.get_key(),
                            false,
                            false,
                            &mut batch_operations
                        )
                    );
                }
                Op::DeleteTree => {
                    cost_return_on_error!(
                        &mut cost,
                        Element::delete_into_batch_operations(
                            key_info.get_key(),
                            true,
                            false,
                            &mut batch_operations
                        )
                    );
                }
                Op::DeleteSumTree => {
                    cost_return_on_error!(
                        &mut cost,
                        Element::delete_into_batch_operations(
                            key_info.get_key(),
                            true,
                            true,
                            &mut batch_operations
                        )
                    );
                }
                Op::ReplaceTreeRootKey {
                    hash,
                    root_key,
                    sum,
                } => {
                    cost_return_on_error!(
                        &mut cost,
                        GroveDb::update_tree_item_preserve_flag_into_batch_operations(
                            &merk,
                            key_info.get_key(),
                            root_key,
                            hash,
                            sum,
                            &mut batch_operations
                        )
                    );
                }
                Op::InsertTreeWithRootHash {
                    hash,
                    root_key,
                    flags,
                    sum,
                } => {
                    let element = match sum {
                        None => Element::new_tree_with_flags(root_key, flags),
                        Some(sum_value) => Element::new_sum_tree_with_flags_and_sum_value(
                            root_key, sum_value, flags,
                        ),
                    };
                    let merk_feature_type =
                        cost_return_on_error_no_add!(&cost, element.get_feature_type(is_sum_tree));

                    cost_return_on_error!(
                        &mut cost,
                        element.insert_subtree_into_batch_operations(
                            key_info.get_key_clone(),
                            hash,
                            false,
                            &mut batch_operations,
                            merk_feature_type
                        )
                    );
                }
            }
        }
        cost_return_on_error!(
            &mut cost,
            merk.apply_unchecked::<_, Vec<u8>, _, _, _>(
                &batch_operations,
                &[],
                Some(batch_apply_options.as_merk_options()),
                &|key, value| {
                    let element = Element::deserialize(value)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let is_sum_tree = element.is_sum_tree();
                    match element {
                        Element::Tree(_, flags) | Element::SumTree(_, _, flags) => {
                            let tree_cost_size = if is_sum_tree {
                                SUM_TREE_COST_SIZE
                            } else {
                                TREE_COST_SIZE
                            };
                            let flags_len = flags.map_or(0, |flags| {
                                let flags_len = flags.len() as u32;
                                flags_len + flags_len.required_space() as u32
                            });
                            let value_len = tree_cost_size + flags_len;
                            let key_len = key.len() as u32;
                            Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                                key_len,
                                value_len,
                                is_sum_tree,
                            ))
                        }
                        _ => Err(MerkError::SpecializedCostsError(
                            "only trees are supported for specialized costs",
                        )),
                    }
                },
                &mut |storage_costs, old_value, new_value| {
                    // todo: change the flags without full deserialization
                    let old_element = Element::deserialize(old_value.as_slice())
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_old_flags = old_element.get_flags_owned();

                    let mut new_element = Element::deserialize(new_value.as_slice())
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_new_flags = new_element.get_flags_mut();
                    match maybe_new_flags {
                        None => Ok((false, None)),
                        Some(new_flags) => {
                            let changed = (flags_update)(storage_costs, maybe_old_flags, new_flags)
                                .map_err(|e| match e {
                                    Error::JustInTimeElementFlagsClientError(_) => {
                                        MerkError::ClientCorruptionError(e.to_string())
                                    }
                                    _ => MerkError::ClientCorruptionError(
                                        "non client error".to_string(),
                                    ),
                                })?;
                            if changed {
                                let flags_len = new_flags.len() as u32;
                                new_value.clone_from(&new_element.serialize().map_err(|e| {
                                    MerkError::ClientCorruptionError(e.to_string())
                                })?);
                                // we need to give back the value defined cost in the case that the
                                // new element is a tree
                                match new_element {
                                    Element::Tree(..) | Element::SumTree(..) => {
                                        let tree_cost_size = if new_element.is_sum_tree() {
                                            SUM_TREE_COST_SIZE
                                        } else {
                                            TREE_COST_SIZE
                                        };
                                        let tree_value_cost = tree_cost_size
                                            + flags_len
                                            + flags_len.required_space() as u32;
                                        Ok((true, Some(tree_value_cost)))
                                    }
                                    _ => Ok((true, None)),
                                }
                            } else {
                                Ok((false, None))
                            }
                        }
                    }
                },
                &mut |value, removed_key_bytes, removed_value_bytes| {
                    let mut element = Element::deserialize(value.as_slice())
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_flags = element.get_flags_mut();
                    match maybe_flags {
                        None => Ok((
                            BasicStorageRemoval(removed_key_bytes),
                            BasicStorageRemoval(removed_value_bytes),
                        )),
                        Some(flags) => {
                            (split_removal_bytes)(flags, removed_key_bytes, removed_value_bytes)
                                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                        }
                    }
                },
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
        );
        let r = merk
            .root_hash_key_and_sum()
            .add_cost(cost)
            .map_err(Error::MerkError);
        // We need to reinsert the merk
        self.merks.insert(path.clone(), merk);
        r
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        ExecuteMode
    }
}

#[cfg(feature = "full")]
impl GroveDb {
    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn apply_batch_structure<C: TreeCache<F, SR>, F, SR>(
        batch_structure: BatchStructure<C, F, SR>,
        batch_apply_options: Option<BatchApplyOptions>,
    ) -> CostResult<(), Error>
    where
        F: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();
        let BatchStructure {
            mut ops_by_level_paths,
            ops_by_qualified_paths,
            mut merk_tree_cache,
            mut flags_update,
            mut split_removal_bytes,
            last_level,
        } = batch_structure;
        let mut current_level = last_level;

        let batch_apply_options = batch_apply_options.unwrap_or_default();

        // We will update up the tree
        while let Some(ops_at_level) = ops_by_level_paths.remove(&current_level) {
            for (path, ops_at_path) in ops_at_level.into_iter() {
                if current_level == 0 {
                    // execute the ops at this path
                    // ignoring sum as root tree cannot be summed
                    let (_root_hash, calculated_root_key, _sum) = cost_return_on_error!(
                        &mut cost,
                        merk_tree_cache.execute_ops_on_path(
                            &path,
                            ops_at_path,
                            &ops_by_qualified_paths,
                            &batch_apply_options,
                            &mut flags_update,
                            &mut split_removal_bytes,
                        )
                    );
                    if batch_apply_options.base_root_storage_is_free {
                        // the base root is free
                        let mut update_root_cost = cost_return_on_error_no_add!(
                            &cost,
                            merk_tree_cache
                                .update_base_merk_root_key(calculated_root_key)
                                .cost_as_result()
                        );
                        update_root_cost.storage_cost = StorageCost::default();
                        cost.add_assign(update_root_cost);
                    } else {
                        cost_return_on_error!(
                            &mut cost,
                            merk_tree_cache.update_base_merk_root_key(calculated_root_key)
                        );
                    }
                } else {
                    let (root_hash, calculated_root_key, sum) = cost_return_on_error!(
                        &mut cost,
                        merk_tree_cache.execute_ops_on_path(
                            &path,
                            ops_at_path,
                            &ops_by_qualified_paths,
                            &batch_apply_options,
                            &mut flags_update,
                            &mut split_removal_bytes,
                        )
                    );

                    if current_level > 0 {
                        // We need to propagate up this root hash, this means adding grove_db
                        // operations up for the level above
                        if let Some((key, parent_path)) = path.split_last() {
                            if let Some(ops_at_level_above) =
                                ops_by_level_paths.get_mut(&(current_level - 1))
                            {
                                // todo: fix this hack
                                let parent_path = KeyInfoPath(parent_path.to_vec());
                                if let Some(ops_on_path) = ops_at_level_above.get_mut(&parent_path)
                                {
                                    match ops_on_path.entry(key.clone()) {
                                        Entry::Vacant(vacant_entry) => {
                                            vacant_entry.insert(Op::ReplaceTreeRootKey {
                                                hash: root_hash,
                                                root_key: calculated_root_key,
                                                sum,
                                            });
                                        }
                                        Entry::Occupied(occupied_entry) => {
                                            let mutable_occupied_entry = occupied_entry.into_mut();
                                            match mutable_occupied_entry {
                                                Op::ReplaceTreeRootKey {
                                                    hash,
                                                    root_key,
                                                    sum,
                                                } => {
                                                    *hash = root_hash;
                                                    *root_key = calculated_root_key;
                                                    *sum = sum.clone();
                                                }
                                                Op::InsertTreeWithRootHash { .. } => {
                                                    return Err(Error::CorruptedCodeExecution(
                                                        "we can not do this operation twice",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                Op::Insert { element }
                                                | Op::Replace { element } => {
                                                    if let Element::Tree(_, flags) = element {
                                                        *mutable_occupied_entry =
                                                            Op::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                sum: None,
                                                            };
                                                    } else if let Element::SumTree(.., flags) =
                                                        element
                                                    {
                                                        *mutable_occupied_entry =
                                                            Op::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                sum,
                                                            };
                                                    } else {
                                                        return Err(Error::InvalidBatchOperation(
                                                            "insertion of element under a non tree",
                                                        ))
                                                        .wrap_with_cost(cost);
                                                    }
                                                }
                                                Op::Delete | Op::DeleteTree | Op::DeleteSumTree => {
                                                    if calculated_root_key.is_some() {
                                                        return Err(Error::InvalidBatchOperation(
                                                            "modification of tree when it will be \
                                                             deleted",
                                                        ))
                                                        .wrap_with_cost(cost);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    let mut ops_on_path: BTreeMap<KeyInfo, Op> = BTreeMap::new();
                                    ops_on_path.insert(
                                        key.clone(),
                                        Op::ReplaceTreeRootKey {
                                            hash: root_hash,
                                            root_key: calculated_root_key,
                                            sum,
                                        },
                                    );
                                    ops_at_level_above.insert(parent_path, ops_on_path);
                                }
                            } else {
                                let mut ops_on_path: BTreeMap<KeyInfo, Op> = BTreeMap::new();
                                ops_on_path.insert(
                                    key.clone(),
                                    Op::ReplaceTreeRootKey {
                                        hash: root_hash,
                                        root_key: calculated_root_key,
                                        sum,
                                    },
                                );
                                let mut ops_on_level: BTreeMap<KeyInfoPath, BTreeMap<KeyInfo, Op>> =
                                    BTreeMap::new();
                                ops_on_level.insert(KeyInfoPath(parent_path.to_vec()), ops_on_path);
                                ops_by_level_paths.insert(current_level - 1, ops_on_level);
                            }
                        }
                    }
                }
            }
            if current_level > 0 {
                current_level -= 1;
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn apply_body<'db, S: StorageContext<'db>>(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removed_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        get_merk_fn: impl FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::from_ops(
                ops,
                update_element_flags_function,
                split_removed_bytes_function,
                TreeCacheMerkByPath {
                    merks: Default::default(),
                    get_merk_fn,
                }
            )
        );
        Self::apply_batch_structure(batch_structure, batch_apply_options).add_cost(cost)
    }

    /// Applies operations on GroveDB without batching
    pub fn apply_operations_without_batching(
        &self,
        ops: Vec<GroveDbOp>,
        options: Option<BatchApplyOptions>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        for op in ops.into_iter() {
            match op.op {
                Op::Insert { element } | Op::Replace { element } => {
                    let path_slices: Vec<&[u8]> = op.path.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.insert(
                            path_slices,
                            op.key.as_slice(),
                            element.to_owned(),
                            options.clone().map(|o| o.as_insert_options()),
                            transaction,
                        )
                    );
                }
                Op::Delete => {
                    let path_slices: Vec<&[u8]> = op.path.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.delete(
                            path_slices,
                            op.key.as_slice(),
                            options.clone().map(|o| o.as_delete_options()),
                            transaction
                        )
                    );
                }
                _ => {}
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    pub fn apply_batch(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        self.apply_batch_with_element_flags_update(
            ops,
            batch_apply_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            transaction,
        )
    }

    pub fn open_batch_transactional_merk_at_path<'db, 'p, P>(
        &'db self,
        storage_batch: &'db StorageBatch,
        path: P,
        tx: &'db Transaction,
        new_merk: bool,
    ) -> CostResult<Merk<PrefixedRocksDbBatchTransactionContext<'db>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + Clone,
    {
        let mut path_iter = path.into_iter();
        let mut cost = OperationCost::default();
        let storage = self
            .db
            .get_batch_transactional_storage_context(path_iter.clone(), &storage_batch, tx)
            .unwrap_add_cost(&mut cost);

        match path_iter.next_back() {
            Some(key) => {
                if new_merk {
                    // TODO: can this be a sum tree
                    Ok(Merk::open_empty(storage, MerkType::LayeredMerk, false)).wrap_with_cost(cost)
                } else {
                    let parent_storage = self
                        .db
                        .get_transactional_storage_context(path_iter.clone(), tx)
                        .unwrap_add_cost(&mut cost);
                    let element = cost_return_on_error!(
                        &mut cost,
                        Element::get_from_storage(&parent_storage, key).map_err(|_| {
                            Error::InvalidPath(format!(
                                "could not get key for parent of subtree for batch at path {}",
                                path_iter.map(hex::encode).join("/")
                            ))
                        })
                    );
                    let is_sum_tree = element.is_sum_tree();
                    if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                        Merk::open_layered_with_root_key(storage, root_key, is_sum_tree)
                            .map_err(|_| {
                                Error::CorruptedData(
                                    "cannot open a subtree with given root key".to_owned(),
                                )
                            })
                            .add_cost(cost)
                    } else {
                        Err(Error::CorruptedPath(
                            "cannot open a subtree as parent exists but is not a tree",
                        ))
                        .wrap_with_cost(OperationCost::default())
                    }
                }
            }
            None => {
                if new_merk {
                    Ok(Merk::open_empty(storage, MerkType::BaseMerk, false)).wrap_with_cost(cost)
                } else {
                    Merk::open_base(storage, false)
                        .map_err(|_| {
                            Error::CorruptedData("cannot open a the root subtree".to_owned())
                        })
                        .add_cost(cost)
                }
            }
        }
    }

    pub fn open_batch_merk_at_path<'a>(
        &'a self,
        storage_batch: &'a StorageBatch,
        path: &[Vec<u8>],
        new_merk: bool,
    ) -> CostResult<Merk<PrefixedRocksDbBatchStorageContext>, Error> {
        let mut local_cost = OperationCost::default();
        let storage = self
            .db
            .get_batch_storage_context(path.iter().map(|x| x.as_slice()), storage_batch)
            .unwrap_add_cost(&mut local_cost);

        if new_merk {
            let merk_type = if path.is_empty() {
                MerkType::BaseMerk
            } else {
                MerkType::LayeredMerk
            };
            Ok(Merk::open_empty(storage, merk_type, false)).wrap_with_cost(local_cost)
        } else {
            if let Some((last, base_path)) = path.split_last() {
                let parent_storage = self
                    .db
                    .get_storage_context(base_path.iter().map(|x| x.as_slice()))
                    .unwrap_add_cost(&mut local_cost);
                let element = cost_return_on_error!(
                    &mut local_cost,
                    Element::get_from_storage(&parent_storage, last)
                );
                let is_sum_tree = element.is_sum_tree();
                if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                    Merk::open_layered_with_root_key(storage, root_key, is_sum_tree)
                        .map_err(|_| {
                            Error::CorruptedData(
                                "cannot open a subtree with given root key".to_owned(),
                            )
                        })
                        .add_cost(local_cost)
                } else {
                    Err(Error::CorruptedData(
                        "cannot open a subtree as parent exists but is not a tree".to_owned(),
                    ))
                    .wrap_with_cost(local_cost)
                }
            } else {
                Merk::open_base(storage, false)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
                    .add_cost(local_cost)
            }
        }
    }

    /// Applies batch of operations on GroveDB
    pub fn apply_batch_with_element_flags_update(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        // Determines whether to check batch operation consistency
        // return false if the disable option is set to true, returns true for any other
        // case
        let check_batch_operation_consistency = batch_apply_options
            .as_ref()
            .map(|batch_options| !batch_options.disable_operation_consistency_check)
            .unwrap_or(true);

        if check_batch_operation_consistency {
            let consistency_result = GroveDbOp::verify_consistency_of_operations(&ops);
            if !consistency_result.is_empty() {
                return Err(Error::InvalidBatchOperation(
                    "batch operations fail consistency checks",
                ))
                .wrap_with_cost(cost);
            }
        }

        // `StorageBatch` allows us to collect operations on different subtrees before
        // execution
        let storage_batch = StorageBatch::new();

        // With the only one difference (if there is a transaction) do the following:
        // 2. If nothing left to do and we were on a non-leaf subtree or we're done with
        //    one subtree and moved to another then add propagation operation to the
        //    operations tree and drop Merk handle;
        // 3. Take Merk from temp subtrees or open a new one with batched storage_cost
        //    context;
        // 4. Apply operation to the Merk;
        // 5. Remove operation from the tree, repeat until there are operations to do;
        // 6. Add root leaves save operation to the batch
        // 7. Apply storage_cost batch
        if let Some(tx) = transaction {
            cost_return_on_error!(
                &mut cost,
                self.apply_body(
                    ops,
                    batch_apply_options,
                    update_element_flags_function,
                    split_removal_bytes_function,
                    |path, new_merk| {
                        self.open_batch_transactional_merk_at_path(
                            &storage_batch,
                            path.iter().map(|x| x.as_slice()),
                            &tx,
                            new_merk,
                        )
                    }
                )
            );

            // TODO: compute batch costs
            cost_return_on_error_no_add!(
                &cost,
                self.db
                    .commit_multi_context_batch(storage_batch, Some(tx))
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );
        } else {
            cost_return_on_error!(
                &mut cost,
                self.apply_body(
                    ops,
                    batch_apply_options,
                    update_element_flags_function,
                    split_removal_bytes_function,
                    |path, new_merk| {
                        self.open_batch_merk_at_path(&storage_batch, path, new_merk)
                    }
                )
            );

            // TODO: compute batch costs
            cost_return_on_error_no_add!(
                &cost,
                self.db
                    .commit_multi_context_batch(storage_batch, None)
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    pub fn estimated_case_operations_for_batch(
        estimated_costs_type: EstimatedCostsType,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        match estimated_costs_type {
            EstimatedCostsType::AverageCaseCostsType(estimated_layer_information) => {
                let batch_structure = cost_return_on_error!(
                    &mut cost,
                    BatchStructure::from_ops(
                        ops,
                        update_element_flags_function,
                        split_removal_bytes_function,
                        AverageCaseTreeCacheKnownPaths::new_with_estimated_layer_information(
                            estimated_layer_information
                        )
                    )
                );
                cost_return_on_error!(
                    &mut cost,
                    Self::apply_batch_structure(batch_structure, batch_apply_options)
                );
            }

            EstimatedCostsType::WorstCaseCostsType(worst_case_layer_information) => {
                let batch_structure = cost_return_on_error!(
                    &mut cost,
                    BatchStructure::from_ops(
                        ops,
                        update_element_flags_function,
                        split_removal_bytes_function,
                        WorstCaseTreeCacheKnownPaths::new_with_worst_case_layer_information(
                            worst_case_layer_information
                        )
                    )
                );
                cost_return_on_error!(
                    &mut cost,
                    Self::apply_batch_structure(batch_structure, batch_apply_options)
                );
            }
        }

        Ok(()).wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    use merk::proofs::Query;

    use super::*;
    use crate::{
        reference_path::ReferencePathType,
        tests::{make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        PathQuery,
    };

    #[test]
    fn test_batch_validation_ok() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            GroveDbOp::insert_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element2.clone(),
            ),
        ];
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect("cannot apply batch");

        // visualize_stderr(&db);
        db.get([], b"key1", None)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref()], b"key2", None)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2"], b"key3", None)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
            .unwrap()
            .expect("cannot get element");

        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
                .unwrap()
                .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF, b"key1"], b"key2", None)
                .unwrap()
                .expect("cannot get element"),
            element2
        );
    }

    #[test]
    fn test_batch_operation_consistency_checker() {
        let db = make_test_grovedb();

        // No two operations should be the same
        let ops = vec![
            GroveDbOp::insert_op(vec![b"a".to_vec()], b"b".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(vec![b"a".to_vec()], b"b".to_vec(), Element::empty_tree()),
        ];
        assert!(matches!(
            db.apply_batch(ops, None, None).unwrap(),
            Err(Error::InvalidBatchOperation(
                "batch operations fail consistency checks"
            ))
        ));

        // Can't perform 2 or more operations on the same node
        let ops = vec![
            GroveDbOp::insert_op(
                vec![b"a".to_vec()],
                b"b".to_vec(),
                Element::new_item(vec![1]),
            ),
            GroveDbOp::insert_op(vec![b"a".to_vec()], b"b".to_vec(), Element::empty_tree()),
        ];
        assert!(matches!(
            db.apply_batch(ops, None, None).unwrap(),
            Err(Error::InvalidBatchOperation(
                "batch operations fail consistency checks"
            ))
        ));

        // Can't insert under a deleted path
        let ops = vec![
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::new_item(vec![1]),
            ),
            GroveDbOp::delete_op(vec![], TEST_LEAF.to_vec()),
        ];
        assert!(matches!(
            db.apply_batch(ops, None, None).unwrap(),
            Err(Error::InvalidBatchOperation(
                "batch operations fail consistency checks"
            ))
        ));

        // Should allow invalid operations pass when disable option is set to true
        let ops = vec![
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(matches!(
            db.apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: false,
                    validate_insertion_does_not_override_tree: true,
                    allow_deleting_non_empty_trees: false,
                    deleting_non_empty_trees_returns_error: true,
                    disable_operation_consistency_check: true,
                    base_root_storage_is_free: true,
                }),
                None
            )
            .unwrap(),
            Ok(_)
        ));
    }

    #[test]
    fn test_batch_validation_ok_on_transaction() {
        let db = make_test_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"keyb", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            GroveDbOp::insert_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element2.clone(),
            ),
        ];
        db.apply_batch(ops, None, Some(&tx))
            .unwrap()
            .expect("cannot apply batch");
        db.get([], b"keyb", None)
            .unwrap()
            .expect_err("we should not get an element");
        db.get([], b"keyb", Some(&tx))
            .unwrap()
            .expect("we should get an element");

        db.get([], b"key1", None)
            .unwrap()
            .expect_err("we should not get an element");
        db.get([], b"key1", Some(&tx))
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref()], b"key2", Some(&tx))
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2"], b"key3", Some(&tx))
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", Some(&tx))
                .unwrap()
                .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF, b"key1"], b"key2", Some(&tx))
                .unwrap()
                .expect("cannot get element"),
            element2
        );
    }

    #[test]
    fn test_batch_add_other_element_in_sub_tree() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        // let's start by inserting a tree structure
        let ops = vec![
            GroveDbOp::insert_op(vec![], b"1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(
                vec![b"1".to_vec()],
                b"my_contract".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"1".to_vec(), b"my_contract".to_vec()],
                b"0".to_vec(),
                Element::new_item(b"this is the contract".to_vec()),
            ),
            GroveDbOp::insert_op(
                vec![b"1".to_vec(), b"my_contract".to_vec()],
                b"1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"1".to_vec(), b"my_contract".to_vec(), b"1".to_vec()],
                b"person".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                ],
                b"0".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                ],
                b"message".to_vec(),
                Element::empty_tree(),
            ),
        ];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
        )
        .unwrap()
        .expect("expected to do tree form insert");

        let some_element_flags = Some(vec![0]);

        // now let's add an item
        let ops = vec![
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"0".to_vec(),
                ],
                b"sam".to_vec(),
                Element::new_item_with_flags(
                    b"Samuel Westrich".to_vec(),
                    some_element_flags.clone(),
                ),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                ],
                b"my apples are safe".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"my apples are safe".to_vec(),
                ],
                b"0".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"my apples are safe".to_vec(),
                    b"0".to_vec(),
                ],
                b"sam".to_vec(),
                Element::new_reference_with_max_hops_and_flags(
                    ReferencePathType::UpstreamRootHeightReference(
                        4,
                        vec![b"0".to_vec(), b"sam".to_vec()],
                    ),
                    Some(2),
                    some_element_flags.clone(),
                ),
            ),
        ];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
        )
        .unwrap()
        .expect("expected to do first insert");

        // now let's add an item
        let ops = vec![
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"0".to_vec(),
                ],
                b"wisdom".to_vec(),
                Element::new_item_with_flags(b"Wisdom Ogwu".to_vec(), some_element_flags.clone()),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                ],
                b"canteloupe!".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"canteloupe!".to_vec(),
                ],
                b"0".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            GroveDbOp::insert_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"canteloupe!".to_vec(),
                    b"0".to_vec(),
                ],
                b"wisdom".to_vec(),
                Element::new_reference_with_max_hops_and_flags(
                    ReferencePathType::UpstreamRootHeightReference(
                        4,
                        vec![b"0".to_vec(), b"wisdom".to_vec()],
                    ),
                    Some(2),
                    some_element_flags.clone(),
                ),
            ),
        ];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |cost, _old_flags, _new_flags| {
                // we should only either have nodes that are completely replaced (inner_trees)
                // or added
                assert!((cost.added_bytes > 0) ^ (cost.replaced_bytes > 0));
                Ok(false)
            },
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
        )
        .unwrap()
        .expect("successful batch apply");
    }

    fn grove_db_ops_for_contract_insert() -> Vec<GroveDbOp> {
        let mut grove_db_ops = vec![];

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![],
            b"contract".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec()],
            (&[0u8]).to_vec(),
            Element::new_item(b"serialized_contract".to_vec()),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec()],
            (&[1u8]).to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec()],
            b"domain".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            (&[0u8]).to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            b"normalized_domain_label".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            b"unique_records".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            b"alias_records".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec()],
            b"preorder".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"preorder".to_vec(),
            ],
            (&[0u8]).to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"preorder".to_vec(),
            ],
            b"salted_domain".to_vec(),
            Element::empty_tree(),
        ));

        grove_db_ops
    }

    fn grove_db_ops_for_contract_document_insert() -> Vec<GroveDbOp> {
        let mut grove_db_ops = vec![];

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                (&[0u8]).to_vec(),
            ],
            b"serialized_domain_id".to_vec(),
            Element::new_item(b"serialized_domain".to_vec()),
        ));

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
            ],
            b"dash".to_vec(),
            Element::empty_tree(),
        ));

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
                b"dash".to_vec(),
            ],
            b"normalized_label".to_vec(),
            Element::empty_tree(),
        ));

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
                b"dash".to_vec(),
                b"normalized_label".to_vec(),
            ],
            b"sam".to_vec(),
            Element::empty_tree(),
        ));

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
                b"dash".to_vec(),
                b"normalized_label".to_vec(),
                b"sam".to_vec(),
            ],
            b"sam_id".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                (&[0u8]).to_vec(),
                b"serialized_domain_id".to_vec(),
            ])),
        ));
        grove_db_ops
    }

    // This test no longer works as of version 5, there might be support for it in
    // the future
    #[ignore]
    #[test]
    fn test_batch_produces_same_result() {
        let db = make_test_grovedb();
        let tx = db.start_transaction();

        let ops = grove_db_ops_for_contract_insert();
        db.apply_batch(ops, None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");

        db.root_hash(None).unwrap().expect("cannot get root hash");

        let db = make_test_grovedb();
        let tx = db.start_transaction();

        let ops = grove_db_ops_for_contract_insert();
        db.apply_batch(ops.clone(), None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");

        let batch_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("cannot get root hash");

        db.rollback_transaction(&tx).expect("expected to rollback");

        db.apply_operations_without_batching(ops, None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");

        let no_batch_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("cannot get root hash");

        assert_eq!(batch_hash, no_batch_hash);
    }

    #[ignore]
    #[test]
    fn test_batch_contract_with_document_produces_same_result() {
        let db = make_test_grovedb();
        let tx = db.start_transaction();

        let ops = grove_db_ops_for_contract_insert();
        db.apply_batch(ops, None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");

        db.root_hash(None).unwrap().expect("cannot get root hash");

        let db = make_test_grovedb();
        let tx = db.start_transaction();

        let ops = grove_db_ops_for_contract_insert();
        let document_ops = grove_db_ops_for_contract_document_insert();
        db.apply_batch(ops.clone(), None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");
        db.apply_batch(document_ops.clone(), None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");

        let batch_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("cannot get root hash");

        db.rollback_transaction(&tx).expect("expected to rollback");

        db.apply_operations_without_batching(ops, None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");
        db.apply_operations_without_batching(document_ops, None, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");

        let no_batch_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("cannot get root hash");

        assert_eq!(batch_hash, no_batch_hash);
    }

    #[test]
    fn test_batch_validation_broken_chain() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
        assert!(db.get([b"key1".as_ref()], b"key2", None).unwrap().is_err());
    }

    #[test]
    fn test_batch_validation_broken_chain_aborts_whole_batch() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
        assert!(db.get([b"key1".as_ref()], b"key2", None).unwrap().is_err());
        assert!(db
            .get([TEST_LEAF, b"key1"], b"key2", None)
            .unwrap()
            .is_err(),);
    }

    #[test]
    fn test_batch_validation_deletion_brokes_chain() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert([], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert(
            [b"key1".as_ref()],
            b"key2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert a subtree");

        let ops = vec![
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::delete_op(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
    }

    #[test]
    fn test_batch_validation_insertion_under_deleted_tree() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::delete_op(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect_err("insertion of element under a deleted tree should not be allowed");
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
            .unwrap()
            .expect_err("nothing should have been inserted");
    }

    #[test]
    fn test_batch_validation_insert_into_existing_tree() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert([TEST_LEAF], b"invalid", element.clone(), None, None)
            .unwrap()
            .expect("cannot insert value");
        db.insert([TEST_LEAF], b"valid", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert value");

        // Insertion into scalar is invalid
        let ops = vec![GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"invalid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());

        // Insertion into a tree is correct
        let ops = vec![GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"valid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect("cannot apply batch");
        assert_eq!(
            db.get([TEST_LEAF, b"valid"], b"key1", None)
                .unwrap()
                .expect("cannot get element"),
            element
        );
    }

    #[test]
    fn test_batch_validation_nested_subtree_overwrite() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        db.insert(
            [TEST_LEAF],
            b"key_subtree",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert a subtree");
        db.insert([TEST_LEAF, b"key_subtree"], b"key2", element, None, None)
            .unwrap()
            .expect("cannot insert an item");

        // TEST_LEAF can not be overwritten
        let ops = vec![
            GroveDbOp::insert_op(vec![], TEST_LEAF.to_vec(), element2.clone()),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec(), b"key_subtree".to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true,
                    validate_insertion_does_not_override_tree: true,
                    allow_deleting_non_empty_trees: false,
                    deleting_non_empty_trees_returns_error: true,
                    disable_operation_consistency_check: false,
                    base_root_storage_is_free: true,
                }),
                None
            )
            .unwrap()
            .is_err());

        // TEST_LEAF will be deleted so you can not insert underneath it
        let ops = vec![
            GroveDbOp::delete_op(vec![], TEST_LEAF.to_vec()),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());

        // TEST_LEAF will be deleted so you can not insert underneath it
        // We are testing with the batch apply option
        // validate_tree_insertion_does_not_override set to true
        let ops = vec![
            GroveDbOp::delete_op(vec![], TEST_LEAF.to_vec()),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    disable_operation_consistency_check: false,
                    validate_insertion_does_not_override_tree: true,
                    allow_deleting_non_empty_trees: false,
                    validate_insertion_does_not_override: true,
                    deleting_non_empty_trees_returns_error: true,
                    base_root_storage_is_free: true,
                }),
                None
            )
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_batch_validation_root_leaf_removal() {
        let db = make_test_grovedb();
        let ops = vec![
            GroveDbOp::insert_op(
                vec![],
                TEST_LEAF.to_vec(),
                Element::new_item(b"ayy".to_vec()),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true,
                    validate_insertion_does_not_override_tree: true,
                    allow_deleting_non_empty_trees: false,
                    deleting_non_empty_trees_returns_error: true,
                    disable_operation_consistency_check: false,
                    base_root_storage_is_free: true,
                }),
                None
            )
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_merk_data_is_deleted() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([TEST_LEAF, b"key1"], b"key2", element.clone(), None, None)
            .unwrap()
            .expect("cannot insert an item");
        let ops = vec![GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"ayy2".to_vec()),
        )];

        assert_eq!(
            db.get([TEST_LEAF, b"key1"], b"key2", None)
                .unwrap()
                .expect("cannot get item"),
            element
        );
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect("cannot apply batch");
        assert!(db
            .get([TEST_LEAF, b"key1"], b"key2", None)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_multi_tree_insertion_deletion_with_propagation_no_tx() {
        let db = make_test_grovedb();
        db.insert([], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert([], b"key2", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert(
            [ANOTHER_TEST_LEAF],
            b"key1",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert root leaf");

        let hash = db.root_hash(None).unwrap().expect("cannot get root hash");
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());

        let ops = vec![
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(vec![TEST_LEAF.to_vec()], b"key".to_vec(), element2.clone()),
            GroveDbOp::delete_op(vec![ANOTHER_TEST_LEAF.to_vec()], b"key1".to_vec()),
        ];
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect("cannot apply batch");

        assert!(db.get([ANOTHER_TEST_LEAF], b"key1", None).unwrap().is_err());

        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
                .unwrap()
                .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF], b"key", None)
                .unwrap()
                .expect("cannot get element"),
            element2
        );
        assert_ne!(
            db.root_hash(None).unwrap().expect("cannot get root hash"),
            hash
        );

        // verify root leaves
        assert!(db.get([], TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get([], ANOTHER_TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get([], b"key1", None).unwrap().is_ok());
        assert!(db.get([], b"key2", None).unwrap().is_ok());
        assert!(db.get([], b"key3", None).unwrap().is_err());
    }

    #[test]
    fn test_nested_batch_insertion_corrupts_state() {
        let db = make_test_grovedb();
        let full_path = vec![
            b"leaf1".to_vec(),
            b"sub1".to_vec(),
            b"sub2".to_vec(),
            b"sub3".to_vec(),
            b"sub4".to_vec(),
            b"sub5".to_vec(),
        ];
        let mut acc_path: Vec<Vec<u8>> = vec![];
        for p in full_path.into_iter() {
            db.insert(
                acc_path.iter().map(|x| x.as_slice()),
                &p,
                Element::empty_tree(),
                None,
                None,
            )
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert_op(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None)
            .unwrap()
            .expect("cannot apply batch");

        let batch = vec![GroveDbOp::insert_op(
            acc_path,
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None)
            .unwrap()
            .expect("cannot apply same batch twice");
    }

    #[test]
    fn test_apply_sorted_pre_validated_batch_propagation() {
        let db = make_test_grovedb();
        let full_path = vec![b"leaf1".to_vec(), b"sub1".to_vec()];
        let mut acc_path: Vec<Vec<u8>> = vec![];
        for p in full_path.into_iter() {
            db.insert(
                acc_path.iter().map(|x| x.as_slice()),
                &p,
                Element::empty_tree(),
                None,
                None,
            )
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let root_hash = db.root_hash(None).unwrap().unwrap();

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert_op(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None)
            .unwrap()
            .expect("cannot apply batch");

        assert_ne!(db.root_hash(None).unwrap().unwrap(), root_hash);
    }

    #[test]
    fn test_references() {
        // insert reference that points to non-existent item
        let db = make_test_grovedb();
        let batch = vec![GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"invalid_path".to_vec(),
            ])),
        )];
        assert!(matches!(
            db.apply_batch(batch, None, None).unwrap(),
            Err(Error::MissingReference(String { .. }))
        ));

        // insert reference with item it points to in the same batch
        let db = make_test_grovedb();
        let elem = Element::new_item(b"ayy".to_vec());
        let batch = vec![
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"invalid_path".to_vec(),
                ])),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"invalid_path".to_vec(),
                elem.clone(),
            ),
        ];
        assert!(matches!(db.apply_batch(batch, None, None).unwrap(), Ok(_)));
        assert_eq!(db.get([TEST_LEAF], b"key1", None).unwrap().unwrap(), elem);

        // should successfully prove reference as the value hash is valid
        let mut reference_key_query = Query::new();
        reference_key_query.insert_key(b"key1".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], reference_key_query);
        let proof = db
            .prove_query(&path_query)
            .unwrap()
            .expect("should generate proof");
        let verification_result = GroveDb::verify_query(&proof, &path_query);
        assert!(matches!(verification_result, Ok(_)));

        // Hit reference limit when you specify max reference hop, lower than actual hop
        // count
        let db = make_test_grovedb();
        let elem = Element::new_item(b"ayy".to_vec());
        let batch = vec![
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key2".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"key1".to_vec(),
                    ]),
                    Some(1),
                ),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"invalid_path".to_vec(),
                    ]),
                    Some(1),
                ),
            ),
            GroveDbOp::insert_op(
                vec![TEST_LEAF.to_vec()],
                b"invalid_path".to_vec(),
                elem.clone(),
            ),
        ];
        assert!(matches!(
            db.apply_batch(batch, None, None).unwrap(),
            Err(Error::ReferenceLimit)
        ));
    }
}
