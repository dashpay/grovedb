//! GroveDB batch operations support

use std::collections::{BTreeMap, HashMap};

use intrusive_collections::{intrusive_adapter, KeyAdapter, RBTree, RBTreeLink};
use merk::Merk;
use storage::{Storage, StorageBatch, StorageContext};

use crate::{Element, Error, GroveDb, TransactionArg, ROOT_LEAFS_SERIALIZED_KEY};

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
    /// Batch application generic over storage context (whether there is a
    /// transaction or not).
    fn apply_body<'db, S: StorageContext<'db>>(
        &self,
        sorted_operations: &mut RBTree<GroveDbOpAdapter>,
        temp_root_leaves: &mut BTreeMap<Vec<u8>, usize>,
        get_merk_fn: impl Fn(&[Vec<u8>]) -> Result<Merk<S>, Error>,
    ) -> Result<(), Error> {
        let mut temp_subtrees: HashMap<Vec<Vec<u8>>, Merk<_>> = HashMap::new();
        let mut cursor = sorted_operations.back_mut();
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
            // TODO: investigate how not to create a new cursor each time
            cursor = sorted_operations.back_mut();
            if let Some(op) = cursor.remove() {
                if op.path.is_empty() {
                    // Altering root leaves
                    if temp_root_leaves.get(&op.key).is_none() {
                        temp_root_leaves.insert(op.key, temp_root_leaves.len());
                    }
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

    /// Applies batch of operations on GroveDB
    pub fn apply_batch(
        &self,
        ops: Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> Result<(), Error> {
        // Helper function to store updated root leaves
        fn save_root_leaves<'db, 'ctx, S>(
            storage: S,
            temp_root_leaves: &BTreeMap<Vec<u8>, usize>,
        ) -> Result<(), Error>
        where
            S: StorageContext<'db>,
            Error: From<<S as storage::StorageContext<'db>>::Error>,
        {
            let root_leaves_serialized = bincode::serialize(&temp_root_leaves).map_err(|_| {
                Error::CorruptedData(String::from("unable to serialize root leaves data"))
            })?;
            Ok(storage.put_meta(ROOT_LEAFS_SERIALIZED_KEY, &root_leaves_serialized)?)
        }

        if ops.is_empty() {
            return Ok(());
        }

        // `StorageBatch` allows us to collect operations on different subtrees before
        // execution
        let storage_batch = StorageBatch::new();
        let mut sorted_operations = RBTree::new(GroveDbOpAdapter::new());
        let mut temp_root_leaves = self.get_root_leaf_keys(transaction)?;

        // 1. Collect all batch operations into RBTree to keep them sorted
        for o in ops {
            sorted_operations.insert(Box::new(o));
        }

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
            self.apply_body(&mut sorted_operations, &mut temp_root_leaves, |path| {
                let storage = self.db.get_batch_transactional_storage_context(
                    path.iter().map(|x| x.as_slice()),
                    &storage_batch,
                    tx,
                );
                Merk::open(storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
            })?;

            let meta_storage = self.db.get_batch_transactional_storage_context(
                std::iter::empty(),
                &storage_batch,
                tx,
            );
            save_root_leaves(meta_storage, &temp_root_leaves)?;
            self.db
                .commit_multi_context_batch_with_transaction(storage_batch, tx)?;
        } else {
            self.apply_body(&mut sorted_operations, &mut temp_root_leaves, |path| {
                let storage = self
                    .db
                    .get_batch_storage_context(path.iter().map(|x| x.as_slice()), &storage_batch);
                Merk::open(storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))
            })?;

            let meta_storage = self
                .db
                .get_batch_storage_context(std::iter::empty(), &storage_batch);
            save_root_leaves(meta_storage, &temp_root_leaves)?;
            self.db.commit_multi_context_batch(storage_batch)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{make_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF};

    #[test]
    fn test_multi_tree_insertion_with_propagation_no_tx() {
        let db = make_grovedb();
        db.insert([], b"key1", Element::empty_tree(), None)
            .expect("cannot insert root leaf");

        let hash = db
            .root_hash(None)
            .ok()
            .flatten()
            .expect("cannot get root hash");
        let element = Element::Item(b"ayy".to_vec());
        let element2 = Element::Item(b"ayy2".to_vec());

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
        ];
        db.apply_batch(ops, None).expect("cannot apply batch");
        assert_eq!(
            db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
                .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF], b"key", None)
                .expect("cannot get element"),
            element2
        );
        assert_ne!(
            db.root_hash(None)
                .ok()
                .flatten()
                .expect("cannot get root hash"),
            hash
        );
        let mut root_leafs = BTreeMap::new();
        root_leafs.insert(TEST_LEAF.to_vec(), 0);
        root_leafs.insert(ANOTHER_TEST_LEAF.to_vec(), 1);
        root_leafs.insert(b"key1".to_vec(), 2);

        assert_eq!(
            db.get_root_leaf_keys(None).expect("cannot get root leafs"),
            root_leafs
        );
    }
}
