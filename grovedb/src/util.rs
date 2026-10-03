pub(crate) mod compat;
pub(crate) mod visitor;

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

/// Maximum key length in bytes. Merk link encoding stores the key length as a
/// single `u8`, so keys longer than 255 bytes would corrupt the encoding.
/// Every path segment is a key, so it bounds path segments too.
pub(crate) const MAX_KEY_LENGTH: usize = u8::MAX as usize;

/// Refuse a path with a segment longer than 255 bytes.
///
/// A subtree prefix records each segment length in one byte, so storage
/// panics if asked to build a prefix for a longer segment. Keys are capped at
/// 255 bytes on insert, so no subtree has such a path, but a caller can still
/// pass one to a public entry point. Each place where a caller's path first
/// reaches prefix construction calls this just before it, so that caller gets
/// an input error instead of a panic, and a call that never builds the prefix
/// keeps its old result (#680).
pub(crate) fn validate_path_segment_lengths<B: AsRef<[u8]>>(
    path: &grovedb_path::SubtreePath<B>,
) -> Result<(), Error> {
    if path
        .clone()
        .into_reverse_iter()
        .any(|segment| segment.len() > MAX_KEY_LENGTH)
    {
        return Err(Error::InvalidInput(
            "path segment length must be at most 255 bytes",
        ));
    }
    Ok(())
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
