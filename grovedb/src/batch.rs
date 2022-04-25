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
        }

        dbg!(tree);
        dbg!(storage_batch);
        todo!()
    }

    // pub fn apply_batch(
    //     &self,
    //     batch: &[GroveDbOp],
    //     transaction: TransactionArg,
    // ) -> Result<(), Error> {
    //     // TODO: validate chains
    //     let mut storage_batch = StorageBatch::new();
    //     if let Some(tx) = transaction {
    //         // Temp subtrees are required to accumulate changes, to compute root
    // hashes and         // to do propagation --- all of these with no `get`
    // queries to the database         let mut temp_subtrees = HashMap::new();
    //         for op in batch {
    //             match op {
    //                 GroveDbOp::Insert { path, key, element } => {
    //                     let storage =
    // self.db.get_batch_transactional_storage_context(
    // path.iter().map(|x| *x),                         &storage_batch,
    //                         tx,
    //                     );
    //                     let merk = Merk::open(storage).map_err(|_| {
    //                         crate::Error::CorruptedData("cannot open a
    // subtree".to_owned())                     })?;
    //                     temp_subtrees.insert(path, merk);
    //                 }
    //                 _ => todo!(),
    //             }
    //         }
    //     } else {
    //         // Temp subtrees are required to accumulate changes, to compute root
    //         // hashes and to do propagation --- all of these with no
    //         // `get` queries to the database let mut temp_subtrees =
    //         // HashMap::new();
    //     }

    //     todo!()
    // }
    //
    // pub fn apply_batch(
    //     &self,
    //     mut ops: Vec<GroveDbOp>,
    //     transaction: TransactionArg,
    // ) -> Result<(), Error> {
    //     if ops.is_empty() {
    //         return Ok(());
    //     }

    //     // TODO: validate chains
    //     let mut storage_batch = StorageBatch::new();

    //     // Opearations need to be sorted and applied using a stack structure:
    //     // 1. First we apply deepmost operations grouped by path
    //     // 2. If there are no operations for a subtree left, we put onto stack a
    //     //    propagation operation: insert to an upper tree its root hash
    //     // 3. Repeat until the stack is empty

    //     // We'll use vector as a stack, so operations will be run from its tail,
    // thus     // deletions go before insertions and operations on children
    // will go after ones     // on parent because of reverse order
    //     ops.sort_by(|a, b| match (a, b) {
    //         // Delete is always before insert (remember reverse order of
    // execution)         (GroveDbOp::Delete { .. }, GroveDbOp::Insert { .. })
    // => Ordering::Less,         (GroveDbOp::Insert { .. }, GroveDbOp::Delete {
    // .. }) => Ordering::Greater,         // For operations of the same kind we
    // put shorter paths first         (GroveDbOp::Delete { path: left, .. },
    // GroveDbOp::Delete { path: right, .. }) => {             left.cmp(right)
    //         }
    //         (GroveDbOp::Insert { path: left, .. }, GroveDbOp::Insert { path:
    // right, .. }) => {             left.cmp(right)
    //         }
    //     });

    //     if let Some(tx) = transaction {
    //         let mut prev_path: Option<&[&[u8]]> = None;
    //         let mut merk: Option<Merk<_>> = None;

    //         while let Some(op) = ops.last() {
    //             let current_path = match op {
    //                 GroveDbOp::Insert { path, .. } => path,
    //                 GroveDbOp::Delete { path, .. } => path,
    //             };
    //             // 1. Initially or because we're moving to next subtree
    //         }

    //         // while let Some(op) = ops.pop() {
    //         //     let current_path = match op {
    //         //         GroveDbOp::Insert { path, .. } => path,
    //         //         GroveDbOp::Delete { path, .. } => path,
    //         //     };

    //         //     if Some(current_path) != prev_path {
    //         //         if let Some(pp) = prev_path {
    //         //             // Finished with a deeper subtree, need to propagate
    // its root hash to parent         //             ops.push(GroveDbOp::Insert
    // {         //                 path: current_path,
    //         //                 key: todo!(),
    //         //                 element: Element::Tree(merk.expect("Merk must
    // exist").root_hash()),         //             });
    //         //         }
    //         //         let storage =
    // self.db.get_batch_transactional_storage_context(         //
    // current_path.iter().map(|x| *x),         //             &storage_batch,
    //         //             tx,
    //         //         );
    //         //         prev_path = Some(current_path);
    //         //         merk =
    //         //             Some(Merk::open(storage).map_err(|_| {
    //         //                 Error::CorruptedData("cannot open a
    // subtree".to_owned())         //             })?);
    //         //     }
    //         // }
    //     } else {
    //     }

    //     // if let Some(tx) = transaction {
    //     //     // Temp subtrees are required to accumulate changes, to compute
    // root hashes and     //     // to do propagation --- all of these with no
    // `get` queries to the database     //     let mut temp_subtrees =
    // HashMap::new();     //     for op in batch {
    //     //         match op {
    //     //             GroveDbOp::Insert { path, key, element } => {
    //     //                 let storage =
    // self.db.get_batch_transactional_storage_context(     //
    // path.iter().map(|x| *x),     //                     &storage_batch,
    //     //                     tx,
    //     //                 );
    //     //                 let merk = Merk::open(storage).map_err(|_| {
    //     //                     crate::Error::CorruptedData("cannot open a
    // subtree".to_owned())     //                 })?;
    //     //                 temp_subtrees.insert(path, merk);
    //     //             }
    //     //             _ => todo!(),
    //     //         }
    //     //     }
    //     // } else {
    //     //     // Temp subtrees are required to accumulate changes, to compute
    // root     //     // hashes and to do propagation --- all of these with no
    //     //     // `get` queries to the database let mut temp_subtrees =
    //     //     // HashMap::new();
    //     // }

    //     todo!()
    // }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_grovedb;

    #[test]
    fn test_something() {
        let ops = vec![
            GroveDbOp::insert(
                vec![b"ass".to_vec(), b"we".to_vec(), b"can".to_vec()],
                b"key".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![
                    b"ass".to_vec(),
                    b"we".to_vec(),
                    b"can".to_vec(),
                    b"ayy".to_vec(),
                ],
                b"key".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert(
                vec![b"ass".to_vec(), b"can".to_vec()],
                b"key".to_vec(),
                Element::empty_tree(),
            ),
        ];
        let db = make_grovedb();
        let tx = db.start_transaction();
        db.apply_batch(ops, Some(&tx));
    }
}
