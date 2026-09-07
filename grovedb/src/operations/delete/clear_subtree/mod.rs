//! `clear_subtree` — versioned dispatch.
//!
//! Deletes all elements in a specified subtree, leaving the subtree itself
//! (and the element pointing at it) in place.
//!
//! For ordinary Merk-backed subtrees both versions behave identically: the
//! Merk is emptied and its new (null) root hash is propagated to the root.
//! The versions differ only for **non-Merk data trees** (`MmrTree`,
//! `BulkAppendTree`, `DenseAppendOnlyFixedSizeTree`, `CommitmentTree`,
//! `PrivateDocumentStore`), whose payload lives in the data namespace and
//! whose count and state commitment live in the *parent* element. What the
//! parent commits to after a clear changes the grovedb root hash, so the
//! difference is **consensus-critical** and version-gated (issue #893):
//!
//! * **[v0]** — legacy behaviour, frozen for `GROVE_V1`..`GROVE_V3` (live in
//!   production). The payload is cleared from the data namespace and the
//!   operation reports success, but the parent element is left untouched: it
//!   still carries the old count / mmr size and the old state commitment,
//!   and nothing is propagated. The database is left contradicting itself —
//!   the retained commitment describes data that no longer exists — and
//!   `verify_grovedb` flags the subtree. Kept bug-for-bug for replay
//!   compatibility.
//! * **[v1]** — `GROVE_V4`+. The payload clear, the parent-element reset and
//!   the upward propagation are staged into one storage batch and committed
//!   atomically. The parent element is rewritten to its canonical empty
//!   state — count / mmr size zeroed, configuration (chunk power, height,
//!   entry size), flags and any `NonCounted`-style wrapper retained — and
//!   the child hash is reset to the same convention the insert path binds
//!   for an empty tree (`NULL_HASH`, or the type's empty-state root for
//!   `CommitmentTree` / `PrivateDocumentStore`). The rewrite propagates
//!   through ancestors with the indexed-aware walk, refreshing the entry's
//!   canonical secondary row when the parent is an indexed primary.
//!
//! The implementations are otherwise identical. See [v0] / [v1].
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::ClearOptions;
use crate::{Error, GroveDb, TransactionArg};

impl GroveDb {
    /// Delete all elements in a specified subtree.
    /// Returns if we successfully cleared the subtree.
    ///
    /// # Dangling references
    ///
    /// This operation does **not** check for incoming references (it has no
    /// backward-references propagation option). Any
    /// [`Reference`](crate::Element::Reference) or
    /// [`BidirectionalReference`](crate::Element::BidirectionalReference)
    /// elements elsewhere in the database that point to elements within the
    /// cleared subtree will become dangling. See the
    /// [module-level documentation](super) for details.
    pub fn clear_subtree<'b, B, P>(
        &self,
        path: P,
        options: Option<ClearOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> Result<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.clear_subtree_with_costs(path, options, transaction, grove_version)
            .unwrap()
    }

    /// Delete all elements in a specified subtree and get back costs
    /// Warning: The costs for this operation are not yet correct, hence we
    /// should keep this private for now
    /// Returns if we successfully cleared the subtree
    ///
    /// Version dispatch (consensus-critical) — see the module documentation.
    fn clear_subtree_with_costs<'b, B, P>(
        &self,
        path: P,
        options: Option<ClearOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        match grove_version
            .grovedb_versions
            .operations
            .delete
            .clear_subtree
        {
            0 => self.clear_subtree_with_costs_v0(path.into(), options, transaction, grove_version),
            1 => self.clear_subtree_with_costs_v1(path.into(), options, transaction, grove_version),
            version => Err(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "clear_subtree".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                }
                .into(),
            )
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
