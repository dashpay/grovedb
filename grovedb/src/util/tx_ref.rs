//! Transaction wrapper module.
//!
//! This module ensures that all operations within GroveDB are executed
//! atomically through transactions. Due to multiple fetches and steps involved,
//! isolation is crucial, and this module addresses potential issues by using
//! transaction wrappers. Previously, if no external transaction scope was
//! provided, no transaction would occur, leading to possible data
//! inconsistencies. To mitigate this, the [TxRef] mechanism ensures that a
//! transaction is always present. The method [TxRef::commit_local] commits the
//! transaction only if it is local; otherwise, control of committing the
//! transaction remains with the caller who initiated it. This setup allows for
//! both external and internal transaction management within GroveDB operations.

use grovedb_storage::Storage;

use crate::{Error, RocksDbStorage, Transaction, TransactionArg};

/// Transaction wrapper to ensure there is always a transaction, either passed
/// from outside or started within otherwise.
pub(crate) enum TxRef<'a, 'db: 'a> {
    Owned(Transaction<'db>),
    Borrowed(&'a Transaction<'db>),
}

impl<'a, 'db> TxRef<'a, 'db> {
    pub(crate) fn new(db: &'db RocksDbStorage, transaction_arg: TransactionArg<'db, 'a>) -> Self {
        if let Some(tx) = transaction_arg {
            Self::Borrowed(tx)
        } else {
            Self::Owned(db.start_transaction())
        }
    }

    /// Commit the transaction if it wasn't received from outside
    pub(crate) fn commit_local(self) -> Result<(), Error> {
        match self {
            TxRef::Owned(tx) => tx
                .commit()
                .map_err(|e| grovedb_storage::Error::from(e).into()),
            TxRef::Borrowed(_) => Ok(()),
        }
    }
}

impl<'db> AsRef<Transaction<'db>> for TxRef<'_, 'db> {
    fn as_ref(&self) -> &Transaction<'db> {
        match self {
            TxRef::Owned(tx) => tx,
            TxRef::Borrowed(tx) => tx,
        }
    }
}
