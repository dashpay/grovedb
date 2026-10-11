//! v0 of [`TreeCacheMerkByPath::conditional_insert_keeps_stored_element`],
//! selected by V1..V3. Differs from v1 only in never reading the key: a
//! reference through a pending `InsertIfNotExists` always reaches the op's
//! element.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::Merk;
use grovedb_storage::StorageContext;

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
    pub(super) fn conditional_insert_keeps_stored_element_v0(&mut self) -> CostResult<bool, Error> {
        Ok(false).wrap_with_cost(OperationCost::default())
    }
}
