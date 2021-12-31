use std::{
    collections::{hash_map::Drain, HashMap},
    rc::Rc,
};

use merk::Merk;
use rs_merkle::{algorithms::Sha256, MerkleTree};
use storage::{
    rocksdb_storage::{
        OptimisticTransactionDBTransaction, PrefixedRocksDbStorage, PrefixedRocksDbStorageError,
    },
    Storage,
};

use super::{subtree, Error};
use crate::GroveDb;
// pub struct GroveDbTransaction<'a, 'db> {
//     grove_db: &'a mut GroveDb,
//     db: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
//     transaction: Option<<PrefixedRocksDbStorage as
// Storage>::DBTransaction<'db>> }
//
// impl<'a, 'db> GroveDbTransaction<'a, 'db> {
//     pub fn new(grove_db: &'a mut GroveDb, db:
// Rc<storage::rocksdb_storage::OptimisticTransactionDB>) -> Self {         let
// kek = Self {             grove_db, db, transaction: None
//         };
//         kek.start()
//     }
//
//     fn start(mut self) -> Self {
//         self.transaction = Some(self.db.transaction());
//         self
//     }
//
//     pub fn insert(
//         &mut self,
//         path: &[&[u8]],
//         key: Vec<u8>,
//         mut element: subtree::Element,
//     ) -> Result<(), Error> {
//         self.grove_db.insert(path, key, element, self.transaction.as_ref())
//     }
//
//     pub fn insert_if_not_exists(
//         &mut self,
//         path: &[&[u8]],
//         key: Vec<u8>,
//         element: subtree::Element,
//     ) -> Result<bool, Error> {
//         self.grove_db.insert_if_not_exists(path, key, element,
// self.transaction.as_ref())     }
//
//     // pub fn get(&self, path: &[&[u8]], key: &[u8]) ->
// Result<subtree::Element, Error> {     //     self.grove_db.get(path, key,
// Some(&self.transaction))     // }
//
//     // /// Commits and consumes the transaction
//     // pub fn commit(self) -> Result<(), Error> {
//     //
// self.transaction.commit().map_err(Into::<PrefixedRocksDbStorageError>::into)?
// ;     //     Ok(())
//     // }
//     //
//     // /// Rolls back the transaction
//     // pub fn rollback(&self) -> Result<(), Error> {
//     //
// self.transaction.rollback().map_err(Into::<PrefixedRocksDbStorageError>::
// into)?;     //     Ok(())
//     // }
// }

pub struct GroveDbTransaction<'a> {
    db_transaction: OptimisticTransactionDBTransaction<'a>,
    root_tree: MerkleTree<Sha256>,
    root_leaf_keys: HashMap<Vec<u8>, usize>,
    subtrees: HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
}

impl<'a> GroveDbTransaction<'a> {
    pub fn new(
        db_transaction: OptimisticTransactionDBTransaction<'a>,
        root_tree: MerkleTree<Sha256>,
        root_leaf_keys: HashMap<Vec<u8>, usize>,
        subtrees: HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
    ) -> Self {
        Self {
            db_transaction,
            root_tree,
            root_leaf_keys,
            subtrees,
        }
    }

    pub fn root_leaf_keys_mut(&mut self) -> &mut HashMap<Vec<u8>, usize> {
        &mut self.root_leaf_keys
    }

    pub fn root_leaf_keys(&self) -> &HashMap<Vec<u8>, usize> {
        &self.root_leaf_keys
    }

    pub fn db_transaction(&self) -> &OptimisticTransactionDBTransaction {
        &self.db_transaction
    }

    pub fn commit_db(self) -> Result<(), storage::rocksdb_storage::Error> {
        self.db_transaction.commit()
    }
}
