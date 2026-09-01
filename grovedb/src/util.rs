pub(crate) mod compat;

use grovedb_storage::Storage;

use crate::{Error, RocksDbStorage, Transaction, TransactionArg};

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

    /// Whether this transaction was started locally (and so will really
    /// commit in `commit_local`) rather than borrowed from the caller.
    pub(crate) fn is_owned(&self) -> bool {
        matches!(self, TxRef::Owned(_))
    }

    /// Commit the transaction if it wasn't received from outside
    pub(crate) fn commit_local(self) -> Result<(), Error> {
        match self {
            TxRef::Owned(tx) => tx.commit().map_err(Into::into),
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

/// Build the storage path of a subtree living at `path`/`key`.
///
/// Every non-Merk tree type (commitment tree, bulk-append tree, private
/// document store) needs its own subtree path to open the data namespace,
/// and each had grown a private copy of this three-line helper under a
/// different name. One shared function keeps them provably identical.
pub(crate) fn subtree_path_with_key<B: AsRef<[u8]>>(
    path: &grovedb_path::SubtreePath<B>,
    key: &[u8],
) -> Vec<Vec<u8>> {
    let mut v = path.to_vec();
    v.push(key.to_vec());
    v
}
