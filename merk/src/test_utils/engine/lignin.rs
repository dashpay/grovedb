//! LigninDB as an engine of the suite.
//!
//! The engine is a database of `lignindb` in the directory it was opened
//! for ([`lignindb::Db`]), a transaction is a write transaction of it, and
//! a handle is the store of `store::lignin` on that transaction, a
//! [`LigninNodeStore`].
//!
//! # Subtrees are entered when a handle that writes is opened
//!
//! LigninDB reads nothing of a subtree that its directory does not hold,
//! and refuses every write to one (`docs/lignindb/node-store.md`, "Opening
//! a Merk"). In a grove a subtree is entered under the element that hosts
//! it. A test of Merk writes a Merk at a path with no element above it,
//! so the engine enters the subtree itself: when it opens a
//! handle that writes, it reads the subtree's entry through the
//! transaction, and where there is none it enters the subtree in that
//! transaction, if it is absent, as a Merk that stands alone. A handle
//! that only reads enters nothing, and reads an empty Merk where no
//! subtree is entered.
//!
//! The enter is a write of the transaction, also where the handle is a
//! deferred one whose batch is never applied: it goes with a transaction
//! that is not committed. So a batch is applied to the transaction that
//! its handles were opened in: another transaction may lack the entry of
//! the subtree and can then refuse the batch, where a storage batch of
//! RocksDB is taken by any transaction.
//!
//! The engine does not know what tree the Merk of a handle is: a handle is
//! opened before its Merk. So a subtree is entered with the bytes
//! [`LigninTestEngine::TREE_TYPE`], which are those of no tree type. The
//! conversion in place of a Merk that has a node states the tree type it
//! leaves (`NodeStore::state_tree_type`), and the directory names that one
//! once the statement is applied: at once through a handle that writes at
//! once, and with the batch of a handle that stages.
//!
//! # Every database is checked when it is closed
//!
//! In a debug build `lignindb` checks a database with its physical
//! invariant checker when the handle is dropped, and panics on a finding
//! (`lignindb::db`, "The check at a close", which also says where the
//! check is left out). So a test of the suite that drops its engine ends
//! with that check of the database as its last commit left it. The engine
//! gives the check Merk's rules for the key-value hash and the hash of a
//! node (`CheckedHashes`), by which the check makes the stored ones of
//! the Merks in use again and compares them.

use std::path::Path;

use grovedb_costs::{CostContext, CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_storage::{prefix::build_prefix, Error};
use grovedb_version::version::GroveVersion;
use lignin_tree::{
    hash::{HashRules, Refusal},
    node::NodeRef,
};
use lignindb::{
    commit::{Committed, NoStaleHashes},
    db::WriteError,
    txn::WriteTxn,
    Db, Options,
};

use super::TestEngine;
use crate::{
    store::lignin::{
        hash::MerkHashRules, LigninBatch, LigninNodeStore, LigninSettled, LigninSubtree, LigninView,
    },
    CryptoHash,
};

/// LigninDB: a handle is the store of `store::lignin` on a write
/// transaction.
pub struct LigninTestEngine {
    db: Db,
}

/// A write transaction of a [`LigninTestEngine`].
///
/// It keeps the transaction as what the engine asks of it, with the
/// lifetime of the borrow of its database alone: a test hands a
/// transaction on by a reference that lives no longer than the test's own
/// borrow of it (`&'a SuiteTransaction<'a>`), as it can a transaction of
/// RocksDB.
pub struct LigninTestTransaction<'db> {
    transaction: Box<dyn Transaction + Send + Sync + 'db>,
}

/// What the engine asks of a write transaction of its database.
trait Transaction {
    /// The transaction as a handle reads and writes through it.
    fn view(&self) -> LigninView<'_>;

    /// Commits the transaction, which was begun from `db`.
    fn commit(&self, db: &Db) -> Result<Committed, WriteError>;
}

impl Transaction for WriteTxn<'_> {
    fn view(&self) -> LigninView<'_> {
        self.into()
    }

    fn commit(&self, db: &Db) -> Result<Committed, WriteError> {
        db.commit(self, &mut NoStaleHashes)
    }
}

/// The rules by which the check at the close of a suite's database makes
/// stored hashes again: Merk's for the key-value hash and the hash of a
/// node, which read no value. A value hash is taken as it is stored: the
/// values of Merk's tests are mostly no elements of GroveDB, whose value
/// hash Merk's rule makes, and some are put with a value hash that their
/// test states.
struct CheckedHashes(MerkHashRules<'static>);

impl HashRules for CheckedHashes {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        _value: &[u8],
        _hosted: &[Option<CryptoHash>],
    ) -> Result<CryptoHash, Refusal> {
        Ok(*node.value_hash())
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &CryptoHash) -> Result<CryptoHash, Refusal> {
        self.0.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<CryptoHash, Refusal> {
        self.0.node_hash(node)
    }
}

fn storage_error(error: crate::Error) -> Error {
    match error {
        crate::Error::StorageError(error) => error,
        other => Error::StorageError(other.to_string()),
    }
}

impl LigninTestEngine {
    /// The bytes the engine enters a subtree with, in place of a tree
    /// type.
    pub const TREE_TYPE: &'static [u8] = b"merk";

    /// The database underneath, for a test that is about LigninDB.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Enters `subtree` in the transaction of `view`, where the
    /// transaction shows no entry of it.
    fn enter(view: LigninView<'_>, subtree: &LigninSubtree) -> Result<(), crate::Error> {
        let handle = LigninNodeStore::read_only(view, subtree.clone());
        if handle.entry()?.is_some() {
            return Ok(());
        }
        let nothing = LigninBatch::new().settle(view).value?;
        let enters = [(subtree.clone(), Self::TREE_TYPE.to_vec())];
        LigninSettled::apply(vec![nothing], view, &enters)
            .value
            .map(|_| ())
    }

    /// The subtree at `path`, as a Merk that stands alone, entered in
    /// the transaction of `view` where `writes` says that its handle
    /// writes.
    fn subtree_at<'b, B: AsRef<[u8]> + 'b>(
        path: SubtreePath<'b, B>,
        view: LigninView<'_>,
        writes: bool,
    ) -> CostContext<LigninSubtree> {
        build_prefix(path).map(|prefix| {
            let subtree = LigninSubtree::alone(prefix);
            if writes {
                Self::enter(view, &subtree).expect("cannot enter the subtree of a handle");
            }
            subtree
        })
    }
}

impl TestEngine for LigninTestEngine {
    type Batch = LigninBatch;
    type ImmediateSubtree<'a> = LigninNodeStore<'a>;
    type Subtree<'a> = LigninNodeStore<'a>;
    type Transaction<'db> = LigninTestTransaction<'db>;

    fn open(path: &Path) -> Self {
        let db = Db::open(path, &Options::default()).expect("cannot open a lignindb database");
        let rules = MerkHashRules::for_check(GroveVersion::latest());
        db.check_hashes_by(Box::new(CheckedHashes(rules)));
        LigninTestEngine { db }
    }

    fn start_transaction(&self) -> Self::Transaction<'_> {
        LigninTestTransaction {
            transaction: Box::new(self.db.begin()),
        }
    }

    fn commit_transaction(&self, transaction: Self::Transaction<'_>) -> CostResult<(), Error> {
        let committed = transaction.transaction.commit(&self.db);
        committed
            .map(|_| ())
            .map_err(|error| Error::StorageError(format!("lignindb: {error}")))
            .wrap_with_cost(OperationCost::default())
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a LigninBatch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        let view = transaction.transaction.view();
        Self::subtree_at(path, view, batch.is_some())
            .map(|subtree| LigninNodeStore::deferred(view, subtree, batch))
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        let view = transaction.transaction.view();
        Self::subtree_at(path, view, true).map(|subtree| LigninNodeStore::immediate(view, subtree))
    }

    fn apply_batch(
        &self,
        batch: LigninBatch,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<(), Error> {
        batch
            .apply(transaction.transaction.view())
            .map_ok(|_| ())
            .map_err(storage_error)
    }
}
