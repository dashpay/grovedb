//! GroveDB batch operations support

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
    ops::Add,
};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostsExt, OperationCost,
};
use intrusive_collections::{intrusive_adapter, Bound, KeyAdapter, RBTree, RBTreeLink};
use merk::Merk;
use nohash_hasher::IntMap;
use storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use visualize::{Drawer, Visualize};

use crate::{
    batch::BatchMerkTreeCache::{KnownMerkTreePaths, MerkTreesByPath},
    Element, Error, GroveDb, Transaction, TransactionArg, ROOT_LEAFS_SERIALIZED_KEY,
};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Op {
    ReplaceTreeHash { hash: [u8; 32] },
    Insert { element: Element },
    Delete,
}

impl Op {
    fn worst_case_cost(&self) -> OperationCost {
        match self {
            Op::ReplaceTreeHash { .. } => OperationCost {
                seek_count: 0,
                storage_written_bytes: 0,
                storage_loaded_bytes: 0,
                loaded_bytes: 0,
                hash_byte_calls: 0,
                hash_node_calls: 0,
            },
            Op::Insert { element } => OperationCost {
                seek_count: 0,
                storage_written_bytes: 0,
                storage_loaded_bytes: 0,
                loaded_bytes: 0,
                hash_byte_calls: 0,
                hash_node_calls: 0,
            },
            Op::Delete => OperationCost {
                seek_count: 0,
                storage_written_bytes: 0,
                storage_loaded_bytes: 0,
                loaded_bytes: 0,
                hash_byte_calls: 0,
                hash_node_calls: 0,
            },
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

/// Batch operation
#[derive(Clone)]
pub struct GroveDbOp {
    /// Path to a subtree - subject to an operation
    pub path: Vec<Vec<u8>>,
    /// Key of an element in the subtree
    pub key: Vec<u8>,
    /// Operation to perform on the key
    pub op: Op,
}

impl PartialEq for GroveDbOp {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.key == other.key && self.op == other.op
    }
}

impl std::fmt::Debug for GroveDbOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut path_out = Vec::new();
        let mut path_drawer = Drawer::new(&mut path_out);
        for p in &self.path {
            path_drawer = p.visualize(path_drawer).unwrap();
            path_drawer.write(b" ").unwrap();
        }
        let mut key_out = Vec::new();
        let key_drawer = Drawer::new(&mut key_out);
        self.key.visualize(key_drawer).unwrap();

        let op_dbg = match self.op {
            Op::Insert {
                element: Element::Tree(..),
            } => "Insert tree",
            Op::Insert { .. } => "Insert",
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

/// Wrapper struct to put shallow subtrees first
#[derive(Debug, Eq, PartialEq)]
struct PathWrapper<'a>(&'a [Vec<u8>]);

impl<'a> PartialOrd for PathWrapper<'a> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let l = self.0.len().partial_cmp(&other.0.len());
        match l {
            Some(Ordering::Equal) => self.0.partial_cmp(other.0),
            _ => l,
        }
    }
}

impl<'a> Ord for PathWrapper<'a> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other)
            .expect("paths are always comparable")
    }
}

impl GroveDbOp {
    pub fn insert(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Insert { element },
        }
    }

    pub fn delete(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        Self {
            path,
            key,
            op: Op::Delete,
        }
    }
}

/// The batch merk tree cache can either cache merks trees, or in the case of
/// worst case scenario calculation is can just identify which trees would
/// normally be cached.
enum BatchMerkTreeCache<S> {
    MerkTreesByPath(HashMap<Vec<Vec<u8>>, Merk<S>>), // for batch operations
    KnownMerkTreePaths(HashSet<Vec<Vec<u8>>>),       // for worst case scenario costs
}

impl<S> BatchMerkTreeCache<S> {
    fn insert(
        &mut self,
        op: &GroveDbOp,
        get_merk_fn: Option<&impl Fn(&[Vec<u8>]) -> CostContext<Result<Merk<S>, Error>>>,
    ) -> CostContext<Result<(), Error>> {
        match self {
            MerkTreesByPath(ref mut merk_trees_by_path) => {
                let mut cost = OperationCost::default();
                let merk = cost_return_on_error!(&mut cost, get_merk_fn.unwrap()(&op.path));
                let mut inserted_path = op.path.clone();
                inserted_path.push(op.key.clone());
                merk_trees_by_path.insert(inserted_path, merk);
                Ok(()).wrap_with_cost(cost)
            }
            KnownMerkTreePaths(ref mut known_merk_tree_paths) => {
                let mut inserted_path = op.path.clone();
                inserted_path.push(op.key.clone());
                known_merk_tree_paths.insert(inserted_path);
                let worst_case_cost = OperationCost::default();
                // let worst_case_cost = OperationCost::worst_case_get_merk();
                Ok(()).wrap_with_cost(worst_case_cost)
            }
        }
    }
}

struct BatchStructure<S> {
    /// Operations by level path
    ops_by_level_path: IntMap<usize, BTreeMap<Vec<Vec<u8>>, BTreeMap<Vec<u8>, Op>>>,
    /// Deleted paths (all elements)
    deleted_paths: HashSet<Vec<Vec<u8>>>,
    /// Merk trees
    merk_tree_cache: BatchMerkTreeCache<S>,
    /// Last level
    last_level: usize,
}

impl<'db, S> BatchStructure<S>
where
    S: StorageContext<'db>,
    <S as StorageContext<'db>>::Error: std::error::Error,
{
    fn from_ops(
        ops: Vec<GroveDbOp>,
        get_merk_fn: Option<impl Fn(&[Vec<u8>]) -> CostContext<Result<Merk<S>, Error>>>,
    ) -> CostContext<Result<BatchStructure<S>, Error>> {
        let mut cost = OperationCost::default();

        let mut ops_by_level_path: IntMap<usize, BTreeMap<Vec<Vec<u8>>, BTreeMap<Vec<u8>, Op>>> =
            IntMap::default();

        let mut current_last_level: usize = 0;

        let mut deleted_paths: HashSet<Vec<Vec<u8>>> = HashSet::new();
        let mut merk_tree_cache = if get_merk_fn.is_some() {
            MerkTreesByPath(HashMap::new())
        } else {
            KnownMerkTreePaths(HashSet::new())
        };

        for op in ops.into_iter() {
            let mut op_cost = OperationCost::default();
            let op_result = match &op.op {
                Op::Insert { element } => {
                    if let Element::Tree(..) = element {
                        merk_tree_cache.insert(&op, get_merk_fn.as_ref());
                    }
                    Ok(())
                }
                Op::Delete => {
                    let mut deleted_path = op.path.clone();
                    deleted_path.push(op.key.clone());
                    deleted_paths.insert(deleted_path);
                    Ok(())
                }
                Op::ReplaceTreeHash { .. } => Err(Error::InvalidBatchOperation(
                    "replace tree hash is an internal operation only",
                )),
            };
            if op_result.is_err() {
                return Err(op_result.err().unwrap()).wrap_with_cost(op_cost);
            }

            let level = op.path.len();
            if let Some(ops_on_level) = ops_by_level_path.get_mut(&level) {
                if let Some(ops_on_path) = ops_on_level.get_mut(op.path.as_slice()) {
                    ops_on_path.insert(op.key, op.op);
                } else {
                    let mut ops_on_path: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
                    ops_on_path.insert(op.key, op.op);
                    ops_on_level.insert(op.path.clone(), ops_on_path);
                }
            } else {
                let mut ops_on_path: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
                ops_on_path.insert(op.key, op.op);
                let mut ops_on_level: BTreeMap<Vec<Vec<u8>>, BTreeMap<Vec<u8>, Op>> =
                    BTreeMap::new();
                ops_on_level.insert(op.path, ops_on_path);
                ops_by_level_path.insert(level, ops_on_level);
                if current_last_level < level {
                    current_last_level = level;
                }
            }
        }
        Ok(BatchStructure {
            ops_by_level_path,
            deleted_paths,
            merk_tree_cache,
            last_level: current_last_level,
        })
        .wrap_with_cost(cost)
    }
}

impl GroveDb {
    // /// Batch application generic over storage context (whether there is a
    // /// transaction or not).
    // fn apply_body<'db, S: StorageContext<'db>>(
    //     &self,
    //     sorted_operations: &mut RBTree<GroveDbOpAdapter>,
    //     temp_root_leaves: &mut BTreeMap<Vec<u8>, usize>,
    //     get_merk_fn: impl Fn(&[Vec<u8>]) -> CostContext<Result<Merk<S>, Error>>,
    // ) -> CostContext<Result<(), Error>> {
    //     let mut cost = OperationCost::default();
    //
    //     let mut temp_subtrees: HashMap<Vec<Vec<u8>>, Merk<_>> = HashMap::new();
    //     let mut cursor = sorted_operations.back_mut();
    //     let mut prev_path = cursor.get().expect("batch is not
    // empty").path.clone();
    //
    //     loop {
    //         // Run propagation if next operation is on different path or no more
    // operations         // left
    //         if cursor.get().map(|op| op.path != prev_path).unwrap_or(true) {
    //             if let Some((key, path_slice)) = prev_path.split_last() {
    //                 let hash = temp_subtrees
    //                     .remove(&prev_path)
    //                     .expect("subtree was inserted before")
    //                     .root_hash()
    //                     .unwrap(); // TODO implement costs
    //
    //                 cursor.insert(Box::new(GroveDbOp::insert(
    //                     path_slice.to_vec(),
    //                     key.to_vec(),
    //                     Element::new_tree(hash),
    //                 )));
    //             }
    //         }
    //
    //         // Execute next available operation
    //         // TODO: investigate how not to create a new cursor each time
    //         cursor = sorted_operations.back_mut();
    //         if let Some(op) = cursor.remove() {
    //             if op.path.is_empty() {
    //                 // Altering root leaves
    //                 // We don't match operation here as only insertion is
    // supported                 if temp_root_leaves.get(&op.key).is_none() {
    //                     temp_root_leaves.insert(op.key, temp_root_leaves.len());
    //                 }
    //             } else {
    //                 // Keep opened Merk instances to accumulate changes before
    // taking final root                 // hash
    //                 let mut merk = cost_return_on_error!(
    //                     &mut cost,
    //                     temp_subtrees
    //                         .remove(&op.path)
    //                         .map(|x| Ok(x).wrap_with_cost(Default::default()))
    //                         .unwrap_or_else(|| get_merk_fn(&op.path))
    //                 );
    //
    //                 // On subtree deletion/overwrite we need to do Merk's cleanup
    //                 match Element::get(&merk, &op.key).unwrap_add_cost(&mut cost)
    // {                     Ok(Element::Tree(..)) => {
    //                         let mut path = op.path.clone();
    //                         path.push(op.key.clone());
    //
    //                         cost_return_on_error!(
    //                             &mut cost,
    //                             temp_subtrees
    //                                 .remove(&path)
    //                                 .map(|x|
    // Ok(x).wrap_with_cost(Default::default()))
    // .unwrap_or_else(|| get_merk_fn(&path))
    // .flat_map_ok(|mut s| s                                     .clear()
    //                                     .map_err(|_| Error::InternalError("cannot
    // clear a Merk")))                         );
    //                     }
    //                     Err(Error::PathKeyNotFound(_) | Error::PathNotFound(_)) |
    // Ok(_) => {                         // TODO: the case when key is
    // scheduled for deletion                         // but cannot be found is
    // weird and requires some                         // investigation
    //                     }
    //                     Err(e) => return Err(e).wrap_with_cost(cost),
    //                 }
    //                 match op.op {
    //                     Op::Insert { element } => {
    //                         cost_return_on_error!(&mut cost, element.insert(&mut
    // merk, op.key));
    // temp_subtrees.insert(op.path.clone(), merk);                     }
    //                     Op::Delete => {
    //                         cost_return_on_error!(&mut cost, Element::delete(&mut
    // merk, op.key));
    // temp_subtrees.insert(op.path.clone(), merk);                     }
    //                 }
    //             }
    //             prev_path = op.path;
    //         } else {
    //             break;
    //         }
    //     }
    //     Ok(()).wrap_with_cost(cost)
    // }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn apply_body<'db, S: StorageContext<'db>>(
        &self,
        ops: Vec<GroveDbOp>,
        temp_root_leaves: &mut BTreeMap<Vec<u8>, usize>,
        get_merk_fn: impl Fn(&[Vec<u8>]) -> CostContext<Result<Merk<S>, Error>>,
    ) -> CostContext<Result<(), Error>> {
        let mut cost = OperationCost::default();

        let BatchStructure {
            mut ops_by_level_path,
            mut deleted_paths,
            mut merk_tree_cache,
            last_level,
        } = cost_return_on_error!(&mut cost, BatchStructure::from_ops(ops, Some(&get_merk_fn)));

        if let MerkTreesByPath(ref mut merk_trees_by_path) = merk_tree_cache {
            let mut current_level = last_level;
            // We will update up the tree
            while let Some(ops_at_level) = ops_by_level_path.remove(&current_level) {
                for (path, ops_at_path) in ops_at_level.into_iter() {
                    if current_level == 0 {
                        for (key, op) in ops_at_path.into_iter() {
                            match op {
                                Op::Insert { element } => {
                                    if temp_root_leaves.get(key.as_slice()).
                                    is_none() {
                                        temp_root_leaves.insert(key,
                                    temp_root_leaves.len());
                                    }
                                }
                                Op::Delete => {
                                    return Err(Error::InvalidBatchOperation(
                                        "deletion of root tree not possible",
                                    ))
                                    .wrap_with_cost(cost);
                                }
                                Op::ReplaceTreeHash { hash } => {}
                            }
                        }
                    } else {
                        let mut merk: Merk<_> = cost_return_on_error!(
                            &mut cost,
                            merk_trees_by_path
                                .remove(&path)
                                .map(|x| Ok(x).wrap_with_cost(Default::default()))
                                .unwrap_or_else(|| get_merk_fn(&path))
                        );
                        for (key, op) in ops_at_path.into_iter() {
                            let mut path_with_key = path.clone();
                            path_with_key.push(key.clone());
                            match op {
                                Op::Insert { element } => {
                                    cost_return_on_error!(
                                        &mut cost,
                                        element.insert(&mut merk, key)
                                    );
                                }
                                Op::Delete => {
                                    cost_return_on_error!(
                                        &mut cost,
                                        Element::delete(&mut merk, key)
                                    );
                                }
                                Op::ReplaceTreeHash { hash } => {
                                    cost_return_on_error!(
                                        &mut cost,
                                        Self::update_tree_item_preserve_flag(
                                            &mut merk,
                                            key.as_slice(),
                                            hash,
                                        )
                                    );
                                }
                            }
                        }

                        if current_level > 1 {
                            let root_hash = merk.root_hash().unwrap_add_cost(&mut cost);

                            // We need to propagate up this root hash, this means adding grove_db
                            // operations up for the level above
                            if let Some((key, parent_path)) = path.split_last() {
                                if let Some(ops_at_level_above) =
                                ops_by_level_path.get_mut(&(current_level - 1))
                                {
                                    if let Some(ops_on_path) = ops_at_level_above.get_mut(parent_path) {
                                        if let Some(op) = ops_on_path.remove(key) {
                                            match op {
                                                Op::ReplaceTreeHash { mut hash } => hash = root_hash,
                                                Op::Insert { element } => {
                                                    if let Element::Tree(mut hash, _) = element {
                                                        hash = root_hash
                                                    }
                                                }
                                                Op::Delete => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "insertion of element under a deleted tree",
                                                    ))
                                                        .wrap_with_cost(cost);
                                                }
                                            }
                                        } else {
                                            ops_on_path.insert(
                                                key.clone(),
                                                Op::ReplaceTreeHash { hash: root_hash },
                                            );
                                        }
                                    } else {
                                        let mut ops_on_path: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
                                        ops_on_path.insert(
                                            key.clone(),
                                            Op::ReplaceTreeHash { hash: root_hash },
                                        );
                                        ops_at_level_above.insert(parent_path.to_vec(), ops_on_path);
                                    }
                                } else {
                                    let mut ops_on_path: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
                                    ops_on_path
                                        .insert(key.clone(), Op::ReplaceTreeHash { hash: root_hash });
                                    let mut ops_on_level: BTreeMap<
                                        Vec<Vec<u8>>,
                                        BTreeMap<Vec<u8>, Op>,
                                    > = BTreeMap::new();
                                    ops_on_level.insert(parent_path.to_vec(), ops_on_path);
                                    ops_by_level_path.insert(current_level - 1, ops_on_level);
                                }
                            }
                        }


                    }
                }
                current_level -= 1;
            }
        } else {
            return Err(Error::CorruptedData(
                "impossible code execution in batch apply body".to_string(),
            ))
            .wrap_with_cost(cost);
        }
        Ok(()).wrap_with_cost(cost)
    }

    // fn apply_body_direct<'db, S: StorageContext<'db>>(
    //     &self,
    //     sorted_operations: Vec<GroveDbOp>,
    //     temp_root_leaves: &mut BTreeMap<Vec<u8>, usize>,
    //     get_merk_fn: impl Fn(&[Vec<u8>]) -> CostContext<Result<Merk<S>, Error>>,
    // ) -> CostContext<Result<(), Error>> {
    //     let mut cost = OperationCost::default();
    //     let mut temp_subtrees: HashMap<Vec<Vec<u8>>, Merk<_>> = HashMap::new();
    //     for op in sorted_operations.into_iter() {
    //         if op.path.is_empty() {
    //             // Altering root leaves
    //             // We don't match operation here as only insertion is supported
    //             if temp_root_leaves.get(&op.key).is_none() {
    //                 temp_root_leaves.insert(op.key, temp_root_leaves.len());
    //             }
    //         } else {
    //             // Keep opened Merk instances to accumulate changes before taking
    // final root             // hash
    //             let mut merk = cost_return_on_error!(
    //                 &mut cost,
    //                 temp_subtrees
    //                     .remove(&op.path)
    //                     .map(|x| Ok(x).wrap_with_cost(Default::default()))
    //                     .unwrap_or_else(|| get_merk_fn(&op.path))
    //             );
    //             match op.op {
    //                 Op::Insert { element } => {
    //                     cost_return_on_error!(&mut cost, element.insert(&mut
    // merk, op.key));                     temp_subtrees.insert(op.path, merk);
    //                 }
    //                 Op::Delete => {
    //                     cost_return_on_error!(&mut cost, Element::delete(&mut
    // merk, op.key));                     temp_subtrees.insert(op.path, merk);
    //                 }
    //             }
    //         }
    //     }
    //     Ok(()).wrap_with_cost(cost)
    // }

    // /// Validates batch using a set of rules:
    // /// 1. Subtree must exist to perform operations on it;
    // /// 2. Subtree is treated as exising if it can be found in storage;
    // /// 3. Subtree is treated as exising if it is created within the same batch;
    // /// 4. Subtree is treated as not existing otherwise or if there is a delete
    // ///    operation with no subtree insertion counterpart;
    // /// 5. Subtree overwrite/deletion produces explicit delete operations for
    // ///    every descendant subtree
    // /// 6. Operations are unique
    // fn validate_batch(
    //     &self,
    //     mut ops: RBTree<GroveDbOpAdapter>,
    //     root_leaves: &BTreeMap<Vec<u8>, usize>,
    //     transaction: TransactionArg,
    // ) -> CostContext<Result<RBTree<GroveDbOpAdapter>, Error>> {
    //     let mut cost = OperationCost::default();
    //
    //     // To ensure that batch `[insert([a, b], c, t), insert([a, b, c], k, v)]`
    // is     // valid we need to check that subtree `[a, b]` exists;
    //     // If we add `insert([a], b, t)` we need to check (query the DB) only
    // `[a]`     // subtree as all operations form a chain and we check only
    // head to exist.     //
    //     // `valid_subtrees` is used to cache check results for these chains
    //     let mut valid_subtrees: HashSet<Vec<Vec<u8>>> = HashSet::new();
    //
    //     // An opposite to `valid_subtrees`, all overwritten and deleted subtrees
    // are     // cached there; This is required as data might be staged for
    // deletion     // and subtree will become invalid to insert to even if it
    // exists in     // pre-batch database state.
    //     let mut removed_subtrees: HashSet<Vec<Vec<u8>>> = HashSet::new();
    //
    //     // First pass is required to expand recursive deletions and possible
    // subtree     // overwrites.
    //     let mut delete_ops = Vec::new();
    //     for op in ops.iter() {
    //         let delete_paths = cost_return_on_error!(
    //             &mut cost,
    //             self.find_subtrees(
    //                 op.path
    //                     .iter()
    //                     .map(|x| x.as_slice())
    //                     .chain(std::iter::once(op.key.as_slice())),
    //                 transaction,
    //             )
    //         );
    //         delete_ops.extend(delete_paths.iter().map(|p| {
    //             let (key, path) = p.split_last().expect("no empty paths
    // expected");             Box::new(GroveDbOp::delete(path.to_vec(),
    // key.to_vec()))         }));
    //         for p in delete_paths {
    //             removed_subtrees.insert(p);
    //         }
    //     }
    //     for op in delete_ops {
    //         insert_unique_op(&mut ops, op);
    //     }
    //
    //     // Insertion to root tree is valid as root tree always exists
    //     valid_subtrees.insert(Vec::new());
    //
    //     // Validation goes from top to bottom so each operation will be in
    // context of     // what happened to ancestors of a subject subtree.
    //     for op in ops.iter() {
    //         let path: &[Vec<u8>] = &op.path;
    //
    //         // Insertion into subtree that was deleted in this batch is invalid
    //         if matches!(op.op, Op::Insert { .. }) &&
    // removed_subtrees.contains(path) {             return
    // Err(Error::InvalidPath("attempt to insert into deleted subtree"))
    //                 .wrap_with_cost(cost);
    //         }
    //
    //         // Attempt to subtrees cache to see if subtree exists or will exists
    // within the         // batch
    //         if !valid_subtrees.contains(path) {
    //             // Tree wasn't checked before and won't be inserted within the
    // batch, need to             // access pre-batch database state:
    //             if path.len() == 0 {
    //                 // We're working with root leaf subtree there
    //                 if !root_leaves.contains_key(&op.key) {
    //                     return Err(Error::PathNotFound("missing root
    // leaf")).wrap_with_cost(cost);                 }
    //                 if let Op::Delete = op.op {
    //                     return Err(Error::InvalidPath(
    //                         "deletion for root leafs is not supported",
    //                     ))
    //                     .wrap_with_cost(cost);
    //                 }
    //             } else {
    //                 // Dealing with a deeper subtree (not a root leaf so to say)
    //                 let (parent_key, parent_path) =
    //                     path.split_last().expect("empty path already checked");
    //                 let subtree = cost_return_on_error!(
    //                     &mut cost,
    //                     self.get(
    //                         parent_path.iter().map(|x| x.as_slice()),
    //                         parent_key,
    //                         transaction,
    //                     )
    //                 );
    //                 if !matches!(subtree, Element::Tree(_, _)) {
    //                     // There is an attempt to insert into a scalar
    //                     return Err(Error::InvalidPath("must be a
    // tree")).wrap_with_cost(cost);                 }
    //             }
    //         }
    //
    //         match *op {
    //             // Insertion of a tree makes this subtree valid
    //             GroveDbOp {
    //                 ref path,
    //                 ref key,
    //                 op:
    //                     Op::Insert {
    //                         element: Element::Tree(..),
    //                     },
    //                 ..
    //             } => {
    //                 let mut new_path = path.to_vec();
    //                 new_path.push(key.to_vec());
    //                 removed_subtrees.remove(&new_path);
    //                 valid_subtrees.insert(new_path);
    //             }
    //             // Deletion of a tree makes a subtree unavailable
    //             GroveDbOp {
    //                 ref path,
    //                 ref key,
    //                 op: Op::Delete,
    //                 ..
    //             } => {
    //                 let mut new_path = path.to_vec();
    //                 new_path.push(key.to_vec());
    //                 valid_subtrees.remove(&new_path);
    //                 removed_subtrees.insert(new_path);
    //             }
    //             _ => {}
    //         }
    //     }
    //
    //     Ok(ops).wrap_with_cost(cost)
    // }

    // pub fn worst_case_fees_for_batch(
    //     &self,
    //     ops: Vec<GroveDbOp>,
    // ) -> CostContext<Result<(), Error>> {
    //     let mut cost = OperationCost::default();
    //
    //     if ops.is_empty() {
    //         return Ok(()).wrap_with_cost(cost);
    //     }
    //
    //     let BatchStructure {
    //         mut ops_by_level_path,
    //         mut deleted_paths,
    //         mut merk_tree_cache,
    //         last_level
    //     } = cost_return_on_error!(
    //                 &mut cost, BatchStructure::from_ops(ops, &get_merk_fn));
    //
    //     if let KnownMerkTreePaths(known_merk_tree_paths) = merk_tree_cache {
    //         let mut current_last_level = last_level;
    //         // We will update up the tree
    //         while let Some(ops_at_level) =
    // ops_by_level_path.remove(&current_last_level) {             for (path,
    // ops_at_path) in ops_at_level.into_iter() {                 if
    // current_last_level == 1 {                     for (key, op) in
    // ops_at_path.into_iter() {                         match op {
    //                             Op::Insert { element } => {
    //                                 // if
    // temp_root_leaves.get(key.as_slice()).is_none() {                         
    // //     temp_root_leaves.insert(key, temp_root_leaves.len());             
    // // }                             }
    //                             Op::Delete => {
    //                                 return
    // Err(Error::InvalidBatchOperation("deletion of root tree not
    // possible")).wrap_with_cost(cost);                             }
    //                             Op::ReplaceTreeHash { hash } => {}
    //                         }
    //                     }
    //                 } else {
    //                     let mut merk: Merk<_> = cost_return_on_error!(
    //                 &mut cost,
    //                 merk_trees_by_path
    //                          .remove(&path)
    //                          .map(|x| Ok(x).wrap_with_cost(Default::default()))
    //                          .unwrap_or_else(|| get_merk_fn(&path))
    //             );
    //                     for (key, op) in ops_at_path.into_iter() {
    //                         let mut path_with_key = path.clone();
    //                         path_with_key.push(key.clone());
    //                         match op {
    //                             Op::Insert { element } => {
    //                                 cost_return_on_error!(&mut cost,
    // element.insert(&mut merk, key));                             }
    //                             Op::Delete => {
    //                                 cost_return_on_error!(&mut cost,
    // Element::delete(&mut merk, key));                             }
    //                             Op::ReplaceTreeHash { hash } => {
    //                                 cost_return_on_error!(
    //                             &mut cost,
    //                             Self::update_tree_item_preserve_flag(
    //                                 &mut merk,
    //                                 key.as_slice(),
    //                                 hash,
    //                             )
    //                         );
    //                             }
    //                         }
    //                     }
    //
    //                     cost.add_worst_case_merk_root_hash();
    //
    //                     // We need to propagate up this root hash, this means
    // adding grove_db operations                     // up for the level above
    //                     if let Some((key, parent_path)) = path.split_last() {
    //                         if let Some(ops_at_level_above) =
    // ops_by_level_path.get_mut(&(current_last_level - 1))                     
    // {                             if let Some(ops_on_path) =
    // ops_at_level_above.get_mut(parent_path) {                                
    // if let Some(op) = ops_on_path.remove(key) {                              
    // match op {                                         Op::ReplaceTreeHash {
    // mut hash } => {                                             hash =
    // root_hash                                         }
    //                                         Op::Insert { element } => {
    //                                             if let Element::Tree(mut hash, _)
    // = element {                                                 hash =
    // root_hash                                             }
    //                                         }
    //                                         Op::Delete => {
    //                                             return
    // Err(Error::InvalidBatchOperation("insertion of element under a deleted
    // tree")).wrap_with_cost(cost);                                         }
    //                                     }
    //                                 } else {
    //                                     ops_on_path
    //                                         .insert(key.clone(),
    // Op::ReplaceTreeHash { hash: root_hash });                                
    // }                             } else {
    //                                 let mut ops_on_path: BTreeMap<Vec<u8>, Op> =
    // BTreeMap::new();                                 
    // ops_on_path.insert(key.clone(), Op::ReplaceTreeHash { hash: root_hash });
    //                                 
    // ops_at_level_above.insert(parent_path.to_vec(), ops_on_path);            
    // }                         } else {
    //                             let mut ops_on_path: BTreeMap<Vec<u8>, Op> =
    // BTreeMap::new();                             
    // ops_on_path.insert(key.clone(), Op::ReplaceTreeHash { hash: root_hash });
    //                             let mut ops_on_level: BTreeMap<Vec<Vec<u8>>,
    // BTreeMap<Vec<u8>, Op>> =                                 BTreeMap::new();
    //                             ops_on_level.insert(parent_path.to_vec(),
    // ops_on_path);                             
    // ops_by_level_path.insert(current_last_level - 1, ops_on_level);
    //                         }
    //                     }
    //                 }
    //             }
    //             current_last_level -= 1;
    //         }
    //         Ok(()).wrap_with_cost(cost)
    //     } else {
    //         Err(Error::CorruptedData("impossible code execution in
    // worst_case_fees_for_batch".to_string())).wrap_with_cost(cost)     }
    // }

    /// Applies batch of operations on GroveDB
    pub fn apply_batch(
        &self,
        ops: Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>> {
        let mut cost = OperationCost::default();

        // Helper function to store updated root leaves
        fn save_root_leaves<'db, S>(
            storage: S,
            temp_root_leaves: &BTreeMap<Vec<u8>, usize>,
        ) -> CostContext<Result<(), Error>>
        where
            S: StorageContext<'db>,
            Error: From<<S as storage::StorageContext<'db>>::Error>,
        {
            let cost = OperationCost::default();

            let root_leaves_serialized = cost_return_on_error_no_add!(
                &cost,
                bincode::serialize(&temp_root_leaves).map_err(|_| {
                    Error::CorruptedData(String::from("unable to serialize root leaves data"))
                })
            );
            storage
                .put_meta(ROOT_LEAFS_SERIALIZED_KEY, &root_leaves_serialized)
                .map_err(|e| e.into())
                .wrap_with_cost(OperationCost {
                    storage_written_bytes: ROOT_LEAFS_SERIALIZED_KEY.len()
                        + root_leaves_serialized.len(),
                    ..Default::default()
                })
        }

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        let mut temp_root_leaves =
            cost_return_on_error!(&mut cost, self.get_root_leaf_keys(transaction));

        // `StorageBatch` allows us to collect operations on different subtrees before
        // execution
        let storage_batch = StorageBatch::new();

        // With the only one difference (if there is a transaction) do the following:
        // 2. If nothing left to do and we were on a non-leaf subtree or we're done with
        //    one subtree and moved to another then add propagation operation to the
        //    operations tree and drop Merk handle;
        // 3. Take Merk from temp subtrees or open a new one with batched storage
        //    context;
        // 4. Apply operation to the Merk;
        // 5. Remove operation from the tree, repeat until there are operations to do;
        // 6. Add root leaves save operation to the batch
        // 7. Apply storage batch
        if let Some(tx) = transaction {
            cost_return_on_error!(
                &mut cost,
                self.apply_body(ops, &mut temp_root_leaves, |path| {
                    let storage = self.db.get_batch_transactional_storage_context(
                        path.iter().map(|x| x.as_slice()),
                        &storage_batch,
                        tx,
                    );
                    Merk::open(storage)
                        .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
                })
            );

            let meta_storage = self.db.get_batch_transactional_storage_context(
                std::iter::empty(),
                &storage_batch,
                tx,
            );

            cost_return_on_error!(&mut cost, save_root_leaves(meta_storage, &temp_root_leaves));

            // TODO: compute batch costs
            cost_return_on_error_no_add!(
                &cost,
                self.db
                    .commit_multi_context_batch(storage_batch, Some(tx))
                    .map_err(|e| e.into())
            );
        } else {
            cost_return_on_error!(
                &mut cost,
                self.apply_body(ops, &mut temp_root_leaves, |path| {
                    let storage = self.db.get_batch_storage_context(
                        path.iter().map(|x| x.as_slice()),
                        &storage_batch,
                    );
                    Merk::open(storage)
                        .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
                })
            );

            let meta_storage = self
                .db
                .get_batch_storage_context(std::iter::empty(), &storage_batch);
            cost_return_on_error!(&mut cost, save_root_leaves(meta_storage, &temp_root_leaves));

            // TODO: compute batch costs
            cost_return_on_error_no_add!(
                &cost,
                self.db
                    .commit_multi_context_batch(storage_batch, None)
                    .map_err(|e| e.into())
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    // /// Applies batch of operations on GroveDB
    // pub fn apply_batch(
    //     &self,
    //     ops: Vec<GroveDbOp>,
    //     validate: bool,
    //     transaction: TransactionArg,
    // ) -> CostContext<Result<(), Error>> {
    //     let mut cost = OperationCost::default();
    //
    //     // Helper function to store updated root leaves
    //     fn save_root_leaves<'db, S>(
    //         storage: S,
    //         temp_root_leaves: &BTreeMap<Vec<u8>, usize>,
    //     ) -> CostContext<Result<(), Error>>
    //     where
    //         S: StorageContext<'db>,
    //         Error: From<<S as storage::StorageContext<'db>>::Error>,
    //     {
    //         let cost = OperationCost::default();
    //
    //         let root_leaves_serialized = cost_return_on_error_no_add!(
    //             &cost,
    //             bincode::serialize(&temp_root_leaves).map_err(|_| {
    //                 Error::CorruptedData(String::from("unable to serialize root
    // leaves data"))             })
    //         );
    //         storage
    //             .put_meta(ROOT_LEAFS_SERIALIZED_KEY, &root_leaves_serialized)
    //             .map_err(|e| e.into())
    //             .wrap_with_cost(OperationCost {
    //                 storage_written_bytes: ROOT_LEAFS_SERIALIZED_KEY.len()
    //                     + root_leaves_serialized.len(),
    //                 ..Default::default()
    //             })
    //     }
    //
    //     if ops.is_empty() {
    //         return Ok(()).wrap_with_cost(cost);
    //     }
    //
    //     let mut temp_root_leaves =
    //         cost_return_on_error!(&mut cost,
    // self.get_root_leaf_keys(transaction));
    //
    //     // 1. Collect all batch operations into RBTree to keep them sorted and
    // validated     let mut sorted_operations =
    // RBTree::new(GroveDbOpAdapter::new());     for op in ops {
    //         insert_unique_op(&mut sorted_operations, Box::new(op));
    //     }
    //
    //     let mut validated_operations = if validate {
    //         cost_return_on_error!(
    //             &mut cost,
    //             self.validate_batch(sorted_operations, &temp_root_leaves,
    // transaction)         )
    //     } else {
    //         sorted_operations
    //     };
    //
    //     // `StorageBatch` allows us to collect operations on different subtrees
    // before     // execution
    //     let storage_batch = StorageBatch::new();
    //
    //     // With the only one difference (if there is a transaction) do the
    // following:     // 2. If nothing left to do and we were on a non-leaf
    // subtree or we're done with     //    one subtree and moved to another
    // then add propagation operation to the     //    operations tree and drop
    // Merk handle;     // 3. Take Merk from temp subtrees or open a new one
    // with batched storage     //    context;
    //     // 4. Apply operation to the Merk;
    //     // 5. Remove operation from the tree, repeat until there are operations
    // to do;     // 6. Add root leaves save operation to the batch
    //     // 7. Apply storage batch
    //     if let Some(tx) = transaction {
    //         cost_return_on_error!(
    //             &mut cost,
    //             self.apply_body(&mut validated_operations, &mut temp_root_leaves,
    // |path| {                 let storage =
    // self.db.get_batch_transactional_storage_context(                     
    // path.iter().map(|x| x.as_slice()),                     &storage_batch,
    //                     tx,
    //                 );
    //                 Merk::open(storage)
    //                     .map_err(|_| Error::CorruptedData("cannot open a
    // subtree".to_owned()))             })
    //         );
    //
    //         let meta_storage = self.db.get_batch_transactional_storage_context(
    //             std::iter::empty(),
    //             &storage_batch,
    //             tx,
    //         );
    //
    //         cost_return_on_error!(&mut cost, save_root_leaves(meta_storage,
    // &temp_root_leaves));
    //
    //         // TODO: compute batch costs
    //         cost_return_on_error_no_add!(
    //             &cost,
    //             self.db
    //                 .commit_multi_context_batch(storage_batch, Some(tx))
    //                 .map_err(|e| e.into())
    //         );
    //     } else {
    //         cost_return_on_error!(
    //             &mut cost,
    //             self.apply_body(&mut validated_operations, &mut temp_root_leaves,
    // |path| {                 let storage = self.db.get_batch_storage_context(
    //                     path.iter().map(|x| x.as_slice()),
    //                     &storage_batch,
    //                 );
    //                 Merk::open(storage)
    //                     .map_err(|_| Error::CorruptedData("cannot open a
    // subtree".to_owned()))             })
    //         );
    //
    //         let meta_storage = self
    //             .db
    //             .get_batch_storage_context(std::iter::empty(), &storage_batch);
    //         cost_return_on_error!(&mut cost, save_root_leaves(meta_storage,
    // &temp_root_leaves));
    //
    //         // TODO: compute batch costs
    //         cost_return_on_error_no_add!(
    //             &cost,
    //             self.db
    //                 .commit_multi_context_batch(storage_batch, None)
    //                 .map_err(|e| e.into())
    //         );
    //     }
    //     Ok(()).wrap_with_cost(cost)
    // }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{make_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF};

    #[test]
    fn test_batch_validation_ok() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element2.clone(),
            ),
        ];
        db.apply_batch(ops, None)
            .unwrap()
            .expect("cannot apply batch");

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
        let db = make_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"keyb", Element::empty_tree(), Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element2.clone(),
            ),
        ];
        db.apply_batch(ops, Some(&tx))
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
    fn test_batch_validation_broken_chain() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_err());
        assert!(db.get([b"key1".as_ref()], b"key2", None).unwrap().is_err());
    }

    #[test]
    fn test_batch_validation_broken_chain_aborts_whole_batch() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_err());
        assert!(db.get([b"key1".as_ref()], b"key2", None).unwrap().is_err());
        assert!(db
            .get([TEST_LEAF, b"key1"], b"key2", None)
            .unwrap()
            .is_err(),);
    }

    #[test]
    fn test_batch_validation_deletion_brokes_chain() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert([], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([], b"key2", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");

        let ops = vec![
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::delete(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_err());
    }

    #[test]
    fn test_batch_validation_deletion_and_insertion_restore_chain() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::delete(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        db.apply_batch(ops, None)
            .unwrap()
            .expect("cannot apply batch");
        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
                .unwrap()
                .expect("cannot get element"),
            element
        );
    }

    #[test]
    fn test_batch_validation_insert_into_existing_tree() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert([TEST_LEAF], b"invalid", element.clone(), None)
            .unwrap()
            .expect("cannot insert value");
        db.insert([TEST_LEAF], b"valid", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert value");

        // Insertion into scalar is invalid
        let ops = vec![GroveDbOp::insert(
            vec![TEST_LEAF.to_vec(), b"invalid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        assert!(db.apply_batch(ops, None).unwrap().is_err());

        // Insertion into a tree is correct
        let ops = vec![GroveDbOp::insert(
            vec![TEST_LEAF.to_vec(), b"valid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        db.apply_batch(ops, None)
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
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        db.insert([TEST_LEAF], b"key_subtree", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([TEST_LEAF, b"key_subtree"], b"key2", element, None)
            .unwrap()
            .expect("cannot insert an item");

        // TEST_LEAF will be overwritten thus nested subtrees will be deleted and it is
        // invalid to insert into them
        let ops = vec![
            GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), element2.clone()),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec(), b"key_subtree".to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_err());

        // TEST_LEAF will became a scalar, insertion into scalar is also invalid
        let ops = vec![
            GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), element2.clone()),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_err());

        // Here TEST_LEAF is overwritten and new data should be available why older data
        // shouldn't
        let ops = vec![
            GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), Element::empty_tree()),
            GroveDbOp::insert(vec![TEST_LEAF.to_vec()], b"key1".to_vec(), element2.clone()),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_ok());

        assert_eq!(
            db.get([TEST_LEAF], b"key1", None)
                .unwrap()
                .expect("cannot get data"),
            element2
        );
        assert!(db
            .get([TEST_LEAF, b"key_subtree"], b"key1", None)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_batch_validation_root_leaf_removal() {
        let db = make_grovedb();
        let ops = vec![
            GroveDbOp::insert(
                vec![],
                TEST_LEAF.to_vec(),
                Element::new_item(b"ayy".to_vec()),
            ),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db.apply_batch(ops, None).unwrap().is_err());
    }

    #[test]
    fn test_merk_data_is_deleted() {
        let db = make_grovedb();
        let element = Element::new_item(b"ayy".to_vec());

        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert a subtree");
        db.insert([TEST_LEAF, b"key1"], b"key2", element.clone(), None)
            .unwrap()
            .expect("cannot insert an item");
        let ops = vec![GroveDbOp::insert(
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
        db.apply_batch(ops, None)
            .unwrap()
            .expect("cannot apply batch");
        assert!(db
            .get([TEST_LEAF, b"key1"], b"key2", None)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_multi_tree_insertion_deletion_with_propagation_no_tx() {
        let db = make_grovedb();
        db.insert([], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert([], b"key2", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert root leaf");
        db.insert([ANOTHER_TEST_LEAF], b"key1", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert root leaf");

        let hash = db
            .root_hash(None)
            .unwrap()
            .ok()
            .flatten()
            .expect("cannot get root hash");
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());

        let ops = vec![
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(vec![TEST_LEAF.to_vec()], b"key".to_vec(), element2.clone()),
            GroveDbOp::delete(vec![ANOTHER_TEST_LEAF.to_vec()], b"key1".to_vec()),
        ];
        db.apply_batch(ops, None)
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
            db.root_hash(None)
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get root hash"),
            hash
        );
        let mut root_leafs = BTreeMap::new();
        root_leafs.insert(TEST_LEAF.to_vec(), 0);
        root_leafs.insert(ANOTHER_TEST_LEAF.to_vec(), 1);
        root_leafs.insert(b"key1".to_vec(), 2);
        root_leafs.insert(b"key2".to_vec(), 3);

        assert_eq!(
            db.get_root_leaf_keys(None)
                .unwrap()
                .expect("cannot get root leafs"),
            root_leafs
        );
    }

    #[test]
    fn test_nested_batch_insertion_corrupts_state() {
        let db = make_grovedb();
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
            .unwrap();
            acc_path.push(p);
        }

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None)
            .unwrap()
            .expect("cannot apply batch");

        let batch = vec![GroveDbOp::insert(
            acc_path,
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None)
            .unwrap()
            .expect("cannot apply same batch twice");
    }

    #[test]
    fn test_apply_sorted_pre_validated_batch_propagation() {
        let db = make_grovedb();
        let full_path = vec![
            b"leaf1".to_vec(),
            b"sub1".to_vec(),
        ];
        let mut acc_path: Vec<Vec<u8>> = vec![];
        for p in full_path.into_iter() {
            db.insert(
                acc_path.iter().map(|x| x.as_slice()),
                &p,
                Element::empty_tree(),
                None,
            )
            .unwrap();
            acc_path.push(p);
        }

        let root_hash = db.root_hash(None).unwrap().unwrap();

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None)
            .unwrap()
            .expect("cannot apply batch");

        assert_ne!(db.root_hash(None).unwrap().unwrap(), root_hash);
    }
}
