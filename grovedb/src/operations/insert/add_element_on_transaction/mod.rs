//! `add_element_on_transaction` — versioned dispatch.
//!
//! Adds a single element (an empty subtree or a value) into an already-open
//! parent merk on a transaction. This is the **non-batch** insert path
//! (`GroveDb::insert` / `insert_if_not_exists`).
//!
//! The choice between writing certain tree types as a layered subtree
//! (`Op::PutLayeredReference`) versus a plain value (`Op::Put`) changes the
//! parent node's `value_hash` and therefore the grovedb root, so it is
//! **consensus-critical** and version-gated:
//!
//! * **[v0]** — grovedb v4.1.0 behaviour. `CountSumTree` / `ProvableCountTree` /
//!   `ProvableCountSumTree` are written as a plain value (`Op::Put`). This is the
//!   behaviour frozen into the live protocol-v11 activation chain (testnet block
//!   245,344, `transition_to_version_11`). Selected by `GROVE_V1` / `GROVE_V2`.
//! * **[v1]** — Those three types are written as layered subtrees, consistent
//!   with the batch insert path (both root hash and fee). Selected by
//!   `GROVE_V3`.
//! * **[v2]** — current behaviour. As v1, but a `Reference` binds the value
//!   hash of its terminal's **stored** bytes (wrapper included for a
//!   `NonCounted`-wrapped terminal), matching the batch reference resolver.
//!   v1 hashed the looked-through terminal, so a directly inserted reference
//!   to a wrapped item committed a different root than the same reference
//!   applied in a batch (issue #858). v2 also refuses a reference whose
//!   chain runs back through the position being written, which v1 accepted
//!   when that position was being overwritten: the stale stored element
//!   made the chain look acyclic and the write committed a cycle that every
//!   later read failed on. The batch path already refuses it. Selected by
//!   `GROVE_V4`+.
//!
//! The implementations are otherwise identical. See [v0] / [v1] / [v2].
//!
//! [v0]: self::v0
//! [v1]: self::v1
//! [v2]: self::v2

mod v0;
mod v1;
mod v2;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::Merk;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch};
use grovedb_version::version::GroveVersion;

use super::InsertOptions;
use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    ///
    /// Version dispatch (consensus-critical) — see the module documentation.
    pub(crate) fn add_element_on_transaction<'db, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .insert
            .add_element_on_transaction
        {
            0 => self.add_element_on_transaction_v0(
                path,
                key,
                element,
                options,
                transaction,
                batch,
                grove_version,
            ),
            1 => self.add_element_on_transaction_v1(
                path,
                key,
                element,
                options,
                transaction,
                batch,
                grove_version,
            ),
            2 => self.add_element_on_transaction_v2(
                path,
                key,
                element,
                options,
                transaction,
                batch,
                grove_version,
            ),
            version => Err(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "add_element_on_transaction".to_string(),
                    known_versions: vec![0, 1, 2],
                    received: version,
                }
                .into(),
            )
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
