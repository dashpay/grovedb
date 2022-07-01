//! GroveDB batch operations support

use core::fmt;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::Merk;
use nohash_hasher::IntMap;
use storage::{Storage, StorageBatch, StorageContext};
use visualize::{DebugByteVectors, DebugBytes, Drawer, Visualize};

use crate::{Element, Error, GroveDb, TransactionArg, ROOT_LEAFS_SERIALIZED_KEY};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Op {
    ReplaceTreeHash { hash: [u8; 32] },
    Insert { element: Element },
    Delete,
}

impl Op {
    fn worst_case_cost(&self, _key: Vec<u8>) -> OperationCost {
        match self {
            Op::ReplaceTreeHash { .. } => OperationCost {
                seek_count: 0,
                storage_written_bytes: 0,
                storage_loaded_bytes: 0,
                loaded_bytes: 0,
                hash_byte_calls: 0,
                hash_node_calls: 0,
            },
            Op::Insert { .. } => OperationCost {
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

impl fmt::Debug for GroveDbOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

/// Cache for Merk trees by their paths.
struct TreeCacheMerkByPath<S, F> {
    merks: HashMap<Vec<Vec<u8>>, Merk<S>>,
    get_merk_fn: F,
}

/// Cache for subtee paths for worst case scenario costs.
#[derive(Default)]
struct TreeCacheKnownPaths {
    paths: HashSet<Vec<Vec<u8>>>,
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

trait TreeCache {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error>;

    fn execute_ops_on_path(
        &mut self,
        path: &[Vec<u8>],
        ops_at_path_by_key: BTreeMap<Vec<u8>, Op>,
        batch_apply_options: &BatchApplyOptions,
    ) -> CostResult<[u8; 32], Error>;
}

impl<'db, S, F> TreeCache for TreeCacheMerkByPath<S, F>
where
    F: Fn(&[Vec<u8>]) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        let merk = cost_return_on_error!(&mut cost, (self.get_merk_fn)(&inserted_path));
        self.merks.insert(inserted_path, merk);

        Ok(()).wrap_with_cost(cost)
    }

    fn execute_ops_on_path(
        &mut self,
        path: &[Vec<u8>],
        ops_at_path_by_key: BTreeMap<Vec<u8>, Op>,
        batch_apply_options: &BatchApplyOptions,
    ) -> CostResult<[u8; 32], Error> {
        let mut cost = OperationCost::default();

        dbg!("executing ops on path", path);
        let merk_wrapped = self
            .merks
            .remove(path)
            .map(|x| Ok(x).wrap_with_cost(Default::default()))
            .unwrap_or_else(|| (self.get_merk_fn)(path));
        let mut merk = cost_return_on_error!(&mut cost, merk_wrapped);
        dbg!("got merk");

        for (key, op) in ops_at_path_by_key.into_iter() {
            dbg!("applying operations", &op);
            match op {
                Op::Insert { element } => {
                    if batch_apply_options.validate_tree_insertion_does_not_override {
                        let inserted = cost_return_on_error!(
                            &mut cost,
                            element.insert_if_not_exists(&mut merk, key.as_slice())
                        );
                        if !inserted {
                            return Err(Error::InvalidBatchOperation(
                                "attempting to overwrite a tree",
                            ))
                            .wrap_with_cost(cost);
                        }
                    } else {
                        cost_return_on_error!(&mut cost, element.insert(&mut merk, key));
                    }
                }
                Op::Delete => {
                    cost_return_on_error!(&mut cost, Element::delete(&mut merk, key));
                }
                Op::ReplaceTreeHash { hash } => {
                    cost_return_on_error!(
                        &mut cost,
                        GroveDb::update_tree_item_preserve_flag(&mut merk, key.as_slice(), hash,)
                    );
                }
            }
        }
        merk.root_hash().add_cost(cost).map(Ok)
    }
}

impl TreeCache for TreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        self.paths.insert(inserted_path);
        let worst_case_cost = OperationCost::default();

        Ok(()).wrap_with_cost(worst_case_cost)
    }

    fn execute_ops_on_path(
        &mut self,
        path: &[Vec<u8>],
        ops_at_path_by_key: BTreeMap<Vec<u8>, Op>,
        _batch_apply_options: &BatchApplyOptions,
    ) -> CostResult<[u8; 32], Error> {
        let mut cost = OperationCost::default();

        if !self.paths.remove(path) {
            // Then we have to get the tree
            let path_slices = path.iter().map(|k| k.as_slice()).collect::<Vec<&[u8]>>();
            cost.add_worst_case_get_merk(path_slices);
        }
        for (key, op) in ops_at_path_by_key.into_iter() {
            cost += op.worst_case_cost(key);
        }
        Ok([0u8; 32]).wrap_with_cost(cost)
    }
}

///                          LEVEL           PATH                   KEY      OP
type OpsByLevelPath = IntMap<usize, BTreeMap<Vec<Vec<u8>>, BTreeMap<Vec<u8>, Op>>>;

struct BatchStructure<C> {
    /// Operations by level path
    ops_by_level_path: OpsByLevelPath,
    /// Merk trees
    merk_tree_cache: C,
    /// Last level
    last_level: usize,
}

impl<S: fmt::Debug> fmt::Debug for BatchStructure<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmt_int_map = IntMap::default();
        for (level, path_map) in self.ops_by_level_path.iter() {
            let mut fmt_path_map = BTreeMap::default();

            for (path, key_map) in path_map.iter() {
                let mut fmt_key_map = BTreeMap::default();

                for (key, op) in key_map.iter() {
                    fmt_key_map.insert(DebugBytes(key.clone()), op);
                }
                fmt_path_map.insert(DebugByteVectors(path.clone()), fmt_key_map);
            }
            fmt_int_map.insert(*level, fmt_path_map);
        }

        f.debug_struct("BatchStructure")
            .field("ops_by_level_path", &fmt_int_map)
            .field("merk_tree_cache", &self.merk_tree_cache)
            .field("last_level", &self.last_level)
            .finish()
    }
}

impl<C> BatchStructure<C>
where
    C: TreeCache,
{
    fn from_ops(
        ops: Vec<GroveDbOp>,
        mut merk_tree_cache: C,
    ) -> CostResult<BatchStructure<C>, Error> {
        let cost = OperationCost::default();

        let mut ops_by_level_path: OpsByLevelPath = IntMap::default();
        dbg!(&ops_by_level_path);
        let mut current_last_level: usize = 0;

        for op in ops.into_iter() {
            let op_cost = OperationCost::default();
            let op_result = match &op.op {
                Op::Insert { element } => {
                    if let Element::Tree(..) = element {
                        merk_tree_cache.insert(&op);
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
            merk_tree_cache,
            last_level: current_last_level,
        })
        .wrap_with_cost(cost)
    }
}

#[derive(Debug, Default)]
pub struct BatchApplyOptions {
    pub validate_tree_insertion_does_not_override: bool,
}

impl GroveDb {
    /// Method to propagate updated subtree root hashes up to GroveDB root
    fn apply_batch_structure<C: TreeCache>(
        &self,
        batch_structure: BatchStructure<C>,
        // temp_root_leaves: &mut BTreeMap<Vec<u8>, usize>,
        batch_apply_options: Option<BatchApplyOptions>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let BatchStructure {
            mut ops_by_level_path,
            mut merk_tree_cache,
            last_level,
        } = batch_structure;
        let mut current_level = last_level;

        let batch_apply_options = batch_apply_options.unwrap_or_default();
        // We will update up the tree
        dbg!(&ops_by_level_path);
        while let Some(ops_at_level) = ops_by_level_path.remove(&current_level) {
            dbg!(current_level);
            for (path, ops_at_path) in ops_at_level.into_iter() {
                if current_level == 0 {
                    // build up new ops maybe?? that only has the insert operation
                    // then apply to merk_tree_cache
                    // how does merk tree cache get built tho
                    dbg!(&ops_at_path);
                    let mut root_tree_ops: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
                    for (key, op) in ops_at_path.into_iter() {
                        match op {
                            Op::Insert { .. } => {
                                // inserts a root element (trees are not enforced)
                                // how do we insert non root elements
                                root_tree_ops.insert(key, op);
                                // if temp_root_leaves.get(key.as_slice()).is_none() {
                                //     temp_root_leaves.insert(key, temp_root_leaves.len());
                                // }
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
                    dbg!("about to apply current level equals zero");
                    dbg!(&root_tree_ops);
                    // execute the ops at this path
                    merk_tree_cache.execute_ops_on_path(&path,root_tree_ops, &batch_apply_options);
                } else {
                    let root_hash = cost_return_on_error!(
                        &mut cost,
                        merk_tree_cache.execute_ops_on_path(
                            &path,
                            ops_at_path,
                            &batch_apply_options,
                        )
                    );

                    if current_level > 0 {
                        dbg!("current level inner", current_level);
                        // We need to propagate up this root hash, this means adding grove_db
                        // operations up for the level above
                        if let Some((key, parent_path)) = path.split_last() {
                            dbg!("split last");
                            dbg!(&root_hash);
                            if let Some(ops_at_level_above) =
                                ops_by_level_path.get_mut(&(current_level - 1))
                            {
                                dbg!("after split last");
                                if let Some(ops_on_path) = ops_at_level_above.get_mut(parent_path) {
                                    dbg!("after after split last");
                                    dbg!(&ops_on_path);
                                    dbg!(&key);
                                    if let Some(op) = ops_on_path.remove(key) {
                                        dbg!("didn't get here");
                                        let new_op = match op {
                                            Op::ReplaceTreeHash { .. } => {
                                                Op::ReplaceTreeHash { hash: root_hash }
                                            }
                                            Op::Insert { element } => {
                                                if let Element::Tree(_, storage_flags) = element {
                                                    Op::Insert {
                                                        element: Element::new_tree_with_flags(
                                                            root_hash,
                                                            storage_flags,
                                                        ),
                                                    }
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
                                                } else {
                                                    op
                                                }
                                            }
                                        };
                                        ops_on_path.insert(key.clone(), new_op);
                                    } else {
                                        ops_on_path.insert(
                                            key.clone(),
                                            Op::ReplaceTreeHash { hash: root_hash },
                                        );
                                        dbg!(&ops_on_path);
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
            if current_level > 0 {
                current_level -= 1;
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    // applies the batch body, updates temp_root_leaves
    fn apply_body<'db, S: StorageContext<'db>>(
        &self,
        ops: Vec<GroveDbOp>,
        // temp_root_leaves: &mut BTreeMap<Vec<u8>, usize>,
        batch_apply_options: Option<BatchApplyOptions>,
        get_merk_fn: impl Fn(&[Vec<u8>]) -> CostResult<Merk<S>, Error>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::from_ops(
                ops,
                TreeCacheMerkByPath {
                    merks: Default::default(),
                    get_merk_fn,
                }
            )
        );
        self.apply_batch_structure(batch_structure,  batch_apply_options)
            .add_cost(cost)
    }

    /// Applies batch of operations on GroveDB
    pub fn apply_batch(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        // Helper function to store updated root leaves
        // fn save_root_leaves<'db, S>(
        //     storage: S,
        //     temp_root_leaves: &BTreeMap<Vec<u8>, usize>,
        // ) -> CostResult<(), Error>
        // where
        //     S: StorageContext<'db>,
        //     Error: From<<S as storage::StorageContext<'db>>::Error>,
        // {
        //     let cost = OperationCost::default();
        //
        //     let root_leaves_serialized = cost_return_on_error_no_add!(
        //         &cost,
        //         bincode::serialize(&temp_root_leaves).map_err(|_| {
        //             Error::CorruptedData(String::from("unable to serialize root leaves data"))
        //         })
        //     );
        //     storage
        //         .put_meta(ROOT_LEAFS_SERIALIZED_KEY, &root_leaves_serialized)
        //         .map_err(|e| e.into())
        //         .wrap_with_cost(OperationCost {
        //             storage_written_bytes: ROOT_LEAFS_SERIALIZED_KEY.len() as u32
        //                 + root_leaves_serialized.len() as u32,
        //             ..Default::default()
        //         })
        // }

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        // let mut temp_root_leaves =
        //     cost_return_on_error!(&mut cost, self.get_root_leaf_keys(transaction));

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
                self.apply_body(ops, batch_apply_options, |path| {
                    let storage = self.db.get_batch_transactional_storage_context(
                        path.iter().map(|x| x.as_slice()),
                        &storage_batch,
                        tx,
                    );
                    Merk::open(storage)
                        .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
                })
            );

            // let meta_storage = self.db.get_batch_transactional_storage_context(
            //     std::iter::empty(),
            //     &storage_batch,
            //     tx,
            // );

            // saves the root leaves
            // cost_return_on_error!(&mut cost, save_root_leaves(meta_storage, &temp_root_leaves));

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
                self.apply_body(ops, batch_apply_options, |path| {
                    let storage = self.db.get_batch_storage_context(
                        path.iter().map(|x| x.as_slice()),
                        &storage_batch,
                    );
                    Merk::open(storage)
                        .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
                })
            );

            // let meta_storage = self
            //     .db
            //     .get_batch_storage_context(std::iter::empty(), &storage_batch);

            // saves root leaves here also
            // cost_return_on_error!(&mut cost, save_root_leaves(meta_storage, &temp_root_leaves));

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

    pub fn worst_case_operations_for_batch(
        &self,
        ops: Vec<GroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        cost.add_worst_case_save_root_leaves();

        let mut temp_root_leaves: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::from_ops(ops, TreeCacheKnownPaths::default())
        );
        cost_return_on_error!(
            &mut cost,
            self.apply_batch_structure(
                batch_structure,
                // &mut temp_root_leaves,
                batch_apply_options,
            )
        );

        cost.add_worst_case_open_root_meta_storage();
        cost.add_worst_case_save_root_leaves();

        // nothing for the commit multi batch?
        Ok(()).wrap_with_cost(cost)
    }
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
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
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
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
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
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
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
        db.insert([b"key1".as_ref()], b"key2", Element::empty_tree(), None)
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
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());
    }

    #[test]
    fn test_batch_validation_insertion_under_deleted_tree() {
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
        db.apply_batch(ops, None, None)
            .unwrap()
            .expect_err("insertion of element under a deleted tree should not be allowed");
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
            .unwrap()
            .expect_err("nothing should have been inserted");
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
        assert!(db.apply_batch(ops, None, None).unwrap().is_err());

        // Insertion into a tree is correct
        let ops = vec![GroveDbOp::insert(
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
        let db = make_grovedb();
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
            GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), element2.clone()),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec(), b"key_subtree".to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_tree_insertion_does_not_override: true
                }),
                None
            )
            .unwrap()
            .is_err());

        // TEST_LEAF will be deleted so you can not insert underneath it
        let ops = vec![
            GroveDbOp::delete(vec![], TEST_LEAF.to_vec()),
            GroveDbOp::insert(
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
            GroveDbOp::delete(vec![], TEST_LEAF.to_vec()),
            GroveDbOp::insert(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_tree_insertion_does_not_override: true
                }),
                None
            )
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
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_tree_insertion_does_not_override: true
                }),
                None
            )
            .unwrap()
            .is_err());
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
            // .ok()
            // .flatten()
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
            db.root_hash(None)
                .unwrap()
                // .ok()
                // .flatten()
                .expect("cannot get root hash"),
            hash
        );

        // verify root leaves
        assert!(db.get([], TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get([],ANOTHER_TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get([],b"key1", None).unwrap().is_ok());
        assert!(db.get([],b"key2", None).unwrap().is_ok());
        assert!(db.get([],b"key3", None).unwrap().is_err());
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
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![GroveDbOp::insert(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None)
            .unwrap()
            .expect("cannot apply batch");

        let batch = vec![GroveDbOp::insert(
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
        let db = make_grovedb();
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
        let batch = vec![GroveDbOp::insert(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None)
            .unwrap()
            .expect("cannot apply batch");

        assert_ne!(db.root_hash(None).unwrap().unwrap(), root_hash);
    }
}
