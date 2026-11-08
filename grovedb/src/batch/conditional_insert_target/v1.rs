//! v1 of [`TreeCacheMerkByPath::conditional_insert_keeps_stored_element`],
//! selected by V4+. Differs from v0 only in reading whether the key already
//! holds an element, with the existence read the insert's own level makes.

use std::collections::hash_map::Entry as HashMapEntry;

use grovedb_costs::{cost_return_on_error, CostResult, OperationCost};
use grovedb_merk::{element::exists::ElementExistsInStorageExtensions, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::super::TreeCacheMerkByPath;
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
    pub(super) fn conditional_insert_keeps_stored_element_v1(
        &mut self,
        qualified_path: &[Vec<u8>],
        element: &Element,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();
        let (key, parent_path) = qualified_path
            .split_last()
            .expect("follow_reference_get_value_hash refuses an empty path");
        let merk = match self.merks.entry(parent_path.to_vec()) {
            HashMapEntry::Occupied(o) => o.into_mut(),
            HashMapEntry::Vacant(v) => v.insert(cost_return_on_error!(
                &mut cost,
                (self.get_merk_fn)(parent_path, false)
            )),
        };
        // The read `insert_if_not_exists_into_batch_operations` makes: it
        // goes to storage, which holds the state from before the batch, so
        // it decides alike whether or not the insert's level has run.
        element
            .element_at_key_already_exists(merk, key, grove_version)
            .map_err(Into::into)
            .add_cost(cost)
    }
}
