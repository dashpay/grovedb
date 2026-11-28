//! What lets [`super::apply()`] build against the engine the golden corpus
//! is pinned to.
//!
//! The corpus is recorded from upstream's engine at a pinned commit (see
//! `corpus/README.md`), with this crate taken from the branch that adds to
//! the corpus. Two things differ there, and this file is the only place that
//! knows either. The build script says which of the two this build is: the
//! cfg `grovedb_engine_seam` is set where GroveDB has the `Engine` seam.
//!
//! # Savepoints
//!
//! Upstream's `GroveDb` has no `set_savepoint` and no
//! `rollback_to_savepoint`: there a savepoint is set on the transaction.
//! The branch's has both, and on an engine whose transaction is a type of
//! its own they are the only way to a savepoint.
//!
//! [`Savepoints`] gives `GroveDb` the two methods in upstream's form. An
//! inherent method is preferred to a trait's, so where `GroveDb` has them
//! the calls of [`super::apply()`] are GroveDB's own, and what is here is
//! not called.
//!
//! # The engine
//!
//! The branch's `GroveDb<E>` is generic over its engine, and
//! [`super::apply()`] applies an operation to a database of any engine
//! that is a [`StreamEngine`]. Upstream's `GroveDb` has no engine
//! parameter. So the functions that are generic over an engine `E` write
//! the parameter under `#[cfg(grovedb_engine_seam)]` and name the database
//! and its transactions through [`Db`], [`Tx`] and [`TxArg`]. Against
//! upstream's engine the parameter is gone and `E` is [`Upstream`], the
//! one engine there is, for which the three are upstream's own types.

use grovedb::{Error, GroveDb, Transaction};

/// Savepoints set on the transaction, under the names and with the
/// signatures `GroveDb` has for them where it sets them itself.
#[allow(dead_code, clippy::result_large_err)]
pub(super) trait Savepoints {
    /// `Transaction::set_savepoint`.
    fn set_savepoint<'db>(&'db self, transaction: &Transaction<'db>) -> Result<(), Error>;

    /// `Transaction::rollback_to_savepoint`.
    fn rollback_to_savepoint<'db>(&'db self, transaction: &Transaction<'db>) -> Result<(), Error>;
}

impl Savepoints for GroveDb {
    fn set_savepoint<'db>(&'db self, transaction: &Transaction<'db>) -> Result<(), Error> {
        transaction.set_savepoint();
        Ok(())
    }

    fn rollback_to_savepoint<'db>(&'db self, transaction: &Transaction<'db>) -> Result<(), Error> {
        transaction.rollback_to_savepoint().map_err(Error::from)
    }
}

#[cfg(grovedb_engine_seam)]
mod engine {
    use grovedb::{Engine, GroveDb, Transaction, TransactionArg};

    /// The engine of a database that names none.
    pub use grovedb::engine::DefaultEngine;

    /// An engine a workload stream is applied to: an [`Engine`] whose
    /// transactions say which kind they are.
    ///
    /// A write transaction and a snapshot read transaction are one type of
    /// an engine, and `Engine` has no way to ask which a value is: whoever
    /// started it knows. [`apply`](super::super::apply()) is handed a
    /// transaction by its caller and refuses a step given the wrong kind,
    /// so it asks here.
    ///
    /// Registering an engine with the workloads is this one function.
    pub trait StreamEngine: Engine {
        /// Whether `transaction` is a snapshot read transaction.
        fn is_snapshot_read(transaction: &Transaction<'_, Self>) -> bool;
    }

    /// A database on engine `E`.
    pub type Db<E> = GroveDb<E>;
    /// A transaction of engine `E`.
    pub type Tx<'db, E> = Transaction<'db, E>;
    /// A transaction of engine `E`, or none.
    pub type TxArg<'db, 'a, E> = TransactionArg<'db, 'a, E>;
}

#[cfg(not(grovedb_engine_seam))]
mod engine {
    use grovedb::{GroveDb, Transaction};

    /// What stands for an engine where `GroveDb` has none to name: the
    /// types of the database and of its transactions.
    pub trait StreamEngine {
        /// The database.
        type Db;
        /// A transaction of the database.
        type Transaction<'db>;

        /// Whether `transaction` is a snapshot read transaction.
        fn is_snapshot_read(transaction: &Self::Transaction<'_>) -> bool;
    }

    /// Upstream's engine: the one a `GroveDb` without the seam runs on.
    pub enum Upstream {}

    impl StreamEngine for Upstream {
        type Db = GroveDb;
        type Transaction<'db> = Transaction<'db>;

        fn is_snapshot_read(transaction: &Transaction<'_>) -> bool {
            transaction.is_snapshot_read()
        }
    }

    /// The engine of a database that names none.
    pub type DefaultEngine = Upstream;
    /// A database on engine `E`.
    pub type Db<E> = <E as StreamEngine>::Db;
    /// A transaction of engine `E`.
    pub type Tx<'db, E> = <E as StreamEngine>::Transaction<'db>;
    /// A transaction of engine `E`, or none.
    pub type TxArg<'db, 'a, E> = Option<&'a Tx<'db, E>>;
}

pub use engine::*;
