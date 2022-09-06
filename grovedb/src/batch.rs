//! GroveDB batch operations support

use core::fmt;
use std::{
    cmp::Ordering,
    collections::{btree_map::Entry, BTreeMap, HashMap, HashSet},
    hash::{Hash, Hasher},
    slice::Iter,
    vec::IntoIter,
};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        removal::{
            StorageRemovedBytes,
            StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval},
        },
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
use merk::{tree::value_hash, CryptoHash, Merk};
use nohash_hasher::IntMap;
use storage::{
    rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength, Storage, StorageBatch,
    StorageContext,
};
use visualize::{DebugByteVectors, DebugBytes, Drawer, Visualize};

use crate::{
    batch::{GroveDbOpMode::WorstCaseOp, KeyInfo::KnownKey},
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{path_from_reference_path_type, path_from_reference_qualified_path_type},
    worst_case_costs::MerkWorstCaseInput,
    Element, ElementFlags, Error, GroveDb, TransactionArg, MAX_ELEMENTS_NUMBER,
};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Op {
    ReplaceTreeHash { hash: [u8; 32] },
    Insert { element: Element },
    Delete,
}

impl Op {
    fn worst_case_cost(&self, key: &KeyInfo, input: MerkWorstCaseInput) -> OperationCost {
        match self {
            Op::ReplaceTreeHash { .. } => OperationCost {
                seek_count: 1,
                storage_cost: StorageCost {
                    added_bytes: 32,
                    ..Default::default()
                },
                ..Default::default()
            },
            Op::Insert { element } => {
                let mut cost = OperationCost::default();
                GroveDb::add_worst_case_merk_insert(&mut cost, key, &element, input);
                cost
            }
            Op::Delete => {
                let mut cost = OperationCost::default();
                GroveDb::add_worst_case_merk_propagate(&mut cost, input);
                cost
            }
        }
    }
}

impl PartialOrd for Op {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Op::Delete, Op::Insert { .. }) => Some(Ordering::Less),
            (Op::Insert { .. }, Op::Delete) => Some(Ordering::Greater),
            _ => Some(Ordering::Equal),
        }
    }
}

impl Ord for Op {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).expect("all ops have order")
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyInfo {
    KnownKey(Vec<u8>),
    MaxKeySize { unique_id: Vec<u8>, max_size: u8 },
}

impl PartialOrd<Self> for KeyInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match self.get_key_ref().partial_cmp(other.get_key_ref()) {
            None => None,
            Some(ord) => match ord {
                Ordering::Less => Some(Ordering::Less),
                Ordering::Equal => {
                    let other_len = other.len();
                    match self.len().partial_cmp(&other_len) {
                        None => Some(Ordering::Equal),
                        Some(ord) => Some(ord),
                    }
                }
                Ordering::Greater => Some(Ordering::Less),
            },
        }
    }
}

impl Ord for KeyInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.get_key_ref().cmp(other.get_key_ref())
    }
}

impl Hash for KeyInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            KnownKey(k) => k.hash(state),
            KeyInfo::MaxKeySize {
                unique_id,
                max_size,
            } => {
                unique_id.hash(state);
                max_size.hash(state);
            }
        }
    }
}

impl WorstKeyLength for KeyInfo {
    fn len(&self) -> u8 {
        match self {
            Self::KnownKey(key) => key.len() as u8,
            Self::MaxKeySize { max_size, .. } => *max_size,
        }
    }
}

impl KeyInfo {
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Self::KnownKey(key) => key.as_slice(),
            Self::MaxKeySize { unique_id, .. } => unique_id.as_slice(),
        }
    }

    fn get_key(self) -> Vec<u8> {
        match self {
            Self::KnownKey(key) => key,
            Self::MaxKeySize { unique_id, .. } => unique_id,
        }
    }

    fn get_key_clone(&self) -> Vec<u8> {
        match self {
            Self::KnownKey(key) => key.clone(),
            Self::MaxKeySize { unique_id, .. } => unique_id.clone(),
        }
    }

    fn get_key_ref(&self) -> &[u8] {
        match self {
            Self::KnownKey(key) => key.as_slice(),
            Self::MaxKeySize { unique_id, .. } => unique_id.as_slice(),
        }
    }
}

impl Visualize for KeyInfo {
    fn visualize<W: std::io::Write>(&self, mut drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
        match self {
            KnownKey(k) => {
                drawer.write(b"key: ")?;
                drawer = k.visualize(drawer)?;
            }
            KeyInfo::MaxKeySize {
                unique_id,
                max_size,
            } => {
                drawer.write(b"max_size_key: ")?;
                drawer = unique_id.visualize(drawer)?;
                drawer.write(format!(", max_size: {max_size}").as_bytes())?;
            }
        }
        Ok(drawer)
    }
}

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct KnownKeysPath(Vec<Vec<u8>>);

impl Hash for KnownKeysPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl PartialEq<KeyInfoPath> for KnownKeysPath {
    fn eq(&self, other: &KeyInfoPath) -> bool {
        self.0 == other.to_path_refs()
    }
}

impl PartialEq<Vec<Vec<u8>>> for KnownKeysPath {
    fn eq(&self, other: &Vec<Vec<u8>>) -> bool {
        self.0 == other.as_slice()
    }
}

#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Debug)]
pub struct KeyInfoPath(Vec<KeyInfo>);

impl Hash for KeyInfoPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl Visualize for KeyInfoPath {
    fn visualize<W: std::io::Write>(&self, mut drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
        drawer.write(b"path: ")?;
        let mut path_out = Vec::new();
        let mut path_drawer = Drawer::new(&mut path_out);
        for k in &self.0 {
            path_drawer = k.visualize(path_drawer).unwrap();
            path_drawer.write(b" ").unwrap();
        }
        Ok(drawer)
    }
}

impl KeyInfoPath {
    pub fn from_vec(vec: Vec<KeyInfo>) -> Self {
        KeyInfoPath(vec)
    }

    pub fn to_path_consume(self) -> Vec<Vec<u8>> {
        self.0.into_iter().map(|k| k.get_key()).collect()
    }

    pub fn to_path(&self) -> Vec<Vec<u8>> {
        self.0.iter().map(|k| k.get_key_clone()).collect()
    }

    pub fn to_path_refs(&self) -> Vec<&[u8]> {
        self.0.iter().map(|k| k.get_key_ref()).collect()
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

/// Batch operation
#[derive(Clone, PartialEq)]
pub enum GroveDbOpMode {
    RunOp,
    WorstCaseOp,
}

/// Batch operation
#[derive(Clone, PartialEq)]
pub struct GroveDbOp {
    /// Path to a subtree - subject to an operation
    pub path: KeyInfoPath,
    /// Key of an element in the subtree
    pub key: KeyInfo,
    /// Operation to perform on the key
    pub op: Op,
    /// Mode of operation, run or worst case
    pub mode: GroveDbOpMode,
    // /// Holds the path based on key info in path
    // resolved_path: Vec<Vec<u8>>,
    // /// Holds the key based on key info
    // resolved_key: Vec<u8>,
}

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
            },
            Op::Delete => "Delete",
            Op::ReplaceTreeHash { .. } => "Replace Tree Hash",
        };

        f.debug_struct("GroveDbOp")
            .field("path", &String::from_utf8_lossy(&path_out))
            .field("key", &String::from_utf8_lossy(&key_out))
            .field("op", &op_dbg)
            .finish()
    }
}

impl GroveDbOp {
    pub fn to_worst_case_clone(&self) -> Self {
        let mut clone = self.clone();
        clone.mode = WorstCaseOp;
        clone
    }

    pub fn insert_run_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath(path.into_iter().map(|k| KnownKey(k)).collect());
        Self {
            path,
            key: KnownKey(key),
            op: Op::Insert { element },
            mode: GroveDbOpMode::RunOp,
        }
    }

    pub fn insert_worst_case_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Insert { element },
            mode: GroveDbOpMode::WorstCaseOp,
        }
    }

    pub fn delete_run_op(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        let path = KeyInfoPath(path.into_iter().map(|k| KnownKey(k)).collect());
        Self {
            path,
            key: KnownKey(key),
            op: Op::Delete,
            mode: GroveDbOpMode::RunOp,
        }
    }

    pub fn delete_worst_case_op(path: KeyInfoPath, key: KeyInfo) -> Self {
        Self {
            path,
            key,
            op: Op::Delete,
            mode: GroveDbOpMode::WorstCaseOp,
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
                        Some(op.op.clone())
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
            .filter_map(|current_op| {
                if let Op::Insert { .. } = current_op.op {
                    Some(current_op.clone())
                } else {
                    None
                }
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

#[derive(Debug)]
pub struct GroveDbOpConsistencyResults {
    repeated_ops: Vec<(GroveDbOp, u16)>, // the u16 is count
    same_path_key_ops: Vec<(KeyInfoPath, KeyInfo, Vec<Op>)>,
    insert_ops_below_deleted_ops: Vec<(GroveDbOp, Vec<GroveDbOp>)>, /* the deleted op first,
                                                                     * then inserts under */
}

impl GroveDbOpConsistencyResults {
    pub fn is_empty(&self) -> bool {
        self.repeated_ops.is_empty()
            && self.same_path_key_ops.is_empty()
            && self.insert_ops_below_deleted_ops.is_empty()
    }
}

/// Cache for Merk trees by their paths.
struct TreeCacheMerkByPath<S, F> {
    merks: HashMap<Vec<Vec<u8>>, Merk<S>>,
    get_merk_fn: F,
}

/// Cache for subtree paths for worst case scenario costs.
#[derive(Default)]
struct TreeCacheKnownPaths {
    paths: HashSet<KeyInfoPath>,
}

impl<S, F> fmt::Debug for TreeCacheMerkByPath<S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheMerkByPath").finish()
    }
}

impl fmt::Debug for TreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

trait TreeCache<G, SR> {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error>;

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<[u8; 32], Error>;
}

impl<'db, S, F> TreeCacheMerkByPath<S, F>
where
    F: FnMut(&[Vec<u8>]) -> CostResult<Merk<S>, Error>,
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
                Op::ReplaceTreeHash { .. } => {
                    return Err(Error::InvalidBatchOperation(
                        "references can not point to trees being updated",
                    ))
                    .wrap_with_cost(cost);
                }
                Op::Insert { element } => match element {
                    Element::Item(..) => {
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
                    Element::Tree(..) => {
                        return Err(Error::InvalidBatchOperation(
                            "references can not point to trees being updated",
                        ))
                        .wrap_with_cost(cost);
                    }
                },
                Op::Delete => {
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
                .unwrap_or_else(|| (self.get_merk_fn)(reference_path));
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
                    Element::Item(..) => {
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
                    Element::Tree(..) => {
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

impl<'db, S, F, G, SR> TreeCache<G, SR> for TreeCacheMerkByPath<S, F>
where
    G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> bool,
    SR: FnMut(&mut ElementFlags, u32) -> Result<StorageRemovedBytes, Error>,
    F: FnMut(&[Vec<u8>]) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut inserted_path = op.path.to_path();
        inserted_path.push(op.key.get_key_clone());
        if !self.merks.contains_key(&inserted_path) {
            let merk = cost_return_on_error!(&mut cost, (self.get_merk_fn)(&inserted_path));
            self.merks.insert(inserted_path, merk);
        }

        Ok(()).wrap_with_cost(cost)
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<[u8; 32], Error> {
        let mut cost = OperationCost::default();
        // todo: fix this
        let p = path.to_path();
        let path = &p;

        let merk_wrapped = self
            .merks
            .remove(path)
            .map(|x| Ok(x).wrap_with_cost(Default::default()))
            .unwrap_or_else(|| (self.get_merk_fn)(path));
        let mut merk = cost_return_on_error!(&mut cost, merk_wrapped);

        let mut batch_operations: Vec<(Vec<u8>, _)> = vec![];
        for (key_info, op) in ops_at_path_by_key.into_iter() {
            match op {
                Op::Insert { element } => match &element {
                    Element::Reference(
                        path_reference,
                        element_max_reference_hop,
                        element_flags,
                    ) => {
                        let path_iter = path.iter().map(|x| x.as_slice());
                        let path_reference = cost_return_on_error!(
                            &mut cost,
                            path_from_reference_path_type(
                                path_reference.clone(),
                                path_iter,
                                Some(key_info.get_key_ref())
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
                                &mut batch_operations
                            )
                        );
                    }
                    Element::Item(_, element_flags) | Element::Tree(_, element_flags) => {
                        if batch_apply_options.validate_insertion_does_not_override {
                            let inserted = cost_return_on_error!(
                                &mut cost,
                                element.insert_if_not_exists_into_batch_operations(
                                    &mut merk,
                                    key_info.get_key(),
                                    &mut batch_operations
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
                                    &mut batch_operations
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
                            &mut batch_operations
                        )
                    );
                }
                Op::ReplaceTreeHash { hash } => {
                    cost_return_on_error!(
                        &mut cost,
                        GroveDb::update_tree_item_preserve_flag_into_batch_operations(
                            &merk,
                            key_info.get_key(),
                            hash,
                            &mut batch_operations
                        )
                    );
                }
            }
        }

        cost_return_on_error!(&mut cost, unsafe {
            merk.apply_unchecked::<_, Vec<u8>, _, _>(
                &batch_operations,
                &[],
                &mut |storage_costs, old_value, new_value| {
                    // todo: change the flags without deserialization
                    let mut old_element = Element::deserialize(old_value.as_slice())?;
                    let maybe_old_flags = old_element.get_flags_owned();

                    let mut new_element = Element::deserialize(new_value.as_slice())?;
                    let maybe_new_flags = new_element.get_flags_mut();
                    match maybe_new_flags {
                        None => Ok(false),
                        Some(new_flags) => {
                            let changed = (flags_update)(storage_costs, maybe_old_flags, new_flags);
                            if changed {
                                new_value.clone_from(&new_element.serialize()?);
                            }
                            Ok(changed)
                        }
                    }
                },
                &mut |value, removed_bytes| {
                    let mut element = Element::deserialize(value.as_slice())?;
                    let maybe_flags = element.get_flags_mut();
                    match maybe_flags {
                        None => Ok(BasicStorageRemoval(removed_bytes)),
                        Some(flags) => {
                            (split_removal_bytes)(flags, removed_bytes).map_err(|e| e.into())
                        }
                    }

                },
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
        });
        merk.root_hash().add_cost(cost).map(Ok)
    }
}

impl<G, SR> TreeCache<G, SR> for TreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        self.paths.insert(inserted_path);
        let mut worst_case_cost = OperationCost::default();
        GroveDb::add_worst_case_get_merk::<RocksDbStorage>(&mut worst_case_cost, &op.path);
        Ok(()).wrap_with_cost(worst_case_cost)
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        _ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        _batch_apply_options: &BatchApplyOptions,
        _flags_update: &mut G,
        _split_removal_bytes: &mut SR,
    ) -> CostResult<[u8; 32], Error> {
        let mut cost = OperationCost::default();

        if !self.paths.remove(path) {
            // Then we have to get the tree
            GroveDb::add_worst_case_get_merk::<RocksDbStorage>(&mut cost, path);
        }
        for (key, op) in ops_at_path_by_key.into_iter() {
            cost += op.worst_case_cost(
                &key,
                MerkWorstCaseInput::MaxElementsNumber(MAX_ELEMENTS_NUMBER),
            );
        }
        GroveDb::add_worst_case_merk_propagate(
            &mut cost,
            MerkWorstCaseInput::NumberOfLevels(path.len()),
        );
        Ok([0u8; 32]).wrap_with_cost(cost)
    }
}

///                          LEVEL           PATH                   KEY      OP
type OpsByLevelPath = IntMap<u32, BTreeMap<KeyInfoPath, BTreeMap<KeyInfo, Op>>>;

struct BatchStructure<C, F, SR> {
    /// Operations by level path
    ops_by_level_paths: OpsByLevelPath,
    /// This is for references
    ops_by_qualified_paths: BTreeMap<Vec<Vec<u8>>, Op>,
    /// Merk trees
    merk_tree_cache: C,
    /// Flags modification function
    flags_update: F,
    ///
    split_removal_bytes: SR,
    /// Last level
    last_level: u32,
}

impl<F, SR, S: fmt::Debug> fmt::Debug for BatchStructure<S, F, SR> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmt_int_map = IntMap::default();
        for (level, path_map) in self.ops_by_level_paths.iter() {
            let mut fmt_path_map = BTreeMap::default();

            for (path, key_map) in path_map.iter() {
                let mut fmt_key_map = BTreeMap::default();

                for (key, op) in key_map.iter() {
                    fmt_key_map.insert(DebugBytes(key.get_key_clone()), op);
                }
                fmt_path_map.insert(DebugByteVectors(path.to_path()), fmt_key_map);
            }
            fmt_int_map.insert(*level, fmt_path_map);
        }

        f.debug_struct("BatchStructure")
            .field("ops_by_level_paths", &fmt_int_map)
            .field("merk_tree_cache", &self.merk_tree_cache)
            .field("last_level", &self.last_level)
            .finish()
    }
}

impl<C, F, SR> BatchStructure<C, F, SR>
where
    C: TreeCache<F, SR>,
    F: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> bool,
    SR: FnMut(&mut ElementFlags, u32) -> Result<StorageRemovedBytes, Error>,
{
    fn from_ops(
        ops: Vec<GroveDbOp>,
        update_element_flags_function: F,
        split_remove_bytes_function: SR,
        mut merk_tree_cache: C,
    ) -> CostResult<BatchStructure<C, F, SR>, Error> {
        let mut cost = OperationCost::default();

        let mut ops_by_level_paths: OpsByLevelPath = IntMap::default();
        let mut current_last_level: u32 = 0;

        // qualified paths meaning path + key
        let mut ops_by_qualified_paths: BTreeMap<Vec<Vec<u8>>, Op> = BTreeMap::new();

        for op in ops.into_iter() {
            let mut path = op.path.clone();
            path.push(op.key.clone());
            ops_by_qualified_paths.insert(path.to_path_consume(), op.op.clone());
            let op_cost = OperationCost::default();
            let op_result = match &op.op {
                Op::Insert { element } => {
                    if let Element::Tree(..) = element {
                        cost_return_on_error!(&mut cost, merk_tree_cache.insert(&op));
                    }
                    Ok(())
                }
                Op::Delete => Ok(()),
                Op::ReplaceTreeHash { .. } => Err(Error::InvalidBatchOperation(
                    "replace tree hash is an internal operation only",
                )),
            };
            if op_result.is_err() {
                return Err(op_result.err().unwrap()).wrap_with_cost(op_cost);
            }

            let level = op.path.len();
            if let Some(ops_on_level) = ops_by_level_paths.get_mut(&level) {
                if let Some(ops_on_path) = ops_on_level.get_mut(&op.path) {
                    ops_on_path.insert(op.key, op.op);
                } else {
                    let mut ops_on_path: BTreeMap<KeyInfo, Op> = BTreeMap::new();
                    ops_on_path.insert(op.key, op.op);
                    ops_on_level.insert(op.path.clone(), ops_on_path);
                }
            } else {
                let mut ops_on_path: BTreeMap<KeyInfo, Op> = BTreeMap::new();
                ops_on_path.insert(op.key, op.op);
                let mut ops_on_level: BTreeMap<KeyInfoPath, BTreeMap<KeyInfo, Op>> =
                    BTreeMap::new();
                ops_on_level.insert(op.path, ops_on_path);
                ops_by_level_paths.insert(level, ops_on_level);
                if current_last_level < level {
                    current_last_level = level;
                }
            }
        }

        Ok(BatchStructure {
            ops_by_level_paths,
            ops_by_qualified_paths,
            merk_tree_cache,
            flags_update: update_element_flags_function,
            split_removal_bytes: split_remove_bytes_function,
            last_level: current_last_level,
        })
        .wrap_with_cost(cost)
    }
}

#[derive(Debug, Default)]
pub struct BatchApplyOptions {
    pub validate_insertion_does_not_override: bool,
}

impl GroveDb {
    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn apply_batch_structure<C: TreeCache<F, SR>, F, SR>(
        batch_structure: BatchStructure<C, F, SR>,
        batch_apply_options: Option<BatchApplyOptions>,
    ) -> CostResult<(), Error>
    where
        F: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> bool,
        SR: FnMut(&mut ElementFlags, u32) -> Result<StorageRemovedBytes, Error>,
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
                    let mut root_tree_ops: BTreeMap<KeyInfo, Op> = BTreeMap::new();
                    for (key, op) in ops_at_path.into_iter() {
                        match op {
                            Op::Insert { .. } => {
                                root_tree_ops.insert(key, op);
                            }
                            Op::Delete => {
                                return Err(Error::InvalidBatchOperation(
                                    "deletion of root tree not possible",
                                ))
                                .wrap_with_cost(cost);
                            }
                            Op::ReplaceTreeHash { hash } => {
                                root_tree_ops.insert(key, Op::ReplaceTreeHash { hash });
                            }
                        }
                    }
                    // execute the ops at this path
                    cost_return_on_error!(
                        &mut cost,
                        merk_tree_cache.execute_ops_on_path(
                            &path,
                            root_tree_ops,
                            &ops_by_qualified_paths,
                            &batch_apply_options,
                            &mut flags_update,
                            &mut split_removal_bytes,
                        )
                    );
                } else {
                    let root_hash = cost_return_on_error!(
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
                                            vacant_entry
                                                .insert(Op::ReplaceTreeHash { hash: root_hash });
                                        }
                                        Entry::Occupied(occupied_entry) => {
                                            match occupied_entry.into_mut() {
                                                Op::ReplaceTreeHash { hash } => *hash = root_hash,
                                                Op::Insert { element } => {
                                                    if let Element::Tree(hash, _) = element {
                                                        *hash = root_hash
                                                    } else {
                                                        return Err(Error::InvalidBatchOperation(
                                                            "insertion of element under a non tree",
                                                        ))
                                                        .wrap_with_cost(cost);
                                                    }
                                                }
                                                Op::Delete => {
                                                    if root_hash != [0u8; 32] {
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
                                        Op::ReplaceTreeHash { hash: root_hash },
                                    );
                                    ops_at_level_above.insert(parent_path, ops_on_path);
                                }
                            } else {
                                let mut ops_on_path: BTreeMap<KeyInfo, Op> = BTreeMap::new();
                                ops_on_path
                                    .insert(key.clone(), Op::ReplaceTreeHash { hash: root_hash });
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
        ) -> bool,
        split_removed_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32,
        ) -> Result<StorageRemovedBytes, Error>,
        get_merk_fn: impl FnMut(&[Vec<u8>]) -> CostResult<Merk<S>, Error>,
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
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        for op in ops.into_iter() {
            match op.op {
                Op::Insert { element } => {
                    let path_slices: Vec<&[u8]> = op.path.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.insert(
                            path_slices,
                            op.key.as_slice(),
                            element.to_owned(),
                            transaction,
                        )
                    );
                }
                Op::Delete => {
                    let path_slices: Vec<&[u8]> = op.path.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.delete(path_slices, op.key.as_slice(), transaction,)
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
            |cost, old_flags, new_flags| false,
            |flags, removed_bytes| Ok(BasicStorageRemoval(removed_bytes)),
            transaction,
        )
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
        ) -> bool,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32,
        ) -> Result<StorageRemovedBytes, Error>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
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
                    |path| {
                        let storage = self
                            .db
                            .get_batch_transactional_storage_context(
                                path.iter().map(|x| x.as_slice()),
                                &storage_batch,
                                tx,
                            )
                            .unwrap_add_cost(&mut cost);
                        Merk::open(storage)
                            .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
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
                    |path| {
                        let storage = self
                            .db
                            .get_batch_storage_context(
                                path.iter().map(|x| x.as_slice()),
                                &storage_batch,
                            )
                            .unwrap_add_cost(&mut cost);
                        Merk::open(storage)
                            .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
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

    pub fn worst_case_operations_for_batch(
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> bool,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32,
        ) -> Result<StorageRemovedBytes, Error>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::from_ops(
                ops,
                update_element_flags_function,
                split_removal_bytes_function,
                TreeCacheKnownPaths::default()
            )
        );
        cost_return_on_error!(
            &mut cost,
            Self::apply_batch_structure(batch_structure, batch_apply_options)
        );

        Ok(()).wrap_with_cost(cost)
    }
}

#[cfg(test)]
mod tests {
    use costs::storage_cost::{
        removal::StorageRemovedBytes::NoStorageRemoval, transition::OperationStorageTransitionType,
        StorageCost,
    };
    use integer_encoding::VarInt;
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
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
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
    fn test_batch_validation_ok_on_transaction() {
        let db = make_test_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"keyb", Element::empty_tree(), Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
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
    fn test_batch_root_one_insert_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        // Explanation for 176 storage_written_bytes
        // 2 bytes for left and right height
        // 1 byte for the key length size
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // Value
        //   1 for the value length size
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   32 for empty tree
        // 32 for node hash
        // 32 for value hash

        // 1 byte for the root key length size
        // 1 byte for the root value length size
        // 32 for the root key prefix
        // 4 bytes for the key to put in root
        // 1 byte for the root "r"

        // Hash node calls
        // 2 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 2, // 1 to get tree, 1 to insert
                storage_cost: StorageCost {
                    added_bytes: 177,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 6,
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_bigger_cost_no_flags() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0])),
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![GroveDbOp::insert_run_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_flags(b"value100".to_vec(), Some(vec![1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |cost, old_flags, new_flags| false,
                |flags, removed_bytes| Ok(NoStorageRemoval),
                Some(&tx),
            )
            .cost;

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 4, // 1 to get tree, 1 to insert
                storage_cost: StorageCost {
                    added_bytes: 2,
                    replaced_bytes: 257,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 185,
                hash_node_calls: 6,
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_bigger_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0, 0])),
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![GroveDbOp::insert_run_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_flags(b"value100".to_vec(), Some(vec![0, 1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |cost, old_flags, new_flags| match cost.transition_type() {
                    OperationStorageTransitionType::OperationUpdateBiggerSize => {
                        if new_flags[0] == 0 {
                            new_flags[0] = 1;
                            let new_flags_epoch = new_flags[1];
                            new_flags[1] = old_flags.unwrap()[1];
                            new_flags.push(new_flags_epoch);
                            new_flags.extend(cost.added_bytes.encode_var_vec());
                            assert_eq!(new_flags, &vec![1u8, 0, 1, 2]);
                            true
                        } else {
                            assert_eq!(new_flags[0], 1);
                            false
                        }
                    }
                    OperationStorageTransitionType::OperationUpdateSmallerSize => {
                        new_flags.extend(vec![1, 2]);
                        true
                    }
                    _ => false,
                },
                |flags, removed| Ok(BasicStorageRemoval(removed)),
                Some(&tx),
            )
            .cost;

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 4, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 4,
                    replaced_bytes: 258,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 186, // todo: verify this
                hash_node_calls: 6,        // todo: verify this
            }
        );
    }

    #[test]
    fn test_batch_root_one_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost_result = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |cost, old_flags, new_flags| false,
            |flags, removed_bytes| Ok(NoStorageRemoval),
        );
        assert!(worst_case_cost_result.value.is_ok());
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            worst_case_cost_result.cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            worst_case_cost_result.cost,
            cost
        );

        assert_eq!(
            worst_case_cost_result.cost,
            OperationCost {
                seek_count: 4, // todo: why is this 4
                storage_cost: StorageCost {
                    added_bytes: 177,
                    replaced_bytes: 640, // log(max_elements) * 32 = 640 // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 22, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_op_under_element_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"0", Element::empty_tree(), Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost_result = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |cost, old_flags, new_flags| false,
            |flags, removed_bytes| Ok(NoStorageRemoval),
        );
        assert!(worst_case_cost_result.value.is_ok());
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(worst_case_cost_result.cost, cost);
    }

    #[test]
    fn test_batch_costs_match_non_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost = db
            .insert(vec![], b"key1", Element::empty_tree(), Some(&tx))
            .cost;
        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(non_batch_cost, cost);
    }

    #[test]
    fn test_batch_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"keyb", Element::empty_tree(), Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost_result = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |cost, old_flags, new_flags| false,
            |flags, removed_bytes| Ok(NoStorageRemoval),
        );
        assert!(worst_case_cost_result.value.is_ok());
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(worst_case_cost_result.cost, cost);
    }

    fn grove_db_ops_for_contract_insert() -> Vec<GroveDbOp> {
        let mut grove_db_ops = vec![];

        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![],
            b"contract".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec()],
            (&[0u8]).to_vec(),
            Element::new_item(b"serialized_contract".to_vec()),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec()],
            (&[1u8]).to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec()],
            b"domain".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            (&[0u8]).to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            b"normalized_domain_label".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            b"unique_records".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec(), b"domain".to_vec()],
            b"alias_records".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![b"contract".to_vec(), (&[1u8]).to_vec()],
            b"preorder".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"preorder".to_vec(),
            ],
            (&[0u8]).to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_run_op(
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

        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                (&[0u8]).to_vec(),
            ],
            b"serialized_domain_id".to_vec(),
            Element::new_item(b"serialized_domain".to_vec()),
        ));

        grove_db_ops.push(GroveDbOp::insert_run_op(
            vec![
                b"contract".to_vec(),
                (&[1u8]).to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
            ],
            b"dash".to_vec(),
            Element::empty_tree(),
        ));

        grove_db_ops.push(GroveDbOp::insert_run_op(
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

        grove_db_ops.push(GroveDbOp::insert_run_op(
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

        grove_db_ops.push(GroveDbOp::insert_run_op(
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

        db.apply_operations_without_batching(ops, Some(&tx))
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

        db.apply_operations_without_batching(ops, Some(&tx))
            .unwrap()
            .expect("expected to apply batch");
        db.apply_operations_without_batching(document_ops, Some(&tx))
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
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
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
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
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

        db.insert([], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([b"key1".as_ref()], b"key2", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");

        let ops = vec![
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::delete_run_op(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
    }

    #[test]
    fn test_batch_validation_insertion_under_deleted_tree() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::delete_run_op(vec![b"key1".to_vec()], b"key2".to_vec()),
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

        db.insert([TEST_LEAF], b"invalid", element.clone(), None)
            .unwrap()
            .expect("cannot insert value");
        db.insert([TEST_LEAF], b"valid", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert value");

        // Insertion into scalar is invalid
        let ops = vec![GroveDbOp::insert_run_op(
            vec![TEST_LEAF.to_vec(), b"invalid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());

        // Insertion into a tree is correct
        let ops = vec![GroveDbOp::insert_run_op(
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
        db.insert([TEST_LEAF], b"key_subtree", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([TEST_LEAF, b"key_subtree"], b"key2", element, None)
            .unwrap()
            .expect("cannot insert an item");

        // TEST_LEAF can not be overwritten
        let ops = vec![
            GroveDbOp::insert_run_op(vec![], TEST_LEAF.to_vec(), element2.clone()),
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec(), b"key_subtree".to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true
                }),
                None
            )
            .unwrap()
            .is_err());

        // TEST_LEAF will be deleted so you can not insert underneath it
        let ops = vec![
            GroveDbOp::delete_run_op(vec![], TEST_LEAF.to_vec()),
            GroveDbOp::insert_run_op(
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
            GroveDbOp::delete_run_op(vec![], TEST_LEAF.to_vec()),
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true
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
            GroveDbOp::insert_run_op(
                vec![],
                TEST_LEAF.to_vec(),
                Element::new_item(b"ayy".to_vec()),
            ),
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true
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

        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([TEST_LEAF, b"key1"], b"key2", element.clone(), None)
            .unwrap()
            .expect("cannot insert an item");
        let ops = vec![GroveDbOp::insert_run_op(
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
        db.insert([], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert([], b"key2", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert([ANOTHER_TEST_LEAF], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert root leaf");

        let hash = db.root_hash(None).unwrap().expect("cannot get root hash");
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());

        let ops = vec![
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(vec![TEST_LEAF.to_vec()], b"key".to_vec(), element2.clone()),
            GroveDbOp::delete_run_op(vec![ANOTHER_TEST_LEAF.to_vec()], b"key1".to_vec()),
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
            )
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert_run_op(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None)
            .unwrap()
            .expect("cannot apply batch");

        let batch = vec![GroveDbOp::insert_run_op(
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
            )
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let root_hash = db.root_hash(None).unwrap().unwrap();

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert_run_op(
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
        let batch = vec![GroveDbOp::insert_run_op(
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
            GroveDbOp::insert_run_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"invalid_path".to_vec(),
                ])),
            ),
            GroveDbOp::insert_run_op(
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
            GroveDbOp::insert_run_op(
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
            GroveDbOp::insert_run_op(
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
            GroveDbOp::insert_run_op(
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
