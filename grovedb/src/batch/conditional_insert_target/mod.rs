//! What a reference the batch resolves through a pending `InsertIfNotExists`
//! reaches (`apply_batch.reference_through_conditional_insert`).
//!
//! A skip-mode `InsertIfNotExists` over a key that already holds an element
//! writes nothing. The two versions differ only in whether resolution looks
//! at that:
//!
//! - v0 (V1..V3): resolution always reaches the op's element, so a reference
//!   through a skipped insert commits to an element that is never stored and
//!   `verify_grovedb` reports it. Preserved for replay.
//! - v1 (V4+): when the key already holds an element, resolution reaches
//!   that element instead, found by the same existence read the insert's own
//!   level makes. Storage holds the state from before the batch, so the
//!   answer does not depend on which level runs first.

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::Merk;
use grovedb_storage::StorageContext;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use super::TreeCacheMerkByPath;
use crate::{Element, Error};

impl<'db, S, F, F2> TreeCacheMerkByPath<S, F, F2>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    F2: FnMut(
        &[Vec<u8>],
        Option<&Element>,
    ) -> CostResult<Vec<(grovedb_element::indexed::IndexAxis, Merk<S>)>, Error>,
    S: StorageContext<'db>,
{
    /// Whether the pending `InsertIfNotExists` of `element` at
    /// `qualified_path` leaves the element already stored there in place, so
    /// that a reference through it resolves that element rather than the
    /// op's.
    pub(super) fn conditional_insert_keeps_stored_element(
        &mut self,
        qualified_path: &[Vec<u8>],
        element: &Element,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        match grove_version
            .grovedb_versions
            .apply_batch
            .reference_through_conditional_insert
        {
            0 => self.conditional_insert_keeps_stored_element_v0(),
            1 => self.conditional_insert_keeps_stored_element_v1(
                qualified_path,
                element,
                grove_version,
            ),
            version => Err(GroveVersionError::UnknownVersionMismatch {
                method: "apply_batch: reference through conditional insert".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .into())
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
