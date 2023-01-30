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

//! Merk tree commit

#[cfg(feature = "full")]
use costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};

#[cfg(feature = "full")]
use super::Tree;
#[cfg(feature = "full")]
use crate::error::Error;
use crate::tree::kv::ValueDefinedCostType;

#[cfg(feature = "full")]
/// To be used when committing a tree (writing it to a store after applying the
/// changes).
pub trait Commit {
    /// Called once per updated node when a finalized tree is to be written to a
    /// backing store or cache.
    fn write(
        &mut self,
        tree: &mut Tree,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<
            (bool, Option<ValueDefinedCostType>),
            Error,
        >,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> Result<(), Error>;

    /// Called once per node after writing a node and its children. The returned
    /// tuple specifies whether or not to prune the left and right child nodes,
    /// respectively. For example, returning `(true, true)` will prune both
    /// nodes, removing them from memory.
    fn prune(&self, _tree: &Tree) -> (bool, bool) {
        (true, true)
    }
}

#[cfg(feature = "full")]
/// A `Commit` implementation which does not write to a store and does not prune
/// any nodes from the Tree. Useful when only keeping a tree in memory.
pub struct NoopCommit {}

#[cfg(feature = "full")]
impl Commit for NoopCommit {
    fn write(
        &mut self,
        _tree: &mut Tree,
        _old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        _update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<
            (bool, Option<ValueDefinedCostType>),
            Error,
        >,
        _section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn prune(&self, _tree: &Tree) -> (bool, bool) {
        (false, false)
    }
}
