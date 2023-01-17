// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Check if empty tree operations

#[cfg(feature = "full")]
use costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

#[cfg(feature = "full")]
use crate::{util::merk_optional_tx, Element, Error, GroveDb, TransactionArg};

#[cfg(feature = "full")]
impl GroveDb {
    /// Check if it's an empty tree
    pub fn is_empty_tree<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator + ExactSizeIterator,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter().peekable();
        cost_return_on_error!(
            &mut cost,
            self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)
        );
        merk_optional_tx!(&mut cost, self.db, path_iter, transaction, subtree, {
            Ok(subtree.is_empty_tree().unwrap_add_cost(&mut cost)).wrap_with_cost(cost)
        })
    }
}
