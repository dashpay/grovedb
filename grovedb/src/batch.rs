//! GroveDB batch operations support

use std::{cmp::Ordering, collections::HashMap};

use intrusive_collections::{intrusive_adapter, KeyAdapter, RBTree, RBTreeLink};
use merk::Merk;
use storage::{Storage, StorageBatch, StorageContext};

use crate::{
    util::storage_context_optional_tx, Element, Error, GroveDb, TransactionArg,
    ROOT_LEAFS_SERIALIZED_KEY,
};

#[derive(Debug)]
enum Op {
    Insert { element: Element },
    Delete,
}

/// Batch operation
#[derive(Debug)]
pub struct GroveDbOp {
    /// Path to a subtree - subject to an operation
    path: Vec<Vec<u8>>,
    /// Key of an element in the subtree
    key: Vec<u8>,
    /// Operation to perform on the key
    op: Op,
    /// Link used in intrusive tree to maintain operations order
    link: RBTreeLink,
}

// TODO: keep allocation number small
intrusive_adapter!(GroveDbOpAdapter = Box<GroveDbOp> : GroveDbOp { link: RBTreeLink });

impl<'a> KeyAdapter<'a> for GroveDbOpAdapter {
    type Key = &'a [Vec<u8>];

    fn get_key(&self, value: &'a GroveDbOp) -> Self::Key {
        &value.path
    }
}

impl GroveDbOp {
    pub fn insert(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        Self {
            path,
            key,
            op: Op::Insert { element },
            link: RBTreeLink::new(),
        }
    }

    pub fn delete(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        Self {
            path,
            key,
            op: Op::Delete,
            link: RBTreeLink::new(),
        }
    }
}

impl GroveDb {
    fn apply_body<'db, 'ctx, S: StorageContext<'db, 'ctx> + 'ctx>(
        &self,
        tree: &mut RBTree<GroveDbOpAdapter>,
        get_merk_fn: impl Fn(&[Vec<u8>]) -> Result<Merk<S>, Error>,
    ) -> Result<(), Error> {
        let mut temp_subtrees: HashMap<Vec<Vec<u8>>, Merk<_>> = HashMap::new();
        let mut cursor = tree.back_mut();
        let mut prev_path = cursor.get().expect("batch is not empty").path.clone();

        loop {
            // Run propagation if next operation is on different path or no more operations
            // left
            if cursor.get().map(|op| op.path != prev_path).unwrap_or(true) {
                if let Some((key, path_slice)) = prev_path.split_last() {
                    let hash = temp_subtrees
                        .remove(&prev_path)
                        .expect("subtree was inserted before")
                        .root_hash();

                    cursor.insert(Box::new(GroveDbOp::insert(
                        path_slice.to_vec(),
                        key.to_vec(),
                        Element::Tree(hash),
                    )));
                }
            }

            // Execute next available operation
            cursor = tree.back_mut();
            if let Some(op) = cursor.remove() {
                if op.path.is_empty() {
                    // Altering root leafs
                    // if temp_root_leafs.get(&op.key).is_none() {
                    //     temp_root_leafs.insert(op.key,
                    // temp_root_leafs.len()); }
                } else {
                    // Keep opened Merk instances to accumulate changes before taking final root
                    // hash
                    if !temp_subtrees.contains_key(&op.path) {
                        let merk = get_merk_fn(&op.path)?;
                        temp_subtrees.insert(op.path.clone(), merk);
                    }
                    let mut merk = temp_subtrees
                        .get_mut(&op.path)
                        .expect("subtree was inserted before");
                    match op.op {
                        Op::Insert { element } => {
                            element.insert(&mut merk, op.key)?;
                        }
                        Op::Delete => {
                            Element::delete(&mut merk, op.key)?;
                        }
                    }
                }
                prev_path = op.path;
            } else {
                break;
            }
        }
        Ok(())
    }

    pub fn apply_batch(
        &self,
        ops: Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> Result<(), Error> {
        if ops.is_empty() {
            return Ok(());
        }
        let storage_batch = StorageBatch::new();
        let mut tree = RBTree::new(GroveDbOpAdapter::new());
        let mut temp_root_leafs = self.get_root_leaf_keys(transaction)?;

        // 1. Collect all batch operations into RBTree to keep them sorted
        for o in ops {
            tree.insert(Box::new(o));
        }
        if let Some(tx) = transaction {
            let mut temp_subtrees: HashMap<Vec<Vec<u8>>, Merk<_>> = HashMap::new();
            let mut cursor = tree.back_mut();
            let mut prev_path = cursor.get().expect("batch is not empty").path.clone();

            loop {
                // Run propagation if next operation is on different path or no more operations
                // left
                if cursor.get().map(|op| op.path != prev_path).unwrap_or(true) {
                    if let Some((key, path_slice)) = prev_path.split_last() {
                        let hash = temp_subtrees
                            .remove(&prev_path)
                            .expect("subtree was inserted before")
                            .root_hash();

                        cursor.insert(Box::new(GroveDbOp::insert(
                            path_slice.to_vec(),
                            key.to_vec(),
                            Element::Tree(hash),
                        )));
                    }
                }

                // Execute next available operation
                cursor = tree.back_mut();
                if let Some(op) = cursor.remove() {
                    if op.path.is_empty() {
                        // Altering root leafs
                        if temp_root_leafs.get(&op.key).is_none() {
                            temp_root_leafs.insert(op.key, temp_root_leafs.len());
                        }
                    } else {
                        // Keep opened Merk instances to accumulate changes before taking final root
                        // hash
                        if !temp_subtrees.contains_key(&op.path) {
                            let storage = self.db.get_batch_transactional_storage_context(
                                op.path.iter().map(|x| x.as_slice()),
                                &storage_batch,
                                tx,
                            );
                            let merk = Merk::open(storage).map_err(|_| {
                                Error::CorruptedData("cannot open a subtree".to_owned())
                            })?;
                            temp_subtrees.insert(op.path.clone(), merk);
                        }
                        let mut merk = temp_subtrees
                            .get_mut(&op.path)
                            .expect("subtree was inserted before");
                        match op.op {
                            Op::Insert { element } => {
                                element.insert(&mut merk, op.key)?;
                            }
                            Op::Delete => {
                                Element::delete(&mut merk, op.key)?;
                            }
                        }
                    }
                    prev_path = op.path;
                } else {
                    break;
                }
            }
            let meta_storage = self.db.get_batch_transactional_storage_context(
                std::iter::empty(),
                &storage_batch,
                tx,
            );
            let root_leafs_serialized = bincode::serialize(&temp_root_leafs).map_err(|_| {
                Error::CorruptedData(String::from("unable to serialize root leaves data"))
            })?;
            meta_storage.put_meta(ROOT_LEAFS_SERIALIZED_KEY, &root_leafs_serialized)?;
            self.db
                .commit_multi_context_batch_with_transaction(storage_batch, tx)?;
        }

        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_grovedb;

    // #[test]
    // fn test_something() {
    //     let ops = vec![
    //         GroveDbOp::insert(
    //             vec![b"ass".to_vec(), b"we".to_vec(), b"can".to_vec()],
    //             b"key".to_vec(),
    //             Element::empty_tree(),
    //         ),
    //         GroveDbOp::insert(
    //             vec![
    //                 b"ass".to_vec(),
    //                 b"we".to_vec(),
    //                 b"can".to_vec(),
    //                 b"ayy".to_vec(),
    //             ],
    //             b"key".to_vec(),
    //             Element::empty_tree(),
    //         ),
    //         GroveDbOp::insert(
    //             vec![b"ass".to_vec(), b"can".to_vec()],
    //             b"key".to_vec(),
    //             Element::empty_tree(),
    //         ),
    //     ];
    //     let db = make_grovedb();
    //     let tx = db.start_transaction();
    //     db.apply_batch(ops, Some(&tx));
    // }
}
