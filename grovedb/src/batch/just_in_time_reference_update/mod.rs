//! The value hashes a reference written in a batch commits to when its
//! target is a pending write of the same batch.
//!
//! `TreeCacheMerkByPath::process_old_element_flags` gives the hash of the
//! bytes a pending item write finally stores, after the apply has run the
//! caller's flags update on it. Each `v*.rs` defines a
//! `process_old_element_flags_v*`, selected by
//! `apply_batch.same_batch_reference_target_prediction`:
//!
//! - version 0 is a hand-written copy of Merk's just-in-time value update
//!   over the target's bytes as the Merk cache holds them. For a sum item it
//!   assumes the new element keeps the stored flags and does not consult
//!   the flags update, while the apply stores the flags the flags update
//!   leaves it. When those differ (an owned sum item updated in a later
//!   epoch, an unowned one gaining an owner) the reference commits to bytes
//!   that are never stored and the grove no longer verifies. Grove v1..v3
//!   are consensus-locked to this.
//! - version 1 hashes the bytes the target stores once this pass has written
//!   it, and otherwise predicts them by running Merk's own just-in-time value
//!   update on the stored bytes and the new ones, with the put the apply
//!   performs, once per target. The reference commits to exactly the bytes
//!   the apply stores.
//!
//! `settle_owner_changes` is new and has no legacy behaviour to keep, so a
//! batch with it predicts with version 1 on every grove version.

mod v0;
mod v1;

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error_into_no_add,
    storage_cost::{removal::StorageRemovedBytes, transition::ElementFlagsUpdate, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{tree::value_hash, tree_type::TreeType, CryptoHash, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{batch::TreeCacheMerkByPath, Element, ElementFlags, Error};

impl<'db, S, F, F2> TreeCacheMerkByPath<S, F, F2>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    F2: FnMut(
        &[Vec<u8>],
        Option<&Element>,
    ) -> CostResult<Vec<(grovedb_element::indexed::IndexAxis, Merk<S>)>, Error>,
    S: StorageContext<'db>,
{
    /// The hash a reference written in this batch commits to for a pending
    /// write of a backward-references ITEM at `qualified_path`: the logical
    /// (stripped) hash of the bytes the apply finally stores there — the
    /// preprocessor's prediction when the caller's flags update may rewrite
    /// them, the op's own element otherwise.
    pub(crate) fn landed_backward_references_item_value_hash(
        &self,
        qualified_path: &[Vec<u8>],
        element: &Element,
        grove_version: &GroveVersion,
    ) -> CostResult<CryptoHash, Error> {
        let mut cost = OperationCost::default();
        let landed = self
            .landed_backward_references_items
            .get(qualified_path)
            .unwrap_or(element);
        let serialized = cost_return_on_error_into_no_add!(
            cost,
            landed
                .stripped_of_backward_references()
                .serialize(grove_version)
        );
        let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
        Ok(val_hash).wrap_with_cost(cost)
    }

    /// The value hash of the bytes a pending write of `new_element` at
    /// `qualified_path` over the stored `old_element` finally stores, after
    /// the apply has run the caller's flags update on it, so that a
    /// reference written in the same batch commits to them. `target_written`
    /// says this pass has already written the target, so the stored bytes
    /// are the ones it stores. See the module docs for the version
    /// semantics.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_old_element_flags<G, SR>(
        key: &[u8],
        qualified_path: &[Vec<u8>],
        serialized: Vec<u8>,
        new_element: &Element,
        old_element: Element,
        old_serialized_element: Vec<u8>,
        in_tree_type: TreeType,
        settle_owner_changes: bool,
        target_written: bool,
        predicted_targets: &mut HashMap<Vec<Vec<u8>>, Vec<u8>>,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
        grove_version: &GroveVersion,
    ) -> CostResult<CryptoHash, Error>
    where
        G: FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<ElementFlagsUpdate, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        match (
            grove_version
                .grovedb_versions
                .apply_batch
                .same_batch_reference_target_prediction,
            settle_owner_changes,
        ) {
            (0, false) => v0::process_old_element_flags_v0(
                key,
                &serialized,
                new_element,
                old_element,
                &old_serialized_element,
                in_tree_type,
                flags_update,
                split_removal_bytes,
                grove_version,
            ),
            (0, true) | (1, _) => v1::process_old_element_flags_v1(
                key,
                qualified_path,
                serialized,
                new_element,
                old_serialized_element,
                in_tree_type,
                target_written,
                predicted_targets,
                flags_update,
                split_removal_bytes,
                grove_version,
            ),
            (version, _) => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "process_old_element_flags".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
