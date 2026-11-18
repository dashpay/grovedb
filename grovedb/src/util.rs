pub(crate) mod compat;
pub(crate) mod visitor;

use crate::{Engine, Error, Transaction, TransactionArg};

/// The transaction of one operation: the caller's, or one the operation
/// started for itself and commits when it is done.
pub(crate) enum TxRef<'a, 'db: 'a, E: Engine> {
    Owned(&'db E, Transaction<'db, E>),
    Borrowed(&'a Transaction<'db, E>),
}

impl<'a, 'db, E: Engine> TxRef<'a, 'db, E> {
    pub(crate) fn new(db: &'db E, transaction_arg: TransactionArg<'db, 'a, E>) -> Self {
        if let Some(tx) = transaction_arg {
            Self::Borrowed(tx)
        } else {
            Self::Owned(db, db.start_transaction())
        }
    }

    /// The transaction of an operation that only reads: the caller's, or one
    /// the engine may pin for the length of the operation.
    pub(crate) fn for_read(db: &'db E, transaction_arg: TransactionArg<'db, 'a, E>) -> Self {
        if let Some(tx) = transaction_arg {
            Self::Borrowed(tx)
        } else {
            Self::Owned(db, db.start_read_transaction())
        }
    }

    /// Whether this transaction was started locally (and so will really
    /// commit in `commit_local`) rather than borrowed from the caller.
    pub(crate) fn is_owned(&self) -> bool {
        matches!(self, TxRef::Owned(..))
    }

    /// Commit the transaction if it wasn't received from outside
    pub(crate) fn commit_local(self) -> Result<(), Error> {
        match self {
            TxRef::Owned(db, tx) => db.commit_transaction(tx).value.map_err(Into::into),
            TxRef::Borrowed(_) => Ok(()),
        }
    }
}

impl<'db, E: Engine> AsRef<Transaction<'db, E>> for TxRef<'_, 'db, E> {
    fn as_ref(&self) -> &Transaction<'db, E> {
        match self {
            TxRef::Owned(_, tx) => tx,
            TxRef::Borrowed(tx) => tx,
        }
    }
}

/// Maximum key length in bytes. Merk link encoding stores the key length as a
/// single `u8`, so keys longer than 255 bytes would corrupt the encoding.
/// Every path segment is a key, so it bounds path segments too.
pub(crate) const MAX_KEY_LENGTH: usize = u8::MAX as usize;

/// Refuse an entry point whose capability slot is not active: slot `0`
/// (every version before the one that ships it) means unavailable, slot `1`
/// is the v1 implementation. The comparison is an EXACT match: accepting
/// `slot > 1` would silently run v1 code under a future protocol version that
/// gave the slot new semantics.
pub(crate) fn check_v1_slot_enabled(
    method: &str,
    slot: grovedb_version::version::FeatureVersion,
) -> Result<(), Error> {
    if slot != 1 {
        return Err(
            grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                method: method.to_string(),
                known_versions: vec![1],
                received: slot,
            }
            .into(),
        );
    }
    Ok(())
}

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
