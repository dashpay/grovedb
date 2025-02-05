//! Insert operations

mod v0;
mod v1;

#[cfg(feature = "minimal")]
use std::option::Option::None;

#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_merk::{tree::NULL_HASH, Merk, MerkOptions};
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_storage::rocksdb_storage::PrefixedRocksDbTransactionContext;
use grovedb_storage::{Storage, StorageBatch};
use grovedb_version::{check_grovedb_v0_with_cost, dispatch_version, version::GroveVersion};

use crate::util::TxRef;
#[cfg(feature = "minimal")]
use crate::{
    reference_path::path_from_reference_path_type, Element, Error, GroveDb, Transaction,
    TransactionArg,
};

#[cfg(feature = "minimal")]
#[derive(Clone)]
/// Insert options
pub struct InsertOptions {
    /// Validate insertion does not override
    pub validate_insertion_does_not_override: bool,
    /// Validate insertion does not override tree
    pub validate_insertion_does_not_override_tree: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
    /// Ensure proper maintenance of backward references when
    /// updating/overwriting/ deleting items that use bidirectional
    /// reference functionality of GroveDB. Since it requires additional
    /// seeks and checks by default we turn it off.
    pub propagate_backward_references: bool,
}

#[cfg(feature = "minimal")]
impl Default for InsertOptions {
    fn default() -> Self {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: true,
            base_root_storage_is_free: true,
            propagate_backward_references: false,
        }
    }
}

#[cfg(feature = "minimal")]
impl InsertOptions {
    fn checks_for_override(&self) -> bool {
        self.validate_insertion_does_not_override_tree || self.validate_insertion_does_not_override
    }

    pub fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Insert a GroveDB element given a path to the subtree and the key to
    /// insert at
    pub fn insert<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        options: Option<InsertOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let subtree_path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();

        let tx = TxRef::new(&self.db, transaction);

        let mut cost = Default::default();

        cost_return_on_error!(
            &mut cost,
            self.insert_on_transaction(
                subtree_path,
                key,
                element,
                options.unwrap_or_default(),
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        dispatch_version!(
            "insert_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_on_transaction,
            0 => {
                v0::insert_on_transaction(
                    self,
                    path,
                    key,
                    element,
                    options,
                    transaction,
                    batch,
                    grove_version
                )
            }
            1 => {
                v1::insert_on_transaction(
                    self,
                    path,
                    key,
                    element,
                    options,
                    transaction,
                    batch,
                    grove_version
                )
            }
        )
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_element_on_transaction<'db, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        check_grovedb_v0_with_cost!(
            "add_element_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .add_element_on_transaction
        );

        let mut cost = OperationCost::default();

        let mut subtree_to_insert_into = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        // if we don't allow a tree override then we should check

        if options.checks_for_override() {
            let maybe_element_bytes = cost_return_on_error!(
                &mut cost,
                subtree_to_insert_into
                    .get(
                        key,
                        true,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| Error::CorruptedData(e.to_string()))
            );
            if let Some(element_bytes) = maybe_element_bytes {
                if options.validate_insertion_does_not_override {
                    return Err(Error::OverrideNotAllowed(
                        "insertion not allowed to override",
                    ))
                    .wrap_with_cost(cost);
                }
                if options.validate_insertion_does_not_override_tree {
                    let element = cost_return_on_error_no_add!(
                        cost,
                        Element::deserialize(element_bytes.as_slice(), grove_version).map_err(
                            |_| {
                                Error::CorruptedData(String::from("unable to deserialize element"))
                            }
                        )
                    );
                    if element.is_any_tree() {
                        return Err(Error::OverrideNotAllowed(
                            "insertion not allowed to override tree",
                        ))
                        .wrap_with_cost(cost);
                    }
                }
            }
        }

        match element {
            Element::Reference(ref reference_path, ..) => {
                let path = path.to_vec(); // TODO: need for support for references in path library
                let reference_path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(reference_path.clone(), &path, Some(key))
                        .wrap_with_cost(OperationCost::default())
                );

                let referenced_item = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(
                        reference_path.as_slice().into(),
                        false,
                        transaction,
                        grove_version
                    )
                );

                let referenced_element_value_hash =
                    cost_return_on_error!(&mut cost, referenced_item.value_hash(grove_version));

                cost_return_on_error!(
                    &mut cost,
                    element.insert_reference(
                        &mut subtree_to_insert_into,
                        key,
                        referenced_element_value_hash,
                        Some(options.as_merk_options()),
                        grove_version,
                    )
                );
            }
            Element::Tree(ref value, _)
            | Element::SumTree(ref value, ..)
            | Element::BigSumTree(ref value, ..)
            | Element::CountTree(ref value, ..) => {
                if value.is_some() {
                    return Err(Error::InvalidCodeExecution(
                        "a tree should be empty at the moment of insertion when not using batches",
                    ))
                    .wrap_with_cost(cost);
                } else {
                    cost_return_on_error!(
                        &mut cost,
                        element.insert_subtree(
                            &mut subtree_to_insert_into,
                            key,
                            NULL_HASH,
                            Some(options.as_merk_options()),
                            grove_version
                        )
                    );
                }
            }
            _ => {
                cost_return_on_error!(
                    &mut cost,
                    element.insert(
                        &mut subtree_to_insert_into,
                        key,
                        Some(options.as_merk_options()),
                        grove_version
                    )
                );
            }
        }

        Ok(subtree_to_insert_into).wrap_with_cost(cost)
    }

    /// Insert if not exists
    /// Insert if not exists
    ///
    /// Inserts an element at the specified path and key if it does not already
    /// exist.
    ///
    /// # Arguments
    ///
    /// * `path` - The path where the element should be inserted.
    /// * `key` - The key under which the element should be inserted.
    /// * `element` - The element to insert.
    /// * `transaction` - The transaction argument, if any.
    /// * `grove_version` - The GroveDB version.
    ///
    /// # Returns
    ///
    /// Returns a `CostResult<bool, Error>` indicating whether the element was
    /// inserted (`true`) or already existed (`false`).
    pub fn insert_if_not_exists<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_not_exists
        );

        let mut cost = OperationCost::default();
        let subtree_path: SubtreePath<_> = path.into();

        if cost_return_on_error!(
            &mut cost,
            self.has_raw(subtree_path.clone(), key, transaction, grove_version)
        ) {
            Ok(false).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| true)
                .add_cost(cost)
        }
    }

    /// Insert if not exists
    /// If the item does exist return it
    ///
    /// Inserts an element at the given `path` and `key` if it does not exist.
    /// If the element already exists, returns the existing element.
    ///
    /// # Arguments
    ///
    /// * `path` - The path where the element should be inserted.
    /// * `key` - The key under which the element should be inserted.
    /// * `element` - The element to insert.
    /// * `transaction` - The transaction argument, if any.
    /// * `grove_version` - The GroveDB version.
    ///
    /// # Returns
    ///
    /// Returns a `CostResult<Option<Element>, Error>`, where
    /// `Ok(Some(element))` is the existing element if it was found, or
    /// `Ok(None)` if the new element was inserted.
    pub fn insert_if_not_exists_return_existing_element<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists_return_existing_element",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_not_exists_return_existing_element
        );

        let mut cost = OperationCost::default();
        let subtree_path: SubtreePath<_> = path.into();

        let previous_element = cost_return_on_error!(
            &mut cost,
            self.get_raw_optional(subtree_path.clone(), key, transaction, grove_version)
        );
        if previous_element.is_some() {
            Ok(previous_element).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| None)
                .add_cost(cost)
        }
    }

    /// Insert if the value changed
    /// We return if the value was inserted
    /// If the value was changed then we return the previous element
    pub fn insert_if_changed_value<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_changed_value
        );

        let mut cost = OperationCost::default();
        let subtree_path: SubtreePath<B> = path.into();

        let previous_element = cost_return_on_error!(
            &mut cost,
            self.get_raw_optional(subtree_path.clone(), key, transaction, grove_version)
        );
        let needs_insert = match &previous_element {
            None => true,
            Some(previous_element) => previous_element != &element,
        };
        if !needs_insert {
            Ok((false, None)).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| (true, previous_element))
                .add_cost(cost)
        }
    }
}
