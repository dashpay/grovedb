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

//! Get operations and costs

#[cfg(feature = "full")]
mod average_case;
#[cfg(feature = "full")]
mod query;
#[cfg(feature = "full")]
mod worst_case;

#[cfg(feature = "full")]
use std::collections::HashSet;

use costs::cost_return_on_error_no_add;
#[cfg(feature = "full")]
use costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
#[cfg(feature = "full")]
use merk::Merk;
use path::SubtreePath;
#[cfg(feature = "full")]
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext},
    StorageContext,
};

#[cfg(feature = "full")]
use crate::{
    reference_path::{path_from_reference_path_type, path_from_reference_qualified_path_type},
    util::storage_context_optional_tx,
    Element, Error, GroveDb, Transaction, TransactionArg,
};

#[cfg(feature = "full")]
/// Limit of possible indirections
pub const MAX_REFERENCE_HOPS: usize = 10;

#[cfg(feature = "full")]
impl GroveDb {
    /// Get an element from the backing store
    /// Merk Caching is on by default
    /// use get_caching_optional if no caching is desired
    pub fn get<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.get_caching_optional(path, key, true, transaction)
    }

    /// Get an element from the backing store
    /// Merk Caching can be set
    pub fn get_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
        transaction: TransactionArg,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        match cost_return_on_error!(
            &mut cost,
            self.get_raw_caching_optional(path_iter.clone(), key, allow_cache, transaction)
        ) {
            Element::Reference(reference_path, ..) => {
                let path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(reference_path, path_iter, Some(key))
                        .wrap_with_cost(OperationCost::default())
                );
                self.follow_reference(path, allow_cache, transaction)
                    .add_cost(cost)
            }
            other => Ok(other).wrap_with_cost(cost),
        }
    }

    /// Follow reference
    pub fn follow_reference(
        &self,
        mut path: Vec<Vec<u8>>,
        allow_cache: bool,
        transaction: TransactionArg,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();

        let mut hops_left = MAX_REFERENCE_HOPS;
        let mut current_element;
        let mut visited = HashSet::new();

        while hops_left > 0 {
            if visited.contains(&path) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            if let Some((key, path_slice)) = path.split_last() {
                current_element = cost_return_on_error!(
                    &mut cost,
                    self.get_raw_caching_optional(
                        path_slice.iter().map(|x| x.as_slice()),
                        key,
                        allow_cache,
                        transaction
                    )
                    .map_err(|e| match e {
                        Error::PathParentLayerNotFound(p) => {
                            Error::CorruptedReferencePathParentLayerNotFound(p)
                        }
                        Error::PathKeyNotFound(p) => {
                            Error::CorruptedReferencePathKeyNotFound(p)
                        }
                        Error::PathNotFound(p) => {
                            Error::CorruptedReferencePathNotFound(p)
                        }
                        _ => e,
                    })
                )
            } else {
                return Err(Error::CorruptedPath("empty path")).wrap_with_cost(cost);
            }
            visited.insert(path.clone());
            match current_element {
                Element::Reference(reference_path, ..) => {
                    path = cost_return_on_error!(
                        &mut cost,
                        path_from_reference_qualified_path_type(reference_path, &path)
                            .wrap_with_cost(OperationCost::default())
                    )
                }
                other => return Ok(other).wrap_with_cost(cost),
            }
            hops_left -= 1;
        }
        Err(Error::ReferenceLimit).wrap_with_cost(cost)
    }

    /// Get tree item without following references
    pub fn get_raw<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        self.get_raw_caching_optional(path, key, true, transaction)
    }

    /// Get tree item without following references
    pub fn get_raw_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
        transaction: TransactionArg,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.get_raw_on_transaction_caching_optional(path, key, allow_cache, transaction)
        } else {
            self.get_raw_without_transaction_caching_optional(path, key, allow_cache)
        }
    }

    /// Get tree item without following references
    pub fn get_raw_optional<B: AsRef<[u8]>>(
        &self,
        path: &[B],
        key: &[u8],
        transaction: TransactionArg,
    ) -> CostResult<Option<Element>, Error> {
        self.get_raw_optional_caching_optional(path, key, true, transaction)
    }

    /// Get tree item without following references
    pub fn get_raw_optional_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
        transaction: TransactionArg,
    ) -> CostResult<Option<Element>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.get_raw_optional_on_transaction_caching_optional(
                path,
                key,
                allow_cache,
                transaction,
            )
        } else {
            self.get_raw_optional_without_transaction_caching_optional(path, key, allow_cache)
        }
    }

    /// Get tree item without following references
    pub(crate) fn get_raw_on_transaction_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
        transaction: &Transaction,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let merk_to_get_from: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path.into_iter(), transaction)
                .map_err(|e| match e {
                    Error::InvalidParentLayerPath(s) => {
                        Error::PathParentLayerNotFound(s)
                    }
                    _ => e,
                })
        );

        Element::get(&merk_to_get_from, key, allow_cache).add_cost(cost)
    }

    /// Get tree item without following references
    pub(crate) fn get_raw_optional_on_transaction_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
        transaction: &Transaction,
    ) -> CostResult<Option<Element>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();
        let merk_result = self
            .open_transactional_merk_at_path(path.into_iter(), transaction)
            .map_err(|e| match e {
                Error::InvalidParentLayerPath(s) => Error::PathParentLayerNotFound(s),
                _ => e,
            })
            .unwrap_add_cost(&mut cost);
        let merk: Option<Merk<PrefixedRocksDbTransactionContext>> = cost_return_on_error_no_add!(
            &cost,
            match merk_result {
                Ok(result) => Ok(Some(result)),
                Err(Error::PathParentLayerNotFound(_)) | Err(Error::InvalidParentLayerPath(_)) =>
                    Ok(None),
                Err(e) => Err(e),
            }
        );

        if let Some(merk_to_get_from) = merk {
            Element::get_optional(&merk_to_get_from, key, allow_cache).add_cost(cost)
        } else {
            Ok(None).wrap_with_cost(cost)
        }
    }

    /// Get tree item without following references
    pub(crate) fn get_raw_without_transaction_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let merk_to_get_from: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path.into_iter())
                .map_err(|e| match e {
                    Error::InvalidParentLayerPath(s) => {
                        Error::PathParentLayerNotFound(s)
                    }
                    _ => e,
                })
        );

        Element::get(&merk_to_get_from, key, allow_cache).add_cost(cost)
    }

    /// Get tree item without following references
    pub(crate) fn get_raw_optional_without_transaction_caching_optional<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        allow_cache: bool,
    ) -> CostResult<Option<Element>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();
        let merk_result = self
            .open_non_transactional_merk_at_path(path.into_iter())
            .map_err(|e| match e {
                Error::InvalidParentLayerPath(s) => Error::PathParentLayerNotFound(s),
                _ => e,
            })
            .unwrap_add_cost(&mut cost);
        let merk: Option<Merk<PrefixedRocksDbStorageContext>> = cost_return_on_error_no_add!(
            &cost,
            match merk_result {
                Ok(result) => Ok(Some(result)),
                Err(Error::PathParentLayerNotFound(_)) | Err(Error::InvalidParentLayerPath(_)) =>
                    Ok(None),
                Err(e) => Err(e),
            }
        );

        if let Some(merk_to_get_from) = merk {
            Element::get_optional(&merk_to_get_from, key, allow_cache).add_cost(cost)
        } else {
            Ok(None).wrap_with_cost(cost)
        }
    }

    /// Does tree element exist without following references
    /// There is no cache for has_raw
    pub fn has_raw<B: AsRef<[u8]>>(
        &self,
        path: &[B],
        key: &[u8],
        transaction: TransactionArg,
    ) -> CostResult<bool, Error> {
        // Merk's items should be written into data storage and checked accordingly
        storage_context_optional_tx!(self.db, path, transaction, storage, {
            storage.flat_map(|s| s.get(key).map_err(|e| e.into()).map_ok(|x| x.is_some()))
        })
    }

    fn check_subtree_exists<'b, B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        error: Error, // TODO: this requires constructing the error for every call
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if let Some((parent_path, parent_key)) = path.derive_parent() {
            let element = if let Some(transaction) = transaction {
            let merk_to_get_from: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(parent_path, transaction)
            );

            Element::get(&merk_to_get_from, parent_key, true)
        } else {
            let merk_to_get_from: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
                &mut cost,
                self.open_non_transactional_merk_at_path(parent_path)
            );

            Element::get(&merk_to_get_from, parent_key, true)
        }
        .unwrap_add_cost(&mut cost);
            match element {
                Ok(Element::Tree(..)) | Ok(Element::SumTree(..)) => Ok(()).wrap_with_cost(cost),
                Ok(_) | Err(Error::PathKeyNotFound(_)) => Err(error).wrap_with_cost(cost),
                Err(e) => Err(e).wrap_with_cost(cost),
            }
        } else {
            Ok(()).wrap_with_cost(cost)
        }
    }

    /// Check that subtree exists with path not found error
    pub(crate) fn check_subtree_exists_path_not_found<'b, B, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path = path.into();
        self.check_subtree_exists(
            &path,
            transaction,
            Error::PathNotFound(format!(
                "subtree doesn't exist at path {:?}",
                path.to_owned()
                    .into_iter()
                    .map(hex::encode)
                    .collect::<Vec<String>>()
            )),
        )
    }

    /// Check subtree exists with invalid path error
    pub fn check_subtree_exists_invalid_path<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.check_subtree_exists(
            path,
            transaction,
            Error::InvalidPath("subtree doesn't exist".to_owned()),
        )
    }
}
