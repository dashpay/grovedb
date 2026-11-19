//! What lets [`super::apply()`] build against the engine the golden corpus
//! is pinned to.
//!
//! The corpus is recorded from upstream's engine at a pinned commit (see
//! `corpus/README.md`), with this crate taken from the branch that adds to
//! the corpus. Upstream's `GroveDb` has no `set_savepoint` and no
//! `rollback_to_savepoint`: there a savepoint is set on the transaction.
//! The branch's has both, and on an engine whose transaction is a type of
//! its own they are the only way to a savepoint.
//!
//! [`Savepoints`] gives `GroveDb` the two methods in upstream's form. An
//! inherent method is preferred to a trait's, so where `GroveDb` has them
//! the calls of [`super::apply()`] are GroveDB's own, and what is here is
//! not called.

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
