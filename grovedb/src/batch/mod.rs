// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Apply multiple GroveDB operations atomically.

mod batch_structure;

#[cfg(feature = "estimated_costs")]
pub mod estimated_costs;

pub mod key_info;

mod mode;
#[cfg(test)]
mod multi_insert_cost_tests;

#[cfg(test)]
mod just_in_time_cost_tests;
mod options;
#[cfg(test)]
mod single_deletion_cost_tests;
#[cfg(test)]
mod single_insert_cost_tests;
#[cfg(test)]
mod single_sum_item_deletion_cost_tests;
#[cfg(test)]
mod single_sum_item_insert_cost_tests;

use core::fmt;
use std::{
    cmp::Ordering,
    collections::{btree_map::Entry, hash_map::Entry as HashMapEntry, BTreeMap, HashMap},
    hash::{Hash, Hasher},
    ops::{Add, AddAssign},
    slice::Iter,
    vec::IntoIter,
};

#[cfg(feature = "estimated_costs")]
use estimated_costs::{
    average_case_costs::AverageCaseTreeCacheKnownPaths,
    worst_case_costs::WorstCaseTreeCacheKnownPaths,
};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    tree::{
        kv::ValueDefinedCostType::{LayeredValueDefinedCost, SpecializedValueDefinedCost},
        value_hash, NULL_HASH,
    },
    CryptoHash, Error as MerkError, Merk, MerkType, RootHashKeyAndSum,
    TreeFeatureType::{BasicMerkNode, SummedMerkNode},
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext},
    Storage, StorageBatch, StorageContext,
};
use grovedb_visualize::{Drawer, Visualize};
use integer_encoding::VarInt;
use itertools::Itertools;
use key_info::{KeyInfo, KeyInfo::KnownKey};
pub use options::BatchApplyOptions;

pub use crate::batch::batch_structure::{OpsByLevelPath, OpsByPath};
#[cfg(feature = "estimated_costs")]
use crate::batch::estimated_costs::EstimatedCostsType;
use crate::{
    batch::{batch_structure::BatchStructure, mode::BatchRunMode},
    element::{MaxReferenceHop, SUM_ITEM_COST_SIZE, SUM_TREE_COST_SIZE, TREE_COST_SIZE},
    operations::get::MAX_REFERENCE_HOPS,
    reference_path::{
        path_from_reference_path_type, path_from_reference_qualified_path_type, ReferencePathType,
    },
    Element, ElementFlags, Error, GroveDb, Transaction, TransactionArg,
};

/// Operations
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Op {
    /// Replace tree root key
    ReplaceTreeRootKey {
        /// Hash
        hash: [u8; 32],
        /// Root key
        root_key: Option<Vec<u8>>,
        /// Sum
        sum: Option<i64>,
    },
    /// Insert
    Insert {
        /// Element
        element: Element,
    },
    /// Replace
    Replace {
        /// Element
        element: Element,
    },
    /// Patch
    Patch {
        /// Element
        element: Element,
        /// Byte change
        change_in_bytes: i32,
    },
    /// Insert tree with root hash
    InsertTreeWithRootHash {
        /// Hash
        hash: [u8; 32],
        /// Root key
        root_key: Option<Vec<u8>>,
        /// Flags
        flags: Option<ElementFlags>,
        /// Sum
        sum: Option<i64>,
    },
    /// Refresh the reference with information provided
    /// Providing this information is necessary to be able to calculate
    /// average case and worst case costs
    /// If TrustRefreshReference is true, then we do not query the element on
    /// disk before write If it is false, the provided information is only
    /// used for average case and worse case costs
    RefreshReference {
        reference_path_type: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
        trust_refresh_reference: bool,
    },
    /// Delete
    Delete,
    /// Delete tree
    DeleteTree,
    /// Delete sum tree
    DeleteSumTree,
}

impl Op {
    fn to_u8(&self) -> u8 {
        match self {
            Op::DeleteTree => 0,
            Op::DeleteSumTree => 1,
            Op::Delete => 2,
            Op::InsertTreeWithRootHash {..} => 3,
            Op::ReplaceTreeRootKey {..} => 4,
            Op::RefreshReference {..} => 5,
            Op::Replace {..} => 6,
            Op::Patch {..} => 7,
            Op::Insert {..} => 8
        }
    }
}

impl PartialOrd for Op {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Op {
    fn cmp(&self, other: &Self) -> Ordering {
        self.to_u8().cmp(&other.to_u8())
    }
}

/// Known keys path
#[derive(Eq, Clone, Debug)]
pub struct KnownKeysPath(Vec<Vec<u8>>);

impl Hash for KnownKeysPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl PartialEq for KnownKeysPath {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
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

/// Key info path
#[derive(PartialOrd, Ord, Eq, Clone, Debug, Default)]
pub struct KeyInfoPath(pub Vec<KeyInfo>);

impl Hash for KeyInfoPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl PartialEq for KeyInfoPath {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<Vec<Vec<u8>>> for KeyInfoPath {
    fn eq(&self, other: &Vec<Vec<u8>>) -> bool {
        if self.len() != other.len() as u32 {
            return false;
        }
        self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl PartialEq<Vec<&[u8]>> for KeyInfoPath {
    fn eq(&self, other: &Vec<&[u8]>) -> bool {
        if self.len() != other.len() as u32 {
            return false;
        }
        self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<const N: usize> PartialEq<[&[u8]; N]> for KeyInfoPath {
    fn eq(&self, other: &[&[u8]; N]) -> bool {
        if self.len() != N as u32 {
            return false;
        }
        self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
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
        drawer.write(path_out.as_slice()).unwrap();
        Ok(drawer)
    }
}

impl KeyInfoPath {
    /// From a vector
    pub fn from_vec(vec: Vec<KeyInfo>) -> Self {
        KeyInfoPath(vec)
    }

    /// From a known path
    pub fn from_known_path<'p, P>(path: P) -> Self
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        KeyInfoPath(path.into_iter().map(|k| KnownKey(k.to_vec())).collect())
    }

    /// From a known owned path
    pub fn from_known_owned_path<P>(path: P) -> Self
    where
        P: IntoIterator<Item = Vec<u8>>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        KeyInfoPath(path.into_iter().map(KnownKey).collect())
    }

    /// To a path and consume
    pub fn to_path_consume(self) -> Vec<Vec<u8>> {
        self.0.into_iter().map(|k| k.get_key()).collect()
    }

    /// To a path
    pub fn to_path(&self) -> Vec<Vec<u8>> {
        self.0.iter().map(|k| k.get_key_clone()).collect()
    }

    /// To a path of refs
    pub fn to_path_refs(&self) -> Vec<&[u8]> {
        self.0.iter().map(|k| k.as_slice()).collect()
    }

    /// Return the last and all the other elements split
    pub fn split_last(&self) -> Option<(&KeyInfo, &[KeyInfo])> {
        self.0.split_last()
    }

    /// Return the last element
    pub fn last(&self) -> Option<&KeyInfo> {
        self.0.last()
    }

    /// As vector
    pub fn as_vec(&self) -> &Vec<KeyInfo> {
        &self.0
    }

    /// Check if it's empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return length
    pub fn len(&self) -> u32 {
        self.0.len() as u32
    }

    /// Push a KeyInfo to self
    pub fn push(&mut self, k: KeyInfo) {
        self.0.push(k);
    }

    /// Iterate KeyInfo
    pub fn iterator(&self) -> Iter<'_, KeyInfo> {
        self.0.iter()
    }

    /// Into iterator
    pub fn into_iterator(self) -> IntoIter<KeyInfo> {
        self.0.into_iter()
    }
}

/// Batch operation
#[derive(Clone, PartialEq, Eq)]
pub struct GroveDbOp {
    /// Path to a subtree - subject to an operation
    pub path: KeyInfoPath,
    /// Key of an element in the subtree
    pub key: KeyInfo,
    /// Operation to perform on the key
    pub op: Op,
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
                Element::Item(..) => "Insert Item".to_string(),
                Element::Reference(..) => "Insert Ref".to_string(),
                Element::Tree(..) => "Insert Tree".to_string(),
                Element::SumTree(..) => "Insert Sum Tree".to_string(),
                Element::SumItem(..) => "Insert Sum Item".to_string(),
            },
            Op::Replace { element } => match element {
                Element::Item(..) => "Replace Item".to_string(),
                Element::Reference(..) => "Replace Ref".to_string(),
                Element::Tree(..) => "Replace Tree".to_string(),
                Element::SumTree(..) => "Replace Sum Tree".to_string(),
                Element::SumItem(..) => "Replace Sum Item".to_string(),
            },
            Op::Patch { element, .. } => match element {
                Element::Item(..) => "Patch Item".to_string(),
                Element::Reference(..) => "Patch Ref".to_string(),
                Element::Tree(..) => "Patch Tree".to_string(),
                Element::SumTree(..) => "Patch Sum Tree".to_string(),
                Element::SumItem(..) => "Patch Sum Item".to_string(),
            },
            Op::RefreshReference {
                reference_path_type,
                max_reference_hop,
                trust_refresh_reference,
                ..
            } => {
                format!(
                    "Refresh Reference: path {:?}, max_hop {:?}, trust_reference {} ",
                    reference_path_type, max_reference_hop, trust_refresh_reference
                )
            }
            Op::Delete => "Delete".to_string(),
            Op::DeleteTree => "Delete Tree".to_string(),
            Op::DeleteSumTree => "Delete Sum Tree".to_string(),
            Op::ReplaceTreeRootKey { .. } => "Replace Tree Hash and Root Key".to_string(),
            Op::InsertTreeWithRootHash { .. } => "Insert Tree Hash and Root Key".to_string(),
        };

        f.debug_struct("GroveDbOp")
            .field("path", &String::from_utf8_lossy(&path_out))
            .field("key", &String::from_utf8_lossy(&key_out))
            .field("op", &op_dbg)
            .finish()
    }
}

impl GroveDbOp {
    /// An insert op using a known owned path and known key
    pub fn insert_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Insert { element },
        }
    }

    /// An insert op
    pub fn insert_estimated_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Insert { element },
        }
    }

    /// A replace op using a known owned path and known key
    pub fn replace_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Replace { element },
        }
    }

    /// A replace op
    pub fn replace_estimated_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Replace { element },
        }
    }

    /// A patch op using a known owned path and known key
    pub fn patch_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
        change_in_bytes: i32,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Patch {
                element,
                change_in_bytes,
            },
        }
    }

    /// A patch op
    pub fn patch_estimated_op(
        path: KeyInfoPath,
        key: KeyInfo,
        element: Element,
        change_in_bytes: i32,
    ) -> Self {
        Self {
            path,
            key,
            op: Op::Patch {
                element,
                change_in_bytes,
            },
        }
    }

    /// A refresh reference op using a known owned path and known key
    pub fn refresh_reference_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        reference_path_type: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
        trust_refresh_reference: bool,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::RefreshReference {
                reference_path_type,
                max_reference_hop,
                flags,
                trust_refresh_reference,
            },
        }
    }

    /// A delete op using a known owned path and known key
    pub fn delete_op(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: KnownKey(key),
            op: Op::Delete,
        }
    }

    /// A delete tree op using a known owned path and known key
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

    /// A delete op
    pub fn delete_estimated_op(path: KeyInfoPath, key: KeyInfo) -> Self {
        Self {
            path,
            key,
            op: Op::Delete,
        }
    }

    /// A delete tree op
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

    /// Verify consistency of operations
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
            if !doubled_ops.is_empty() {
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
                            .iterator()
                            .zip(inserted_op.path.iterator())
                            .all(|(a, b)| a == b)
                    {
                        Some(inserted_op.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<GroveDbOp>>();
            if !inserts_with_deleted_ops_above.is_empty() {
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

/// Results of a consistency check on an operation batch
#[derive(Debug)]
pub struct GroveDbOpConsistencyResults {
    repeated_ops: Vec<(GroveDbOp, u16)>, // the u16 is count
    same_path_key_ops: Vec<(KeyInfoPath, KeyInfo, Vec<Op>)>,
    insert_ops_below_deleted_ops: Vec<(GroveDbOp, Vec<GroveDbOp>)>, /* the deleted op first,
                                                                     * then inserts under */
}

impl GroveDbOpConsistencyResults {
    /// Check if results are empty
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

impl<S, F> fmt::Debug for TreeCacheMerkByPath<S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheMerkByPath").finish()
    }
}

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
    ) -> CostResult<RootHashKeyAndSum, Error>;

    fn update_base_merk_root_key(&mut self, root_key: Option<Vec<u8>>) -> CostResult<(), Error>;
}

impl<'db, S, F> TreeCacheMerkByPath<S, F>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    /// Processes a reference, determining whether it can be retrieved from a
    /// batch operation.
    ///
    /// This function performs the processing for a reference when it does not
    /// change in the same batch. It distinguishes between two cases:
    ///
    /// 1. When the hop count is exactly 1, it tries to directly extract the
    /// value hash from the reference element.
    ///
    /// 2. When the hop count is greater than 1, it retrieves the referenced
    /// element and then determines the next step based on the type of the
    /// element.
    ///
    /// # Arguments
    ///
    /// * `qualified_path`: The path to the referenced element. It should be
    ///   already checked to be a valid path.
    /// * `recursions_allowed`: The maximum allowed hop count to reach the
    ///   target element.
    ///
    /// # Returns
    ///
    /// * `Ok(CryptoHash)`: Returns the crypto hash of the referenced element
    ///   wrapped in the
    /// associated cost, if successful.
    ///
    /// * `Err(Error)`: Returns an error if there is an issue with the
    ///   operation, such as
    /// missing reference, corrupted data, or invalid batch operation.
    ///
    /// # Errors
    ///
    /// This function will return `Err(Error)` if there are any issues
    /// encountered while processing the reference. Possible errors include:
    ///
    /// * `Error::MissingReference`: If a direct or indirect reference to the
    ///   target element is missing in the batch.
    /// * `Error::CorruptedData`: If there is an issue while retrieving or
    ///   deserializing the referenced element.
    /// * `Error::InvalidBatchOperation`: If the referenced element points to a
    ///   tree being updated.
    fn process_reference<'a>(
        &'a mut self,
        qualified_path: &[Vec<u8>],
        ops_by_qualified_paths: &'a BTreeMap<Vec<Vec<u8>>, Op>,
        recursions_allowed: u8,
        intermediate_reference_info: Option<&'a ReferencePathType>,
    ) -> CostResult<CryptoHash, Error> {
        let mut cost = OperationCost::default();
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
                merk.get_value_hash(
                    key.as_ref(),
                    true,
                    Some(Element::value_defined_cost_for_serialized_value)
                )
                .map_err(|e| Error::CorruptedData(e.to_string()))
            );

            let referenced_element_value_hash = cost_return_on_error!(
                &mut cost,
                referenced_element_value_hash_opt
                    .ok_or({
                        let reference_string = reference_path
                            .iter()
                            .map(hex::encode)
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

            Ok(referenced_element_value_hash).wrap_with_cost(cost)
        } else if let Some(referenced_path) = intermediate_reference_info {
            let path = cost_return_on_error_no_add!(
                &cost,
                path_from_reference_qualified_path_type(referenced_path.clone(), qualified_path)
            );
            self.follow_reference_get_value_hash(
                path.as_slice(),
                ops_by_qualified_paths,
                recursions_allowed - 1,
            )
        } else {
            // Here the element being referenced doesn't change in the same batch
            // but the hop count is greater than 1, we can't just take the value hash from
            // the referenced element as an element further down in the chain might still
            // change in the batch.
            let referenced_element = cost_return_on_error!(
                &mut cost,
                merk.get(
                    key.as_ref(),
                    true,
                    Some(Element::value_defined_cost_for_serialized_value)
                )
                .map_err(|e| Error::CorruptedData(e.to_string()))
            );

            let referenced_element = cost_return_on_error_no_add!(
                &cost,
                referenced_element.ok_or({
                    let reference_string = reference_path
                        .iter()
                        .map(hex::encode)
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
                        path_from_reference_qualified_path_type(path, qualified_path)
                    );
                    self.follow_reference_get_value_hash(
                        path.as_slice(),
                        ops_by_qualified_paths,
                        recursions_allowed - 1,
                    )
                }
                Element::Tree(..) | Element::SumTree(..) => Err(Error::InvalidBatchOperation(
                    "references can not point to trees being updated",
                ))
                .wrap_with_cost(cost),
            }
        }
    }

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
                Op::ReplaceTreeRootKey { .. } | Op::InsertTreeWithRootHash { .. } => Err(
                    Error::InvalidBatchOperation("references can not point to trees being updated"),
                )
                .wrap_with_cost(cost),
                Op::Insert { element } | Op::Replace { element } | Op::Patch { element, .. } => {
                    match element {
                        Element::Item(..) | Element::SumItem(..) => {
                            let serialized =
                                cost_return_on_error_no_add!(&cost, element.serialize());
                            let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
                            Ok(val_hash).wrap_with_cost(cost)
                        }
                        Element::Reference(path, ..) => {
                            let path = cost_return_on_error_no_add!(
                                &cost,
                                path_from_reference_qualified_path_type(
                                    path.clone(),
                                    qualified_path
                                )
                            );
                            self.follow_reference_get_value_hash(
                                path.as_slice(),
                                ops_by_qualified_paths,
                                recursions_allowed - 1,
                            )
                        }
                        Element::Tree(..) | Element::SumTree(..) => {
                            Err(Error::InvalidBatchOperation(
                                "references can not point to trees being updated",
                            ))
                            .wrap_with_cost(cost)
                        }
                    }
                }
                Op::RefreshReference {
                    reference_path_type,
                    trust_refresh_reference,
                    ..
                } => {
                    // We are pointing towards a reference that will be refreshed
                    let reference_info = if *trust_refresh_reference {
                        Some(reference_path_type)
                    } else {
                        None
                    };
                    self.process_reference(
                        qualified_path,
                        ops_by_qualified_paths,
                        recursions_allowed,
                        reference_info,
                    )
                }
                Op::Delete | Op::DeleteTree | Op::DeleteSumTree => {
                    Err(Error::InvalidBatchOperation(
                        "references can not point to something currently being deleted",
                    ))
                    .wrap_with_cost(cost)
                }
            }
        } else {
            self.process_reference(
                qualified_path,
                ops_by_qualified_paths,
                recursions_allowed,
                None,
            )
        }
    }
}

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
        if let HashMapEntry::Vacant(e) = self.merks.entry(inserted_path.clone()) {
            let mut merk =
                cost_return_on_error!(&mut cost, (self.get_merk_fn)(&inserted_path, true));
            merk.is_sum_tree = is_sum_tree;
            e.insert(merk);
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
    ) -> CostResult<RootHashKeyAndSum, Error> {
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
                Op::Insert { element } | Op::Replace { element } | Op::Patch { element, .. } => {
                    match &element {
                        Element::Reference(path_reference, element_max_reference_hop, _) => {
                            let merk_feature_type = cost_return_on_error!(
                                &mut cost,
                                element
                                    .get_feature_type(is_sum_tree)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            let path_reference = cost_return_on_error!(
                                &mut cost,
                                path_from_reference_path_type(
                                    path_reference.clone(),
                                    path,
                                    Some(key_info.as_slice())
                                )
                                .wrap_with_cost(OperationCost::default())
                            );
                            if path_reference.is_empty() {
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
                    }
                }
                Op::RefreshReference {
                    reference_path_type,
                    max_reference_hop,
                    flags,
                    trust_refresh_reference,
                } => {
                    // We have a refresh reference Op, this means we need to get the actual
                    // reference element on disk first

                    let element = if trust_refresh_reference {
                        Element::Reference(reference_path_type, max_reference_hop, flags)
                    } else {
                        let value = cost_return_on_error!(
                            &mut cost,
                            merk.get(
                                key_info.as_slice(),
                                true,
                                Some(Element::value_defined_cost_for_serialized_value)
                            )
                            .map(
                                |result_value| result_value.map_err(Error::MerkError).and_then(
                                    |maybe_value| maybe_value.ok_or(Error::InvalidInput(
                                        "trying to refresh a non existing reference",
                                    ))
                                )
                            )
                        );
                        cost_return_on_error_no_add!(
                            &cost,
                            Element::deserialize(value.as_slice()).map_err(|_| {
                                Error::CorruptedData(String::from("unable to deserialize element"))
                            })
                        )
                    };

                    let Element::Reference(path_reference, max_reference_hop, _) = &element else {
                        return Err(Error::InvalidInput(
                            "trying to refresh a an element that is not a reference",
                        ))
                        .wrap_with_cost(cost);
                    };

                    let merk_feature_type = if is_sum_tree {
                        SummedMerkNode(0)
                    } else {
                        BasicMerkNode
                    };

                    let path_reference = cost_return_on_error!(
                        &mut cost,
                        path_from_reference_path_type(
                            path_reference.clone(),
                            path,
                            Some(key_info.as_slice())
                        )
                        .wrap_with_cost(OperationCost::default())
                    );
                    if path_reference.is_empty() {
                        return Err(Error::CorruptedReferencePathNotFound(
                            "attempting to refresh an empty reference".to_string(),
                        ))
                        .wrap_with_cost(cost);
                    }

                    let referenced_element_value_hash = cost_return_on_error!(
                        &mut cost,
                        self.follow_reference_get_value_hash(
                            path_reference.as_slice(),
                            ops_by_qualified_paths,
                            max_reference_hop.unwrap_or(MAX_REFERENCE_HOPS as u8)
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
                Op::Delete => {
                    cost_return_on_error!(
                        &mut cost,
                        Element::delete_into_batch_operations(
                            key_info.get_key(),
                            false,
                            is_sum_tree, /* we are in a sum tree, this might or might not be a
                                          * sum item */
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
            merk.apply_unchecked::<_, Vec<u8>, _, _, _, _>(
                &batch_operations,
                &[],
                Some(batch_apply_options.as_merk_options()),
                &|key, value| {
                    Element::specialized_costs_for_key_value(key, value, is_sum_tree)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                },
                Some(&Element::value_defined_cost_for_serialized_value),
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
                                        Ok((true, Some(LayeredValueDefinedCost(tree_value_cost))))
                                    }
                                    Element::SumItem(..) => {
                                        let sum_item_value_cost = SUM_ITEM_COST_SIZE
                                            + flags_len
                                            + flags_len.required_space() as u32;
                                        Ok((
                                            true,
                                            Some(SpecializedValueDefinedCost(sum_item_value_cost)),
                                        ))
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
        BatchRunMode::Execute
    }
}

impl GroveDb {
    /// Method to propagate updated subtree root hashes up to GroveDB root
    /// If the stop level is set in the apply options the remaining operations
    /// are returned
    fn apply_batch_structure<C: TreeCache<F, SR>, F, SR>(
        batch_structure: BatchStructure<C, F, SR>,
        batch_apply_options: Option<BatchApplyOptions>,
    ) -> CostResult<Option<OpsByLevelPath>, Error>
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
        let stop_level = batch_apply_options.batch_pause_height.unwrap_or_default() as u32;

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
                    let (root_hash, calculated_root_key, sum_value) = cost_return_on_error!(
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
                                                sum: sum_value,
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
                                                    *sum = sum_value;
                                                }
                                                Op::InsertTreeWithRootHash { .. } => {
                                                    return Err(Error::CorruptedCodeExecution(
                                                        "we can not do this operation twice",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                Op::Insert { element }
                                                | Op::Replace { element }
                                                | Op::Patch { element, .. } => {
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
                                                                sum: sum_value,
                                                            };
                                                    } else {
                                                        return Err(Error::InvalidBatchOperation(
                                                            "insertion of element under a non tree",
                                                        ))
                                                        .wrap_with_cost(cost);
                                                    }
                                                }
                                                Op::RefreshReference { .. } => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "insertion of element under a refreshed \
                                                         reference",
                                                    ))
                                                    .wrap_with_cost(cost);
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
                                            sum: sum_value,
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
                                        sum: sum_value,
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
            if current_level == stop_level {
                // we need to pause the batch execution
                return Ok(Some(ops_by_level_paths)).wrap_with_cost(cost);
            }
            current_level = current_level.saturating_sub(1);
        }
        Ok(None).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    /// If the pause height is set in the batch apply options
    /// Then return the list of leftover operations
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
    ) -> CostResult<Option<OpsByLevelPath>, Error> {
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

    /// Method to propagate updated subtree root hashes up to GroveDB root
    /// If the pause height is set in the batch apply options
    /// Then return the list of leftover operations
    fn continue_partial_apply_body<'db, S: StorageContext<'db>>(
        &self,
        previous_leftover_operations: Option<OpsByLevelPath>,
        additional_ops: Vec<GroveDbOp>,
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
    ) -> CostResult<Option<OpsByLevelPath>, Error> {
        let mut cost = OperationCost::default();
        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::continue_from_ops(
                previous_leftover_operations,
                additional_ops,
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
                    // TODO: paths in batches is something to think about
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.insert(
                            path_slices.as_slice(),
                            op.key.as_slice(),
                            element.to_owned(),
                            options.clone().map(|o| o.as_insert_options()),
                            transaction,
                        )
                    );
                }
                Op::Delete => {
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.delete(
                            path_slices.as_slice(),
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

    /// Applies batch on GroveDB
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

    /// Applies batch on GroveDB
    pub fn apply_partial_batch(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        cost_based_add_on_operations: impl FnMut(
            &OperationCost,
            &Option<OpsByLevelPath>,
        ) -> Result<Vec<GroveDbOp>, Error>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        self.apply_partial_batch_with_element_flags_update(
            ops,
            batch_apply_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            cost_based_add_on_operations,
            transaction,
        )
    }

    /// Opens transactional merk at path with given storage batch context.
    /// Returns CostResult.
    pub fn open_batch_transactional_merk_at_path<'db, B: AsRef<[u8]>>(
        &'db self,
        storage_batch: &'db StorageBatch,
        path: SubtreePath<B>,
        tx: &'db Transaction,
        new_merk: bool,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        let mut cost = OperationCost::default();
        let storage = self
            .db
            .get_transactional_storage_context(path.clone(), Some(storage_batch), tx)
            .unwrap_add_cost(&mut cost);

        if let Some((parent_path, parent_key)) = path.derive_parent() {
            if new_merk {
                // TODO: can this be a sum tree
                Ok(Merk::open_empty(storage, MerkType::LayeredMerk, false)).wrap_with_cost(cost)
            } else {
                let parent_storage = self
                    .db
                    .get_transactional_storage_context(parent_path.clone(), Some(storage_batch), tx)
                    .unwrap_add_cost(&mut cost);
                let element = cost_return_on_error!(
                    &mut cost,
                    Element::get_from_storage(&parent_storage, parent_key).map_err(|_| {
                        Error::InvalidPath(format!(
                            "could not get key for parent of subtree for batch at path {}",
                            parent_path.to_vec().into_iter().map(hex::encode).join("/")
                        ))
                    })
                );
                let is_sum_tree = element.is_sum_tree();
                if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                    Merk::open_layered_with_root_key(
                        storage,
                        root_key,
                        is_sum_tree,
                        Some(&Element::value_defined_cost_for_serialized_value),
                    )
                    .map_err(|_| {
                        Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                    })
                    .add_cost(cost)
                } else {
                    Err(Error::CorruptedPath(
                        "cannot open a subtree as parent exists but is not a tree",
                    ))
                    .wrap_with_cost(OperationCost::default())
                }
            }
        } else if new_merk {
            Ok(Merk::open_empty(storage, MerkType::BaseMerk, false)).wrap_with_cost(cost)
        } else {
            Merk::open_base(
                storage,
                false,
                Some(&Element::value_defined_cost_for_serialized_value),
            )
            .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
            .add_cost(cost)
        }
    }

    /// Opens merk at path with given storage batch context. Returns CostResult.
    pub fn open_batch_merk_at_path<'a, B: AsRef<[u8]>>(
        &'a self,
        storage_batch: &'a StorageBatch,
        path: SubtreePath<B>,
        new_merk: bool,
    ) -> CostResult<Merk<PrefixedRocksDbStorageContext>, Error> {
        let mut local_cost = OperationCost::default();
        let storage = self
            .db
            .get_storage_context(path.clone(), Some(storage_batch))
            .unwrap_add_cost(&mut local_cost);

        if new_merk {
            let merk_type = if path.is_root() {
                MerkType::BaseMerk
            } else {
                MerkType::LayeredMerk
            };
            Ok(Merk::open_empty(storage, merk_type, false)).wrap_with_cost(local_cost)
        } else if let Some((base_path, last)) = path.derive_parent() {
            let parent_storage = self
                .db
                .get_storage_context(base_path, Some(storage_batch))
                .unwrap_add_cost(&mut local_cost);
            let element = cost_return_on_error!(
                &mut local_cost,
                Element::get_from_storage(&parent_storage, last)
            );
            let is_sum_tree = element.is_sum_tree();
            if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
                Merk::open_layered_with_root_key(
                    storage,
                    root_key,
                    is_sum_tree,
                    Some(&Element::value_defined_cost_for_serialized_value),
                )
                .map_err(|_| {
                    Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                })
                .add_cost(local_cost)
            } else {
                Err(Error::CorruptedData(
                    "cannot open a subtree as parent exists but is not a tree".to_owned(),
                ))
                .wrap_with_cost(local_cost)
            }
        } else {
            Merk::open_base(
                storage,
                false,
                Some(&Element::value_defined_cost_for_serialized_value),
            )
            .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
            .add_cost(local_cost)
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
                            path.into(),
                            tx,
                            new_merk,
                        )
                    }
                )
            );

            // TODO: compute batch costs
            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_multi_context_batch(storage_batch, Some(tx))
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
                        self.open_batch_merk_at_path(&storage_batch, path.into(), new_merk)
                    }
                )
            );

            // TODO: compute batch costs
            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_multi_context_batch(storage_batch, None)
                    .map_err(|e| e.into())
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Applies a partial batch of operations on GroveDB
    /// The batch is not committed
    /// Clients should set the Batch Apply Options batch pause height
    /// If it is not set we default to pausing at the root tree
    pub fn apply_partial_batch_with_element_flags_update(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        mut update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        mut split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        mut add_on_operations: impl FnMut(
            &OperationCost,
            &Option<OpsByLevelPath>,
        ) -> Result<Vec<GroveDbOp>, Error>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        let mut batch_apply_options = batch_apply_options.unwrap_or_default();
        if batch_apply_options.batch_pause_height.is_none() {
            // we default to pausing at the root tree, which is the most common case
            batch_apply_options.batch_pause_height = Some(1);
        }

        // Determines whether to check batch operation consistency
        // return false if the disable option is set to true, returns true for any other
        // case
        let check_batch_operation_consistency =
            !batch_apply_options.disable_operation_consistency_check;

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
            let left_over_operations = cost_return_on_error!(
                &mut cost,
                self.apply_body(
                    ops,
                    Some(batch_apply_options.clone()),
                    &mut update_element_flags_function,
                    &mut split_removal_bytes_function,
                    |path, new_merk| {
                        self.open_batch_transactional_merk_at_path(
                            &storage_batch,
                            path.into(),
                            tx,
                            new_merk,
                        )
                    }
                )
            );
            // if we paused at the root height, the left over operations would be to replace
            // a lot of leaf nodes in the root tree

            // let's build the write batch
            let (mut write_batch, mut pending_costs) = cost_return_on_error!(
                &mut cost,
                self.db
                    .build_write_batch(storage_batch)
                    .map_err(|e| e.into())
            );

            let total_current_costs = cost.clone().add(pending_costs.clone());

            // todo: estimate root costs

            // at this point we need to send the pending costs back
            // we will get GroveDB a new set of GroveDBOps

            let new_operations = cost_return_on_error_no_add!(
                &cost,
                add_on_operations(&total_current_costs, &left_over_operations)
            );

            // we are trying to finalize
            batch_apply_options.batch_pause_height = None;

            let continue_storage_batch = StorageBatch::new();

            cost_return_on_error!(
                &mut cost,
                self.continue_partial_apply_body(
                    left_over_operations,
                    new_operations,
                    Some(batch_apply_options),
                    update_element_flags_function,
                    split_removal_bytes_function,
                    |path, new_merk| {
                        self.open_batch_transactional_merk_at_path(
                            &continue_storage_batch,
                            path.into(),
                            tx,
                            new_merk,
                        )
                    }
                )
            );

            // let's build the write batch
            let continued_pending_costs = cost_return_on_error!(
                &mut cost,
                self.db
                    .continue_write_batch(&mut write_batch, continue_storage_batch)
                    .map_err(|e| e.into())
            );

            pending_costs.add_assign(continued_pending_costs);

            // TODO: compute batch costs
            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_db_write_batch(write_batch, pending_costs, Some(tx))
                    .map_err(|e| e.into())
            );
        } else {
            let left_over_operations = cost_return_on_error!(
                &mut cost,
                self.apply_body(
                    ops,
                    Some(batch_apply_options.clone()),
                    &mut update_element_flags_function,
                    &mut split_removal_bytes_function,
                    |path, new_merk| {
                        self.open_batch_merk_at_path(&storage_batch, path.into(), new_merk)
                    }
                )
            );

            // if we paused at the root height, the left over operations would be to replace
            // a lot of leaf nodes in the root tree

            // let's build the write batch
            let (mut write_batch, mut pending_costs) = cost_return_on_error!(
                &mut cost,
                self.db
                    .build_write_batch(storage_batch)
                    .map_err(|e| e.into())
            );

            let total_current_costs = cost.clone().add(pending_costs.clone());

            // at this point we need to send the pending costs back
            // we will get GroveDB a new set of GroveDBOps

            let new_operations = cost_return_on_error_no_add!(
                &cost,
                add_on_operations(&total_current_costs, &left_over_operations)
            );

            // we are trying to finalize
            batch_apply_options.batch_pause_height = None;

            let continue_storage_batch = StorageBatch::new();

            cost_return_on_error!(
                &mut cost,
                self.continue_partial_apply_body(
                    left_over_operations,
                    new_operations,
                    Some(batch_apply_options),
                    update_element_flags_function,
                    split_removal_bytes_function,
                    |path, new_merk| {
                        self.open_batch_merk_at_path(&continue_storage_batch, path.into(), new_merk)
                    }
                )
            );

            // let's build the write batch
            let continued_pending_costs = cost_return_on_error!(
                &mut cost,
                self.db
                    .continue_write_batch(&mut write_batch, continue_storage_batch)
                    .map_err(|e| e.into())
            );

            pending_costs.add_assign(continued_pending_costs);

            // TODO: compute batch costs
            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_db_write_batch(write_batch, pending_costs, None)
                    .map_err(|e| e.into())
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    #[cfg(feature = "estimated_costs")]
    /// Returns the estimated average or worst case cost for an entire batch of
    /// ops
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

#[cfg(test)]
mod tests {
    use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    use grovedb_merk::proofs::Query;

    use super::*;
    use crate::{
        reference_path::ReferencePathType,
        tests::{
            common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF,
        },
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
        db.get(EMPTY_PATH, b"key1", None)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref()].as_ref(), b"key2", None)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2"].as_ref(), b"key3", None)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2", b"key3"].as_ref(), b"key4", None)
            .unwrap()
            .expect("cannot get element");

        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"].as_ref(), b"key4", None)
                .unwrap()
                .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None)
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
                    batch_pause_height: None,
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

        db.insert(EMPTY_PATH, b"keyb", Element::empty_tree(), None, Some(&tx))
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
        db.get(EMPTY_PATH, b"keyb", None)
            .unwrap()
            .expect_err("we should not get an element");
        db.get(EMPTY_PATH, b"keyb", Some(&tx))
            .unwrap()
            .expect("we should get an element");

        db.get(EMPTY_PATH, b"key1", None)
            .unwrap()
            .expect_err("we should not get an element");
        db.get(EMPTY_PATH, b"key1", Some(&tx))
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref()].as_ref(), b"key2", Some(&tx))
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref(), b"key2"].as_ref(), b"key3", Some(&tx))
            .unwrap()
            .expect("cannot get element");
        db.get(
            [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
            b"key4",
            Some(&tx),
        )
        .unwrap()
        .expect("cannot get element");

        assert_eq!(
            db.get(
                [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
                b"key4",
                Some(&tx)
            )
            .unwrap()
            .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", Some(&tx))
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
                    some_element_flags,
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
            [0u8].to_vec(),
            Element::new_item(b"serialized_contract".to_vec()),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec()],
            [1u8].to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec()],
            b"domain".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec(), b"domain".to_vec()],
            [0u8].to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec(), b"domain".to_vec()],
            b"normalized_domain_label".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec(), b"domain".to_vec()],
            b"unique_records".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec(), b"domain".to_vec()],
            b"alias_records".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec()],
            b"preorder".to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec(), b"preorder".to_vec()],
            [0u8].to_vec(),
            Element::empty_tree(),
        ));
        grove_db_ops.push(GroveDbOp::insert_op(
            vec![b"contract".to_vec(), [1u8].to_vec(), b"preorder".to_vec()],
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
                [1u8].to_vec(),
                b"domain".to_vec(),
                [0u8].to_vec(),
            ],
            b"serialized_domain_id".to_vec(),
            Element::new_item(b"serialized_domain".to_vec()),
        ));

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                [1u8].to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
            ],
            b"dash".to_vec(),
            Element::empty_tree(),
        ));

        grove_db_ops.push(GroveDbOp::insert_op(
            vec![
                b"contract".to_vec(),
                [1u8].to_vec(),
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
                [1u8].to_vec(),
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
                [1u8].to_vec(),
                b"domain".to_vec(),
                b"normalized_domain_label".to_vec(),
                b"dash".to_vec(),
                b"normalized_label".to_vec(),
                b"sam".to_vec(),
            ],
            b"sam_id".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"contract".to_vec(),
                [1u8].to_vec(),
                b"domain".to_vec(),
                [0u8].to_vec(),
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
                element,
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
        assert!(db
            .get([b"key1".as_ref()].as_ref(), b"key2", None)
            .unwrap()
            .is_err());
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
                element,
            ),
            GroveDbOp::insert_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
        assert!(db
            .get([b"key1".as_ref()].as_ref(), b"key2", None)
            .unwrap()
            .is_err());
        assert!(db
            .get([TEST_LEAF, b"key1"].as_ref(), b"key2", None)
            .unwrap()
            .is_err(),);
    }

    #[test]
    fn test_batch_validation_deletion_brokes_chain() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert(EMPTY_PATH, b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert(
            [b"key1".as_ref()].as_ref(),
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
                element,
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
                element,
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
        db.get([b"key1".as_ref(), b"key2", b"key3"].as_ref(), b"key4", None)
            .unwrap()
            .expect_err("nothing should have been inserted");
    }

    #[test]
    fn test_batch_validation_insert_into_existing_tree() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert(
            [TEST_LEAF].as_ref(),
            b"invalid",
            element.clone(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert value");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"valid",
            Element::empty_tree(),
            None,
            None,
        )
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
            db.get([TEST_LEAF, b"valid"].as_ref(), b"key1", None)
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
            [TEST_LEAF].as_ref(),
            b"key_subtree",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert a subtree");
        db.insert(
            [TEST_LEAF, b"key_subtree"].as_ref(),
            b"key2",
            element,
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert an item");

        // TEST_LEAF can not be overwritten
        let ops = vec![
            GroveDbOp::insert_op(vec![], TEST_LEAF.to_vec(), element2),
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
                    batch_pause_height: None,
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
                    batch_pause_height: None,
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
                    batch_pause_height: None,
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

        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert a subtree");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            element.clone(),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert an item");
        let ops = vec![GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"ayy2".to_vec()),
        )];

        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None)
                .unwrap()
                .expect("cannot get item"),
            element
        );
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect("cannot apply batch");
        assert!(db
            .get([TEST_LEAF, b"key1"].as_ref(), b"key2", None)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_multi_tree_insertion_deletion_with_propagation_no_tx() {
        let db = make_test_grovedb();
        db.insert(EMPTY_PATH, b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert(EMPTY_PATH, b"key2", Element::empty_tree(), None, None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
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

        assert!(db
            .get([ANOTHER_TEST_LEAF].as_ref(), b"key1", None)
            .unwrap()
            .is_err());

        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"].as_ref(), b"key4", None)
                .unwrap()
                .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key", None)
                .unwrap()
                .expect("cannot get element"),
            element2
        );
        assert_ne!(
            db.root_hash(None).unwrap().expect("cannot get root hash"),
            hash
        );

        // verify root leaves
        assert!(db.get(EMPTY_PATH, TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get(EMPTY_PATH, ANOTHER_TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get(EMPTY_PATH, b"key1", None).unwrap().is_ok());
        assert!(db.get(EMPTY_PATH, b"key2", None).unwrap().is_ok());
        assert!(db.get(EMPTY_PATH, b"key3", None).unwrap().is_err());
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
            db.insert(acc_path.as_slice(), &p, Element::empty_tree(), None, None)
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

        let batch = vec![GroveDbOp::insert_op(acc_path, b"key".to_vec(), element)];
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
            db.insert(acc_path.as_slice(), &p, Element::empty_tree(), None, None)
                .unwrap()
                .expect("expected to insert");
            acc_path.push(p);
        }

        let root_hash = db.root_hash(None).unwrap().unwrap();

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert_op(
            acc_path.clone(),
            b"key".to_vec(),
            element,
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
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key1", None)
                .unwrap()
                .unwrap(),
            elem
        );

        // should successfully prove reference as the value hash is valid
        let mut reference_key_query = Query::new();
        reference_key_query.insert_key(b"key1".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], reference_key_query);
        let proof = db
            .prove_query(&path_query)
            .unwrap()
            .expect("should generate proof");
        let verification_result = GroveDb::verify_query_raw(&proof, &path_query);
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
            GroveDbOp::insert_op(vec![TEST_LEAF.to_vec()], b"invalid_path".to_vec(), elem),
        ];
        assert!(matches!(
            db.apply_batch(batch, None, None).unwrap(),
            Err(Error::ReferenceLimit)
        ));
    }
}
